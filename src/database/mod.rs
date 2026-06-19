pub mod models;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::utils::error::SerenyaError;
use models::{Database, GuildSettings, PlaylistTrack, UserSettings};

#[derive(Clone)]
pub struct DatabaseManager {
    data: Arc<RwLock<Database>>,
    path: PathBuf,
}

impl DatabaseManager {
    pub async fn load(path: impl AsRef<Path>) -> Result<Self, SerenyaError> {
        let path = path.as_ref().to_path_buf();

        let db = if path.exists() {
            let contents = tokio::fs::read_to_string(&path).await?;
            serde_saphyr::from_str(&contents).map_err(|e| {
                SerenyaError::Database(format!("failed to parse {}: {e}", path.display()))
            })?
        } else {
            let db = Database::default();
            let yaml = serde_saphyr::to_string(&db).map_err(|e| {
                SerenyaError::Database(format!("failed to serialize defaults: {e}"))
            })?;
            tokio::fs::write(&path, &yaml).await?;
            tracing::info!(path = %path.display(), "created default database file");
            db
        };

        tracing::info!(path = %path.display(), "database loaded");
        Ok(Self {
            data: Arc::new(RwLock::new(db)),
            path,
        })
    }

    pub async fn save(&self) -> Result<(), SerenyaError> {
        let yaml = {
            let data = self.data.read().await;
            serde_saphyr::to_string(&*data)
                .map_err(|e| SerenyaError::Database(format!("serialization failed: {e}")))?
        };

        let tmp_path = self.path.with_extension("yml.tmp");
        let bak_path = self.path.with_extension("yml.bak");

        tokio::fs::write(&tmp_path, &yaml).await?;

        if self.path.exists() {
            tokio::fs::copy(&self.path, &bak_path).await?;
        }

        tokio::fs::rename(&tmp_path, &self.path).await?;
        Ok(())
    }

    pub fn start_auto_save(
        &self,
        interval: Duration,
        cancel_token: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // skip immediate first tick

            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        tracing::info!("auto-save cancelled");
                        break;
                    }
                    _ = ticker.tick() => {
                        if let Err(e) = manager.save().await {
                            tracing::error!(error = %e, "auto-save failed");
                        }
                    }
                }
            }
        })
    }

    pub async fn shutdown(&self) -> Result<(), SerenyaError> {
        tracing::info!("performing final database save");
        self.save().await?;
        tracing::info!("database saved successfully on shutdown");
        Ok(())
    }

    pub async fn get_guild_settings(&self, guild_id: u64) -> GuildSettings {
        let data = self.data.read().await;
        let key = guild_id.to_string();
        data.guild_settings.get(&key).cloned().unwrap_or_default()
    }

    pub async fn update_guild_settings(&self, guild_id: u64, settings: GuildSettings) {
        let mut data = self.data.write().await;
        data.guild_settings.insert(guild_id.to_string(), settings);
    }

    pub async fn get_user_settings(&self, user_id: u64) -> UserSettings {
        let data = self.data.read().await;
        let key = user_id.to_string();
        data.user_settings.get(&key).cloned().unwrap_or_default()
    }

    pub async fn update_user_settings(&self, user_id: u64, settings: UserSettings) {
        let mut data = self.data.write().await;
        data.user_settings.insert(user_id.to_string(), settings);
    }

    pub async fn get_user_playlist_names(&self, user_id: u64) -> Vec<String> {
        let data = self.data.read().await;
        let key = user_id.to_string();
        data.user_playlists
            .get(&key)
            .map(|playlists| playlists.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn create_playlist(
        &self,
        user_id: u64,
        name: &str,
        max_playlists: usize,
    ) -> Result<(), SerenyaError> {
        let mut data = self.data.write().await;
        let key = user_id.to_string();
        let user_playlists = data.user_playlists.entry(key).or_default();

        if user_playlists.contains_key(name) {
            return Err(SerenyaError::Database(format!(
                "playlist '{}' already exists",
                name
            )));
        }

        if user_playlists.len() >= max_playlists {
            return Err(SerenyaError::Database(format!(
                "maximum of {} playlists reached",
                max_playlists
            )));
        }

        let now = chrono::Utc::now().to_rfc3339();
        user_playlists.insert(
            name.to_owned(),
            models::UserPlaylist {
                created_at: now.clone(),
                updated_at: now,
                tracks: Vec::new(),
            },
        );

        Ok(())
    }

    pub async fn add_to_playlist(
        &self,
        user_id: u64,
        name: &str,
        track: PlaylistTrack,
        max_tracks: usize,
    ) -> Result<(), SerenyaError> {
        let mut data = self.data.write().await;
        let key = user_id.to_string();

        let playlist = data
            .user_playlists
            .get_mut(&key)
            .and_then(|p| p.get_mut(name))
            .ok_or_else(|| SerenyaError::NotFound(format!("playlist '{name}' not found")))?;

        if playlist.tracks.len() >= max_tracks {
            return Err(SerenyaError::Database(format!(
                "playlist '{name}' is full ({max_tracks} tracks)"
            )));
        }

        playlist.updated_at = chrono::Utc::now().to_rfc3339();
        playlist.tracks.push(track);
        Ok(())
    }

    pub async fn delete_playlist(&self, user_id: u64, name: &str) -> Result<(), SerenyaError> {
        let mut data = self.data.write().await;
        let key = user_id.to_string();

        let removed = data
            .user_playlists
            .get_mut(&key)
            .and_then(|p| p.remove(name));

        if removed.is_none() {
            return Err(SerenyaError::NotFound(format!(
                "playlist '{name}' not found"
            )));
        }

        Ok(())
    }

    pub async fn increment_songs_played(&self, guild_id: u64) {
        let mut data = self.data.write().await;
        let key = guild_id.to_string();
        let settings = data.guild_settings.entry(key).or_default();
        settings.total_songs_played += 1;
    }
}
