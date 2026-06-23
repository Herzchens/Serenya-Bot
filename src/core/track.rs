use poise::serenity_prelude as serenity;

#[derive(Debug, Clone)]
pub struct Track {
    pub title: String,
    pub url: String,
    pub duration: Option<std::time::Duration>,
    pub requester_id: serenity::UserId,
    pub requester_name: String,
    pub source_type: SourceType,
    pub resolved_url: Option<youtube_resolver::ResolvedStream>,
    pub thumbnail: Option<std::sync::Arc<str>>,
    pub source_provider: String,
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
