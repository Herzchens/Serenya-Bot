use async_trait::async_trait;
use poise::serenity_prelude as serenity;
use songbird::{Event, EventContext, EventHandler};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::database::DatabaseManager;
use crate::discord::embeds::now_playing_announce_embed;
use crate::utils::SerenyaError;

#[derive(Clone)]
pub struct PlaybackContext {
    pub guild_id: serenity::GuildId,
    pub database: Arc<DatabaseManager>,
    pub guild_players: Arc<
        dashmap::DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>>,
    >,
    pub http_client: reqwest::Client,
    pub serenity_ctx: serenity::Context,
    pub config: Arc<crate::config::BotConfig>,
}

pub struct TrackEndHandler {
    pub ctx: PlaybackContext,
}

#[async_trait]
impl EventHandler for TrackEndHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let (ended_uuid, was_skipped, play_time, is_stale) =
            if let EventContext::Track(track_events) = ctx {
                if let Some((state, handle)) = track_events.first() {
                    let ended = handle.uuid();
                    let (skip_forced, is_stale) = if let Some(player_lock_ref) =
                        self.ctx.guild_players.get(&self.ctx.guild_id)
                    {
                        let player_lock = player_lock_ref.value().clone();
                        drop(player_lock_ref);
                        let player = player_lock.read().await;
                        let is_stale = if let Some(ref current_handle) = player.current_track_handle
                        {
                            current_handle.uuid() != ended
                        } else {
                            true
                        };
                        (player.skip_forced, is_stale)
                    } else {
                        (false, true)
                    };
                    (Some(ended), skip_forced, state.play_time, is_stale)
                } else {
                    (None, false, Duration::from_secs(0), true)
                }
            } else {
                (None, false, Duration::from_secs(0), true)
            };

        if is_stale {
            if let Some(ended) = ended_uuid {
                tracing::info!(
                    "Ignoring TrackEnd event from stale or stopped track handle: {:?}",
                    ended
                );
            }
            return None;
        }
        if let Some(ended) = ended_uuid {
            let mut retry_current = false;
            if play_time < Duration::from_secs(2) && !was_skipped {
                tracing::warn!(
                    guild_id = %self.ctx.guild_id,
                    ?play_time,
                    "Track ended too quickly without being skipped, incrementing consecutive errors"
                );
                if let Some(player_lock_ref) = self.ctx.guild_players.get(&self.ctx.guild_id) {
                    let player_lock = player_lock_ref.value().clone();
                    drop(player_lock_ref);
                    let mut player = player_lock.write().await;
                    player.consecutive_errors += 1;
                    let consecutive_errors = player.consecutive_errors;
                    retry_current = true;

                    let url_opt = player.now_playing.as_ref().map(|np| np.url.clone());
                    if let Some(ref mut np) = player.now_playing {
                        np.resolved_url = None;
                    }
                    drop(player);

                    if let Some(url) = url_opt {
                        crate::audio::source::cache_invalidate_stream(&url).await;
                    }

                    tracing::warn!(
                        guild_id = %self.ctx.guild_id,
                        consecutive_errors,
                        "Consecutive errors count: {}",
                        consecutive_errors
                    );
                }
            }

            let ctx_clone = self.ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = play_next(
                    ctx_clone,
                    if retry_current { None } else { Some(ended) },
                    !retry_current,
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

pub struct TrackErrorHandler {
    pub ctx: PlaybackContext,
}

#[async_trait]
impl EventHandler for TrackErrorHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(track_events) = ctx
            && let Some((_, handle)) = track_events.first()
        {
            let ended = handle.uuid();
            let is_stale =
                if let Some(player_lock_ref) = self.ctx.guild_players.get(&self.ctx.guild_id) {
                    let player_lock = player_lock_ref.value().clone();
                    drop(player_lock_ref);
                    let player = player_lock.read().await;
                    if let Some(ref current_handle) = player.current_track_handle {
                        current_handle.uuid() != ended
                    } else {
                        true
                    }
                } else {
                    true
                };

            if is_stale {
                tracing::info!(
                    "Ignoring TrackError event from stale or stopped track handle: {:?}",
                    ended
                );
                return None;
            }

            tracing::error!(guild_id = %self.ctx.guild_id, "Track errored (End handler will advance queue)");
            if let Some(player_lock_ref) = self.ctx.guild_players.get(&self.ctx.guild_id) {
                let player_lock = player_lock_ref.value().clone();
                drop(player_lock_ref);
                let mut player = player_lock.write().await;
                player.consecutive_errors += 1;
                let consecutive_errors = player.consecutive_errors;
                let url_opt = player.now_playing.as_ref().map(|np| np.url.clone());
                drop(player);

                tracing::warn!(
                    guild_id = %self.ctx.guild_id,
                    consecutive_errors,
                    "Track errored: consecutive errors count: {}",
                    consecutive_errors
                );

                if let Some(url) = url_opt {
                    crate::audio::source::cache_invalidate_stream(&url).await;
                }
            }
        }
        None
    }
}

async fn fail_and_maybe_advance(
    ctx: &PlaybackContext,
    player_lock: &Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    call_lock: &Arc<tokio::sync::Mutex<songbird::Call>>,
    track_url: &str,
    track_title: &str,
    announce_channel: Option<serenity::ChannelId>,
) -> Result<(), SerenyaError> {
    let (consecutive_errors, url_opt) = {
        let mut player = player_lock.write().await;
        player.consecutive_errors += 1;

        if player.now_playing.as_ref().map(|current| &*current.url) == Some(track_url) {
            player.now_playing = None;
            player.current_track_handle = None;
            player.playback_status = crate::core::PlaybackStatus::Idle;
        }
        (player.consecutive_errors, Some(track_url.to_string()))
    };

    if let Some(url) = url_opt {
        crate::audio::source::cache_invalidate_stream(&url).await;
    }

    if consecutive_errors >= 3 {
        tracing::error!(
            "Aborting play_next: too many consecutive errors ({})",
            consecutive_errors
        );
        {
            let mut call = call_lock.lock().await;
            call.stop();
        }
        {
            let mut player = player_lock.write().await;
            player.reset();
        }
        if let Some(channel) = announce_channel {
            let ctx_clone = ctx.serenity_ctx.clone();
            tokio::spawn(async move {
                let embed = crate::discord::embeds::error_embed(
                    "Dừng phát nhạc do quá nhiều lỗi liên tiếp. Vui lòng kiểm tra lại nguồn phát hoặc thử lại sau.",
                );
                let _ = channel
                    .send_message(&ctx_clone.http, serenity::CreateMessage::new().embed(embed))
                    .await;
            });
        }
        return Ok(());
    }

    if let Some(channel) = announce_channel {
        let ctx_clone = ctx.serenity_ctx.clone();
        let title_clone = track_title.to_owned();
        tokio::spawn(async move {
            let embed = crate::discord::embeds::playback_status_embed(
                "⚠️ Warning",
                &format!(
                    "Could not resolve **{}**. Trying the next track.",
                    title_clone
                ),
                0xFEE75C,
            );
            let _ = channel
                .send_message(&ctx_clone.http, serenity::CreateMessage::new().embed(embed))
                .await;
        });
    }

    let ctx_clone = ctx.clone();
    tokio::spawn(async move {
        if let Err(next_err) = play_next(ctx_clone.clone(), None, true).await {
            tracing::error!(
                guild_id = %ctx_clone.guild_id,
                "Failed to continue in play_next after stream resolution error: {:?}",
                next_err
            );
        }
    });

    Ok(())
}

pub fn play_next(
    ctx: PlaybackContext,
    ended_uuid: Option<uuid::Uuid>,
    advance: bool,
) -> std::pin::Pin<Box<dyn Future<Output = Result<(), SerenyaError>> + Send + 'static>> {
    Box::pin(async move {
        let player_lock = ctx
            .guild_players
            .get(&ctx.guild_id)
            .map(|r| r.value().clone())
            .ok_or_else(|| SerenyaError::NotFound("Guild player not found".into()))?;

        if let Some(ended) = ended_uuid {
            let player = player_lock.read().await;
            if player.is_seeking {
                tracing::info!("Ignoring End/Error event because player is seeking");
                return Ok(());
            }
            if let Some(ref current_handle) = player.current_track_handle
                && current_handle.uuid() != ended
            {
                tracing::info!("Ignoring End/Error event from stale track handle");
                return Ok(());
            }
        }

        let songbird_manager = songbird::get(&ctx.serenity_ctx)
            .await
            .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized".into()))?
            .clone();

        let call_lock = songbird_manager
            .get(ctx.guild_id)
            .ok_or_else(|| SerenyaError::Voice("Not connected to a voice channel".into()))?;

        let finished_track = {
            let player = player_lock.read().await;
            player.now_playing.clone()
        };

        let guild_settings = ctx.database.get_guild_settings(ctx.guild_id.get()).await;

        if let Some(track) = finished_track {
            ctx.database
                .update_guild_settings_mut(ctx.guild_id.get(), |settings| {
                    settings.total_songs_played += 1;
                    if let Some(dur) = track.duration {
                        settings.total_listening_seconds += dur.as_secs();
                    }
                })
                .await;
        }

        let (track, announce_channel, consecutive_errors) = {
            let mut player = player_lock.write().await;
            if advance {
                player.advance_queue();
            }
            (
                player.now_playing.clone(),
                player.announce_channel,
                player.consecutive_errors,
            )
        };

        if consecutive_errors >= 3 {
            tracing::error!(
                "Aborting play_next: too many consecutive errors ({})",
                consecutive_errors
            );
            {
                let mut call = call_lock.lock().await;
                call.stop();
            }
            {
                let mut player = player_lock.write().await;
                player.reset();
            }
            if let Some(channel) = announce_channel {
                let ctx_clone = ctx.serenity_ctx.clone();
                tokio::spawn(async move {
                    let embed = crate::discord::embeds::error_embed(
                        "Dừng phát nhạc do quá nhiều lỗi liên tiếp. Vui lòng kiểm tra lại nguồn phát hoặc thử lại sau.",
                    );
                    let _ = channel
                        .send_message(&ctx_clone.http, serenity::CreateMessage::new().embed(embed))
                        .await;
                });
            }
            return Ok(());
        }

        let Some(mut track) = track else {
            {
                let mut call = call_lock.lock().await;
                call.stop();
            }
            {
                let mut player = player_lock.write().await;
                player.current_track_handle = None;
                player.playback_status = crate::core::PlaybackStatus::Idle;
            }

            let announce_setting = guild_settings.announce_track;

            if announce_setting && let Some(channel) = announce_channel {
                let ctx_clone = ctx.serenity_ctx.clone();
                tokio::spawn(async move {
                    let embed = crate::discord::embeds::queue_finished_embed();
                    let _ = channel
                        .send_message(&ctx_clone.http, serenity::CreateMessage::new().embed(embed))
                        .await;
                });
            }

            // If stay_in_voice is disabled, disconnect and reclaim resources
            if !ctx.config.playback.stay_in_voice {
                tracing::info!(
                    guild_id = %ctx.guild_id,
                    "Queue finished and stay_in_voice=false, disconnecting"
                );
                {
                    let mut player = player_lock.write().await;
                    player.reset();
                    player.voice_channel = None;
                    player.announce_channel = None;
                }
                ctx.guild_players.remove(&ctx.guild_id);
                let _ = songbird_manager.remove(ctx.guild_id).await;
                crate::audio::runtime::cleanup_guild(ctx.guild_id.get());
            }

            return Ok(());
        };

        if track.url.starts_with("ytsearch1:") {
            if let Err(e) =
                crate::audio::resolver::resolve_ytsearch_track(&mut track, &ctx.http_client).await
            {
                tracing::error!("Failed to resolve Spotify track search: {:?}", e);
                return fail_and_maybe_advance(
                    &ctx,
                    &player_lock,
                    &call_lock,
                    &track.url,
                    &track.title,
                    announce_channel,
                )
                .await;
            } else {
                let mut player = player_lock.write().await;
                if let Some(ref mut np) = player.now_playing
                    && np.url.starts_with("ytsearch1:")
                {
                    *np = track.clone();
                }
            }
        }

        let resolved_res = match track.resolved_url.clone() {
            Some(url) => Ok(url),
            None => crate::audio::source::extract_stream_url_for_guild(
                ctx.guild_id.get(),
                &track.url,
                &ctx.http_client,
            )
            .await
            .map(Arc::new),
        };

        let resolved = match resolved_res {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!(
                    guild_id = %ctx.guild_id,
                    track = %track.title,
                    "Failed to resolve stream URL in play_next: {:?}",
                    e
                );
                return fail_and_maybe_advance(
                    &ctx,
                    &player_lock,
                    &call_lock,
                    &track.url,
                    &track.title,
                    announce_channel,
                )
                .await;
            }
        };

        tracing::info!(
            guild_id = %ctx.guild_id,
            track = %track.title,
            "Playing resolved stream URL"
        );

        let eight_d_enabled = {
            let player = player_lock.read().await;
            player.eight_d_enabled
        };

        let source = match crate::audio::source::create_stream_input(
            Some(track.url.to_string()),
            &resolved,
            eight_d_enabled,
        )
        .await
        {
            Ok(src) => src,
            Err(e) => {
                tracing::warn!(
                    guild_id = %ctx.guild_id,
                    track = %track.title,
                    "Failed to create stream input in play_next: {:?}",
                    e
                );
                return fail_and_maybe_advance(
                    &ctx,
                    &player_lock,
                    &call_lock,
                    &track.url,
                    &track.title,
                    announce_channel,
                )
                .await;
            }
        };

        let handle = {
            let mut call = call_lock.lock().await;
            call.play_input(source)
        };

        let _ = handle.add_event(
            Event::Track(songbird::TrackEvent::End),
            TrackEndHandler { ctx: ctx.clone() },
        );
        let _ = handle.add_event(
            Event::Track(songbird::TrackEvent::Error),
            TrackErrorHandler { ctx: ctx.clone() },
        );

        {
            let mut player = player_lock.write().await;
            if player.now_playing.as_ref().map(|current| &*current.url) == Some(&*track.url) {
                if let Some(ref mut np) = player.now_playing {
                    np.resolved_url = Some(resolved);
                }
                player.current_track_handle = Some(handle.clone());
                player.playback_status = crate::core::PlaybackStatus::Playing;

                let player_lock_clone = player_lock.clone();
                let track_uuid = handle.uuid();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let mut player = player_lock_clone.write().await;
                    if let Some(ref current_handle) = player.current_track_handle
                        && current_handle.uuid() == track_uuid
                    {
                        player.consecutive_errors = 0;
                        tracing::debug!(
                            "Reset consecutive errors to 0 after 5 seconds of successful playback"
                        );
                    }
                });
            } else {
                let _ = handle.stop();
                return Ok(());
            }
        }

        schedule_prefetch(
            ctx.guild_id,
            Arc::clone(&ctx.guild_players),
            track.duration,
            ctx.http_client.clone(),
        );

        let announce_setting = guild_settings.announce_track;

        if advance
            && announce_setting
            && let Some(channel) = announce_channel
        {
            let ctx_clone = ctx.serenity_ctx.clone();
            let config_clone = ctx.config.clone();
            tokio::spawn(async move {
                let embed = now_playing_announce_embed(&track, &config_clone);
                let _ = channel
                    .send_message(
                        &ctx_clone.http,
                        serenity::CreateMessage::new()
                            .embed(embed)
                            .flags(serenity::MessageFlags::SUPPRESS_NOTIFICATIONS),
                    )
                    .await;
            });
        }

        Ok(())
    })
}

pub async fn trigger_prefetch(
    guild_id: serenity::GuildId,
    guild_players: Arc<
        dashmap::DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>>,
    >,
    http_client: reqwest::Client,
) {
    let (token, generation) = {
        let player_lock = match guild_players.get(&guild_id) {
            Some(p) => p.value().clone(),
            None => return,
        };
        let mut player = player_lock.write().await;
        if player.queue.is_empty() {
            return;
        }
        player.start_prefetch()
    };

    trigger_prefetch_with_context(guild_id, guild_players, http_client, token, generation).await;
}

pub async fn trigger_prefetch_with_context(
    guild_id: serenity::GuildId,
    guild_players: Arc<
        dashmap::DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>>,
    >,
    http_client: reqwest::Client,
    token: CancellationToken,
    generation: u64,
) {
    let player_lock = match guild_players.get(&guild_id) {
        Some(p) => p.value().clone(),
        None => return,
    };

    if token.is_cancelled() {
        return;
    }

    let mut needs_resolution = false;
    let mut track_to_resolve = {
        let player = player_lock.read().await;
        if player.prefetch_generation != generation {
            return;
        }
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

    if needs_resolution && let Some(ref mut track) = track_to_resolve {
        if token.is_cancelled() {
            return;
        }
        if let Err(e) = crate::audio::resolver::resolve_ytsearch_track(track, &http_client).await {
            tracing::error!("Failed to resolve Spotify track in prefetcher: {:?}", e);
        } else {
            if token.is_cancelled() {
                return;
            }
            let mut player = player_lock.write().await;
            if player.prefetch_generation == generation {
                if let Some(t) = player.queue.get_mut(0)
                    && t.url.starts_with("ytsearch1:")
                {
                    t.url = track.url.clone();
                    if t.thumbnail.is_none() {
                        t.thumbnail = track.thumbnail.clone();
                    }
                }
            } else {
                return;
            }
        }
    }

    if token.is_cancelled() {
        return;
    }

    let next_track_url = {
        let player = player_lock.read().await;
        if player.prefetch_generation != generation {
            return;
        }
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

    tracing::debug!(guild_id = %guild_id, "Prefetching stream URL for: {}", url_to_resolve);

    if token.is_cancelled() {
        return;
    }

    match crate::audio::source::prefetch_stream_url_for_guild(
        guild_id.get(),
        &url_to_resolve,
        &http_client,
    )
    .await
    {
        Ok(Some(resolved_url)) => {
            if token.is_cancelled() {
                return;
            }
            let mut player = player_lock.write().await;
            if player.prefetch_generation == generation
                && let Some(track) = player.queue.get_mut(0)
                && track.url == url_to_resolve
            {
                track.resolved_url = Some(Arc::new(resolved_url));
                tracing::debug!(guild_id = %guild_id, "Prefetch successful for: {}", track.title);
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
    guild_players: Arc<
        dashmap::DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>>,
    >,
    duration: Option<Duration>,
    http_client: reqwest::Client,
) {
    let gp_clone = guild_players.clone();
    let http_client_clone = http_client.clone();
    tokio::spawn(async move {
        let (token, generation) = {
            let player_lock = match gp_clone.get(&guild_id) {
                Some(p) => p.value().clone(),
                None => return,
            };
            let mut player = player_lock.write().await;
            if player.queue.is_empty() {
                return;
            }
            player.start_prefetch()
        };

        let sleep_duration = if let Some(dur) = duration {
            let settings = crate::audio::runtime::settings();
            let limit = Duration::from_secs(settings.prefetch_when_remaining_seconds).min(dur / 10);
            dur.saturating_sub(limit)
        } else {
            Duration::from_secs(5)
        };

        tokio::select! {
            _ = tokio::time::sleep(sleep_duration) => {}
            _ = token.cancelled() => {
                tracing::debug!(guild_id = %guild_id, "Scheduled prefetch cancelled during sleep");
                return;
            }
        }

        trigger_prefetch_with_context(guild_id, gp_clone, http_client_clone, token, generation)
            .await;
    });
}
