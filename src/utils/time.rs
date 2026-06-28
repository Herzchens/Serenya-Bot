use std::time::Duration;

use super::SerenyaError;

/// Parses human-readable duration strings like "1h30m", "2m10s", "45".
/// Bare numbers are treated as seconds.
pub fn parse_duration(input: &str) -> Result<Duration, SerenyaError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SerenyaError::Config(
            "Invalid duration format: empty string".into(),
        ));
    }

    let mut total_secs: u64 = 0;
    let mut current: u64 = 0;
    let mut has_suffix = false;
    let mut has_digits = false;

    for ch in input.chars() {
        match ch {
            '0'..='9' => {
                has_digits = true;
                current = current
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(ch as u64 - '0' as u64))
                    .ok_or_else(|| {
                        SerenyaError::Config(format!("Invalid duration format: {input}"))
                    })?;
            }
            'h' | 'H' => {
                total_secs += current * 3600;
                current = 0;
                has_suffix = true;
            }
            'm' | 'M' => {
                total_secs += current * 60;
                current = 0;
                has_suffix = true;
            }
            's' | 'S' => {
                total_secs += current;
                current = 0;
                has_suffix = true;
            }
            _ => {
                return Err(SerenyaError::Config(format!(
                    "Invalid duration format: {input}"
                )));
            }
        }
    }

    if !has_digits {
        return Err(SerenyaError::Config(format!(
            "Invalid duration format: {input}"
        )));
    }

    // Bare trailing number: seconds if suffixes were used, otherwise whole input as seconds
    if current > 0 || !has_suffix {
        total_secs += current;
    }

    Ok(Duration::from_secs(total_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_number_as_seconds() {
        assert_eq!(parse_duration("45").ok(), Some(Duration::from_secs(45)));
    }

    #[test]
    fn seconds_only() {
        assert_eq!(parse_duration("30s").ok(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn minutes_only() {
        assert_eq!(parse_duration("2m").ok(), Some(Duration::from_secs(120)));
    }

    #[test]
    fn minutes_and_seconds() {
        assert_eq!(parse_duration("1m20s").ok(), Some(Duration::from_secs(80)));
    }

    #[test]
    fn hours_and_minutes() {
        assert_eq!(
            parse_duration("1h30m").ok(),
            Some(Duration::from_secs(5400))
        );
    }

    #[test]
    fn full_hms() {
        assert_eq!(
            parse_duration("1h2m3s").ok(),
            Some(Duration::from_secs(3723))
        );
    }

    #[test]
    fn empty_string_is_error() {
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn invalid_chars_is_error() {
        assert!(parse_duration("10x").is_err());
    }

    #[test]
    fn whitespace_trimmed() {
        assert_eq!(parse_duration("  5s  ").ok(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn zero_is_valid() {
        assert_eq!(parse_duration("0").ok(), Some(Duration::from_secs(0)));
    }
}
