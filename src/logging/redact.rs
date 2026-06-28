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
    
    // Acquire registry lock, add secret, then IMMEDIATELY release before acquiring redactor lock
    let should_rebuild_redactor = {
        match registry.lock() {
            Ok(mut guard) => {
                let sec = trimmed.to_owned();
                if !guard.contains(&sec) {
                    guard.push(sec);
                    true
                } else {
                    false
                }
            }
            Err(poisoned) => {
                // Recover from poisoned mutex
                let mut guard = poisoned.into_inner();
                let sec = trimmed.to_owned();
                if !guard.contains(&sec) {
                    guard.push(sec);
                    true
                } else {
                    false
                }
            }
        }
    }; // Lock released here
    
    // Now acquire redactor lock independently
    if should_rebuild_redactor {
        match redactor.lock() {
            Ok(mut r_guard) => {
                let secrets = match registry.lock() {
                    Ok(g) => g.iter().cloned().collect::<Vec<_>>(),
                    Err(poisoned) => poisoned.into_inner().iter().cloned().collect::<Vec<_>>(),
                };
                *r_guard = AhoCorasick::new(secrets.iter()).ok();
            }
            Err(poisoned) => {
                let mut r_guard = poisoned.into_inner();
                let secrets = match registry.lock() {
                    Ok(g) => g.iter().cloned().collect::<Vec<_>>(),
                    Err(poisoned) => poisoned.into_inner().iter().cloned().collect::<Vec<_>>(),
                };
                *r_guard = AhoCorasick::new(secrets.iter()).ok();
            }
        }
    }
}

pub fn redact_secrets(input: &str) -> String {
    if let Some(redactor_mutex) = REDACTOR.get() {
        // Try to acquire lock, recover from poison if needed
        let guard = match redactor_mutex.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!("Redaction mutex was poisoned, recovering...");
                poisoned.into_inner()
            }
        };
        if let Some(ac) = guard.as_ref() {
            return ac.replace_all(input, &vec!["[REDACTED]"; ac.patterns_len()]);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_from_poisoned_redaction_lock() {
        let registry = SECRETS_TO_REDACT.get_or_init(|| Mutex::new(Vec::new()));
        let _ = std::panic::catch_unwind(|| {
            let _guard = registry.lock().unwrap();
            panic!("poison the redaction registry");
        });

        register_secret_to_redact("top-secret-token");
        let redacted = redact_secrets("token=top-secret-token");

        assert!(redacted.contains("[REDACTED]"));
    }
}
