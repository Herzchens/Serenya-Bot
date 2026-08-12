use aho_corasick::AhoCorasick;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

const MAX_FIXED_SECRETS: usize = 32;
const ROTATING_SECRET_HISTORY: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RotatingSecretSlot {
    SpotifyAccessToken,
    SpotifyClientToken,
}

impl RotatingSecretSlot {
    const fn index(self) -> usize {
        match self {
            Self::SpotifyAccessToken => 0,
            Self::SpotifyClientToken => 1,
        }
    }
}

#[derive(Default)]
struct RedactorState {
    fixed: VecDeque<String>,
    rotating: [VecDeque<String>; 2],
    matcher: Option<AhoCorasick>,
}

impl RedactorState {
    fn normalize(secret: &str) -> Option<String> {
        let trimmed = secret.trim();
        if trimmed.len() < 4 {
            None
        } else {
            Some(trimmed.to_owned())
        }
    }

    fn register_fixed(&mut self, secret: &str) {
        let Some(secret) = Self::normalize(secret) else {
            return;
        };
        if self.fixed.iter().any(|value| value == &secret) {
            return;
        }
        if self.fixed.len() == MAX_FIXED_SECRETS {
            self.fixed.pop_front();
        }
        self.fixed.push_back(secret);
        self.rebuild();
    }

    fn set_rotating(&mut self, slot: RotatingSecretSlot, secret: &str) {
        let Some(secret) = Self::normalize(secret) else {
            return;
        };
        let history = &mut self.rotating[slot.index()];
        if history.front() == Some(&secret) {
            return;
        }
        history.retain(|value| value != &secret);
        history.push_front(secret);
        history.truncate(ROTATING_SECRET_HISTORY);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let patterns: Vec<&str> = self
            .fixed
            .iter()
            .map(String::as_str)
            .chain(
                self.rotating
                    .iter()
                    .flat_map(|h| h.iter().map(String::as_str)),
            )
            .collect();
        self.matcher = if patterns.is_empty() {
            None
        } else {
            AhoCorasick::new(patterns).ok()
        };
    }

    fn redact(&self, input: &str) -> String {
        let Some(matcher) = self.matcher.as_ref() else {
            return input.to_owned();
        };
        let mut output = String::with_capacity(input.len());
        matcher.replace_all_with(input, &mut output, |_, _, dst| {
            dst.push_str("[REDACTED]");
            true
        });
        output
    }

    #[cfg(test)]
    fn pattern_count(&self) -> usize {
        self.matcher
            .as_ref()
            .map(AhoCorasick::patterns_len)
            .unwrap_or(0)
    }
}

static REDACTOR: OnceLock<Mutex<RedactorState>> = OnceLock::new();

fn global_redactor() -> &'static Mutex<RedactorState> {
    REDACTOR.get_or_init(|| Mutex::new(RedactorState::default()))
}

fn lock_redactor() -> std::sync::MutexGuard<'static, RedactorState> {
    match global_redactor().lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub fn register_secret_to_redact(secret: &str) {
    lock_redactor().register_fixed(secret);
}

pub fn set_rotating_secret_to_redact(slot: RotatingSecretSlot, secret: &str) {
    lock_redactor().set_rotating(slot, secret);
}

pub fn redact_secrets(input: &str) -> String {
    lock_redactor().redact(input)
}

pub struct RedactingWriter<W> {
    inner: W,
}

impl<W: std::io::Write> std::io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let redacted = redact_secrets(&String::from_utf8_lossy(buf));
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
    fn rotating_history_is_bounded_after_ten_thousand_unique_rotations() {
        let mut state = RedactorState::default();
        for i in 0..10_000 {
            state.set_rotating(
                RotatingSecretSlot::SpotifyAccessToken,
                &format!("access-token-{i:05}"),
            );
        }
        let history = &state.rotating[RotatingSecretSlot::SpotifyAccessToken.index()];
        assert_eq!(history.len(), ROTATING_SECRET_HISTORY);
        assert_eq!(state.pattern_count(), ROTATING_SECRET_HISTORY);
        assert_eq!(
            state.redact("access-token-09999 access-token-09998"),
            "[REDACTED] [REDACTED]"
        );
        assert_eq!(state.redact("access-token-00001"), "access-token-00001");
    }

    #[cfg(feature = "dhat-heap")]
    #[test]
    fn rotating_history_retained_heap_stays_bounded_after_ten_thousand_unique_rotations() {
        let _profiler = dhat::Profiler::builder().testing().build();
        let mut state = RedactorState::default();

        for i in 0..128 {
            state.set_rotating(
                RotatingSecretSlot::SpotifyAccessToken,
                &format!("heap-token-{i:05}"),
            );
        }
        let warm = dhat::HeapStats::get();

        for i in 128..10_000 {
            state.set_rotating(
                RotatingSecretSlot::SpotifyAccessToken,
                &format!("heap-token-{i:05}"),
            );
        }
        let after = dhat::HeapStats::get();

        assert_eq!(
            state.rotating[RotatingSecretSlot::SpotifyAccessToken.index()].len(),
            ROTATING_SECRET_HISTORY
        );
        assert_eq!(state.pattern_count(), ROTATING_SECRET_HISTORY);
        dhat::assert!(
            after.curr_bytes <= warm.curr_bytes.saturating_add(64 * 1024),
            "retained heap grew from {} to {} bytes",
            warm.curr_bytes,
            after.curr_bytes
        );
        dhat::assert!(
            after.curr_blocks <= warm.curr_blocks.saturating_add(128),
            "retained allocation count grew from {} to {} blocks",
            warm.curr_blocks,
            after.curr_blocks
        );
    }

    #[test]
    fn fixed_registry_is_bounded_and_deduplicated() {
        let mut state = RedactorState::default();
        for i in 0..(MAX_FIXED_SECRETS + 10) {
            state.register_fixed(&format!("fixed-secret-{i:03}"));
        }
        state.register_fixed("fixed-secret-041");
        assert_eq!(state.fixed.len(), MAX_FIXED_SECRETS);
        assert_eq!(state.pattern_count(), MAX_FIXED_SECRETS);
        assert_eq!(state.redact("fixed-secret-041"), "[REDACTED]");
    }
}
