use crate::utils::{Context, Error, SerenyaError};

/// Checks if the command is run inside a guild (server).
pub async fn require_guild(ctx: Context<'_>) -> Result<bool, Error> {
    if ctx.guild_id().is_some() {
        Ok(true)
    } else {
        Err(SerenyaError::Config("This command can only be used in a server.".into()).into())
    }
}

/// Checks if the user is in the same voice channel as the bot.
pub async fn require_same_voice_channel(ctx: Context<'_>) -> Result<bool, Error> {
    let owner_id = ctx.data().config().bot.owner;
    if ctx.author().id.get() == owner_id {
        return Ok(true);
    }

    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?;

    let bot_chan = if let Some(call_lock) = manager.get(guild_id) {
        let call = call_lock.lock().await;
        call.current_channel()
    } else {
        None
    };

    let bot_chan_id =
        bot_chan.ok_or_else(|| SerenyaError::Voice("Bot is not in a voice channel.".into()))?;

    let guild = ctx
        .guild()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let user_chan_id = guild
        .voice_states
        .get(&ctx.author().id)
        .and_then(|state| state.channel_id)
        .ok_or_else(|| {
            SerenyaError::Voice("You must be in a voice channel to use this command.".into())
        })?;

    if bot_chan_id.0.get() == user_chan_id.get() {
        Ok(true)
    } else {
        Err(SerenyaError::Voice("You must be in the same voice channel as the bot.".into()).into())
    }
}
