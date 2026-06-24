use std::fmt;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// A tracing layer that forwards log entries above a minimum level to a Discord webhook.
pub struct WebhookLayer {
    sender: mpsc::UnboundedSender<LogEntry>,
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

impl WebhookLayer {
    /// Spawns a background flusher task and returns the layer.
    pub fn new(
        webhook_url: String,
        http_client: reqwest::Client,
        min_level: Level,
        plain_text: bool,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(flush_loop(
            rx,
            webhook_url,
            http_client,
            min_level,
            plain_text,
        ));
        Self { sender: tx }
    }
}

impl<S: Subscriber> Layer<S> for WebhookLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = *meta.level();
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
        let _ = self.sender.send(entry);
    }
}

/// Background task that batches log entries and sends them to Discord.
async fn flush_loop(
    mut rx: mpsc::UnboundedReceiver<LogEntry>,
    webhook_url: String,
    http_client: reqwest::Client,
    min_level: Level,
    plain_text: bool,
) {
    let mut buffer: Vec<LogEntry> = Vec::new();

    loop {
        let recv = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;

        match recv {
            Ok(Some(entry)) => {
                // Filter by min level
                if entry.level <= min_level {
                    buffer.push(entry);
                    if buffer.len() < 10 {
                        continue;
                    }
                }
            }
            Ok(None) => {
                // Channel closed, flush remaining and exit
                if !buffer.is_empty() {
                    send_batch(&http_client, &webhook_url, &buffer, plain_text).await;
                }
                break;
            }
            Err(_) => {
                // Timeout — flush whatever we have
            }
        }

        if !buffer.is_empty() {
            send_batch(&http_client, &webhook_url, &buffer, plain_text).await;
            buffer.clear();
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
            let emoji = match entry.level {
                Level::ERROR => "🔴",
                Level::WARN => "🟡",
                Level::INFO => "🔵",
                Level::DEBUG => "⚙️",
                Level::TRACE => "🧬",
            };
            let log_line = format!(
                "{} **[{}]** `{}`: {}\n",
                emoji, entry.level, target_clean, msg_truncated
            );

            // Redact secrets in the log line!
            let log_line_redacted = crate::logging::redact_secrets(&log_line);

            if current_msg.len() + log_line_redacted.len() > 1900 {
                let body = serde_json::json!({ "content": current_msg });
                let _ = http_client.post(webhook_url).json(&body).send().await;
                current_msg = String::new();
            }
            current_msg.push_str(&log_line_redacted);
        }

        if !current_msg.is_empty() {
            let body = serde_json::json!({ "content": current_msg });
            let _ = http_client.post(webhook_url).json(&body).send().await;
        }
    } else {
        let mut embeds = Vec::new();
        for entry in entries {
            let color = match entry.level {
                Level::ERROR => 0xED4245, // red
                Level::WARN => 0xFEE75C,  // yellow
                Level::INFO => 0x3498DB,  // blue
                _ => 0x979C9F,            // grey
            };
            let target_clean = entry
                .target
                .strip_prefix("serenya::")
                .unwrap_or(&entry.target);
            let title = format!("{} — {}", entry.level, target_clean);
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
            let result = http_client.post(webhook_url).json(&body).send().await;
            if let Err(e) = result {
                eprintln!("Failed to send webhook log: {e}");
            }
        }
    }
}
