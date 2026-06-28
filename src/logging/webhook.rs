use std::fmt;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

const WEBHOOK_CHANNEL_CAPACITY: usize = 512;
const WEBHOOK_BATCH_SIZE: usize = 10;
const WEBHOOK_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const WEBHOOK_SHUTDOWN_DRAIN_LIMIT: usize = 512;

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
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
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

/// Send a webhook request with retry logic that respects Discord rate limits.
async fn send_webhook_with_retry(
    http_client: &reqwest::Client,
    webhook_url: &str,
    payload: &serde_json::Value,
) {
    let mut backoff_ms = 100u64;
    let max_backoff_ms = 5000u64;
    let mut attempts = 0;
    const MAX_ATTEMPTS: u32 = 3;

    loop {
        attempts += 1;
        match http_client.post(webhook_url).json(payload).send().await {
            Ok(response) => {
                match response.status().as_u16() {
                    200..=299 => {
                        // Success
                        return;
                    }
                    429 => {
                        // Rate limited - extract retry-after header
                        let retry_after = response
                            .headers()
                            .get("Retry-After")
                            .and_then(|h| h.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .map(|s| std::time::Duration::from_secs(s))
                            .unwrap_or_else(|| std::time::Duration::from_millis(backoff_ms));
                        
                        tracing::warn!(
                            \"Discord webhook rate limited. Backoff: {:?} (attempt {}/{})\",
                            retry_after,
                            attempts,
                            MAX_ATTEMPTS
                        );
                        
                        if attempts >= MAX_ATTEMPTS {
                            tracing::error!(\"Discord webhook rate limit persisted after {} attempts. Dropping logs.\", MAX_ATTEMPTS);
                            return;
                        }
                        tokio::time::sleep(retry_after).await;
                    }
                    status_code => {
                        tracing::error!(
                            \"Discord webhook failed with status {}: {:?} (attempt {}/{})\",
                            status_code,
                            response.text().await.ok(),
                            attempts,
                            MAX_ATTEMPTS
                        );
                        
                        if attempts >= MAX_ATTEMPTS {
                            tracing::error!(\"Discord webhook failed after {} attempts. Dropping logs.\", MAX_ATTEMPTS);
                            return;
                        }
                        
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    \"Discord webhook HTTP error: {} (attempt {}/{})\",
                    e,
                    attempts,
                    MAX_ATTEMPTS
                );
                
                if attempts >= MAX_ATTEMPTS {
                    tracing::error!(\"Discord webhook HTTP error persisted after {} attempts. Dropping logs.\", MAX_ATTEMPTS);
                    return;
                }
                
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
            }
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
        };
        event.record(&mut visitor);

        let entry = LogEntry {
            level,
            message: visitor.message,
            target: target.to_owned(),
        };

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
                send_webhook_with_retry(http_client, webhook_url, &body).await;
                current_msg = String::new();
            }
            current_msg.push_str(&log_line_redacted);
        }

        if !current_msg.is_empty() {
            let body = serde_json::json!({ "content": current_msg });
            send_webhook_with_retry(http_client, webhook_url, &body).await;
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
            send_webhook_with_retry(http_client, webhook_url, &body).await;
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
        assert!(!(Level::ERROR > Level::INFO));
        assert!(!(Level::WARN > Level::INFO));
        assert!(!(Level::INFO > Level::INFO));
    }
}
