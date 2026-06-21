use serde::Deserialize;

use crate::utils::error::SerenyaError;

#[derive(Deserialize, Clone, Debug)]
pub struct BotConfig {
    pub bot: BotSection,
    pub logging: LoggingSection,
    pub spotify: SpotifySection,
    pub playback: PlaybackSection,
    #[serde(default)]
    pub resolver: ResolverSection,
    #[serde(default)]
    pub emojis: Option<EmojisSection>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct EmojisSection {
    pub spotify: Option<String>,
    pub apple_music: Option<String>,
    pub deezer: Option<String>,
    pub youtube: Option<String>,
    pub soundcloud: Option<String>,
    pub default: Option<String>,
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
    pub log_webhook_url: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct LoggingSection {
    pub level: String,
    pub webhook_enabled: bool,
    pub webhook_url: Option<String>,
    pub webhook_min_level: String,
    pub webhook_plain_text: bool,
}

#[derive(Deserialize, Clone, Debug)]
pub struct SpotifySection {
    pub enabled: bool,
    pub sp_dc: Option<String>,
    pub enable_track: bool,
    pub enable_playlist: bool,
    pub enable_album: bool,
    pub enable_artist_top_tracks: bool,
    pub enable_text_search: bool,
    pub max_playlist_import: usize,
    pub max_album_import: usize,
    pub max_artist_top_tracks: usize,
    pub market: String,
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
#[serde(default)]
pub struct ResolverSection {
    pub max_concurrent_ytdlp: usize,
    pub max_concurrent_resolves_per_guild: usize,
    pub global_search_timeout_ms: u64,
    pub deezer_timeout_ms: u64,
    pub apple_music_timeout_ms: u64,
    pub youtube_music_timeout_ms: u64,
    pub soundcloud_timeout_ms: u64,
    pub spotify_timeout_ms: u64,
    pub youtube_timeout_ms: u64,
    pub ytsearch_timeout_ms: u64,
    pub yt_dlp_timeout_seconds: u64,
    pub prefetch_timeout_seconds: u64,
    pub prefetch_when_remaining_seconds: u64,
    pub query_cache_ttl_seconds: u64,
    pub metadata_cache_ttl_seconds: u64,
    pub stream_cache_ttl_seconds: u64,
    pub negative_cache_ttl_seconds: u64,
    pub auto_pick_threshold: f64,
    pub perfect_threshold: f64,
}

impl Default for ResolverSection {
    fn default() -> Self {
        Self {
            max_concurrent_ytdlp: 2,
            max_concurrent_resolves_per_guild: 1,
            global_search_timeout_ms: 1800,
            deezer_timeout_ms: 800,
            apple_music_timeout_ms: 800,
            youtube_music_timeout_ms: 1000,
            soundcloud_timeout_ms: 1500,
            spotify_timeout_ms: 800,
            youtube_timeout_ms: 1500,
            ytsearch_timeout_ms: 3000,
            yt_dlp_timeout_seconds: 10,
            prefetch_timeout_seconds: 8,
            prefetch_when_remaining_seconds: 10,
            query_cache_ttl_seconds: 3600,
            metadata_cache_ttl_seconds: 86400,
            stream_cache_ttl_seconds: 3600,
            negative_cache_ttl_seconds: 1800,
            auto_pick_threshold: 0.90,
            perfect_threshold: 0.97,
        }
    }
}

/// Loads, expands env vars, parses, and validates a YAML config file.
pub fn load_config(path: &str) -> Result<BotConfig, SerenyaError> {
    let raw = std::fs::read_to_string(path).map_err(SerenyaError::Io)?;
    let expanded = expand_env_vars(&raw)?;
    let mut config: BotConfig = serde_saphyr::from_str(&expanded)
        .map_err(|e| SerenyaError::Config(format!("YAML parse error: {e}")))?;

    // Backward compatibility fallback for log_webhook_url
    if config.logging.webhook_url.is_none() {
        config.logging.webhook_url = config.bot.log_webhook_url.clone();
    }

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

    validate_resolver_config(&config.resolver)?;
    Ok(())
}

fn validate_resolver_config(resolver: &ResolverSection) -> Result<(), SerenyaError> {
    if resolver.max_concurrent_ytdlp == 0 {
        return Err(SerenyaError::Config(
            "resolver.max_concurrent_ytdlp must be greater than zero".into(),
        ));
    }
    if resolver.max_concurrent_resolves_per_guild == 0 {
        return Err(SerenyaError::Config(
            "resolver.max_concurrent_resolves_per_guild must be greater than zero".into(),
        ));
    }
    if resolver.yt_dlp_timeout_seconds == 0 {
        return Err(SerenyaError::Config(
            "resolver.yt_dlp_timeout_seconds must be greater than zero".into(),
        ));
    }
    if resolver.prefetch_timeout_seconds == 0 {
        return Err(SerenyaError::Config(
            "resolver.prefetch_timeout_seconds must be greater than zero".into(),
        ));
    }
    if !(0.0..=1.0).contains(&resolver.auto_pick_threshold)
        || !(0.0..=1.0).contains(&resolver.perfect_threshold)
        || resolver.auto_pick_threshold > resolver.perfect_threshold
    {
        return Err(SerenyaError::Config(
            "resolver confidence thresholds must be between 0.0 and 1.0 and auto_pick_threshold must not exceed perfect_threshold".into(),
        ));
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
                log_webhook_url: None,
            },
            logging: test_logging(),
            spotify: test_spotify(),
            playback: test_playback(),
            resolver: ResolverSection::default(),
            emojis: Some(crate::config::EmojisSection::default()),
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
                log_webhook_url: None,
            },
            logging: test_logging(),
            spotify: test_spotify(),
            playback: test_playback(),
            resolver: ResolverSection::default(),
            emojis: Some(crate::config::EmojisSection::default()),
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
                log_webhook_url: None,
            },
            logging: test_logging(),
            spotify: test_spotify(),
            playback: test_playback(),
            resolver: ResolverSection::default(),
            emojis: Some(crate::config::EmojisSection::default()),
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
                log_webhook_url: None,
            },
            logging: test_logging(),
            spotify: test_spotify(),
            playback: test_playback(),
            resolver: ResolverSection::default(),
            emojis: Some(crate::config::EmojisSection::default()),
        };
        assert!(validate_config(&config).is_ok());
    }

    fn test_logging() -> LoggingSection {
        LoggingSection {
            level: "debug".into(),
            webhook_enabled: false,
            webhook_url: None,
            webhook_min_level: "debug".into(),
            webhook_plain_text: true,
        }
    }

    fn test_spotify() -> SpotifySection {
        SpotifySection {
            enabled: false,
            sp_dc: None,
            enable_track: true,
            enable_playlist: true,
            enable_album: true,
            enable_artist_top_tracks: true,
            enable_text_search: true,
            max_playlist_import: 100,
            max_album_import: 100,
            max_artist_top_tracks: 20,
            market: "US".into(),
        }
    }

    fn test_playback() -> PlaybackSection {
        PlaybackSection {
            stay_in_voice: true,
            announce_track: true,
            max_queue_size: 500,
            max_playlist_import: 100,
            max_user_playlists: 10,
            max_tracks_per_user_playlist: 500,
        }
    }

}
