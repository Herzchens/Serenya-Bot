use crate::utils::SerenyaError;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    Performance,
    Turbo,
    Balanced,
    #[default]
    Auto,
    High, // quality
    Premium,
    Max,
    Lossless,
}

impl Quality {
    pub fn display_name(self) -> &'static str {
        match self {
            Quality::Performance => "Performance (8Kbps)",
            Quality::Turbo => "Turbo (32Kbps)",
            Quality::Balanced => "Balanced (64Kbps)",
            Quality::Auto => "Auto (Dynamic Bitrate)",
            Quality::High => "Quality (128Kbps)",
            Quality::Premium => "Premium (256Kbps)",
            Quality::Max => "Max (320Kbps)",
            Quality::Lossless => "Lossless (384Kbps)",
        }
    }

    pub fn to_bitrate(self) -> u32 {
        match self {
            Quality::Performance => 8000,
            Quality::Turbo => 32000,
            Quality::Balanced => 64000,
            Quality::Auto => 0, // 0 signifies dynamic
            Quality::High => 128000,
            Quality::Premium => 256000,
            Quality::Max => 320000,
            Quality::Lossless => 384000,
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            Quality::Performance => "performance",
            Quality::Turbo => "turbo",
            Quality::Balanced => "balanced",
            Quality::Auto => "auto",
            Quality::High => "quality",
            Quality::Premium => "premium",
            Quality::Max => "max",
            Quality::Lossless => "lossless",
        }
    }
}

impl FromStr for Quality {
    type Err = SerenyaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let first_word = s
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| !c.is_alphanumeric());
        match first_word.to_lowercase().as_str() {
            "performance" | "perf" => Ok(Quality::Performance),
            "turbo" => Ok(Quality::Turbo),
            "balanced" | "balance" => Ok(Quality::Balanced),
            "auto" => Ok(Quality::Auto),
            "quality" | "high" => Ok(Quality::High),
            "premium" => Ok(Quality::Premium),
            "max" => Ok(Quality::Max),
            "lossless" => Ok(Quality::Lossless),
            _ => Err(SerenyaError::Config(format!(
                "Invalid quality mode: '{}'. Use 'performance', 'turbo', 'balanced', 'auto', 'quality', 'premium', 'max', or 'lossless'.",
                s
            ))),
        }
    }
}

pub async fn apply_bitrate(
    ctx: crate::utils::Context<'_>,
    guild_id: poise::serenity_prelude::GuildId,
    vc_id: poise::serenity_prelude::ChannelId,
) -> Result<(), crate::utils::Error> {
    use poise::serenity_prelude as serenity;
    use std::str::FromStr;

    let premium_tier = {
        let guild = ctx
            .guild()
            .ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?;
        guild.premium_tier
    };

    let db = &ctx.data().database;
    let settings = db.get_guild_settings(guild_id.get()).await;
    let quality_mode = Quality::from_str(&settings.quality).unwrap_or(Quality::Auto);

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

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    if let Some(call_lock) = manager.get(guild_id) {
        let ch_bitrate = if quality_mode == Quality::Auto {
            if let Ok(serenity::Channel::Guild(channel)) =
                vc_id.to_channel(&ctx.serenity_context().http).await
            {
                channel.bitrate.unwrap_or(64_000)
            } else {
                64_000
            }
        } else {
            target_bitrate
        };

        let mut call = call_lock.lock().await;
        call.set_bitrate(songbird::driver::Bitrate::Bits(ch_bitrate as i32));
    }
    Ok(())
}
