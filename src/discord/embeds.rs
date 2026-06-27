#![allow(dead_code, clippy::too_many_arguments)]
use crate::config::BotConfig;
use crate::core::Track;
use poise::serenity_prelude as serenity;
use std::time::Duration;

/// Helper to format a duration into a human-readable HH:MM:SS or MM:SS string.
pub fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}

/// Helper to construct a progress bar string.
pub fn make_progress_bar(elapsed: Duration, duration: Option<Duration>) -> String {
    if let Some(dur) = duration {
        let total_secs = dur.as_secs();
        if total_secs == 0 {
            return "🔘▬▬▬▬▬▬▬▬▬▬".to_string();
        }
        let elapsed_secs = elapsed.as_secs();
        let fraction = (elapsed_secs as f64 / total_secs as f64).clamp(0.0, 1.0);
        let bar_len = 10;
        let filled_len = ((fraction * bar_len as f64).round() as usize).clamp(0, bar_len);
        let mut bar = String::new();
        for i in 0..bar_len {
            if i == filled_len {
                bar.push('🔘');
            } else {
                bar.push('▬');
            }
        }
        if filled_len == bar_len {
            bar.push('🔘');
        }
        format!(
            "{} ({}/{})",
            bar,
            format_duration(elapsed),
            format_duration(dur)
        )
    } else {
        format!("🔘▬▬▬▬▬▬▬▬▬▬ Live ({})", format_duration(elapsed))
    }
}

/// Helper to get the provider emoji from config.
pub fn get_provider_emoji(track: &Track, config: &BotConfig) -> String {
    let default_emoji = "🎵".to_string();
    let emojis = match &config.emojis {
        Some(e) => e,
        None => return default_emoji,
    };

    let source = track.clean_source().to_lowercase();
    let emoji_opt = if source.contains("spotify") {
        emojis.spotify.as_deref()
    } else if source.contains("youtube") {
        emojis.youtube.as_deref()
    } else if source.contains("soundcloud") {
        emojis.soundcloud.as_deref()
    } else if source.contains("apple") {
        emojis.apple_music.as_deref()
    } else if source.contains("deezer") {
        emojis.deezer.as_deref()
    } else {
        None
    };

    emoji_opt
        .or(emojis.default.as_deref())
        .map(|s| s.to_string())
        .unwrap_or(default_emoji)
}

/// Creates a now playing embed.
pub fn now_playing_embed(
    track: &Track,
    _elapsed: Duration,
    queue_pos: Option<usize>,
    config: &BotConfig,
) -> serenity::CreateEmbed {
    let duration_str = track
        .duration
        .map(format_duration)
        .unwrap_or_else(|| "Live".to_string());

    let provider_emoji = get_provider_emoji(track, config);
    let mut embed = serenity::CreateEmbed::new()
        .title("🎶 Now Playing")
        .description(if track.url.starts_with("http") {
            format!("{} [{}]({})", provider_emoji, track.title, track.url)
        } else {
            format!("{} **{}**", provider_emoji, track.title)
        })
        .field(
            "Requested By",
            track.requester_name.as_deref().unwrap_or("Unknown"),
            true,
        )
        .field("Duration", duration_str, true)
        .field("Source", track.clean_source(), true);

    if let Some(pos) = queue_pos {
        embed = embed.field("Queue Position", pos.to_string(), true);
    }

    embed = embed.color(0x5865F2);

    if let Some(ref thumb) = track.thumbnail {
        embed = embed.thumbnail(thumb.to_string());
    }
    embed
}

/// Creates a simplified minimal now playing embed for announcements.
pub fn now_playing_announce_embed(track: &Track, config: &BotConfig) -> serenity::CreateEmbed {
    let provider_emoji = get_provider_emoji(track, config);
    serenity::CreateEmbed::new()
        .color(0x5865F2)
        .description(if track.url.starts_with("http") {
            format!(
                "{} **Now Playing:** [{}]({})",
                provider_emoji, track.title, track.url
            )
        } else {
            format!("{} **Now Playing:** **{}**", provider_emoji, track.title)
        })
}

/// Creates a simplified minimal embed when a track is added to queue.
pub fn minimal_track_added_embed(track: &Track, config: &BotConfig) -> serenity::CreateEmbed {
    let provider_emoji = get_provider_emoji(track, config);
    serenity::CreateEmbed::new()
        .color(0x5865F2)
        .description(if track.url.starts_with("http") {
            format!(
                "{} **Added to queue:** [{}]({})",
                provider_emoji, track.title, track.url
            )
        } else {
            format!("{} **Added to queue:** **{}**", provider_emoji, track.title)
        })
}

/// Creates a track added to queue embed.
pub fn track_added_embed(
    track: &Track,
    queue_pos: usize,
    config: &BotConfig,
) -> serenity::CreateEmbed {
    let duration_str = track
        .duration
        .map(format_duration)
        .unwrap_or_else(|| "Live".to_string());

    let provider_emoji = get_provider_emoji(track, config);
    let mut embed = serenity::CreateEmbed::new()
        .title("📝 Track Enqueued")
        .description(if track.url.starts_with("http") {
            format!("{} [{}]({})", provider_emoji, track.title, track.url)
        } else {
            format!("{} **{}**", provider_emoji, track.title)
        })
        .field(
            "Requested By",
            track.requester_name.as_deref().unwrap_or("Unknown"),
            true,
        )
        .field("Duration", duration_str, true)
        .field("Queue Position", format!("#{}", queue_pos), true)
        .field("Source", track.clean_source(), true)
        .color(0x57F287);

    if let Some(ref thumb) = track.thumbnail {
        embed = embed.thumbnail(thumb.to_string());
    }
    embed
}

/// Creates a paginated queue embed.
pub fn queue_embed(
    tracks: &[Track],
    page: usize,
    total_pages: usize,
    total_tracks: usize,
    title: &str,
) -> serenity::CreateEmbed {
    let mut desc = String::new();
    for (i, track) in tracks.iter().enumerate() {
        let index = page * 10 + i + 1;
        let duration = track
            .duration
            .map(format_duration)
            .unwrap_or_else(|| "Live".to_string());
        let emoji = if page == 0 && i == 0 { "🔊" } else { "🎵" };
        let requester = track.requester_name.as_deref().unwrap_or("Unknown");
        desc.push_str(&format!(
            "{} `{:02}.` **{}** — `{}`\n╰ Requested by **{}**\n",
            emoji, index, track.title, duration, requester
        ));
    }

    if desc.is_empty() {
        desc = "The queue is empty.".to_string();
    }

    serenity::CreateEmbed::new()
        .title(title)
        .description(desc)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "Page {}/{} • {} tracks",
            page + 1,
            total_pages,
            total_tracks
        )))
        .color(0x5865F2)
}

/// Creates a playlist summary embed.
pub fn playlist_summary_embed(
    name: &str,
    count: usize,
    total_dur: Duration,
) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title(format!("📁 Playlist: {}", name))
        .field("Tracks", count.to_string(), true)
        .field("Total Duration", format_duration(total_dur), true)
        .color(0xFEE75C)
}

/// Creates a statistics embed.
pub fn stats_embed(
    uptime_str: &str,
    memory_str: &str,
    guilds: usize,
    active_vcs: usize,
    guild_songs_played: u64,
    guild_listening_time: u64,
    queue_size: usize,
    listeners: usize,
    instance_name: &str,
    query_cache: u64,
    metadata_cache: u64,
    stream_cache: u64,
    sc_stream_cache: u64,
    negative_cache: u64,
    dropped_webhooks: u64,
) -> serenity::CreateEmbed {
    let cache_info = format!(
        "Query: {} | Meta: {} | Stream: {} | SC: {} | Neg: {}",
        query_cache, metadata_cache, stream_cache, sc_stream_cache, negative_cache
    );
    let mut embed = serenity::CreateEmbed::new()
        .title("\u{1f4ca} Serenya Bot Statistics")
        .field("Uptime", uptime_str, true)
        .field("Memory Usage", memory_str, true)
        .field("Global Guilds", guilds.to_string(), true)
        .field("Active Voice Connections", active_vcs.to_string(), true)
        .field("Bot Instance", instance_name, true)
        .field("Queue Size", queue_size.to_string(), true)
        .field("Guild Played Tracks", guild_songs_played.to_string(), true)
        .field(
            "Guild Listening Time",
            format_duration(Duration::from_secs(guild_listening_time)),
            true,
        )
        .field("Listeners in VC", listeners.to_string(), true)
        .field("\u{1f5c4}\u{fe0f} Cache Entries", &cache_info, false)
        .color(0x5865F2);
    if dropped_webhooks > 0 {
        embed = embed.field(
            "\u{26a0}\u{fe0f} Dropped Webhook Logs",
            dropped_webhooks.to_string(),
            true,
        );
    }
    embed
}

/// Creates a queue finished/empty embed.
pub fn queue_finished_embed() -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title("⏹️ Queue Finished")
        .description("No more tracks left to play.")
        .color(0xED4245)
}

/// Creates an error embed.
pub fn error_embed(message: &str) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title("❌ Error")
        .description(message)
        .color(0xED4245)
}

/// Creates a queue stopped embed.
pub fn queue_stopped_embed() -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title("⏹️ Queue Stopped")
        .description("Playback has been stopped and the queue has been cleared.")
        .color(0xED4245)
}

/// Creates a generic playback status embed.
pub fn playback_status_embed(title: &str, description: &str, color: u32) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title(title)
        .description(description)
        .color(color)
}
