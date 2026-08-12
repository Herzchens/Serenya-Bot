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

impl SerenyaError {
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Config(_) => "Config",
            Self::Database(_) => "Database",
            Self::Audio(_) => "Audio",
            Self::Voice(_) => "Voice",
            Self::Queue(_) => "Queue",
            Self::Permission(_) => "Permission",
            Self::NotFound(_) => "NotFound",
            Self::Io(_) => "Io",
            Self::Discord(_) => "Discord",
        }
    }
}

impl From<poise::serenity_prelude::Error> for SerenyaError {
    fn from(err: poise::serenity_prelude::Error) -> Self {
        SerenyaError::Discord(Box::new(err))
    }
}

#[cfg(test)]
mod tests {
    use super::SerenyaError;

    #[test]
    fn error_classes_are_stable_and_message_free() {
        assert_eq!(SerenyaError::Voice("secret detail".into()).class(), "Voice");
        assert_eq!(SerenyaError::Audio("secret detail".into()).class(), "Audio");
        assert_eq!(
            SerenyaError::Database("secret detail".into()).class(),
            "Database"
        );
        assert!(
            !SerenyaError::Voice("secret detail".into())
                .class()
                .contains("secret")
        );
    }
}
