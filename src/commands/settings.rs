use poise::serenity_prelude as serenity;
use crate::utils::{Context, Error, SerenyaError};
use crate::audio::quality::Quality;

pub async fn autocomplete_quality(_ctx: Context<'_>, partial: &str) -> Vec<String> {
    let choices = vec![
        "Performance".to_string(),
        "Turbo".to_string(),
        "Balanced".to_string(),
        "Auto".to_string(),
        "Quality".to_string(),
        "Premium".to_string(),
        "Max".to_string(),
        "Lossless".to_string(),
    ];

    choices
        .into_iter()
        .filter(|choice| choice.to_lowercase().contains(&partial.to_lowercase()))
        .collect()
}

/// Toggle track announcements in this server.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_guild"
)]
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

    let embed = serenity::CreateEmbed::new()
        .title("📢 Settings Updated")
        .description(format!(
            "Track announcements have been **{}** for this server.",
            if enable { "enabled" } else { "disabled" }
        ))
        .color(0x5865F2);

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Set the audio quality for this server.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_guild"
)]
pub async fn quality(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_quality"]
    #[description = "Performance (8K) to Lossless (384K). Auto is dynamic to voice room."]
    mode: String,
) -> Result<(), Error> {
    use std::str::FromStr;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let quality_mode = Quality::from_str(&mode)?;

    // 1. Get guild to check premium boost tier
    let premium_tier = {
        let guild = ctx.guild().ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?;
        guild.premium_tier
    };

    // 2. Validate boost tier requirements
    match quality_mode {
        Quality::Premium => {
            if premium_tier < serenity::PremiumTier::Tier2 {
                let embed = serenity::CreateEmbed::new()
                    .title("❌ Boost Level Required")
                    .description("Cấp độ **Premium (256Kbps)** yêu cầu Server đạt tối thiểu **Boost Level 2**.")
                    .color(0xFF0000);
                ctx.send(poise::CreateReply::default().embed(embed)).await?;
                return Ok(());
            }
        }
        Quality::Max | Quality::Lossless => {
            if premium_tier < serenity::PremiumTier::Tier3 {
                let embed = serenity::CreateEmbed::new()
                    .title("❌ Boost Level Required")
                    .description("Cấp độ này yêu cầu Server đạt tối thiểu **Boost Level 3** để mở khóa bitrate lớn hơn 256Kbps.")
                    .color(0xFF0000);
                ctx.send(poise::CreateReply::default().embed(embed)).await?;
                return Ok(());
            }
        }
        _ => {}
    }

    // 3. Save quality to guild settings
    let db = &ctx.data().database;
    let mut settings = db.get_guild_settings(guild_id.get()).await;
    settings.quality = quality_mode.to_str().to_owned();
    db.update_guild_settings(guild_id.get(), settings).await;

    // 4. Calculate target bitrate for the voice channel based on premium tier limits
    let raw_bitrate = quality_mode.to_bitrate();
    let max_tier_bitrate = match premium_tier {
        serenity::PremiumTier::Tier3 => 384_000,
        serenity::PremiumTier::Tier2 => 256_000,
        serenity::PremiumTier::Tier1 => 128_000,
        _ => 96_000,
    };
    let target_bitrate = if raw_bitrate == 0 {
        0 // dynamic
    } else {
        raw_bitrate.min(max_tier_bitrate)
    };

    // 5. Update active voice channel bitrate and Songbird encoder
    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id);

    if let Some(player_lock) = player_lock {
        let player = player_lock.read().await;
        if let Some(vc_id) = player.voice_channel {
            // Edit Discord channel bitrate if not Auto
            if quality_mode != Quality::Auto {
                let _ = vc_id.edit(&ctx.serenity_context().http, serenity::EditChannel::new().bitrate(target_bitrate)).await;
            }

            // Update Songbird encoder bitrate if connected
            let manager = songbird::get(ctx.serenity_context()).await.ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized".into()))?.clone();
            if let Some(call_lock) = manager.get(guild_id) {
                let mut call = call_lock.lock().await;
                if quality_mode == Quality::Auto {
                    if let Ok(serenity::Channel::Guild(channel)) = vc_id.to_channel(&ctx.serenity_context().http).await {
                        let ch_bitrate = channel.bitrate.unwrap_or(64_000);
                        call.set_bitrate(songbird::driver::Bitrate::Bits(ch_bitrate as i32));
                    }
                } else {
                    call.set_bitrate(songbird::driver::Bitrate::Bits(target_bitrate as i32));
                }
            }
        }
    }

    let embed = serenity::CreateEmbed::new()
        .title("🎧 Audio Quality Updated")
        .description(format!(
            "Audio quality for this server has been set to **{}**.",
            quality_mode.display_name()
        ))
        .color(0x5865F2);

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// View or set custom prefix for this server.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_guild"
)]
pub async fn prefix(
    ctx: Context<'_>,
    #[description = "New prefix (optional)"] set: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let db = &ctx.data().database;

    if let Some(new_prefix) = set {
        let is_admin = if let Ok(member) = guild_id.member(ctx, ctx.author().id).await {
            if let Some(guild) = ctx.guild() {
                guild.member_permissions(&member).administrator()
            } else {
                false
            }
        } else {
            false
        };

        if !is_admin && ctx.author().id.get() != ctx.data().config().bot.owner {
            return Err(SerenyaError::Permission("Only server administrators can change prefix.".into()).into());
        }

        let mut settings = db.get_guild_settings(guild_id.get()).await;
        settings.prefix = Some(new_prefix.clone());
        db.update_guild_settings(guild_id.get(), settings).await;

        ctx.say(format!("✅ Prefix has been changed to `{new_prefix}` for this server.")).await?;
    } else {
        let settings = db.get_guild_settings(guild_id.get()).await;
        let current_prefix = settings.prefix.unwrap_or_else(|| ctx.data().config().bot.prefix.clone());
        ctx.say(format!("`[{current_prefix}]`")).await?;
    }
    Ok(())
}

