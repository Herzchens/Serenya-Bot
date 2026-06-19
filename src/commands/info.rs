use std::time::Duration;

use crate::discord::embeds::now_playing_embed;
use crate::utils::{Context, Error, SerenyaError};

/// Show details of the currently playing track.
#[poise::command(
    slash_command,
    prefix_command,
    rename = "nowplaying",
    aliases("np"),
    check = "crate::discord::checks::require_guild"
)]
pub async fn nowplaying(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let player = player_lock.read().await;

    let track = match player.now_playing.as_ref() {
        Some(t) => t,
        None => {
            ctx.say("Nothing is currently playing.").await?;
            return Ok(());
        }
    };

    let elapsed = if let Some(ref handle) = player.current_track_handle {
        match handle.get_info().await {
            Ok(info) => info.position,
            Err(_) => Duration::from_secs(0),
        }
    } else {
        Duration::from_secs(0)
    };

    let embed = now_playing_embed(track, elapsed, None);
    let reply = poise::CreateReply::default().embed(embed);
    ctx.send(reply).await?;
    Ok(())
}
