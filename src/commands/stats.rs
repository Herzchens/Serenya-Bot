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

    #[cfg(target_os = "windows")]
    {
        mod win32 {
            use std::ffi::c_void;

            #[repr(C)]
            #[allow(non_snake_case)]
            pub struct ProcessMemoryCounters {
                pub cb: u32,
                pub PageFaultCount: u32,
                pub PeakWorkingSetSize: usize,
                pub WorkingSetSize: usize,
                pub QuotaPeakPagedPoolUsage: usize,
                pub QuotaPagedPoolUsage: usize,
                pub QuotaPeakNonPagedPoolUsage: usize,
                pub QuotaNonPagedPoolUsage: usize,
                pub PagefileUsage: usize,
                pub PeakPagefileUsage: usize,
            }

            #[link(name = "kernel32")]
            unsafe extern "system" {
                pub fn GetCurrentProcess() -> *mut c_void;
            }

            #[link(name = "psapi")]
            unsafe extern "system" {
                pub fn GetProcessMemoryInfo(
                    process: *mut c_void,
                    pmc: *mut ProcessMemoryCounters,
                    cb: u32,
                ) -> i32;
            }
        }

        let mut pmc = win32::ProcessMemoryCounters {
            cb: size_of::<win32::ProcessMemoryCounters>() as u32,
            PageFaultCount: 0,
            PeakWorkingSetSize: 0,
            WorkingSetSize: 0,
            QuotaPeakPagedPoolUsage: 0,
            QuotaPagedPoolUsage: 0,
            QuotaPeakNonPagedPoolUsage: 0,
            QuotaNonPagedPoolUsage: 0,
            PagefileUsage: 0,
            PeakPagefileUsage: 0,
        };

        unsafe {
            let process = win32::GetCurrentProcess();
            if win32::GetProcessMemoryInfo(process, &mut pmc, pmc.cb) != 0 {
                return format!("{:.2} MB", pmc.WorkingSetSize as f64 / 1024.0 / 1024.0);
            }
        }
    }

    "N/A".to_string()
}

/// Show statistics about the bot and the current guild.
#[poise::command(slash_command, prefix_command)]
pub async fn stats(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

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

    let database = &ctx.data().database;
    let guild_settings = database.get_guild_settings(guild_id.get()).await;
    let guild_songs_played = guild_settings.total_songs_played;
    let guild_listening_time = guild_settings.total_listening_seconds;

    let mut queue_size = 0;
    let mut listeners = 0;

    let player_lock_opt = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|p| p.value().clone());

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

    let (query_cache, metadata_cache, stream_cache, sc_stream_cache) =
        crate::audio::source::cache_entry_counts();
    let negative_cache = crate::audio::runtime::negative_cache_entry_count();
    let dropped_webhooks = crate::logging::webhook::dropped_webhook_logs();

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
        query_cache,
        metadata_cache,
        stream_cache,
        sc_stream_cache,
        negative_cache,
        dropped_webhooks,
    );

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
