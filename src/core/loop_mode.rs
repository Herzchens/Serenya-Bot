use std::fmt;
use std::str::FromStr;

use crate::utils::SerenyaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopMode {
    #[default]
    Off,
    Track,
    Queue,
}

impl fmt::Display for LoopMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoopMode::Off => write!(f, "Off"),
            LoopMode::Track => write!(f, "Track"),
            LoopMode::Queue => write!(f, "Queue"),
        }
    }
}

impl FromStr for LoopMode {
    type Err = SerenyaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" | "none" => Ok(LoopMode::Off),
            "track" | "single" | "song" => Ok(LoopMode::Track),
            "queue" | "all" => Ok(LoopMode::Queue),
            _ => Err(SerenyaError::Config(format!(
                "Invalid loop mode: '{}'. Use 'off', 'track', or 'queue'.",
                s
            ))),
        }
    }
}
