use crate::utils::{Context, Error, SerenyaError};

async fn get_memory_usage() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = tokio::fs::read_to_string("/proc/self/status").await {
            for line in status.lines() {
                if line.starts_with("VmRSS:") {
                    return line.trim_start_matches("VmRSS:").trim().to_string();
                }
            }
        }
    }
    "N/A (Non-Linux)".to_string()
}

/// Show statistics about the bot and the current guild.
#[poise::command(slash_command, prefix_command)]
pub async fn stats(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    // 1. Bot-wide statistics
    let uptime = ctx.data().start_time.elapsed();
    let uptime_str = crate::discord::embeds::format_duration(uptime);
    let memory_str = get_memory_usage().await;
    let guilds = ctx.cache().guilds().len();

    let guild_ids: Vec<_> = ctx.data().guild_players.iter().map(|e| *e.key()).collect();
    let mut active_vcs = 0;
    for gid in guild_ids {
        let player_lock = match ctx.data().guild_players.get(&gid) {
            Some(p) => p.value().clone(),
            None => continue,
        };
        let player = player_lock.read().await;
        if player.voice_channel.is_some() {
            active_vcs += 1;
        }
    }

    let instance_name = ctx.data().config().bot.instance_id.clone();

    // 2. Guild-specific statistics
    let database = &ctx.data().database;
    let guild_settings = database.get_guild_settings(guild_id.get()).await;
    let guild_songs_played = guild_settings.total_songs_played;
    let guild_listening_time = guild_settings.total_listening_seconds;

    // 3. Current active queue statistics
    let mut queue_size = 0;
    let mut listeners = 0;

    let player_lock_opt = match ctx.data().guild_players.get(&guild_id) {
        Some(p) => Some(p.value().clone()),
        None => None,
    };

    if let Some(player_lock) = player_lock_opt {
        let player = player_lock.read().await;
        queue_size = player.queue.len();

        if let Some(vc_channel_id) = player.voice_channel
            && let Some(guild) = ctx.guild()
        {
            for state in guild.voice_states.values() {
                if state.channel_id == Some(vc_channel_id) {
                    let is_bot = ctx
                        .cache()
                        .user(state.user_id)
                        .map(|u| u.bot)
                        .unwrap_or(false);
                    if !is_bot {
                        listeners += 1;
                    }
                }
            }
        }
    }

    let embed = crate::discord::embeds::stats_embed(
        &uptime_str,
        &memory_str,
        guilds,
        active_vcs,
        guild_songs_played,
        guild_listening_time,
        queue_size,
        listeners,
        &instance_name,
    );

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
