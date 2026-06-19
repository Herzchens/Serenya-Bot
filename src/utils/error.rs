use thiserror::Error;

#[derive(Debug, Error)]
pub enum SerenyaError {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Database error: {0}")]
    Database(String),
    #[error("Audio error: {0}")]
    Audio(String),
    #[error("Voice error: {0}")]
    Voice(String),
    #[error("Queue error: {0}")]
    Queue(String),
    #[error("Permission denied: {0}")]
    Permission(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Discord error: {0}")]
    Discord(Box<poise::serenity_prelude::Error>),
}

impl From<poise::serenity_prelude::Error> for SerenyaError {
    fn from(err: poise::serenity_prelude::Error) -> Self {
        SerenyaError::Discord(Box::new(err))
    }
}
