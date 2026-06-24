use aho_corasick::AhoCorasick;
use std::sync::{Mutex, OnceLock};

static SECRETS_TO_REDACT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static REDACTOR: OnceLock<Mutex<Option<AhoCorasick>>> = OnceLock::new();

pub fn register_secret_to_redact(secret: &str) {
    let trimmed = secret.trim();
    if trimmed.is_empty() || trimmed.len() < 4 {
        return;
    }
    let registry = SECRETS_TO_REDACT.get_or_init(|| Mutex::new(Vec::new()));
    let redactor = REDACTOR.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = registry.lock() {
        let sec = trimmed.to_owned();
        if !guard.contains(&sec) {
            guard.push(sec);
            if let Ok(mut r_guard) = redactor.lock() {
                *r_guard = AhoCorasick::new(guard.iter()).ok();
            }
        }
    }
}

pub fn redact_secrets(input: &str) -> String {
    if let Some(redactor_mutex) = REDACTOR.get() {
        if let Ok(guard) = redactor_mutex.lock() {
            if let Some(ac) = guard.as_ref() {
                return ac.replace_all(input, &vec!["[REDACTED]"; ac.patterns_len()]);
            }
        }
    }
    input.to_owned()
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
