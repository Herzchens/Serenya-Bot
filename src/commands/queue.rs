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
        .map(|r| r.value().clone())
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

enum RemoveOutcome {
    Removed(Box<str>),
    CannotRemoveCurrent,
    OutOfBounds { requested: usize, queue_size: usize },
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
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let outcome = {
        let mut player = player_lock.write().await;
        let queue_len = player.queue.len();

        if player.now_playing.is_some() {
            if position == 1 {
                RemoveOutcome::CannotRemoveCurrent
            } else {
                let idx = position - 2;
                if idx >= queue_len {
                    RemoveOutcome::OutOfBounds {
                        requested: position,
                        queue_size: queue_len + 1,
                    }
                } else {
                    player.cancel_prefetch();
                    match player.queue.remove(idx) {
                        Ok(track) => RemoveOutcome::Removed(track.title.clone()),
                        Err(e) => {
                            return Err(SerenyaError::Queue(format!(
                                "Failed to remove track: {}",
                                e
                            ))
                            .into());
                        }
                    }
                }
            }
        } else {
            let idx = position - 1;
            if idx >= queue_len {
                RemoveOutcome::OutOfBounds {
                    requested: position,
                    queue_size: queue_len,
                }
            } else {
                player.cancel_prefetch();
                match player.queue.remove(idx) {
                    Ok(track) => RemoveOutcome::Removed(track.title.clone()),
                    Err(e) => {
                        return Err(
                            SerenyaError::Queue(format!("Failed to remove track: {}", e)).into(),
                        );
                    }
                }
            }
        }
    };

    match outcome {
        RemoveOutcome::CannotRemoveCurrent => {
            ctx.say("❌ Cannot remove the currently playing track. Use `/skip` to skip it.")
                .await?;
        }
        RemoveOutcome::OutOfBounds {
            requested,
            queue_size,
        } => {
            ctx.say(format!(
                "❌ Position {} is out of bounds (queue size is {}).",
                requested, queue_size
            ))
            .await?;
        }
        RemoveOutcome::Removed(title) => {
            let gp_clone = ctx.data().guild_players.clone();
            let http_client_clone = ctx.data().http_client.clone();
            tokio::spawn(async move {
                crate::audio::events::trigger_prefetch(guild_id, gp_clone, http_client_clone).await;
            });
            ctx.say(format!("❌ Removed **{}** from the queue.", title))
                .await?;
        }
    }
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
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let handle_opt = {
        let mut player = player_lock.write().await;
        player.cancel_prefetch();
        player.queue.clear();
        let handle = player.current_track_handle.take();
        player.now_playing = None;
        player.playback_status = crate::core::PlaybackStatus::Idle;
        player.clear_skip_votes();
        handle
    };

    if let Some(ref handle) = handle_opt {
        let _ = handle.stop();
    }

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
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    {
        let mut player = player_lock.write().await;
        player.cancel_prefetch();
        player.queue.shuffle();
    }

    let gp_clone = ctx.data().guild_players.clone();
    let http_client_clone = ctx.data().http_client.clone();
    tokio::spawn(async move {
        crate::audio::events::trigger_prefetch(guild_id, gp_clone, http_client_clone).await;
    });

    ctx.say("🔀 Shuffled the queue.").await?;
    Ok(())
}
