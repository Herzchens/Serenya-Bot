use crate::utils::{Context, Error, SerenyaError};

#[derive(poise::ChoiceParameter, Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityChoice {
    #[name = "performance"]
    Performance,
    #[name = "balanced"]
    Balanced,
    #[name = "quality"]
    Quality,
}

/// Toggle track announcements in this server.
#[poise::command(slash_command, prefix_command)]
pub async fn announce_track(
    ctx: Context<'_>,
    #[description = "Enable or disable track announcements"] enable: bool,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let db = &ctx.data().database;
    let mut settings = db.get_guild_settings(guild_id.get()).await;
    settings.announce_track = enable;
    db.update_guild_settings(guild_id.get(), settings).await;

    ctx.say(format!(
        "📢 Track announcements have been **{}** for this server.",
        if enable { "enabled" } else { "disabled" }
    ))
    .await?;
    Ok(())
}

/// Set your preferred audio quality.
#[poise::command(slash_command, prefix_command)]
pub async fn quality(
    ctx: Context<'_>,
    #[description = "Audio quality mode"] mode: QualityChoice,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;

    let mode_str = match mode {
        QualityChoice::Performance => "performance",
        QualityChoice::Balanced => "balanced",
        QualityChoice::Quality => "quality",
    };

    let mut settings = db.get_user_settings(user_id).await;
    settings.quality = mode_str.to_owned();
    db.update_user_settings(user_id, settings).await;

    ctx.say(format!(
        "🎧 Your preferred audio quality has been set to **{}**.",
        mode_str
    ))
    .await?;
    Ok(())
}

/// Manage bot settings.
#[poise::command(
    slash_command,
    prefix_command,
    subcommands("announce_track", "quality"),
    subcommand_required
)]
pub async fn settings(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}
