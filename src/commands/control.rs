use crate::utils::{Context, Error, SerenyaError};
use std::time::Duration;

fn format_seek_time(d: Duration) -> String {
    let total_secs = d.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}", mins, secs)
}

async fn seek_by_restart(
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
        None => {
            crate::audio::source::extract_stream_url_for_guild(guild_id.get(), &url).await?
        }
    };

    // 2. Spawn FFMPEG process starting at target_position
    let seek_time_secs = target_position.as_secs();
    
    use std::process::{Command, Stdio};
    let child = Command::new("ffmpeg")
        .args(&[
            "-ss", &seek_time_secs.to_string(),
            "-i", &stream_url,
            "-f", "wav",
            "-acodec", "pcm_s16le",
            "-ar", "48000",
            "-ac", "2",
            "pipe:1"
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| SerenyaError::Audio(format!("Failed to spawn ffmpeg: {e}")))?;

    let child_container: songbird::input::ChildContainer = child.into();
    let source: songbird::input::Input = child_container.into();

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
    let database = ctx.data().database.clone();
    let guild_players = ctx.data().guild_players.clone();
    let http_client = ctx.data().http_client.clone();
    let serenity_ctx = ctx.serenity_context().clone();

    let _ = handle.add_event(
        songbird::Event::Track(songbird::TrackEvent::End),
        crate::audio::events::TrackEndHandler {
            guild_id,
            database: database.clone(),
            guild_players: guild_players.clone(),
            http_client: http_client.clone(),
            serenity_ctx: serenity_ctx.clone(),
        },
    );
    let _ = handle.add_event(
        songbird::Event::Track(songbird::TrackEvent::Error),
        crate::audio::events::TrackErrorHandler {
            guild_id,
            database,
            guild_players,
            http_client,
            serenity_ctx,
        },
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
    ctx.say(format!("⏩ Forwarded by **{}s** → `{}`", duration.as_secs(), new_pos_fmt))
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
    let new_pos = total_elapsed.checked_sub(duration).unwrap_or(Duration::from_secs(0));

    seek_by_restart(ctx, guild_id, player_lock, new_pos).await?;
    let new_pos_fmt = format_seek_time(new_pos);
    ctx.say(format!("⏪ Rewound by **{}s** → `{}`", duration.as_secs(), new_pos_fmt))
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
    } else if let Some(prev) = player.previous_track.clone() {
        ctx.say(format!("🔄 Replaying previous track: **{}**", prev.title))
            .await?;
        player.queue.push_front(prev);
        drop(player);
        crate::audio::events::play_next(
            guild_id,
            &ctx.data().database,
            &ctx.data().guild_players,
            &ctx.data().http_client,
            ctx.serenity_context(),
            None,
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
        .clone()
        .ok_or_else(|| SerenyaError::NotFound("No previous track found.".into()))?;

    player.queue.push_front(prev.clone());
    ctx.say(format!("⏮️ Playing previous track: **{}**", prev.title))
        .await?;

    player.skip_forced = true;
    if let Some(handle) = &player.current_track_handle {
        let _ = handle.stop();
    } else {
        drop(player);
        crate::audio::events::play_next(
            guild_id,
            &ctx.data().database,
            &ctx.data().guild_players,
            &ctx.data().http_client,
            ctx.serenity_context(),
            None,
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

    if position == 0 || position > player.queue.len() {
        return Err(SerenyaError::Queue(format!("Index {position} out of bounds.")).into());
    }

    let skipped = player.queue.jump(position - 1)?;
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
            &ctx.data().database,
            &ctx.data().guild_players,
            &ctx.data().http_client,
            ctx.serenity_context(),
            None,
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

    if from == 0 || to == 0 || from > player.queue.len() || to > player.queue.len() {
        return Err(SerenyaError::Queue("Index out of bounds.".into()).into());
    }

    player.queue.move_item(from - 1, to - 1)?;
    ctx.say(format!("↕️ Moved track from #{from} to #{to}."))
        .await?;
    Ok(())
}
