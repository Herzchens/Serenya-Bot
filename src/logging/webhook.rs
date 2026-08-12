use std::fmt;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

const WEBHOOK_CHANNEL_CAPACITY: usize = 512;
const WEBHOOK_BATCH_SIZE: usize = 10;
const WEBHOOK_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const WEBHOOK_SHUTDOWN_DRAIN_LIMIT: usize = 512;
const WEBHOOK_MAX_ATTEMPTS: usize = 3;
const WEBHOOK_MAX_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(10);

static DROPPED_WEBHOOK_LOGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn dropped_webhook_logs() -> u64 {
    DROPPED_WEBHOOK_LOGS.load(std::sync::atomic::Ordering::Relaxed)
}

/// A tracing layer that forwards log entries above a minimum level to a Discord webhook.
pub struct WebhookLayer {
    sender: mpsc::Sender<LogEntry>,
    min_level: Level,
}

struct LogEntry {
    level: Level,
    message: String,
    target: String,
}

struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

fn is_safe_webhook_field(name: &str) -> bool {
    matches!(
        name,
        "guild_id"
            | "client"
            | "client_kind"
            | "resolve_source"
            | "provider"
            | "status"
            | "speed"
            | "elapsed_ms"
            | "attempt"
            | "kind"
            | "action"
            | "cache"
            | "track"
            | "command"
            | "error_class"
    )
}

fn push_safe_webhook_field(fields: &mut Vec<String>, name: &str, rendered: String) {
    if is_safe_webhook_field(name) {
        fields.push(format!("{name}={rendered}"));
    }
}

impl MessageVisitor {
    fn into_message(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields.join(" ")
        } else {
            format!("{} | {}", self.message, self.fields.join(" "))
        }
    }
}

fn prepare_log_entry(level: Level, target: &str, message: String) -> LogEntry {
    LogEntry {
        level,
        message: crate::logging::redact_secrets(&message),
        target: crate::logging::redact_secrets(target),
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            self.message = rendered;
        } else {
            push_safe_webhook_field(&mut self.fields, field.name(), rendered);
        }
    }
}

use std::sync::Mutex;

static SHUTDOWN_TX: Mutex<Option<tokio::sync::oneshot::Sender<tokio::sync::oneshot::Sender<()>>>> =
    Mutex::new(None);

pub async fn shutdown() {
    if let Some(shutdown_tx) = SHUTDOWN_TX.lock().ok().and_then(|mut guard| guard.take()) {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        if shutdown_tx.send(ack_tx).is_ok() {
            // Wait up to 5 seconds for final logs to flush
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), ack_rx).await;
        }
    }
}

fn get_emoji_tag_and_color(level: Level, message: &str) -> (&'static str, &'static str, u32) {
    let msg_lower = message.to_lowercase();
    match level {
        Level::ERROR => ("🔴", "ERROR", 0xED4245),
        Level::WARN => ("🟡", "WARN", 0xFEE75C),
        Level::INFO => {
            if msg_lower.contains("starting")
                || msg_lower.contains("ready")
                || msg_lower.contains("register")
                || msg_lower.contains("loaded")
            {
                ("🟢", "START", 0x2ECC71) // Green for start/init
            } else if msg_lower.contains("shutdown")
                || msg_lower.contains("shut down")
                || msg_lower.contains("signal received")
            {
                ("🟠", "SHUTDOWN", 0xE67E22) // Orange for shutdown
            } else {
                ("🔵", "INFO", 0x3498DB) // Blue for normal info
            }
        }
        Level::DEBUG => ("⚙️", "DEBUG", 0x979C9F),
        Level::TRACE => ("🧬", "TRACE", 0x979C9F),
    }
}

impl WebhookLayer {
    /// Spawns a background flusher task and returns the layer.
    pub fn new(
        webhook_url: String,
        http_client: reqwest::Client,
        min_level: Level,
        plain_text: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel(WEBHOOK_CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        if let Ok(mut guard) = SHUTDOWN_TX.lock() {
            *guard = Some(shutdown_tx);
        }

        tokio::spawn(flush_loop(
            rx,
            shutdown_rx,
            webhook_url,
            http_client,
            min_level,
            plain_text,
        ));
        Self {
            sender: tx,
            min_level,
        }
    }
}

impl<S: Subscriber> Layer<S> for WebhookLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = *meta.level();

        if level > self.min_level {
            return;
        }

        let target = meta.target();

        let mut visitor = MessageVisitor {
            message: String::new(),
            fields: Vec::new(),
        };
        event.record(&mut visitor);

        let entry = prepare_log_entry(level, target, visitor.into_message());

        if let Err(mpsc::error::TrySendError::Full(_)) = self.sender.try_send(entry) {
            DROPPED_WEBHOOK_LOGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Background task that batches log entries and sends them to Discord.
async fn flush_loop(
    mut rx: mpsc::Receiver<LogEntry>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<tokio::sync::oneshot::Sender<()>>,
    webhook_url: String,
    http_client: reqwest::Client,
    min_level: Level,
    plain_text: bool,
) {
    let mut buffer: Vec<LogEntry> = Vec::new();

    loop {
        let sleep_fut = tokio::time::sleep(WEBHOOK_FLUSH_INTERVAL);
        tokio::pin!(sleep_fut);

        tokio::select! {
            entry_opt = rx.recv() => {
                match entry_opt {
                    Some(entry) => {
                        if entry.level <= min_level {
                            buffer.push(entry);
                            if buffer.len() >= WEBHOOK_BATCH_SIZE {
                                send_batch(&http_client, &webhook_url, &buffer, plain_text).await;
                                buffer.clear();
                            }
                        }
                    }
                    None => {
                        if !buffer.is_empty() {
                            send_batch(&http_client, &webhook_url, &buffer, plain_text).await;
                        }
                        break;
                    }
                }
            }
            ack_sender_res = &mut shutdown_rx => {
                if let Ok(ack_sender) = ack_sender_res {
                    rx.close();
                    let mut drained = 0;
                    while drained < WEBHOOK_SHUTDOWN_DRAIN_LIMIT {
                        if let Some(entry) = rx.recv().await {
                            drained += 1;
                            if entry.level <= min_level {
                                buffer.push(entry);
                            }
                        } else {
                            break;
                        }
                    }
                    if !buffer.is_empty() {
                        send_batch(&http_client, &webhook_url, &buffer, plain_text).await;
                    }
                    let _ = ack_sender.send(());
                }
                break;
            }
            _ = &mut sleep_fut, if !buffer.is_empty() => {
                send_batch(&http_client, &webhook_url, &buffer, plain_text).await;
                buffer.clear();
            }
        }
    }
}

fn parse_retry_after_seconds(value: &str) -> Option<std::time::Duration> {
    let seconds = value.trim().parse::<f64>().ok()?;
    std::time::Duration::try_from_secs_f64(seconds).ok()
}

async fn retry_after(response: reqwest::Response) -> Option<std::time::Duration> {
    if let Some(value) = response.headers().get(reqwest::header::RETRY_AFTER)
        && let Ok(value) = value.to_str()
        && let Some(delay) = parse_retry_after_seconds(value)
    {
        return Some(delay);
    }

    response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|body| body.get("retry_after").and_then(serde_json::Value::as_f64))
        .and_then(|seconds| std::time::Duration::try_from_secs_f64(seconds).ok())
}

async fn post_webhook_payload(
    http_client: &reqwest::Client,
    webhook_url: &str,
    body: &serde_json::Value,
) -> Result<(), String> {
    for attempt in 1..=WEBHOOK_MAX_ATTEMPTS {
        let response = match http_client.post(webhook_url).json(body).send().await {
            Ok(response) => response,
            Err(error) => {
                if attempt < WEBHOOK_MAX_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(250 * attempt as u64))
                        .await;
                    continue;
                }
                return Err(format!(
                    "webhook transport error after {attempt} attempts: {error}"
                ));
            }
        };
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let delay = retry_after(response)
                .await
                .unwrap_or_else(|| std::time::Duration::from_secs(1));
            if attempt < WEBHOOK_MAX_ATTEMPTS && delay <= WEBHOOK_MAX_RETRY_DELAY {
                tokio::time::sleep(delay).await;
                continue;
            }
            return Err(format!(
                "webhook rate limited with HTTP {status}; retry_after={delay:?}"
            ));
        }

        if status.is_server_error() && attempt < WEBHOOK_MAX_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_millis(250 * attempt as u64)).await;
            continue;
        }

        return Err(format!("webhook returned HTTP {status}"));
    }

    Err("webhook delivery attempts exhausted".to_owned())
}

async fn send_batch(
    http_client: &reqwest::Client,
    webhook_url: &str,
    entries: &[LogEntry],
    plain_text: bool,
) {
    if plain_text {
        let mut current_msg = String::new();

        for entry in entries {
            // Truncate entry message to avoid exceeding limits
            let msg_truncated = crate::utils::truncate_chars(&entry.message, 300);
            let target_clean = entry
                .target
                .strip_prefix("serenya::")
                .unwrap_or(&entry.target);
            let (emoji, tag, _) = get_emoji_tag_and_color(entry.level, &entry.message);
            let log_line = format!(
                "{} **[{}]** `{}`: {}\n",
                emoji, tag, target_clean, msg_truncated
            );

            // Redact secrets in the log line!
            let log_line_redacted = crate::logging::redact_secrets(&log_line);

            if current_msg.len() + log_line_redacted.len() > 1900 {
                let body = serde_json::json!({ "content": current_msg });
                if let Err(error) = post_webhook_payload(http_client, webhook_url, &body).await {
                    eprintln!("Failed to send webhook log: {error}");
                }
                current_msg = String::new();
            }
            current_msg.push_str(&log_line_redacted);
        }

        if !current_msg.is_empty() {
            let body = serde_json::json!({ "content": current_msg });
            if let Err(error) = post_webhook_payload(http_client, webhook_url, &body).await {
                eprintln!("Failed to send webhook log: {error}");
            }
        }
    } else {
        let mut embeds = Vec::new();
        for entry in entries {
            let (emoji, tag, color) = get_emoji_tag_and_color(entry.level, &entry.message);
            let target_clean = entry
                .target
                .strip_prefix("serenya::")
                .unwrap_or(&entry.target);
            let title = format!("{} {} — {}", emoji, tag, target_clean);
            let description = crate::utils::truncate_chars(&entry.message, 1997);
            // Redact secrets in the description!
            let description_redacted = crate::logging::redact_secrets(&description);

            embeds.push(serde_json::json!({
                "title": title,
                "description": description_redacted,
                "color": color,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }

        // Discord allows max 10 embeds per message
        for chunk in embeds.chunks(10) {
            let body = serde_json::json!({ "embeds": chunk });
            if let Err(error) = post_webhook_payload(http_client, webhook_url, &body).await {
                eprintln!("Failed to send webhook log: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_ordering() {
        assert!(Level::DEBUG > Level::INFO);
        assert!(Level::TRACE > Level::INFO);
        assert!(Level::ERROR <= Level::INFO);
        assert!(Level::WARN <= Level::INFO);
    }

    #[test]
    fn structured_fields_are_allowlisted_before_rendering() {
        let mut fields = Vec::new();
        push_safe_webhook_field(&mut fields, "client", "\"IOS\"".to_owned());
        push_safe_webhook_field(&mut fields, "guild_id", "42".to_owned());
        for sensitive in ["stream_url", "url", "query", "token", "cookie"] {
            push_safe_webhook_field(
                &mut fields,
                sensitive,
                "\"https://secret.example/signed?token=abc\"".to_owned(),
            );
        }
        let visitor = MessageVisitor {
            message: "probe succeeded".to_owned(),
            fields,
        };
        assert_eq!(
            visitor.into_message(),
            "probe succeeded | client=\"IOS\" guild_id=42"
        );
    }

    #[test]
    fn required_observability_fields_are_allowlisted() {
        let mut fields = Vec::new();
        push_safe_webhook_field(&mut fields, "track", "\"Example Track\"".to_owned());
        push_safe_webhook_field(&mut fields, "command", "\"play\"".to_owned());
        push_safe_webhook_field(&mut fields, "error_class", "\"Voice\"".to_owned());

        assert_eq!(
            fields,
            vec![
                "track=\"Example Track\"".to_owned(),
                "command=\"play\"".to_owned(),
                "error_class=\"Voice\"".to_owned(),
            ]
        );
        assert!(!is_safe_webhook_field("url"));
        assert!(!is_safe_webhook_field("token"));
    }

    #[test]
    fn queued_entry_is_redacted_before_async_buffering() {
        let secret = "webhook-buffer-secret-20260811";
        crate::logging::register_secret_to_redact(secret);
        let entry = prepare_log_entry(
            Level::INFO,
            "serenya::test",
            format!("access_token={secret}"),
        );

        assert_eq!(entry.message, "access_token=[REDACTED]");
        assert!(!entry.message.contains(secret));
    }

    #[test]
    fn retry_after_parser_accepts_fractional_seconds_and_rejects_invalid_values() {
        assert_eq!(
            parse_retry_after_seconds("0.25"),
            Some(std::time::Duration::from_millis(250))
        );
        assert_eq!(parse_retry_after_seconds("-1"), None);
        assert_eq!(parse_retry_after_seconds("not-a-number"), None);
    }

    async fn spawn_http_server(responses: Vec<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test webhook server");
        let address = listener.local_addr().expect("test webhook address");
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.expect("accept webhook request");
                let mut request = [0_u8; 4096];
                let _ = socket
                    .read(&mut request)
                    .await
                    .expect("read webhook request");
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("write webhook response");
            }
        });
        format!("http://{address}/webhook")
    }

    fn http_response(status: &str, extra_headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn webhook_retries_429_after_retry_after_then_succeeds() {
        let url = spawn_http_server(vec![
            http_response(
                "429 Too Many Requests",
                "Content-Type: application/json\r\nRetry-After: 0.01\r\n",
                r#"{"retry_after":0.01}"#,
            ),
            http_response("204 No Content", "", ""),
        ])
        .await;
        let body = serde_json::json!({"content": "test"});
        let result = post_webhook_payload(&reqwest::Client::new(), &url, &body).await;
        assert!(result.is_ok(), "expected retry to succeed: {result:?}");
    }

    #[tokio::test]
    async fn webhook_retries_transport_failure_then_succeeds() {
        let url =
            spawn_http_server(vec![String::new(), http_response("204 No Content", "", "")]).await;
        let body = serde_json::json!({"content": "test"});
        let result = post_webhook_payload(&reqwest::Client::new(), &url, &body).await;
        assert!(
            result.is_ok(),
            "expected transient transport failure to retry: {result:?}"
        );
    }

    #[tokio::test]
    async fn webhook_reports_non_retryable_http_error() {
        let url = spawn_http_server(vec![http_response(
            "400 Bad Request",
            "Content-Type: application/json\r\n",
            r#"{"message":"bad request"}"#,
        )])
        .await;
        let body = serde_json::json!({"content": "test"});
        let error = post_webhook_payload(&reqwest::Client::new(), &url, &body)
            .await
            .expect_err("HTTP 400 must not be treated as success");
        assert!(error.contains("400"));
    }
}
