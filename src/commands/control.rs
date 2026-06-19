use crate::utils::{Context, Error, SerenyaError};
use std::time::Duration;

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
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let duration = crate::utils::time::parse_duration(&time)
        .map_err(|e| SerenyaError::Config(format!("Invalid time format: {e}")))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let player = player_lock.read().await;

    let handle = player
        .current_track_handle
        .as_ref()
        .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?;

    let _ = handle.seek(duration);
    ctx.say(format!("⏩ Seeked to **{}**.", time)).await?;
    Ok(())
}

/// Fast forward the song by a duration.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn forward(
    ctx: Context<'_>,
    #[description = "Time to forward (default 10s)"] time: Option<String>,
) -> Result<(), Error> {
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let player = player_lock.read().await;

    let handle = player
        .current_track_handle
        .as_ref()
        .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?;

    let info = handle.get_info().await?;
    let new_pos = info.position + duration;
    let _ = handle.seek(new_pos);
    ctx.say(format!("⏩ Forwarded by **{}s**.", duration.as_secs()))
        .await?;
    Ok(())
}

/// Rewind the song by a duration.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn rewind(
    ctx: Context<'_>,
    #[description = "Time to rewind (default 10s)"] time: Option<String>,
) -> Result<(), Error> {
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let player = player_lock.read().await;

    let handle = player
        .current_track_handle
        .as_ref()
        .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?;

    let info = handle.get_info().await?;
    let new_pos = info
        .position
        .checked_sub(duration)
        .unwrap_or(Duration::from_secs(0));
    let _ = handle.seek(new_pos);
    ctx.say(format!("⏪ Rewound by **{}s**.", duration.as_secs()))
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
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn previous(ctx: Context<'_>) -> Result<(), Error> {
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
