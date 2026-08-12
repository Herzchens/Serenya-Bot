use async_trait::async_trait;
use poise::serenity_prelude as serenity;
use songbird::{Event, EventContext, EventHandler};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::core::guild_player::{PlaybackFailureAction, PlaybackFailureState};
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
    pub config: Arc<arc_swap::ArcSwap<crate::config::BotConfig>>,
}

fn stay_in_voice(config: &Arc<arc_swap::ArcSwap<crate::config::BotConfig>>) -> bool {
    config.load().playback.stay_in_voice
}

fn playback_stat_delta(play_time: Duration, completed: bool) -> (u64, u64) {
    (u64::from(completed), play_time.as_secs())
}

fn retry_client_for_track(track: Option<&crate::core::Track>) -> Option<String> {
    track
        .and_then(|track| track.resolved_url.as_ref())
        .filter(|stream| stream.resolve_source.starts_with("api_client_"))
        .map(|stream| stream.client_kind.clone())
}

fn is_suspicious_early_end(
    play_time: Duration,
    expected_duration: Option<Duration>,
    was_skipped: bool,
) -> bool {
    if was_skipped || play_time >= Duration::from_secs(2) {
        return false;
    }

    matches!(
        expected_duration,
        Some(duration) if duration > Duration::from_secs(2)
    )
}

pub(crate) async fn record_guild_playback_stats(
    database: &DatabaseManager,
    guild_id: serenity::GuildId,
    play_time: Duration,
    completed: bool,
) {
    let (songs_played, listening_seconds) = playback_stat_delta(play_time, completed);
    database
        .update_guild_settings_mut(guild_id.get(), |settings| {
            settings.total_songs_played += songs_played;
            settings.total_listening_seconds += listening_seconds;
        })
        .await;
}

async fn record_playback_stats(ctx: &PlaybackContext, play_time: Duration, completed: bool) {
    record_guild_playback_stats(ctx.database.as_ref(), ctx.guild_id, play_time, completed).await;
}

fn claim_interrupted_terminal(
    failure_state: &mut PlaybackFailureState,
    current_handle_uuid: Option<uuid::Uuid>,
    observed_handle_uuid: uuid::Uuid,
) -> bool {
    current_handle_uuid == Some(observed_handle_uuid)
        && failure_state.claim_terminal(observed_handle_uuid)
}

pub(crate) async fn finalize_interrupted_playback_stats(
    database: &DatabaseManager,
    guild_id: serenity::GuildId,
    player_lock: &Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
) {
    let handle_uuid = {
        let player = player_lock.read().await;
        player
            .current_track_handle
            .as_ref()
            .map(|handle| handle.uuid())
    };
    if let Some(handle_uuid) = handle_uuid {
        finalize_interrupted_playback_stats_for_handle(
            database,
            guild_id,
            player_lock,
            handle_uuid,
        )
        .await;
    }
}

pub(crate) async fn interrupted_play_time_for_handle(
    player_lock: &Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    handle_uuid: uuid::Uuid,
) -> Option<Duration> {
    let handle = {
        let player = player_lock.read().await;
        player
            .current_track_handle
            .as_ref()
            .filter(|handle| handle.uuid() == handle_uuid)
            .cloned()
    }?;

    match handle.get_info().await {
        Ok(state) => Some(state.play_time),
        Err(err) => {
            tracing::debug!(
                ?handle_uuid,
                error = %err,
                "Could not read interrupted track state for listening statistics"
            );
            None
        }
    }
}

pub(crate) async fn finalize_interrupted_playback_stats_for_handle(
    database: &DatabaseManager,
    guild_id: serenity::GuildId,
    player_lock: &Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    handle_uuid: uuid::Uuid,
) {
    let Some(play_time) = interrupted_play_time_for_handle(player_lock, handle_uuid).await else {
        return;
    };

    let claimed = {
        let mut player = player_lock.write().await;
        let current_handle_uuid = player
            .current_track_handle
            .as_ref()
            .map(|handle| handle.uuid());
        claim_interrupted_terminal(&mut player.failure_state, current_handle_uuid, handle_uuid)
    };

    if claimed {
        record_guild_playback_stats(database, guild_id, play_time, false).await;
        tracing::debug!(
            guild_id = %guild_id,
            ?handle_uuid,
            listening_seconds = play_time.as_secs(),
            "Recorded interrupted playback listening time"
        );
    }
}

async fn apply_failure_action(
    ctx: &PlaybackContext,
    player_lock: &Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    action: PlaybackFailureAction,
    ended_uuid: Option<uuid::Uuid>,
    track_title: &str,
    announce_channel: Option<serenity::ChannelId>,
) {
    match action {
        PlaybackFailureAction::RetryCurrent => {
            tracing::warn!(
                guild_id = %ctx.guild_id,
                track = track_title,
                "Retrying current track after playback failure"
            );
            let ctx_clone = ctx.clone();
            tokio::spawn(async move {
                if let Err(err) = play_next(ctx_clone.clone(), None, false).await {
                    tracing::error!(
                        guild_id = %ctx_clone.guild_id,
                        "Failed to retry current track: {:?}",
                        err
                    );
                }
            });
        }
        PlaybackFailureAction::Advance => {
            if let Some(channel) = announce_channel {
                let serenity_ctx = ctx.serenity_ctx.clone();
                let title = track_title.to_owned();
                tokio::spawn(async move {
                    let embed = crate::discord::embeds::playback_status_embed(
                        "⚠️ Warning",
                        &format!("Could not play **{}**. Trying the next track.", title),
                        0xFEE75C,
                    );
                    let _ = channel
                        .send_message(
                            &serenity_ctx.http,
                            serenity::CreateMessage::new().embed(embed),
                        )
                        .await;
                });
            }

            let ctx_clone = ctx.clone();
            tokio::spawn(async move {
                if let Err(err) = play_next(ctx_clone.clone(), ended_uuid, true).await {
                    tracing::error!(
                        guild_id = %ctx_clone.guild_id,
                        "Failed to advance after playback failure: {:?}",
                        err
                    );
                }
            });
        }
        PlaybackFailureAction::Abort => {
            {
                let mut player = player_lock.write().await;
                player.cancel_prefetch();
                if let Some(mut failed_track) = player.now_playing.take() {
                    failed_track.resolved_url = None;
                    player.previous_track = Some(failed_track);
                }
                player.current_track_handle = None;
                player.playback_status = crate::core::PlaybackStatus::Idle;
                player.skip_forced = false;
                player.seek_offset = Duration::ZERO;
                player.is_seeking = false;
                player.clear_skip_votes();
                player.failure_state.reset();
            }

            if let Some(manager) = songbird::get(&ctx.serenity_ctx).await
                && let Some(call_lock) = manager.get(ctx.guild_id)
            {
                let mut call = call_lock.lock().await;
                call.stop();
            }

            tracing::error!(
                guild_id = %ctx.guild_id,
                "Playback stopped after three consecutive tracks failed"
            );
            if let Some(channel) = announce_channel {
                let serenity_ctx = ctx.serenity_ctx.clone();
                tokio::spawn(async move {
                    let embed = crate::discord::embeds::error_embed(
                        "Dừng phát nhạc vì ba bài liên tiếp không thể phát. Hàng chờ chưa thử vẫn được giữ lại.",
                    );
                    let _ = channel
                        .send_message(
                            &serenity_ctx.http,
                            serenity::CreateMessage::new().embed(embed),
                        )
                        .await;
                });
            }
        }
    }
}

pub struct TrackEndHandler {
    pub ctx: PlaybackContext,
}

#[async_trait]
impl EventHandler for TrackEndHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let (ended, play_time) = if let EventContext::Track(track_events) = ctx {
            let (state, handle) = track_events.first()?;
            (handle.uuid(), state.play_time)
        } else {
            return None;
        };

        let player_lock = match self.ctx.guild_players.get(&self.ctx.guild_id) {
            Some(entry) => entry.value().clone(),
            None => return None,
        };

        let (was_skipped, expected_duration, claimed) = {
            let mut player = player_lock.write().await;
            if !player.failure_state.matches_active(ended) {
                tracing::info!(
                    guild_id = %self.ctx.guild_id,
                    ?ended,
                    "Ignoring TrackEnd event from stale or stopped track handle"
                );
                return None;
            }
            let claimed = player.failure_state.claim_terminal(ended);
            (
                player.skip_forced,
                player.now_playing.as_ref().and_then(|track| track.duration),
                claimed,
            )
        };

        if !claimed {
            tracing::debug!(
                guild_id = %self.ctx.guild_id,
                ?ended,
                "Ignoring duplicate terminal event"
            );
            return None;
        }

        if is_suspicious_early_end(play_time, expected_duration, was_skipped) {
            let (action, url, title, announce_channel) = {
                let mut player = player_lock.write().await;
                let retry_client = retry_client_for_track(player.now_playing.as_ref());
                let action = player.failure_state.register_failure();
                if action == PlaybackFailureAction::RetryCurrent {
                    player.failure_state.set_retry_excluded_client(retry_client);
                }
                if action == PlaybackFailureAction::Advance {
                    player.skip_forced = true;
                }
                let url = player.now_playing.as_ref().map(|track| track.url.clone());
                let title = player
                    .now_playing
                    .as_ref()
                    .map(|track| track.title.to_string())
                    .unwrap_or_else(|| "current track".to_owned());
                if let Some(ref mut track) = player.now_playing {
                    track.resolved_url = None;
                }
                player.current_track_handle = None;
                player.playback_status = crate::core::PlaybackStatus::Idle;
                (action, url, title, player.announce_channel)
            };

            if let Some(url) = url {
                crate::audio::source::cache_invalidate_stream(&url).await;
            }
            tracing::warn!(
                guild_id = %self.ctx.guild_id,
                ?play_time,
                ?expected_duration,
                ?action,
                "Track ended suspiciously early"
            );
            record_playback_stats(&self.ctx, play_time, false).await;
            apply_failure_action(
                &self.ctx,
                &player_lock,
                action,
                Some(ended),
                &title,
                announce_channel,
            )
            .await;
            return None;
        }

        record_playback_stats(&self.ctx, play_time, !was_skipped).await;
        {
            let mut player = player_lock.write().await;
            player.failure_state.mark_completed(ended);
            player.current_track_handle = None;
            player.playback_status = crate::core::PlaybackStatus::Idle;
        }

        let ctx_clone = self.ctx.clone();
        tokio::spawn(async move {
            if let Err(err) = play_next(ctx_clone.clone(), Some(ended), true).await {
                tracing::error!(
                    guild_id = %ctx_clone.guild_id,
                    "Error in play_next during TrackEnd handling: {:?}",
                    err
                );
            }
        });
        None
    }
}

pub struct TrackErrorHandler {
    pub ctx: PlaybackContext,
}

#[async_trait]
impl EventHandler for TrackErrorHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let (ended, play_time) = if let EventContext::Track(track_events) = ctx {
            let (state, handle) = track_events.first()?;
            (handle.uuid(), state.play_time)
        } else {
            return None;
        };

        let player_lock = match self.ctx.guild_players.get(&self.ctx.guild_id) {
            Some(entry) => entry.value().clone(),
            None => return None,
        };

        let (action, url, title, announce_channel) = {
            let mut player = player_lock.write().await;
            if !player.failure_state.matches_active(ended) {
                tracing::info!(
                    guild_id = %self.ctx.guild_id,
                    ?ended,
                    "Ignoring TrackError event from stale or stopped track handle"
                );
                return None;
            }
            if !player.failure_state.claim_terminal(ended) {
                tracing::debug!(
                    guild_id = %self.ctx.guild_id,
                    ?ended,
                    "Ignoring duplicate terminal event"
                );
                return None;
            }

            let retry_client = retry_client_for_track(player.now_playing.as_ref());
            let action = player.failure_state.register_failure();
            if action == PlaybackFailureAction::RetryCurrent {
                player.failure_state.set_retry_excluded_client(retry_client);
            }
            if action == PlaybackFailureAction::Advance {
                player.skip_forced = true;
            }
            let url = player.now_playing.as_ref().map(|track| track.url.clone());
            let title = player
                .now_playing
                .as_ref()
                .map(|track| track.title.to_string())
                .unwrap_or_else(|| "current track".to_owned());
            if let Some(ref mut track) = player.now_playing {
                track.resolved_url = None;
            }
            player.current_track_handle = None;
            player.playback_status = crate::core::PlaybackStatus::Idle;
            (action, url, title, player.announce_channel)
        };

        if let Some(url) = url {
            crate::audio::source::cache_invalidate_stream(&url).await;
        }
        record_playback_stats(&self.ctx, play_time, false).await;
        tracing::error!(
            guild_id = %self.ctx.guild_id,
            ?action,
            "Track playback error"
        );
        apply_failure_action(
            &self.ctx,
            &player_lock,
            action,
            Some(ended),
            &title,
            announce_channel,
        )
        .await;
        None
    }
}

pub(crate) async fn fail_and_maybe_advance(
    ctx: &PlaybackContext,
    player_lock: &Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    _call_lock: &Arc<tokio::sync::Mutex<songbird::Call>>,
    track_url: &str,
    track_title: &str,
    announce_channel: Option<serenity::ChannelId>,
) -> Result<(), SerenyaError> {
    let action = {
        let mut player = player_lock.write().await;
        let retry_client = retry_client_for_track(player.now_playing.as_ref());
        let action = player.failure_state.register_failure();
        if action == PlaybackFailureAction::RetryCurrent {
            player.failure_state.set_retry_excluded_client(retry_client);
        }
        if action == PlaybackFailureAction::Advance {
            player.skip_forced = true;
        }
        if player.now_playing.as_ref().map(|current| &*current.url) == Some(track_url) {
            if let Some(ref mut current) = player.now_playing {
                current.resolved_url = None;
            }
            player.current_track_handle = None;
            player.playback_status = crate::core::PlaybackStatus::Idle;
        }
        action
    };

    crate::audio::source::cache_invalidate_stream(track_url).await;
    apply_failure_action(
        ctx,
        player_lock,
        action,
        None,
        track_title,
        announce_channel,
    )
    .await;
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
            if !player.failure_state.matches_active(ended) {
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

        let guild_settings = ctx.database.get_guild_settings(ctx.guild_id.get()).await;

        let (track, announce_channel) = {
            let mut player = player_lock.write().await;
            if advance {
                player.advance_queue();
            }
            (player.now_playing.clone(), player.announce_channel)
        };

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
            if !stay_in_voice(&ctx.config) {
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
            let mut track_clone = track.clone();
            let http_client = ctx.http_client.clone();

            let handle = tokio::spawn(async move {
                let res =
                    crate::audio::resolver::resolve_ytsearch_track(&mut track_clone, &http_client)
                        .await;
                (res, track_clone)
            });

            match handle.await {
                Ok((Ok(()), updated_track)) => {
                    track = updated_track;
                    let mut player = player_lock.write().await;
                    if let Some(ref mut np) = player.now_playing
                        && np.url.starts_with("ytsearch1:")
                    {
                        *np = track.clone();
                    }
                }
                Ok((Err(e), _)) => {
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
                }
                Err(join_err) => {
                    tracing::error!(
                        "resolve_ytsearch_track task panicked or was aborted: {:?}",
                        join_err
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
            }
        }

        let cached_resolved = match track.resolved_url.clone() {
            Some(url)
                if crate::audio::source::cached_resolved_stream_is_current(&track.url, &url)
                    .await =>
            {
                Some(url)
            }
            Some(_) => {
                tracing::debug!(guild_id = %ctx.guild_id, track = %track.title, "Discarding stale prefetched stream URL");
                None
            }
            None => None,
        };

        let resolved_res = if let Some(url) = cached_resolved {
            if !crate::audio::source::is_verified_stream_domain(&url.url) {
                Err(SerenyaError::Audio(
                    "Cached stream returned unverified domain".into(),
                ))
            } else {
                Ok(url)
            }
        } else {
            let guild_id = ctx.guild_id.get();
            let url = track.url.clone();
            let client = ctx.http_client.clone();
            let excluded_client = {
                let player = player_lock.read().await;
                player
                    .failure_state
                    .retry_excluded_client()
                    .map(str::to_owned)
            };
            let handle = tokio::spawn(async move {
                crate::audio::source::extract_stream_url_for_guild_excluding(
                    guild_id,
                    &url,
                    &client,
                    excluded_client.as_deref(),
                )
                .await
            });
            match handle.await {
                Ok(Ok(url)) => {
                    if !crate::audio::source::is_verified_stream_domain(&url.url) {
                        tracing::warn!("Stream resolution returned unverified domain: {}", url.url);
                        Err(SerenyaError::Audio(
                            "Stream resolution returned unverified domain".into(),
                        ))
                    } else {
                        Ok(Arc::new(url))
                    }
                }
                Ok(Err(e)) => Err(e),
                Err(join_err) => {
                    tracing::error!("Stream resolution task panicked or aborted: {:?}", join_err);
                    Err(SerenyaError::Audio(
                        "Stream resolution task panicked or aborted".into(),
                    ))
                }
            }
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
                player.failure_state.begin_attempt(handle.uuid());
                player.playback_status = crate::core::PlaybackStatus::Playing;

                let player_lock_clone = player_lock.clone();
                let track_uuid = handle.uuid();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let mut player = player_lock_clone.write().await;
                    if let Some(ref current_handle) = player.current_track_handle
                        && current_handle.uuid() == track_uuid
                    {
                        player.failure_state.mark_stable_success(track_uuid);
                        tracing::debug!(
                            "Reset playback failure streak after 5 seconds of successful playback"
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
            let config_clone = ctx.config.load_full();
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

        let mut track_clone = track.clone();
        let client_clone = http_client.clone();

        let handle = tokio::spawn(async move {
            let res =
                crate::audio::resolver::resolve_ytsearch_track(&mut track_clone, &client_clone)
                    .await;
            (res, track_clone)
        });

        match handle.await {
            Ok((Ok(()), updated_track)) => {
                *track = updated_track;
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
            Ok((Err(e), _)) => {
                tracing::error!("Failed to resolve Spotify track in prefetcher: {:?}", e);
            }
            Err(e) => {
                tracing::error!("Prefetch resolver task panicked or was aborted: {:?}", e);
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

    let guild_id_val = guild_id.get();
    let url_clone = url_to_resolve.clone();
    let client_clone = http_client.clone();

    let handle = tokio::spawn(async move {
        crate::audio::source::prefetch_stream_url_for_guild(guild_id_val, &url_clone, &client_clone)
            .await
    });

    match handle.await {
        Ok(Ok(Some(resolved_url))) => {
            if token.is_cancelled() {
                return;
            }
            let mut player = player_lock.write().await;
            if player.prefetch_generation == generation
                && let Some(track) = player.queue.get_mut(0)
                && track.url == url_to_resolve
            {
                if crate::audio::source::is_verified_stream_domain(&resolved_url.url) {
                    track.resolved_url = Some(Arc::new(resolved_url));
                    tracing::debug!(guild_id = %guild_id, "Prefetch successful for: {}", track.title);
                } else {
                    tracing::warn!(guild_id = %guild_id, url = %resolved_url.url, "Prefetch rejected due to unverified domain");
                }
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => {
            tracing::warn!(guild_id = %guild_id, "Prefetch failed for {}: {:?}", url_to_resolve, e);
        }
        Err(e) => {
            tracing::error!(guild_id = %guild_id, "Prefetch task panicked or was aborted for {}: {:?}", url_to_resolve, e);
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

#[cfg(test)]
mod tests {
    use super::is_suspicious_early_end;
    use std::time::Duration;

    #[test]
    fn legitimate_short_track_is_not_classified_as_failure() {
        assert!(!is_suspicious_early_end(
            Duration::from_secs(1),
            Some(Duration::from_secs(1)),
            false,
        ));
    }

    #[test]
    fn long_track_ending_immediately_is_classified_as_failure() {
        assert!(is_suspicious_early_end(
            Duration::from_secs(1),
            Some(Duration::from_secs(180)),
            false,
        ));
    }

    #[test]
    fn unknown_duration_is_not_guessed_to_be_a_failure() {
        assert!(!is_suspicious_early_end(
            Duration::from_millis(500),
            None,
            false,
        ));
    }

    #[test]
    fn manual_skip_is_never_classified_as_early_failure() {
        assert!(!is_suspicious_early_end(
            Duration::from_millis(100),
            Some(Duration::from_secs(180)),
            true,
        ));
    }

    #[test]
    fn playback_past_early_window_is_not_classified_as_failure() {
        assert!(!is_suspicious_early_end(
            Duration::from_secs(2),
            Some(Duration::from_secs(180)),
            false,
        ));
    }
}

#[cfg(test)]
mod live_config_and_stats_tests {
    use super::{playback_stat_delta, stay_in_voice};
    use crate::config::BotConfig;
    use std::sync::Arc;
    use std::time::Duration;

    fn example_config() -> BotConfig {
        serde_saphyr::from_str(include_str!("../../config.example.yml")).unwrap()
    }

    #[test]
    fn active_playback_reads_latest_stay_in_voice_value() {
        let mut initial = example_config();
        initial.playback.stay_in_voice = true;
        let live = Arc::new(arc_swap::ArcSwap::from_pointee(initial));
        assert!(stay_in_voice(&live));

        let mut reloaded = example_config();
        reloaded.playback.stay_in_voice = false;
        live.store(Arc::new(reloaded));
        assert!(!stay_in_voice(&live));
    }

    #[test]
    fn skipped_tracks_add_listening_time_but_not_completed_count() {
        assert_eq!(playback_stat_delta(Duration::from_secs(37), false), (0, 37));
        assert_eq!(playback_stat_delta(Duration::from_secs(37), true), (1, 37));
    }
}

#[cfg(test)]
mod retry_client_source_tests {
    use super::retry_client_for_track;
    use crate::core::{SourceType, Track};
    use poise::serenity_prelude as serenity;
    use std::sync::Arc;

    fn track_with_stream(resolve_source: &str, client_kind: &str) -> Track {
        Track {
            title: "test".into(),
            url: "https://www.youtube.com/watch?v=test".into(),
            duration: None,
            requester_id: serenity::UserId::new(1),
            requester_name: None,
            source_type: SourceType::Url,
            resolved_url: Some(Arc::new(youtube_resolver::ResolvedStream {
                url: "https://rr1.googlevideo.com/test".to_owned(),
                client_kind: client_kind.to_owned(),
                user_agent: "test".to_owned(),
                expires_at: None,
                mime_type: None,
                bitrate: None,
                resolve_source: resolve_source.to_owned(),
            })),
            thumbnail: None,
            source_provider: Arc::from("test"),
        }
    }

    #[test]
    fn native_api_stream_client_is_excluded_for_retry() {
        let track = track_with_stream("api_client_android_vr", "ANDROID_VR");
        assert_eq!(
            retry_client_for_track(Some(&track)).as_deref(),
            Some("ANDROID_VR")
        );
    }

    #[test]
    fn non_api_stream_sources_are_not_client_excluded() {
        let track = track_with_stream("invidious", "WEB");
        assert_eq!(retry_client_for_track(Some(&track)), None);
    }
}

#[cfg(test)]
mod interrupted_stats_tests {
    use super::claim_interrupted_terminal;
    use crate::core::guild_player::PlaybackFailureState;

    #[test]
    fn interrupted_terminal_is_claimed_once_for_the_current_handle() {
        let mut state = PlaybackFailureState::default();
        let handle = uuid::Uuid::from_u128(401);
        state.begin_attempt(handle);
        assert!(claim_interrupted_terminal(&mut state, Some(handle), handle));
        assert!(!claim_interrupted_terminal(
            &mut state,
            Some(handle),
            handle
        ));
    }

    #[test]
    fn interrupted_terminal_does_not_claim_a_replaced_handle() {
        let mut state = PlaybackFailureState::default();
        let observed = uuid::Uuid::from_u128(402);
        let replacement = uuid::Uuid::from_u128(403);
        state.begin_attempt(observed);
        assert!(!claim_interrupted_terminal(
            &mut state,
            Some(replacement),
            observed,
        ));
        assert!(claim_interrupted_terminal(
            &mut state,
            Some(observed),
            observed,
        ));
    }
}
