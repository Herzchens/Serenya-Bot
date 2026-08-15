use crate::utils::{Context, Error, SerenyaError};
use std::time::Duration;

fn restart_play_mode(status: crate::core::PlaybackStatus) -> songbird::tracks::PlayMode {
    match status {
        crate::core::PlaybackStatus::Paused => songbird::tracks::PlayMode::Pause,
        _ => songbird::tracks::PlayMode::Play,
    }
}

fn format_seek_time(d: Duration) -> String {
    let total_secs = d.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}", mins, secs)
}

pub(crate) fn add_seek_duration(left: Duration, right: Duration) -> Result<Duration, SerenyaError> {
    left.checked_add(right).ok_or_else(|| {
        SerenyaError::Config("Seek target exceeds the supported duration range.".into())
    })
}

pub(crate) async fn seek_by_restart(
    ctx: Context<'_>,
    guild_id: poise::serenity_prelude::GuildId,
    player_lock: std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    target_position: Duration,
) -> Result<(), Error> {
    let (url, interrupted_handle_uuid) = {
        let player = player_lock.read().await;
        let url = player
            .now_playing
            .as_ref()
            .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?
            .url
            .clone();
        let handle_uuid = player
            .current_track_handle
            .as_ref()
            .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?
            .uuid();
        (url, handle_uuid)
    };

    let stream = std::sync::Arc::new(
        crate::audio::source::extract_stream_url_for_guild(
            guild_id.get(),
            &url,
            &ctx.data().http_client,
        )
        .await?,
    );

    let eight_d_enabled = {
        let player = player_lock.read().await;
        player.eight_d_enabled
    };
    let source = crate::audio::source::create_ffmpeg_stream_input(
        Some(url.to_string()),
        &stream,
        Some(target_position),
        eight_d_enabled,
    )
    .await?;

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    let call_lock = manager
        .get(guild_id)
        .ok_or_else(|| SerenyaError::Voice("Not connected to a voice channel.".into()))?;

    crate::audio::events::finalize_interrupted_playback_stats_for_handle(
        ctx.data().database.as_ref(),
        guild_id,
        &player_lock,
        interrupted_handle_uuid,
    )
    .await;

    let old_handle_opt = {
        let mut player = player_lock.write().await;
        let current_handle_uuid = player
            .current_track_handle
            .as_ref()
            .map(|handle| handle.uuid());
        if current_handle_uuid != Some(interrupted_handle_uuid)
            || player.now_playing.as_ref().map(|track| &*track.url) != Some(&*url)
        {
            return Err(SerenyaError::Voice(
                "Track changed while seek was being prepared. Try the command again.".into(),
            )
            .into());
        }
        player.is_seeking = true;
        player.seek_offset = target_position;
        let restart_mode = restart_play_mode(player.playback_status);
        (player.current_track_handle.take(), restart_mode)
    };

    let (old_handle_opt, restart_mode) = old_handle_opt;
    let handle = {
        let mut call = call_lock.lock().await;
        let mut track = songbird::tracks::Track::from(source);
        track.playing = restart_mode;
        call.play(track)
    };

    let playback_ctx = crate::audio::events::PlaybackContext {
        guild_id,
        database: std::sync::Arc::clone(&ctx.data().database),
        guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
        http_client: ctx.data().http_client.clone(),
        serenity_ctx: ctx.serenity_context().clone(),
        config: std::sync::Arc::clone(&ctx.data().config),
    };

    let end_handler = crate::audio::events::TrackEndHandler {
        ctx: playback_ctx.clone(),
    };
    let error_handler = crate::audio::events::TrackErrorHandler { ctx: playback_ctx };
    if let Err(err) = crate::audio::events::register_terminal_handlers(
        || {
            handle.add_event(
                songbird::Event::Track(songbird::TrackEvent::End),
                end_handler,
            )
        },
        || {
            handle.add_event(
                songbird::Event::Track(songbird::TrackEvent::Error),
                error_handler,
            )
        },
    ) {
        let _ = handle.stop();
        if let Some(ref old_handle) = old_handle_opt {
            let _ = old_handle.stop();
        }
        let mut player = player_lock.write().await;
        player.current_track_handle = None;
        player.playback_status = crate::core::PlaybackStatus::Idle;
        player.is_seeking = false;
        return Err(SerenyaError::Voice(format!(
            "Failed to register seek playback lifecycle handlers: {err}"
        ))
        .into());
    }

    {
        let mut player = player_lock.write().await;
        if player.now_playing.as_ref().map(|current| &*current.url) == Some(&*url) {
            player.failure_state.reset();
            player.failure_state.begin_attempt(handle.uuid());
            player.current_track_handle = Some(handle.clone());
        } else {
            let _ = handle.stop();
        }
        player.is_seeking = false;
    }

    if let Some(old_handle) = old_handle_opt {
        let _ = old_handle.stop();
    }

    Ok(())
}

/// Seek to a specific position in the track.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn seek(
    ctx: Context<'_>,
    #[description = "Time to seek (e.g. 1m20s or 80)"] time: String,
) -> Result<(), Error> {
    ctx.defer().await?;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let duration = crate::utils::time::parse_duration(&time)
        .map_err(|e| SerenyaError::Config(format!("Invalid time format: {e}")))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    seek_by_restart(ctx, guild_id, player_lock, duration).await?;
    let embed = crate::discord::embeds::playback_status_embed(
        "⏩ Seek",
        &format!("Seeked to **{time}**."),
        0x5865F2,
    );
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Fast-forward the song by a duration.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("fw"),
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn forward(
    ctx: Context<'_>,
    #[description = "Time to forward (default 10s)"] time: Option<String>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let duration = match time {
        Some(t) => crate::utils::time::parse_duration(&t)?,
        None => Duration::from_secs(10),
    };

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let (handle, seek_offset) = {
        let player = player_lock.read().await;
        let handle = player
            .current_track_handle
            .as_ref()
            .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?
            .clone();
        (handle, player.seek_offset)
    };

    let info = handle.get_info().await?;
    let elapsed = add_seek_duration(seek_offset, info.position)?;
    let new_pos = add_seek_duration(elapsed, duration)?;

    seek_by_restart(ctx, guild_id, player_lock, new_pos).await?;
    let new_pos_fmt = format_seek_time(new_pos);
    let embed = crate::discord::embeds::playback_status_embed(
        "⏩ Forward",
        &format!(
            "Forwarded by **{}s** → `{}`",
            duration.as_secs(),
            new_pos_fmt
        ),
        0x5865F2,
    );
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Rewind the song by a duration.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("rw"),
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn rewind(
    ctx: Context<'_>,
    #[description = "Time to rewind (default 10s)"] time: Option<String>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let duration = match time {
        Some(t) => crate::utils::time::parse_duration(&t)?,
        None => Duration::from_secs(10),
    };

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let (handle, seek_offset) = {
        let player = player_lock.read().await;
        let handle = player
            .current_track_handle
            .as_ref()
            .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?
            .clone();
        (handle, player.seek_offset)
    };

    let info = handle.get_info().await?;
    let total_elapsed = add_seek_duration(seek_offset, info.position)?;
    let new_pos = total_elapsed
        .checked_sub(duration)
        .unwrap_or(Duration::from_secs(0));

    seek_by_restart(ctx, guild_id, player_lock, new_pos).await?;
    let new_pos_fmt = format_seek_time(new_pos);
    let embed = crate::discord::embeds::playback_status_embed(
        "⏪ Rewind",
        &format!("Rewound by **{}s** → `{}`", duration.as_secs(), new_pos_fmt),
        0x5865F2,
    );
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

pub(crate) async fn run_control_transition<RFut, TFut, E>(
    reply: RFut,
    transition: TFut,
) -> Result<(), E>
where
    RFut: std::future::Future<Output = Result<(), E>>,
    TFut: std::future::Future<Output = Result<(), E>>,
{
    transition.await?;
    reply.await
}

/// Replay the current song, or play the previous one if idle.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn replay(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let mut player = player_lock.write().await;

    if player.current_track_handle.is_some() {
        drop(player);
        seek_by_restart(ctx, guild_id, player_lock, Duration::ZERO).await?;
        let embed = crate::discord::embeds::playback_status_embed(
            "🔄 Replay",
            "Replaying current track from the beginning.",
            0x5865F2,
        );
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
    } else if let Some(prev) = player.previous_track.take() {
        let prev_title = prev.title.clone();
        player.queue.push_front(prev);
        drop(player);
        let embed = crate::discord::embeds::playback_status_embed(
            "🔄 Replay",
            &format!("Replaying previous track: **{}**", prev_title),
            0x5865F2,
        );
        let playback_ctx = crate::audio::events::PlaybackContext {
            guild_id,
            database: std::sync::Arc::clone(&ctx.data().database),
            guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
            http_client: ctx.data().http_client.clone(),
            serenity_ctx: ctx.serenity_context().clone(),
            config: std::sync::Arc::clone(&ctx.data().config),
        };
        run_control_transition(
            async {
                ctx.send(poise::CreateReply::default().embed(embed)).await?;
                Ok::<(), Error>(())
            },
            async move {
                crate::audio::events::play_next(playback_ctx, None, true).await?;
                Ok::<(), Error>(())
            },
        )
        .await?;
    } else {
        drop(player);
        let embed = crate::discord::embeds::playback_status_embed(
            "❌ Error",
            "Nothing is playing, and there is no previous track.",
            0xED4245,
        );
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
    }
    Ok(())
}

/// Play the previously played track.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("pv"),
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn previous(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let mut player = player_lock.write().await;

    let prev = player
        .previous_track
        .take()
        .ok_or_else(|| SerenyaError::NotFound("No previous track found.".into()))?;

    player.cancel_prefetch();
    if let Some(mut curr) = player.now_playing.take() {
        curr.resolved_url = None;
        player.queue.push_front(curr);
    }
    let mut prev_to_play = prev.clone();
    prev_to_play.resolved_url = None;
    player.queue.push_front(prev_to_play);

    player.skip_forced = true;
    let handle_opt = player.current_track_handle.clone();

    drop(player);

    let embed = crate::discord::embeds::playback_status_embed(
        "⏮️ Previous",
        &format!("Playing previous track: **{}**", prev.title),
        0x5865F2,
    );
    let playback_ctx = crate::audio::events::PlaybackContext {
        guild_id,
        database: std::sync::Arc::clone(&ctx.data().database),
        guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
        http_client: ctx.data().http_client.clone(),
        serenity_ctx: ctx.serenity_context().clone(),
        config: std::sync::Arc::clone(&ctx.data().config),
    };
    run_control_transition(
        async {
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            Ok::<(), Error>(())
        },
        async move {
            if let Some(handle) = handle_opt {
                let _ = handle.stop();
            } else {
                crate::audio::events::play_next(playback_ctx, None, true).await?;
            }
            Ok::<(), Error>(())
        },
    )
    .await?;
    Ok(())
}

fn jump_skipped_count(has_current: bool, queued_skipped: usize) -> usize {
    queued_skipped + usize::from(has_current)
}

/// Jump to a specific track in the queue, skipping all tracks before it.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn jump(
    ctx: Context<'_>,
    #[description = "1-based index of the track to jump to"] position: usize,
) -> Result<(), Error> {
    ctx.defer().await?;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let mut player = player_lock.write().await;
    let queue_len = player.queue.len();
    let has_current = player.now_playing.is_some();

    if position == 0 {
        return Err(SerenyaError::Queue("Position must be 1 or greater.".into()).into());
    }

    let index = if has_current {
        if position == 1 {
            return Err(SerenyaError::Queue(
                "Cannot jump to the currently playing track. Use `/replay` to restart it.".into(),
            )
            .into());
        }
        if position > queue_len + 1 {
            return Err(SerenyaError::Queue(format!(
                "Index {position} out of bounds (queue size is {}).",
                queue_len + 1
            ))
            .into());
        }
        position - 2
    } else {
        if position > queue_len {
            return Err(SerenyaError::Queue(format!(
                "Index {position} out of bounds (queue size is {}).",
                queue_len
            ))
            .into());
        }
        position - 1
    };

    player.cancel_prefetch();
    let skipped = player.queue.jump(index)?;
    player.skip_forced = true;
    let handle_opt = player.current_track_handle.clone();

    drop(player);

    let skipped_count = jump_skipped_count(has_current, skipped.len());
    let embed = crate::discord::embeds::playback_status_embed(
        "⏭️ Jump",
        &format!(
            "Jumped to track #{position}. Skipped {} tracks.",
            skipped_count
        ),
        0x5865F2,
    );
    let playback_ctx = crate::audio::events::PlaybackContext {
        guild_id,
        database: std::sync::Arc::clone(&ctx.data().database),
        guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
        http_client: ctx.data().http_client.clone(),
        serenity_ctx: ctx.serenity_context().clone(),
        config: std::sync::Arc::clone(&ctx.data().config),
    };
    run_control_transition(
        async {
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            Ok::<(), Error>(())
        },
        async move {
            if let Some(handle) = handle_opt {
                let _ = handle.stop();
            } else {
                crate::audio::events::play_next(playback_ctx, None, true).await?;
            }
            Ok::<(), Error>(())
        },
    )
    .await?;
    Ok(())
}

/// Move a track within the queue.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn r#move(
    ctx: Context<'_>,
    #[description = "1-based index of the track to move"] from: usize,
    #[description = "1-based index of the destination position"] to: usize,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let mut player = player_lock.write().await;
    let queue_len = player.queue.len();

    if from == 0 || to == 0 {
        return Err(SerenyaError::Queue("Index must be 1 or greater.".into()).into());
    }

    let (from_idx, to_idx) = if player.now_playing.is_some() {
        if from == 1 || to == 1 {
            return Err(
                SerenyaError::Queue("Cannot move the currently playing track.".into()).into(),
            );
        }
        if from > queue_len + 1 || to > queue_len + 1 {
            return Err(SerenyaError::Queue("Index out of bounds.".into()).into());
        }
        (from - 2, to - 2)
    } else {
        if from > queue_len || to > queue_len {
            return Err(SerenyaError::Queue("Index out of bounds.".into()).into());
        }
        (from - 1, to - 1)
    };

    player.cancel_prefetch();
    player.queue.move_item(from_idx, to_idx)?;
    drop(player);

    let gp_clone = ctx.data().guild_players.clone();
    let http_client_clone = ctx.data().http_client.clone();
    tokio::spawn(async move {
        crate::audio::events::trigger_prefetch(guild_id, gp_clone, http_client_clone).await;
    });
    let embed = crate::discord::embeds::playback_status_embed(
        "↕️ Move",
        &format!("Moved track from #{from} to #{to}."),
        0x5865F2,
    );
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

#[cfg(test)]
mod restart_play_mode_tests {
    use super::restart_play_mode;
    use crate::core::PlaybackStatus;
    use songbird::tracks::PlayMode;

    #[test]
    fn paused_seek_restart_stays_paused() {
        assert_eq!(restart_play_mode(PlaybackStatus::Paused), PlayMode::Pause);
    }

    #[test]
    fn playing_seek_restart_keeps_playing() {
        assert_eq!(restart_play_mode(PlaybackStatus::Playing), PlayMode::Play);
    }
}

#[cfg(test)]
mod jump_count_tests {
    use super::jump_skipped_count;

    #[test]
    fn jump_count_includes_current_track_when_playback_is_active() {
        assert_eq!(jump_skipped_count(true, 0), 1);
        assert_eq!(jump_skipped_count(true, 2), 3);
    }

    #[test]
    fn jump_count_without_current_track_is_only_queue_prefix() {
        assert_eq!(jump_skipped_count(false, 0), 0);
        assert_eq!(jump_skipped_count(false, 2), 2);
    }
}

#[cfg(test)]
mod seek_duration_overflow_tests {
    use super::add_seek_duration;
    use std::time::Duration;

    #[test]
    fn maximum_user_duration_does_not_panic_when_forwarding() {
        let user_duration = crate::utils::time::parse_duration("18446744073709551615")
            .expect("u64::MAX seconds is accepted by the duration parser");
        assert_eq!(user_duration, Duration::from_secs(u64::MAX));

        let result =
            std::panic::catch_unwind(|| add_seek_duration(Duration::from_secs(1), user_duration));
        assert!(
            result.is_ok(),
            "a syntactically valid /forward duration must not panic the command task"
        );
        assert!(
            result
                .expect("seek arithmetic should return normally")
                .is_err(),
            "overflowing forward targets must be rejected"
        );
    }

    #[test]
    fn maximum_seek_offset_does_not_panic_when_rewinding() {
        let result = std::panic::catch_unwind(|| {
            add_seek_duration(Duration::from_secs(u64::MAX), Duration::from_secs(1))
        });
        assert!(
            result.is_ok(),
            "rewind elapsed-position arithmetic must not panic after a very large seek offset"
        );
        assert!(
            result
                .expect("seek arithmetic should return normally")
                .is_err(),
            "overflowing accumulated seek positions must be rejected"
        );
    }

    #[test]
    fn ordinary_seek_duration_addition_is_preserved() {
        assert_eq!(
            add_seek_duration(Duration::from_secs(40), Duration::from_secs(2))
                .expect("ordinary duration addition should succeed"),
            Duration::from_secs(42)
        );
    }
}

#[cfg(test)]
mod control_transition_order_tests {
    use super::run_control_transition;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[tokio::test]
    async fn required_transition_runs_even_when_success_reply_fails() {
        let transitioned = Arc::new(AtomicBool::new(false));
        let transition_flag = Arc::clone(&transitioned);

        let result = run_control_transition(
            async { Err::<(), &'static str>("discord reply failed") },
            async move {
                transition_flag.store(true, Ordering::SeqCst);
                Ok::<(), &'static str>(())
            },
        )
        .await;

        assert_eq!(result, Err("discord reply failed"));
        assert!(
            transitioned.load(Ordering::SeqCst),
            "a fallible Discord success reply must not prevent the already-committed playback transition"
        );
    }

    #[tokio::test]
    async fn successful_transition_and_reply_each_run_once() {
        let transitioned = Arc::new(AtomicBool::new(false));
        let replied = Arc::new(AtomicBool::new(false));
        let transition_flag = Arc::clone(&transitioned);
        let reply_flag = Arc::clone(&replied);

        let result = run_control_transition(
            async move {
                reply_flag.store(true, Ordering::SeqCst);
                Ok::<(), &'static str>(())
            },
            async move {
                transition_flag.store(true, Ordering::SeqCst);
                Ok::<(), &'static str>(())
            },
        )
        .await;

        assert_eq!(result, Ok(()));
        assert!(transitioned.load(Ordering::SeqCst));
        assert!(replied.load(Ordering::SeqCst));
    }
}
