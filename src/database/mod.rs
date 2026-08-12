pub mod models;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::utils::error::SerenyaError;
use models::{Database, GuildSettings, PlaylistTrack, UserSettings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachedPrefix {
    Default,
    Custom(Arc<str>),
}

#[derive(Clone)]
pub struct DatabaseManager {
    data: Arc<RwLock<Arc<Database>>>,
    path: PathBuf,
    revision: Arc<std::sync::atomic::AtomicU64>,
    persisted_revision: Arc<std::sync::atomic::AtomicU64>,
    save_gate: Arc<Mutex<()>>,
    prefix_cache: Arc<dashmap::DashMap<u64, CachedPrefix>>,
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
        let prefix_cache = dashmap::DashMap::new();
        for (guild_id_str, settings) in &db.guild_settings {
            if let Ok(guild_id) = guild_id_str.parse::<u64>() {
                if let Some(ref prefix) = settings.prefix {
                    prefix_cache.insert(guild_id, CachedPrefix::Custom(Arc::from(prefix.as_str())));
                } else {
                    prefix_cache.insert(guild_id, CachedPrefix::Default);
                }
            }
        }

        Ok(Self {
            data: Arc::new(RwLock::new(Arc::new(db))),
            path,
            revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            persisted_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            save_gate: Arc::new(Mutex::new(())),
            prefix_cache: Arc::new(prefix_cache),
        })
    }

    fn mark_dirty(&self) {
        self.revision
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn is_dirty(&self) -> bool {
        self.revision.load(std::sync::atomic::Ordering::SeqCst)
            != self
                .persisted_revision
                .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn snapshot_for_save(&self) -> (Arc<Database>, u64) {
        let data = self.data.read().await;
        let revision = self.revision.load(std::sync::atomic::Ordering::SeqCst);
        (Arc::clone(&*data), revision)
    }

    async fn persist_snapshot(
        &self,
        db_snapshot: Arc<Database>,
        snapshot_revision: u64,
    ) -> Result<(), SerenyaError> {
        let yaml = tokio::task::spawn_blocking(move || {
            serde_saphyr::to_string(&*db_snapshot)
                .map_err(|e| SerenyaError::Database(format!("serialization failed: {e}")))
        })
        .await
        .map_err(|e| SerenyaError::Database(format!("spawn_blocking failed: {e}")))??;

        let tmp_path = self.path.with_extension("yml.tmp");
        let bak_path = self.path.with_extension("yml.bak");

        tokio::fs::write(&tmp_path, &yaml).await?;
        if self.path.exists() {
            tokio::fs::copy(&self.path, &bak_path).await?;
        }
        tokio::fs::rename(&tmp_path, &self.path).await?;

        self.persisted_revision
            .fetch_max(snapshot_revision, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub async fn save(&self) -> Result<(), SerenyaError> {
        let _save_guard = self.save_gate.lock().await;
        let (snapshot, revision) = self.snapshot_for_save().await;
        self.persist_snapshot(snapshot, revision).await
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
                        if manager.is_dirty()
                            && let Err(e) = manager.save().await {
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

    pub fn get_guild_prefix(&self, guild_id: u64, default_prefix: &str) -> Arc<str> {
        if let Some(cached) = self.prefix_cache.get(&guild_id) {
            return match cached.value() {
                CachedPrefix::Default => Arc::from(default_prefix),
                CachedPrefix::Custom(prefix) => prefix.clone(),
            };
        }
        Arc::from(default_prefix)
    }

    pub async fn get_guild_settings(&self, guild_id: u64) -> GuildSettings {
        let data = self.data.read().await;
        let key = guild_id.to_string();
        data.guild_settings.get(&key).cloned().unwrap_or_default()
    }

    pub async fn update_guild_settings(&self, guild_id: u64, settings: GuildSettings) {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
        let prefix_opt = settings.prefix.clone();
        data.guild_settings.insert(guild_id.to_string(), settings);

        if let Some(ref prefix) = prefix_opt {
            self.prefix_cache
                .insert(guild_id, CachedPrefix::Custom(Arc::from(prefix.as_str())));
        } else {
            self.prefix_cache.insert(guild_id, CachedPrefix::Default);
        }

        self.mark_dirty();
    }

    pub async fn update_guild_settings_mut<F, R>(&self, guild_id: u64, f: F) -> R
    where
        F: FnOnce(&mut GuildSettings) -> R,
    {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
        let key = guild_id.to_string();
        let settings = data.guild_settings.entry(key).or_default();

        let res = f(settings);

        if let Some(ref prefix) = settings.prefix {
            self.prefix_cache
                .insert(guild_id, CachedPrefix::Custom(Arc::from(prefix.as_str())));
        } else {
            self.prefix_cache.insert(guild_id, CachedPrefix::Default);
        }

        self.mark_dirty();
        res
    }

    pub async fn get_user_settings(&self, user_id: u64) -> UserSettings {
        let data = self.data.read().await;
        let key = user_id.to_string();
        data.user_settings.get(&key).cloned().unwrap_or_default()
    }

    pub async fn update_user_settings(&self, user_id: u64, settings: UserSettings) {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
        data.user_settings.insert(user_id.to_string(), settings);
        self.mark_dirty();
    }

    pub async fn get_user_playlist_names(&self, user_id: u64) -> Vec<String> {
        let data = self.data.read().await;
        let key = user_id.to_string();
        data.user_playlists
            .get(&key)
            .map(|playlists| playlists.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn get_user_playlist(
        &self,
        user_id: u64,
        name: &str,
    ) -> Option<models::UserPlaylist> {
        let data = self.data.read().await;
        let key = user_id.to_string();
        data.user_playlists
            .get(&key)
            .and_then(|playlists| playlists.get(name))
            .cloned()
    }

    pub async fn create_playlist(
        &self,
        user_id: u64,
        name: &str,
        max_playlists: usize,
    ) -> Result<(), SerenyaError> {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
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
        self.mark_dirty();

        Ok(())
    }

    pub async fn add_to_playlist(
        &self,
        user_id: u64,
        name: &str,
        track: PlaylistTrack,
        max_tracks: usize,
    ) -> Result<(), SerenyaError> {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
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
        self.mark_dirty();
        Ok(())
    }

    pub async fn delete_playlist(&self, user_id: u64, name: &str) -> Result<(), SerenyaError> {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
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
        self.mark_dirty();

        Ok(())
    }

    pub async fn remove_from_playlist(
        &self,
        user_id: u64,
        name: &str,
        index: usize,
    ) -> Result<(), SerenyaError> {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
        let key = user_id.to_string();
        let playlist = data
            .user_playlists
            .get_mut(&key)
            .and_then(|p| p.get_mut(name))
            .ok_or_else(|| SerenyaError::NotFound(format!("playlist '{name}' not found")))?;

        if index == 0 || index > playlist.tracks.len() {
            return Err(SerenyaError::Queue(format!(
                "track index {index} out of bounds (len: {})",
                playlist.tracks.len()
            )));
        }

        playlist.tracks.remove(index - 1);
        playlist.updated_at = chrono::Utc::now().to_rfc3339();
        self.mark_dirty();
        Ok(())
    }

    pub async fn rename_playlist(
        &self,
        user_id: u64,
        old_name: &str,
        new_name: &str,
    ) -> Result<(), SerenyaError> {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
        let key = user_id.to_string();
        let playlists = data
            .user_playlists
            .get_mut(&key)
            .ok_or_else(|| SerenyaError::NotFound("no playlists found for user".into()))?;

        if playlists.contains_key(new_name) {
            return Err(SerenyaError::Database(format!(
                "playlist '{new_name}' already exists"
            )));
        }

        let playlist = playlists
            .remove(old_name)
            .ok_or_else(|| SerenyaError::NotFound(format!("playlist '{old_name}' not found")))?;

        playlists.insert(new_name.to_owned(), playlist);
        self.mark_dirty();
        Ok(())
    }

    pub async fn increment_songs_played(&self, guild_id: u64) {
        let mut data_guard = self.data.write().await;
        let data = Arc::make_mut(&mut *data_guard);
        let key = guild_id.to_string();
        let settings = data.guild_settings.entry(key).or_default();
        settings.total_songs_played += 1;

        if !self.prefix_cache.contains_key(&guild_id) {
            if let Some(ref p) = settings.prefix {
                self.prefix_cache
                    .insert(guild_id, CachedPrefix::Custom(Arc::from(p.as_str())));
            } else {
                self.prefix_cache.insert(guild_id, CachedPrefix::Default);
            }
        }

        self.mark_dirty();
    }
}
