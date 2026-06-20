pub mod error;
pub mod text;
pub mod time;

pub use error::SerenyaError;
pub use text::truncate_chars;

/// Boxed error type used at Poise command boundaries.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
/// Poise context alias with our Data and Error types.
pub type Context<'a> = poise::Context<'a, crate::Data, Error>;
