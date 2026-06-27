use poise::serenity_prelude as serenity;

#[derive(Debug, Clone)]
pub struct Track {
    pub title: Box<str>,
    pub url: Box<str>,
    pub duration: Option<std::time::Duration>,
    pub requester_name: Option<std::sync::Arc<str>>,
    pub thumbnail: Option<std::sync::Arc<str>>,
    pub source_provider: std::sync::Arc<str>,
    pub resolved_url: Option<std::sync::Arc<youtube_resolver::ResolvedStream>>,
    pub requester_id: serenity::UserId,
    pub source_type: SourceType,
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
