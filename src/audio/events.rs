use async_trait::async_trait;
use poise::serenity_prelude as serenity;
use songbird::{Event, EventContext, EventHandler};
use std::time::Duration;

use crate::database::DatabaseManager;
use crate::discord::embeds::now_playing_announce_embed;
use crate::utils::SerenyaError;

pub struct TrackEndHandler {
    pub guild_id: serenity::GuildId,
    pub database: std::sync::Arc<DatabaseManager>,
    pub guild_players: std::sync::Arc<
        dashmap::DashMap<
            serenity::GuildId,
            std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
        >,
    >,
    pub http_client: reqwest::Client,
    pub serenity_ctx: serenity::Context,
}

#[async_trait]
impl EventHandler for TrackEndHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let ended_uuid = if let EventContext::Track(track_events) = ctx {
            track_events.first().map(|(_, handle)| handle.uuid())
        } else {
            None
        };

        if let EventContext::Track(_) = ctx {
            let guild_id = self.guild_id;
            let database = self.database.clone();
            let guild_players = self.guild_players.clone();
            let http_client = self.http_client.clone();
            let serenity_ctx = self.serenity_ctx.clone();

            tokio::spawn(async move {
                if let Err(e) = play_next(
                    guild_id,
                    &database,
                    &guild_players,
                    &http_client,
                    &serenity_ctx,
                    ended_uuid,
                )
                .await
                {
                    tracing::error!("Error in play_next during event handling: {:?}", e);
                }
            });
        }
        None
    }
}

#[allow(dead_code)]
pub struct TrackErrorHandler {
    pub guild_id: serenity::GuildId,
    pub database: std::sync::Arc<DatabaseManager>,
    pub guild_players: std::sync::Arc<
        dashmap::DashMap<
            serenity::GuildId,
            std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
        >,
    >,
    pub http_client: reqwest::Client,
    pub serenity_ctx: serenity::Context,
}

#[async_trait]
impl EventHandler for TrackErrorHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(_) = ctx {
            tracing::error!(guild_id = %self.guild_id, "Track errored (End handler will advance queue)");
            if let Some(player_lock) = self.guild_players.get(&self.guild_id) {
                let mut player = player_lock.value().write().await;
                player.consecutive_errors += 1;
                tracing::warn!(guild_id = %self.guild_id, "Consecutive error count incremented to {}", player.consecutive_errors);
            }
        }
        None
    }
}

pub async fn play_next(
    guild_id: serenity::GuildId,
    database: &std::sync::Arc<DatabaseManager>,
    guild_players: &std::sync::Arc<
        dashmap::DashMap<
            serenity::GuildId,
            std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
        >,
    >,
    http_client: &reqwest::Client,
    serenity_ctx: &serenity::Context,
    ended_uuid: Option<uuid::Uuid>,
) -> Result<(), SerenyaError> {
    // 1. Get the guild player lock
    let player_lock = guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("Guild player not found".into()))?;

    // Check if the event is from a stale track handle (due to seeking)
    if let Some(ended) = ended_uuid {
        let player = player_lock.read().await;
        if let Some(ref current_handle) = player.current_track_handle {
            if current_handle.uuid() != ended {
                tracing::info!("Ignoring End/Error event from stale track handle");
                return Ok(());
            }
        }
    }

    // 2. Get songbird call manager
    let songbird_manager = songbird::get(serenity_ctx)
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized".into()))?
        .clone();

    let call_lock = songbird_manager
        .get(guild_id)
        .ok_or_else(|| SerenyaError::Voice("Not connected to a voice channel".into()))?;

    let finished_track = {
        let player = player_lock.read().await;
        player.now_playing.clone()
    };

    if let Some(track) = finished_track {
        database.increment_songs_played(guild_id.get()).await;
        if let Some(dur) = track.duration {
            let mut settings = database.get_guild_settings(guild_id.get()).await;
            settings.total_listening_seconds += dur.as_secs();
            database
                .update_guild_settings(guild_id.get(), settings)
                .await;
        }
    }

    // Advance queue and get the next track to play
    let (track, announce_channel, consecutive_errors) = {
        let mut player = player_lock.write().await;
        player.advance_queue();
        (player.now_playing.clone(), player.announce_channel, player.consecutive_errors)
    };

    if consecutive_errors >= 3 {
        tracing::error!("Aborting play_next: too many consecutive errors ({})", consecutive_errors);
        let mut call = call_lock.lock().await;
        call.stop();
        {
            let mut player = player_lock.write().await;
            player.reset();
        }
        if let Some(channel) = announce_channel {
            let ctx_clone = serenity_ctx.clone();
            tokio::spawn(async move {
                let _ = channel.say(&ctx_clone.http, "❌ Dừng phát nhạc do quá nhiều lỗi liên tiếp. Vui lòng kiểm tra lại nguồn phát hoặc thử lại sau.").await;
            });
        }
        return Ok(());
    }

    let Some(mut track) = track else {
        // No more tracks, stop player
        let mut call = call_lock.lock().await;
        call.stop();
        {
            let mut player = player_lock.write().await;
            player.current_track_handle = None;
            player.playback_status = crate::core::PlaybackStatus::Idle;
        }

        // Announce track finished if enabled
        let announce_setting = database
            .get_guild_settings(guild_id.get())
            .await
            .announce_track;

        if announce_setting {
            if let Some(channel) = announce_channel {
                let ctx_clone = serenity_ctx.clone();
                tokio::spawn(async move {
                    let _ = channel.say(&ctx_clone.http, "Queue finished. ⏹️").await;
                });
            }
        }
        return Ok(());
    };

    // Resolve ytsearch1: outside of the write lock!
    if track.url.starts_with("ytsearch1:") {
        if let Err(e) =
            crate::audio::resolver::resolve_ytsearch_track(&mut track, http_client).await
        {
            tracing::error!("Failed to resolve Spotify track search: {:?}", e);
        } else {
            // Update player's now_playing field with the resolved track
            let mut player = player_lock.write().await;
            if let Some(ref mut np) = player.now_playing {
                if np.url.starts_with("ytsearch1:") {
                    *np = track.clone();
                }
            }
        }
    }

    let resolved = match track.resolved_url.clone() {
        Some(url) => url,
        None => {
            crate::audio::source::extract_stream_url_for_guild(guild_id.get(), &track.url).await?
        }
    };

    tracing::info!(
        guild_id = %guild_id,
        track = %track.title,
        "Playing resolved stream URL"
    );

    let eight_d_enabled = {
        let player = player_lock.read().await;
        player.eight_d_enabled
    };
    let source = crate::audio::source::create_stream_input(
        http_client.clone(),
        resolved.clone(),
        eight_d_enabled,
    )?;

    let handle = {
        let mut call = call_lock.lock().await;
        call.play_input(source)
    };

    // Register end and error handlers
    let _ = handle.add_event(
        songbird::Event::Track(songbird::TrackEvent::End),
        TrackEndHandler {
            guild_id,
            database: database.clone(),
            guild_players: guild_players.clone(),
            http_client: http_client.clone(),
            serenity_ctx: serenity_ctx.clone(),
        },
    );
    let _ = handle.add_event(
        songbird::Event::Track(songbird::TrackEvent::Error),
        TrackErrorHandler {
            guild_id,
            database: database.clone(),
            guild_players: guild_players.clone(),
            http_client: http_client.clone(),
            serenity_ctx: serenity_ctx.clone(),
        },
    );

    {
        let mut player = player_lock.write().await;
        if player
            .now_playing
            .as_ref()
            .map(|current| current.url.as_str())
            == Some(track.url.as_str())
        {
            if let Some(ref mut np) = player.now_playing {
                np.resolved_url = Some(resolved.clone()); // Save the resolved URL!
            }
            player.current_track_handle = Some(handle.clone());
            player.playback_status = crate::core::PlaybackStatus::Playing;

            // Reset consecutive errors after 5 seconds of successful playback
            let player_lock_clone = player_lock.clone();
            let track_uuid = handle.uuid();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let mut player = player_lock_clone.write().await;
                if let Some(ref current_handle) = player.current_track_handle {
                    if current_handle.uuid() == track_uuid {
                        player.consecutive_errors = 0;
                        tracing::info!("Reset consecutive errors to 0 after 5 seconds of successful playback");
                    }
                }
            });
        } else {
            let _ = handle.stop();
            return Ok(());
        }
    }

    // Schedule prefetching for the next track in the queue (pass http_client!)
    schedule_prefetch(
        guild_id,
        guild_players.clone(),
        track.duration,
        http_client.clone(),
    );

    // Announce track if enabled in database
    let announce_setting = database
        .get_guild_settings(guild_id.get())
        .await
        .announce_track;

    if announce_setting {
        if let Some(channel) = announce_channel {
            let ctx_clone = serenity_ctx.clone();
            let track_clone = track.clone();
            tokio::spawn(async move {
                let embed = now_playing_announce_embed(&track_clone);
                let _ = channel
                    .send_message(&ctx_clone.http, serenity::CreateMessage::new().embed(embed))
                    .await;
            });
        }
    }

    Ok(())
}

pub async fn trigger_prefetch(
    guild_id: serenity::GuildId,
    guild_players: std::sync::Arc<
        dashmap::DashMap<
            serenity::GuildId,
            std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
        >,
    >,
    http_client: reqwest::Client,
) {
    let player_lock = match guild_players.get(&guild_id) {
        Some(p) => p.value().clone(),
        None => return,
    };

    // 1. Resolve ytsearch1: if needed
    let mut needs_resolution = false;
    let mut track_to_resolve = {
        let player = player_lock.read().await;
        if let Some(track) = player.queue.iter().next() {
            if track.url.starts_with("ytsearch1:") {
                needs_resolution = true;
                Some(track.clone())
            } else {
                None
            }
        } else {
            None
        }
    };

    if needs_resolution {
        if let Some(ref mut track) = track_to_resolve {
            if let Err(e) =
                crate::audio::resolver::resolve_ytsearch_track(track, &http_client).await
            {
                tracing::error!("Failed to resolve Spotify track in prefetcher: {:?}", e);
            } else {
                // Update it in the queue
                let mut player = player_lock.write().await;
                if let Some(t) = player.queue.get_mut(0) {
                    if t.url.starts_with("ytsearch1:") {
                        t.url = track.url.clone();
                        if t.thumbnail.is_none() {
                            t.thumbnail = track.thumbnail.clone();
                        }
                    }
                }
            }
        }
    }

    let next_track_url = {
        let player = player_lock.read().await;
        if let Some(track) = player.queue.iter().next() {
            if track.resolved_url.is_none() && !track.url.starts_with("ytsearch1:") {
                Some(track.url.clone())
            } else {
                None
            }
        } else {
            None
        }
    };

    let url_to_resolve = match next_track_url {
        Some(url) => url,
        None => return,
    };

    tracing::info!(guild_id = %guild_id, "Prefetching stream URL for: {}", url_to_resolve);

    match crate::audio::source::prefetch_stream_url_for_guild(guild_id.get(), &url_to_resolve).await
    {
        Ok(Some(resolved_url)) => {
            let mut player = player_lock.write().await;
            if let Some(track) = player.queue.get_mut(0) {
                if track.url == url_to_resolve {
                    track.resolved_url = Some(resolved_url);
                    tracing::info!(guild_id = %guild_id, "Prefetch successful for: {}", track.title);
                }
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(guild_id = %guild_id, "Prefetch failed for {}: {:?}", url_to_resolve, e);
        }
    }
}

pub fn schedule_prefetch(
    guild_id: serenity::GuildId,
    guild_players: std::sync::Arc<
        dashmap::DashMap<
            serenity::GuildId,
            std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
        >,
    >,
    duration: Option<Duration>,
    http_client: reqwest::Client,
) {
    let gp_clone = guild_players.clone();
    let client_clone = http_client.clone();

    // 1. Immediate prefetch to resolve next track search and stream URL right away
    tokio::spawn(async move {
        trigger_prefetch(guild_id, gp_clone, client_clone).await;
    });

    let gp_clone2 = guild_players.clone();
    tokio::spawn(async move {
        if let Some(dur) = duration {
            let settings = crate::audio::runtime::settings();
            let limit = Duration::from_secs(settings.prefetch_when_remaining_seconds).min(dur / 10);
            let delay = dur.saturating_sub(limit);
            tracing::info!(guild_id = %guild_id, "Scheduling fallback prefetch in {:?}", delay);
            tokio::time::sleep(delay).await;
        } else {
            // If duration is unknown, wait 5 seconds and prefetch
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        trigger_prefetch(guild_id, gp_clone2, http_client).await;
    });
}
