use crate::utils::{Context, Error, SerenyaError};
use std::time::Duration;

fn format_seek_time(d: Duration) -> String {
    let total_secs = d.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}", mins, secs)
}

pub(crate) async fn seek_by_restart(
    ctx: Context<'_>,
    guild_id: poise::serenity_prelude::GuildId,
    player_lock: std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
    target_position: Duration,
) -> Result<(), Error> {
    // 1. Get the stream URL of the current track
    let (url, resolved_url) = {
        let player = player_lock.read().await;
        let track = player
            .now_playing
            .as_ref()
            .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?;
        (track.url.clone(), track.resolved_url.clone())
    };

    let stream_url = match resolved_url {
        Some(url) => url,
        None => crate::audio::source::extract_stream_url_for_guild(guild_id.get(), &url).await?,
    };

    let eight_d_enabled = {
        let player = player_lock.read().await;
        player.eight_d_enabled
    };
    let source = crate::audio::source::create_ffmpeg_stream_input(
        &stream_url,
        Some(target_position),
        eight_d_enabled,
    )?;

    // 3. Stop the current track and play the new input
    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    let call_lock = manager
        .get(guild_id)
        .ok_or_else(|| SerenyaError::Voice("Not connected to a voice channel.".into()))?;

    let handle = {
        let mut player = player_lock.write().await;
        let has_old_handle = player.current_track_handle.is_some();
        player.is_seeking = has_old_handle;
        player.seek_offset = target_position;

        if let Some(old_handle) = player.current_track_handle.take() {
            let _ = old_handle.stop();
        }

        let mut call = call_lock.lock().await;
        call.play_input(source)
    };

    // 4. Register event handlers
    let end_handler = crate::audio::events::TrackEndHandler {
        guild_id,
        database: std::sync::Arc::clone(&ctx.data().database),
        guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
        http_client: ctx.data().http_client.clone(),
        serenity_ctx: ctx.serenity_context().clone(),
        config: ctx.data().config(),
    };
    let _ = handle.add_event(
        songbird::Event::Track(songbird::TrackEvent::End),
        end_handler,
    );

    let error_handler = crate::audio::events::TrackErrorHandler {
        guild_id,
        database: std::sync::Arc::clone(&ctx.data().database),
        guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
        http_client: ctx.data().http_client.clone(),
        serenity_ctx: ctx.serenity_context().clone(),
        config: ctx.data().config(),
    };
    let _ = handle.add_event(
        songbird::Event::Track(songbird::TrackEvent::Error),
        error_handler,
    );

    // 5. Update player's track handle
    {
        let mut player = player_lock.write().await;
        if player
            .now_playing
            .as_ref()
            .map(|current| current.url.as_str())
            == Some(url.as_str())
        {
            player.current_track_handle = Some(handle);
        } else {
            let _ = handle.stop();
        }
        player.is_seeking = false;
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?
        .clone();

    seek_by_restart(ctx, guild_id, player_lock, duration).await?;
    ctx.say(format!("⏩ Seeked to **{time}**.")).await?;
    Ok(())
}

/// Fast forward the song by a duration.
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?
        .clone();

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
    let new_pos = seek_offset + info.position + duration;

    seek_by_restart(ctx, guild_id, player_lock, new_pos).await?;
    let new_pos_fmt = format_seek_time(new_pos);
    ctx.say(format!(
        "⏩ Forwarded by **{}s** → `{}`",
        duration.as_secs(),
        new_pos_fmt
    ))
    .await?;
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?
        .clone();

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
    let total_elapsed = seek_offset + info.position;
    let new_pos = total_elapsed
        .checked_sub(duration)
        .unwrap_or(Duration::from_secs(0));

    seek_by_restart(ctx, guild_id, player_lock, new_pos).await?;
    let new_pos_fmt = format_seek_time(new_pos);
    ctx.say(format!(
        "⏪ Rewound by **{}s** → `{}`",
        duration.as_secs(),
        new_pos_fmt
    ))
    .await?;
    Ok(())
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let mut player = player_lock.write().await;

    if let Some(handle) = &player.current_track_handle {
        let _ = handle.seek(Duration::from_secs(0));
        ctx.say("🔄 Replaying current track from the beginning.")
            .await?;
    } else if let Some(prev) = player.previous_track.take() {
        ctx.say(format!("🔄 Replaying previous track: **{}**", prev.title))
            .await?;
        player.queue.push_front(prev);
        drop(player);
        crate::audio::events::play_next(
            guild_id,
            std::sync::Arc::clone(&ctx.data().database),
            std::sync::Arc::clone(&ctx.data().guild_players),
            ctx.data().http_client.clone(),
            ctx.serenity_context().clone(),
            ctx.data().config(),
            None,
            true,
        )
        .await?;
    } else {
        ctx.say("❌ Nothing is playing, and there is no previous track.")
            .await?;
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let mut player = player_lock.write().await;

    let prev = player
        .previous_track
        .take()
        .ok_or_else(|| SerenyaError::NotFound("No previous track found.".into()))?;

    ctx.say(format!("⏮️ Playing previous track: **{}**", prev.title))
        .await?;

    if let Some(mut curr) = player.now_playing.take() {
        curr.resolved_url = None;
        player.queue.push_front(curr);
    }
    let mut prev = prev;
    prev.resolved_url = None;
    player.queue.push_front(prev);

    player.skip_forced = true;
    if let Some(handle) = &player.current_track_handle {
        let _ = handle.stop();
    } else {
        drop(player);
        crate::audio::events::play_next(
            guild_id,
            std::sync::Arc::clone(&ctx.data().database),
            std::sync::Arc::clone(&ctx.data().guild_players),
            ctx.data().http_client.clone(),
            ctx.serenity_context().clone(),
            ctx.data().config(),
            None,
            true,
        )
        .await?;
    }
    Ok(())
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let mut player = player_lock.write().await;
    let queue_len = player.queue.len();

    if position == 0 {
        return Err(SerenyaError::Queue("Position must be 1 or greater.".into()).into());
    }

    let index = if player.now_playing.is_some() {
        if position == 1 {
            return Err(SerenyaError::Queue("Cannot jump to the currently playing track. Use `/replay` to restart it.".into()).into());
        }
        if position > queue_len + 1 {
            return Err(SerenyaError::Queue(format!("Index {position} out of bounds (queue size is {}).", queue_len + 1)).into());
        }
        position - 2
    } else {
        if position > queue_len {
            return Err(SerenyaError::Queue(format!("Index {position} out of bounds (queue size is {}).", queue_len)).into());
        }
        position - 1
    };

    let skipped = player.queue.jump(index)?;
    ctx.say(format!(
        "⏭️ Jumped to track #{position}. Skipped {} tracks.",
        skipped.len()
    ))
    .await?;

    player.skip_forced = true;
    if let Some(handle) = &player.current_track_handle {
        let _ = handle.stop();
    } else {
        drop(player);
        crate::audio::events::play_next(
            guild_id,
            std::sync::Arc::clone(&ctx.data().database),
            std::sync::Arc::clone(&ctx.data().guild_players),
            ctx.data().http_client.clone(),
            ctx.serenity_context().clone(),
            ctx.data().config(),
            None,
            true,
        )
        .await?;
    }
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let mut player = player_lock.write().await;
    let queue_len = player.queue.len();

    if from == 0 || to == 0 {
        return Err(SerenyaError::Queue("Index must be 1 or greater.".into()).into());
    }

    let (from_idx, to_idx) = if player.now_playing.is_some() {
        if from == 1 || to == 1 {
            return Err(SerenyaError::Queue("Cannot move the currently playing track.".into()).into());
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

    player.queue.move_item(from_idx, to_idx)?;
    ctx.say(format!("↕️ Moved track from #{from} to #{to}."))
        .await?;
    Ok(())
}
