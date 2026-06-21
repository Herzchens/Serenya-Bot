use std::sync::Mutex;
use std::sync::OnceLock;

static SECRETS_TO_REDACT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

pub fn register_secret_to_redact(secret: &str) {
    let trimmed = secret.trim();
    if trimmed.is_empty() || trimmed.len() < 4 {
        return;
    }
    let registry = SECRETS_TO_REDACT.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut guard) = registry.lock() {
        let sec = trimmed.to_owned();
        if !guard.contains(&sec) {
            guard.push(sec);
        }
    }
}

pub fn redact_secrets(input: &str) -> String {
    let mut output = input.to_owned();
    if let Some(registry) = SECRETS_TO_REDACT.get()
        && let Ok(guard) = registry.lock()
    {
        for secret in guard.iter() {
            if !secret.is_empty() {
                output = output.replace(secret, "[REDACTED]");
            }
        }
    }
    output
}

pub struct RedactingWriter<W> {
    inner: W,
}

impl<W: std::io::Write> std::io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let s = String::from_utf8_lossy(buf);
        let redacted = redact_secrets(&s);
        self.inner.write_all(redacted.as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Clone)]
pub struct MakeRedactingWriter;

impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for MakeRedactingWriter {
    type Writer = RedactingWriter<std::io::Stdout>;

    fn make_writer(&self) -> Self::Writer {
        RedactingWriter {
            inner: std::io::stdout(),
        }
    }
}
