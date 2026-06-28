use crate::utils::{Context, Error, SerenyaError};

#[derive(Debug, poise::ChoiceParameter, Clone, Copy, PartialEq, Eq)]
pub enum EightDMode {
    #[name = "on"]
    On,
    #[name = "off"]
    Off,
}

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
    #[description = "on or off"] mode: Option<EightDMode>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .entry(guild_id)
        .or_insert_with(|| {
            std::sync::Arc::new(tokio::sync::RwLock::new(crate::core::GuildPlayer::new()))
        })
        .clone();

    let enabled = if let Some(m) = mode {
        match m {
            EightDMode::On => true,
            EightDMode::Off => false,
        }
    } else {
        let player = player_lock.read().await;
        !player.eight_d_enabled
    };

    let player_lock_clone = player_lock.clone();
    let (current_track_handle, seek_offset) = {
        let player = player_lock.read().await;
        (player.current_track_handle.clone(), player.seek_offset)
    };

    let current_pos_opt = if let Some(ref handle) = current_track_handle {
        if let Ok(info) = handle.get_info().await {
            Some(seek_offset + info.position)
        } else {
            None
        }
    } else {
        None
    };

    {
        let mut player = player_lock.write().await;
        player.eight_d_enabled = enabled;
    }

    let state = if enabled { "enabled" } else { "disabled" };
    if let Some(current_pos) = current_pos_opt {
        if let Err(e) =
            crate::commands::control::seek_by_restart(ctx, guild_id, player_lock_clone, current_pos)
                .await
        {
            tracing::error!("Failed to apply 8D effect immediately via restart: {:?}", e);
            let embed = crate::discord::embeds::playback_status_embed(
                "🎧 8D Audio",
                &format!(
                    "8D audio is now **{state}**, but failed to apply immediately. It will apply from the next track."
                ),
                0xFEE75C,
            );
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        } else {
            let embed = crate::discord::embeds::playback_status_embed(
                "🎧 8D Audio",
                &format!(
                    "8D audio is now **{state}** and has been applied to the current session."
                ),
                0x5865F2,
            );
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
    } else {
        let embed = crate::discord::embeds::playback_status_embed(
            "🎧 8D Audio",
            &format!("8D audio is now **{state}**."),
            0x5865F2,
        );
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
    }
    Ok(())
}
