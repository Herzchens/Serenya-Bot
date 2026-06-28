use crate::utils::{Context, Error, SerenyaError};

/// Join the user's voice channel.
#[poise::command(slash_command, prefix_command, aliases("j"))]
pub async fn join(ctx: Context<'_>) -> Result<(), Error> {
    tracing::info!("Entering join command");
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let channel_id = {
        let guild = ctx
            .guild()
            .ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?;
        guild
            .voice_states
            .get(&ctx.author().id)
            .and_then(|state| state.channel_id)
            .ok_or_else(|| {
                SerenyaError::Voice("You must be in a voice channel to use this command.".into())
            })?
    };

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    tracing::info!("Voice connect start: joining channel {:?}", channel_id);
    let _handler = manager.join(guild_id, channel_id).await;
    tracing::info!("Voice connect complete: channel {:?}", channel_id);
    let _ = crate::audio::quality::apply_bitrate(ctx, guild_id, channel_id).await;

    let player_lock = ctx
        .data()
        .guild_players
        .entry(guild_id)
        .or_insert_with(|| {
            std::sync::Arc::new(tokio::sync::RwLock::new(crate::core::GuildPlayer::new()))
        })
        .clone();

    let mut player = player_lock.write().await;
    player.voice_channel = Some(channel_id);
    player.announce_channel = Some(ctx.channel_id());

    tracing::info!("Join completed successfully for channel {:?}", channel_id);
    ctx.say(format!("🔊 Joined <#{channel_id}>")).await?;
    Ok(())
}

/// Leave the voice channel and clear queue state.
#[poise::command(slash_command, prefix_command, aliases("l"))]
pub async fn leave(ctx: Context<'_>) -> Result<(), Error> {
    tracing::info!("Entering leave command");
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    if let Some(player_lock) = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
    {
        let mut player = player_lock.write().await;
        player.reset();
        player.voice_channel = None;
        player.announce_channel = None;
        tracing::info!("Reset guild player state and dropped track handle");
    }

    tracing::info!("Removing guild player from map");
    ctx.data().guild_players.remove(&guild_id);
    tracing::info!("Guild player removed from map");

    tracing::info!("Voice disconnect start: leaving voice channel");
    let has_handler = manager.get(guild_id).is_some();
    if has_handler {
        manager.remove(guild_id).await?;
    }
    tracing::info!("Voice disconnect complete");

    crate::audio::runtime::cleanup_guild(guild_id.get());
    tracing::info!("Leave completed successfully");

    ctx.say("👋 Left voice channel and cleared state.").await?;
    Ok(())
}
