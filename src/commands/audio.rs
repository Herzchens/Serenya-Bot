use crate::utils::{Context, Error, SerenyaError};

/// Toggle the per-guild 8D audio effect.
#[poise::command(
    slash_command,
    prefix_command,
    rename = "8d",
    aliases("8D"),
    check = "crate::discord::checks::require_guild"
)]
pub async fn eight_d(
    ctx: Context<'_>,
    #[description = "on or off"] mode: String,
) -> Result<(), Error> {
    ctx.defer().await?;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let enabled = match mode.trim().to_ascii_lowercase().as_str() {
        "on" => true,
        "off" => false,
        _ => {
            ctx.say("Use `on` or `off`.").await?;
            return Ok(());
        }
    };

    let player_lock = ctx
        .data()
        .guild_players
        .entry(guild_id)
        .or_insert_with(|| {
            std::sync::Arc::new(tokio::sync::RwLock::new(crate::core::GuildPlayer::new()))
        })
        .clone();

    let player_lock_clone = player_lock.clone();
    let current_pos_opt = {
        let mut player = player_lock.write().await;
        player.eight_d_enabled = enabled;
        if let Some(ref handle) = player.current_track_handle {
            if let Ok(info) = handle.get_info().await {
                Some(player.seek_offset + info.position)
            } else {
                None
            }
        } else {
            None
        }
    };

    let state = if enabled { "enabled" } else { "disabled" };
    if let Some(current_pos) = current_pos_opt {
        if let Err(e) = crate::commands::control::seek_by_restart(ctx, guild_id, player_lock_clone, current_pos).await {
            tracing::error!("Failed to apply 8D effect immediately via restart: {:?}", e);
            ctx.say(format!(
                "8D audio is now **{state}**, but failed to apply immediately. It will apply from the next track.",
            ))
            .await?;
        } else {
            ctx.say(format!(
                "8D audio is now **{state}** and has been applied to the current session."
            ))
            .await?;
        }
    } else {
        ctx.say(format!("8D audio is now **{state}**."))
            .await?;
    }
    Ok(())
}
