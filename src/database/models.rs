use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Database {
    #[serde(default)]
    pub guild_settings: HashMap<String, GuildSettings>,
    #[serde(default)]
    pub user_settings: HashMap<String, UserSettings>,
    #[serde(default)]
    pub user_playlists: HashMap<String, HashMap<String, UserPlaylist>>,
    #[serde(default)]
    pub bot_instances: HashMap<String, BotInstance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildSettings {
    #[serde(default = "default_true")]
    pub announce_track: bool,
    #[serde(default)]
    pub total_songs_played: u64,
    #[serde(default)]
    pub total_listening_seconds: u64,
}

impl Default for GuildSettings {
    fn default() -> Self {
        Self {
            announce_track: true,
            total_songs_played: 0,
            total_listening_seconds: 0,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSettings {
    #[serde(default = "default_quality")]
    pub quality: String,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            quality: "balanced".to_owned(),
        }
    }
}

fn default_quality() -> String {
    "balanced".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPlaylist {
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub tracks: Vec<PlaylistTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistTrack {
    pub title: String,
    pub url: String,
    pub duration_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotInstance {
    pub display_name: String,
}
