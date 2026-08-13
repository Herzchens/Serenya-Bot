use crate::audio::quality::Quality;
use crate::utils::{Context, Error, SerenyaError};
use poise::serenity_prelude as serenity;

async fn update_guild_setting_with_gate<F, Fut>(
    db: &crate::database::DatabaseManager,
    guild_id: u64,
    before_write: Fut,
    mutate: F,
) where
    F: FnOnce(&mut crate::database::models::GuildSettings),
    Fut: std::future::Future<Output = ()>,
{
    before_write.await;
    db.update_guild_settings_mut(guild_id, mutate).await;
}

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
    update_guild_setting_with_gate(db, guild_id.get(), std::future::ready(()), |settings| {
        settings.announce_track = enable;
    })
    .await;

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

    let premium_tier = {
        let guild = ctx
            .guild()
            .ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?;
        guild.premium_tier
    };

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
        Quality::Max | Quality::Lossless if premium_tier < serenity::PremiumTier::Tier3 => {
            let embed = serenity::CreateEmbed::new()
                    .title("❌ Boost Level Required")
                    .description("Cấp độ này yêu cầu Server đạt tối thiểu **Boost Level 3** để mở khóa bitrate lớn hơn 256Kbps.")
                    .color(0xFF0000);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            return Ok(());
        }
        _ => {}
    }

    let db = &ctx.data().database;
    let stored_quality = quality_mode.to_str().to_owned();
    update_guild_setting_with_gate(
        db,
        guild_id.get(),
        std::future::ready(()),
        move |settings| {
            settings.quality = stored_quality;
        },
    )
    .await;

    let raw_bitrate = quality_mode.to_bitrate();
    let max_tier_bitrate = match premium_tier {
        serenity::PremiumTier::Tier3 => 384_000,
        serenity::PremiumTier::Tier2 => 256_000,
        serenity::PremiumTier::Tier1 => 128_000,
        _ => 96_000,
    };
    let target_bitrate = if raw_bitrate == 0 {
        0
    } else {
        raw_bitrate.min(max_tier_bitrate)
    };

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone());

    let voice_channel = if let Some(ref player_lock) = player_lock {
        let player = player_lock.read().await;
        player.voice_channel
    } else {
        None
    };

    if let Some(vc_id) = voice_channel {
        if quality_mode != Quality::Auto {
            let _ = vc_id
                .edit(
                    &ctx.serenity_context().http,
                    serenity::EditChannel::new().bitrate(target_bitrate),
                )
                .await;
        }

        let manager = songbird::get(ctx.serenity_context())
            .await
            .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized".into()))?
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
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
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
            return Err(SerenyaError::Permission(
                "Only server administrators can change prefix.".into(),
            )
            .into());
        }

        let stored_prefix = new_prefix.clone();
        update_guild_setting_with_gate(
            db,
            guild_id.get(),
            std::future::ready(()),
            move |settings| {
                settings.prefix = Some(stored_prefix);
            },
        )
        .await;

        ctx.say(format!(
            "✅ Prefix has been changed to `{new_prefix}` for this server."
        ))
        .await?;
    } else {
        let settings = db.get_guild_settings(guild_id.get()).await;
        let current_prefix = settings
            .prefix
            .unwrap_or_else(|| ctx.data().config().bot.prefix.clone());
        ctx.say(format!("`[{current_prefix}]`")).await?;
    }
    Ok(())
}

#[cfg(test)]
mod concurrent_settings_update_tests {
    use super::update_guild_setting_with_gate;
    use crate::database::DatabaseManager;
    use std::sync::Arc;

    fn temp_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "serenya-settings-race-{}-{}.yml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    async fn cleanup(path: &std::path::Path) {
        let _ = tokio::fs::remove_file(path).await;
        let _ = tokio::fs::remove_file(path.with_extension("yml.tmp")).await;
        let _ = tokio::fs::remove_file(path.with_extension("yml.bak")).await;
    }

    #[tokio::test]
    async fn settings_update_does_not_overwrite_concurrent_playback_stats()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = temp_db_path();
        let db = Arc::new(DatabaseManager::load(&path).await?);
        let guild_id = 7_700_020_u64;
        let read_done = Arc::new(tokio::sync::Notify::new());
        let allow_write = Arc::new(tokio::sync::Notify::new());

        let task_db = Arc::clone(&db);
        let task_read_done = Arc::clone(&read_done);
        let task_allow_write = Arc::clone(&allow_write);
        let setting_task = tokio::spawn(async move {
            update_guild_setting_with_gate(
                task_db.as_ref(),
                guild_id,
                async move {
                    task_read_done.notify_one();
                    task_allow_write.notified().await;
                },
                |settings| settings.announce_track = false,
            )
            .await;
        });

        read_done.notified().await;
        db.update_guild_settings_mut(guild_id, |settings| {
            settings.total_songs_played = 17;
            settings.total_listening_seconds = 901;
        })
        .await;
        allow_write.notify_one();
        setting_task.await?;

        let settings = db.get_guild_settings(guild_id).await;
        assert!(
            !settings.announce_track,
            "the requested setting change must land"
        );
        assert_eq!(
            settings.total_songs_played, 17,
            "a stale whole-settings write must not erase concurrently recorded songs"
        );
        assert_eq!(
            settings.total_listening_seconds, 901,
            "a stale whole-settings write must not erase concurrently recorded listening time"
        );

        cleanup(&path).await;
        Ok(())
    }

    #[tokio::test]
    async fn ordinary_settings_update_still_changes_only_requested_field()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = temp_db_path();
        let db = DatabaseManager::load(&path).await?;
        let guild_id = 7_700_021_u64;
        db.update_guild_settings_mut(guild_id, |settings| {
            settings.total_songs_played = 3;
        })
        .await;

        update_guild_setting_with_gate(&db, guild_id, std::future::ready(()), |settings| {
            settings.quality = "quality".to_owned();
        })
        .await;

        let settings = db.get_guild_settings(guild_id).await;
        assert_eq!(settings.quality, "quality");
        assert_eq!(settings.total_songs_played, 3);
        cleanup(&path).await;
        Ok(())
    }
}
