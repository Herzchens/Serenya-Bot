use poise::serenity_prelude as serenity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamTrust {
    /// Resolved by our own resolver (youtube_resolver, soundcloud native).
    Native,
    /// Resolved by yt-dlp or an unknown/untrusted path.
    External,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub title: Box<str>,
    pub url: Box<str>,
    pub duration: Option<std::time::Duration>,
    pub requester_name: Option<std::sync::Arc<str>>,
    pub thumbnail: Option<std::sync::Arc<str>>,
    pub source_provider: std::sync::Arc<str>,
    pub resolved_url: Option<crate::audio::source::VerifiedStream>,
    pub requester_id: serenity::UserId,
    pub source_type: SourceType,
    pub stream_trust: StreamTrust,
}

impl Track {
    pub fn clean_source(&self) -> &str {
        if let Some(pos) = self.source_provider.find(" -> ") {
            self.source_provider[..pos].trim()
        } else {
            &self.source_provider
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Url,
    Search,
    Playlist,
}
