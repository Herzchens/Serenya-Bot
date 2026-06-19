use serde::Deserialize;

use crate::utils::error::SerenyaError;

#[derive(Deserialize, Clone, Debug)]
pub struct BotConfig {
    pub bot: BotSection,
    pub playback: PlaybackSection,
    pub audio: AudioSection,
}

#[derive(Deserialize, Clone, Debug)]
pub struct BotSection {
    pub token: String,
    pub prefix: String,
    pub owner: u64,
    pub instance_id: String,
    pub display_name: String,
    pub invite_url: Option<String>,
    pub support_url: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct PlaybackSection {
    pub stay_in_voice: bool,
    pub announce_track: bool,
    pub max_queue_size: usize,
    pub max_playlist_import: usize,
    pub max_user_playlists: usize,
    pub max_tracks_per_user_playlist: usize,
}

#[derive(Deserialize, Clone, Debug)]
pub struct AudioSection {
    pub default_quality: String,
    pub modes: Vec<String>,
}

/// Loads, expands env vars, parses, and validates a YAML config file.
pub fn load_config(path: &str) -> Result<BotConfig, SerenyaError> {
    let raw = std::fs::read_to_string(path).map_err(SerenyaError::Io)?;
    let expanded = expand_env_vars(&raw)?;
    let config: BotConfig = serde_saphyr::from_str(&expanded)
        .map_err(|e| SerenyaError::Config(format!("YAML parse error: {e}")))?;
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &BotConfig) -> Result<(), SerenyaError> {
    if config.bot.token.is_empty() {
        return Err(SerenyaError::Config("bot.token must not be empty".into()));
    }
    if config.bot.prefix.is_empty() {
        return Err(SerenyaError::Config("bot.prefix must not be empty".into()));
    }
    if config.bot.owner == 0 {
        return Err(SerenyaError::Config("bot.owner must not be zero".into()));
    }
    Ok(())
}

/// Replaces `${VAR_NAME}` patterns with their environment variable values.
fn expand_env_vars(input: &str) -> Result<String, SerenyaError> {
    let mut result = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(start) = remaining.find("${") {
        result.push_str(&remaining[..start]);
        let after_marker = &remaining[start + 2..];
        let end = after_marker
            .find('}')
            .ok_or_else(|| SerenyaError::Config("unclosed ${ in config".into()))?;
        let var_name = &after_marker[..end];
        let value = std::env::var(var_name).map_err(|_| {
            SerenyaError::Config(format!("environment variable '{var_name}' not set"))
        })?;
        result.push_str(&value);
        remaining = &after_marker[end + 1..];
    }

    result.push_str(remaining);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_env_vars_replaces_known_var() {
        unsafe { std::env::set_var("SERENYA_TEST_VAR", "hello") };
        let result = expand_env_vars("prefix_${SERENYA_TEST_VAR}_suffix");
        assert!(result.is_ok());
        assert_eq!(result.ok(), Some("prefix_hello_suffix".to_owned()));
        unsafe { std::env::remove_var("SERENYA_TEST_VAR") };
    }

    #[test]
    fn expand_env_vars_errors_on_missing_var() {
        let result = expand_env_vars("${SERENYA_NONEXISTENT_VAR_12345}");
        assert!(result.is_err());
    }

    #[test]
    fn expand_env_vars_errors_on_unclosed_marker() {
        let result = expand_env_vars("${MISSING_CLOSE");
        assert!(result.is_err());
    }

    #[test]
    fn expand_env_vars_no_placeholders() {
        let result = expand_env_vars("no placeholders here");
        assert!(result.is_ok());
        assert_eq!(result.ok(), Some("no placeholders here".to_owned()));
    }

    #[test]
    fn validate_rejects_empty_token() {
        let config = BotConfig {
            bot: BotSection {
                token: String::new(),
                prefix: "s!".into(),
                owner: 1,
                instance_id: "id".into(),
                display_name: "name".into(),
                invite_url: None,
                support_url: None,
            },
            playback: test_playback(),
            audio: test_audio(),
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_rejects_empty_prefix() {
        let config = BotConfig {
            bot: BotSection {
                token: "tok".into(),
                prefix: String::new(),
                owner: 1,
                instance_id: "id".into(),
                display_name: "name".into(),
                invite_url: None,
                support_url: None,
            },
            playback: test_playback(),
            audio: test_audio(),
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_rejects_zero_owner() {
        let config = BotConfig {
            bot: BotSection {
                token: "tok".into(),
                prefix: "s!".into(),
                owner: 0,
                instance_id: "id".into(),
                display_name: "name".into(),
                invite_url: None,
                support_url: None,
            },
            playback: test_playback(),
            audio: test_audio(),
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_accepts_valid_config() {
        let config = BotConfig {
            bot: BotSection {
                token: "tok".into(),
                prefix: "s!".into(),
                owner: 1,
                instance_id: "id".into(),
                display_name: "name".into(),
                invite_url: None,
                support_url: None,
            },
            playback: test_playback(),
            audio: test_audio(),
        };
        assert!(validate_config(&config).is_ok());
    }

    fn test_playback() -> PlaybackSection {
        PlaybackSection {
            stay_in_voice: true,
            announce_track: true,
            max_queue_size: 500,
            max_playlist_import: 100,
            max_user_playlists: 25,
            max_tracks_per_user_playlist: 500,
        }
    }

    fn test_audio() -> AudioSection {
        AudioSection {
            default_quality: "balanced".into(),
            modes: vec!["performance".into(), "balanced".into(), "quality".into()],
        }
    }
}
