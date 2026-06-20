use crate::discord::pagination::paginate_queue;
use crate::utils::{Context, Error, SerenyaError};

/// View the current queue.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("q"),
    check = "crate::discord::checks::require_guild"
)]
pub async fn queue(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let player = player_lock.read().await;
    let mut tracks = Vec::new();
    if let Some(ref np) = player.now_playing {
        tracks.push(np.clone());
    }
    tracks.extend(player.queue.iter().cloned());

    // Release read lock before awaiting paginate_queue
    drop(player);

    paginate_queue(ctx, &tracks, "🎶 Current Queue").await?;
    Ok(())
}

/// Remove a song from the queue.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("r"),
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn remove(
    ctx: Context<'_>,
    #[description = "Position in queue (1-indexed)"] position: usize,
) -> Result<(), Error> {
    if position == 0 {
        ctx.say("Position must be 1 or greater.").await?;
        return Ok(());
    }

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

    let index = if player.now_playing.is_some() {
        if position == 1 {
            ctx.say("❌ Cannot remove the currently playing track. Use `/skip` to skip it.").await?;
            return Ok(());
        }
        let idx = position - 2;
        if idx >= queue_len {
            ctx.say(format!("❌ Position {} is out of bounds (queue size is {}).", position, queue_len + 1)).await?;
            return Ok(());
        }
        idx
    } else {
        let idx = position - 1;
        if idx >= queue_len {
            ctx.say(format!("❌ Position {} is out of bounds (queue size is {}).", position, queue_len)).await?;
            return Ok(());
        }
        idx
    };

    let removed_track = player
        .queue
        .remove(index)
        .map_err(|e| SerenyaError::Queue(format!("Failed to remove track: {}", e)))?;

    ctx.say(format!(
        "❌ Removed **{}** from the queue.",
        removed_track.title
    ))
    .await?;
    Ok(())
}

/// Clear all songs from the queue.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn clear(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let mut player = player_lock.write().await;
    player.queue.clear();
    if let Some(ref handle) = player.current_track_handle {
        let _ = handle.stop();
    }
    player.current_track_handle = None;
    player.now_playing = None;
    player.playback_status = crate::core::PlaybackStatus::Idle;
    player.clear_skip_votes();

    ctx.say("🧹 Cleared the queue and stopped playback.")
        .await?;
    Ok(())
}

/// Shuffle the queue randomly.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("sh"),
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn shuffle(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let mut player = player_lock.write().await;
    player.queue.shuffle();

    ctx.say("🔀 Shuffled the queue.").await?;
    Ok(())
}
