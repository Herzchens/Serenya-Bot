use crate::utils::{Context, Error};
use poise::serenity_prelude as serenity;

/// Show bot latency.
#[poise::command(slash_command, prefix_command)]
pub async fn ping(ctx: Context<'_>) -> Result<(), Error> {
    let latency = ctx.ping().await;
    let response = format!("🏓 Pong! Latency: {latency:.0?}");
    ctx.say(response).await?;
    Ok(())
}

/// Show bot information.
#[poise::command(slash_command, prefix_command)]
pub async fn about(ctx: Context<'_>) -> Result<(), Error> {
    let config = ctx.data().config();

    let embed = poise::serenity_prelude::CreateEmbed::new()
        .title(format!("🤖 About {}", config.bot.display_name))
        .description("Serenya là một bot nhạc Discord chất lượng cao, mang lại trải nghiệm âm thanh mượt mà và giao diện tương tác trực quan nhất.")
        .field("Người Tạo", "💙 **ItzHerzchen**", true)
        .field("GitHub Repository", "[🔗 Herzchens/Serenya-Bot](https://github.com/Herzchens/Serenya-Bot)", true)
        .field(
            "Khả Năng & Tính Năng",
            "• Phát nhạc cực nhanh từ **YouTube**, **Spotify** và nhiều nền tảng khác\n\
             • Hỗ trợ quản lý hàng chờ nâng cao (chuyển bài, tua nhanh, lặp bài)\n\
             • Quản lý playlist cá nhân và đồng bộ dữ liệu thông minh\n\
             • Tìm kiếm lời bài hát trực tiếp trên Discord và hơn thế nữa",
            false,
        )
        .color(0x5865F2);

    let reply = poise::CreateReply::default().embed(embed);
    ctx.send(reply).await?;
    Ok(())
}

/// Show help menu for commands.
#[poise::command(slash_command, prefix_command)]
pub async fn help(
    ctx: Context<'_>,
    #[description = "Specific command to show help for"]
    #[autocomplete = "poise::builtins::autocomplete_command"]
    command: Option<String>,
) -> Result<(), Error> {
    if let Some(cmd_name) = command {
        let cmd = ctx.framework().options().commands.iter().find(|c| {
            c.name.eq_ignore_ascii_case(&cmd_name)
                || c.aliases.iter().any(|a| a.eq_ignore_ascii_case(&cmd_name))
        });

        if let Some(c) = cmd {
            let mut desc = c
                .description
                .clone()
                .unwrap_or_else(|| "No description provided.".to_string());
            if !c.aliases.is_empty() {
                desc.push_str(&format!("\n\n**Aliases:** {}", c.aliases.join(", ")));
            }

            if !c.subcommands.is_empty() {
                let subs: Vec<String> = c
                    .subcommands
                    .iter()
                    .map(|s| format!("`{}`", s.name))
                    .collect();
                desc.push_str(&format!("\n**Subcommands:** {}", subs.join(", ")));
            }

            let embed = serenity::CreateEmbed::new()
                .title(format!("📖 Help: /{}", c.name))
                .description(desc)
                .color(0x5865F2);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        } else {
            let embed = serenity::CreateEmbed::new()
                .title("❌ Command Not Found")
                .description(format!("Could not find a command named `{}`.", cmd_name))
                .color(0xFF0000);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
    } else {
        let embed = serenity::CreateEmbed::new()
            .title("🎶 Serenya Help Menu")
            .description("Here is a list of all available commands grouped by category. Type `/help <command>` to see more details about a specific command.")
            .field(
                "🎵 Music / Phát nhạc",
                "`play` - Play a song/playlist\n`lyrics` - Search lyrics\n`playlist` - Manage custom playlists\n`join` - Join voice channel\n`leave` - Leave voice channel",
                false
            )
            .field(
                "🎛️ Control / Điều khiển",
                "`pause` - Pause playback\n`resume` - Resume playback\n`stop` - Stop & clear queue\n`skip` - Skip current track\n`replay` - Replay current song\n`previous` - Play previous track\n`loop` - Change loop mode\n`seek` - Seek to time\n`forward` - Fast forward\n`rewind` - Rewind song\n`8d` - Toggle 8D sound effect",
                false
            )
            .field(
                "📋 Queue / Hàng đợi",
                "`queue` - View queue\n`remove` - Remove from queue\n`clear` - Clear queue\n`shuffle` - Shuffle queue\n`jump` - Jump to track\n`move` - Move track in queue",
                false
            )
            .field(
                "⚙️ Settings / Cài đặt",
                "`prefix` - View or set guild prefix\n`quality` - Change audio quality\n`announce_track` - Toggle song announcement",
                false
            )
            .field(
                "ℹ️ General / Thông tin",
                "`ping` - Show latency\n`about` - Bot info\n`stats` - Show bot stats\n`songinfo` - Show track info\n`invite` - Invite link\n`support` - Support server\n`cleanup` - Clean bot chats",
                false
            )
            .footer(serenity::CreateEmbedFooter::new("Type /help <command> for more details."))
            .color(0x5865F2);

        ctx.send(poise::CreateReply::default().embed(embed)).await?;
    }
    Ok(())
}

/// Reload settings that are safe to change without replacing process-global runtime state.
#[poise::command(slash_command, prefix_command)]
pub async fn reload(ctx: Context<'_>) -> Result<(), Error> {
    let current = ctx.data().config();
    if !reload_authorized(ctx.author().id.get(), current.bot.owner) {
        return Err(crate::utils::SerenyaError::Permission(
            "This command is restricted to the bot owner because it changes process-wide configuration.".into(),
        ).into());
    }

    ctx.defer().await?;
    let requested = crate::config::load_config("config.yml").await?;
    let applied = std::sync::Arc::new(build_hot_reload_config(&current, requested));
    let old_prefix = current.bot.prefix.clone();
    let new_prefix = applied.bot.prefix.clone();
    ctx.data().config.store(applied);
    tracing::info!(old_prefix, new_prefix, "Applied safe hot-reload settings");

    let embed = poise::serenity_prelude::CreateEmbed::new()
        .title("🔄 System Reload Complete")
        .description("Safe runtime settings were applied. Resolver, Spotify, logging, token, and resolver-backed playlist limits remain unchanged until restart.")
        .field("Live", "Prefix, voice retention, queue limits, announcements, metadata", false)
        .field("Restart required", "Bot token, logging, Spotify, resolver limits/timeouts/caches, playlist import runtime", false)
        .color(0x3498DB);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

fn reload_authorized(author_id: u64, owner_id: u64) -> bool {
    author_id == owner_id
}

fn build_hot_reload_config(
    current: &crate::config::BotConfig,
    mut requested: crate::config::BotConfig,
) -> crate::config::BotConfig {
    requested.bot.token = current.bot.token.clone();
    requested.bot.log_webhook_url = current.bot.log_webhook_url.clone();
    requested.logging = current.logging.clone();
    requested.spotify = current.spotify.clone();
    requested.resolver = current.resolver.clone();
    requested.playback.max_playlist_import = current.playback.max_playlist_import;
    requested
}

#[cfg(test)]
mod reload_tests {
    use super::{build_hot_reload_config, reload_authorized};
    use crate::config::BotConfig;

    fn example_config() -> BotConfig {
        serde_saphyr::from_str(include_str!("../../config.example.yml")).unwrap()
    }

    #[test]
    fn only_bot_owner_can_hot_reload_process_configuration() {
        assert!(reload_authorized(10, 10));
        assert!(!reload_authorized(11, 10));
    }

    #[test]
    fn hot_reload_preserves_restart_only_sections() {
        let current = example_config();
        let mut requested = current.clone();
        requested.bot.prefix = "new!".to_owned();
        requested.bot.token = "different-token".to_owned();
        requested.playback.stay_in_voice = !current.playback.stay_in_voice;
        requested.playback.max_playlist_import = current.playback.max_playlist_import + 10;
        requested.resolver.max_concurrent_ytdlp = current.resolver.max_concurrent_ytdlp + 10;
        requested.spotify.market = "ZZ".to_owned();
        requested.logging.level = "trace".to_owned();
        let applied = build_hot_reload_config(&current, requested);
        assert_eq!(applied.bot.prefix, "new!");
        assert_ne!(
            applied.playback.stay_in_voice,
            current.playback.stay_in_voice
        );
        assert_eq!(applied.bot.token, current.bot.token);
        assert_eq!(
            applied.playback.max_playlist_import,
            current.playback.max_playlist_import
        );
        assert_eq!(
            applied.resolver.max_concurrent_ytdlp,
            current.resolver.max_concurrent_ytdlp
        );
        assert_eq!(applied.spotify.market, current.spotify.market);
        assert_eq!(applied.logging.level, current.logging.level);
    }
}
