#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(not(feature = "dhat-heap"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod audio;
mod commands;
mod config;
mod core;
mod database;
mod discord;
mod installer;
mod logging;
mod utils;

use std::sync::Arc;

use dashmap::DashMap;
use poise::serenity_prelude as serenity;
use songbird::SerenityInit;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::config::BotConfig;
use crate::database::DatabaseManager;

/// Shared application state accessible from all command handlers.
pub struct Data {
    pub config: arc_swap::ArcSwap<BotConfig>,
    pub database: Arc<DatabaseManager>,
    pub guild_players: Arc<DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<core::GuildPlayer>>>>,
    pub http_client: reqwest::Client,
    pub start_time: std::time::Instant,
}

impl Data {
    pub fn config(&self) -> Arc<BotConfig> {
        self.config.load().clone()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    configure_path();
    let _ = rustls::crypto::ring::default_provider().install_default();
    tokio::runtime::Runtime::new()?.block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    installer::ensure_dependencies().await;

    let config = Arc::new(config::load_config("config.yml").await?);

    // Register secrets for redaction
    logging::register_secret_to_redact(&config.bot.token);
    if let Some(ref cookie) = config.spotify.sp_dc {
        logging::register_secret_to_redact(cookie);
    }
    if let Some(ref url) = config.logging.webhook_url {
        logging::register_secret_to_redact(url);
    }
    if let Some(ref url) = config.bot.log_webhook_url {
        logging::register_secret_to_redact(url);
    }

    audio::runtime::configure(
        &config.resolver,
        &config.spotify,
        config.playback.max_playlist_import,
    );
    init_tracing(&config.logging);
    info!(target: "start", "Starting Serenya...");
    info!(target: "start", instance_id = %config.bot.instance_id, "Configuration loaded");

    let database = Arc::new(DatabaseManager::load("database.yml").await?);
    info!(target: "start", "Database loaded");

    let cancel_token = CancellationToken::new();
    let auto_save_handle =
        database.start_auto_save(std::time::Duration::from_secs(30), cancel_token.clone());

    let http_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(3))
        .timeout(std::time::Duration::from_secs(15))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .pool_max_idle_per_host(16)
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()?;
    let start_time = std::time::Instant::now();

    let config_clone = Arc::clone(&config);
    let database_clone = Arc::clone(&database);

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all_commands(),
            prefix_options: poise::PrefixFrameworkOptions {
                dynamic_prefix: Some(|ctx| {
                    Box::pin(async move {
                        let default_prefix = ctx.data.config().bot.prefix.clone();
                        if let Some(guild_id) = ctx.guild_id {
                            let prefix = ctx
                                .data
                                .database
                                .get_guild_prefix(guild_id.get(), &default_prefix);
                            return Ok(Some(prefix.to_string()));
                        }
                        Ok(Some(default_prefix))
                    })
                }),
                mention_as_prefix: true,
                ..Default::default()
            },
            on_error: |error| Box::pin(on_error(error)),
            pre_command: |ctx| {
                Box::pin(async move {
                    info!(
                        command = ctx.command().name,
                        user = %ctx.author().name,
                        user_id = %ctx.author().id,
                        guild_id = ?ctx.guild_id(),
                        "Command invoked"
                    );
                })
            },
            post_command: |ctx| {
                Box::pin(async move {
                    info!(
                        command = ctx.command().name,
                        user = %ctx.author().name,
                        user_id = %ctx.author().id,
                        guild_id = ?ctx.guild_id(),
                        "Command executed successfully"
                    );
                })
            },
            event_handler: |ctx, event, _framework, data| {
                Box::pin(async move {
                    match event {
                        serenity::FullEvent::VoiceStateUpdate { old, new } => {
                            if let Err(e) = handle_voice_state_update(ctx, old, new, data).await {
                                error!("Error in voice state update handler: {:?}", e);
                            }
                        }
                        serenity::FullEvent::GuildDelete { incomplete, .. } => {
                            let guild_id = incomplete.id;
                            audio::runtime::cleanup_guild(guild_id.get());
                            data.guild_players.remove(&guild_id);
                            info!(guild_id = %guild_id, "Guild removed — cleaned up runtime state");
                        }
                        serenity::FullEvent::Message { new_message } if !new_message.author.bot => {
                            let content = &new_message.content;
                            let config = data.config();
                            let default_prefix = config.bot.prefix.as_str();
                            let prefix = if let Some(guild_id) = new_message.guild_id {
                                data.database
                                    .get_guild_prefix(guild_id.get(), default_prefix)
                            } else {
                                Arc::from(default_prefix)
                            };

                            if content.starts_with(prefix.as_ref()) {
                                let content_lower = content.to_lowercase();
                                let has_music_link = content_lower.contains("spotify.com")
                                    || content_lower.contains("youtube.com")
                                    || content_lower.contains("youtu.be")
                                    || content_lower.contains("soundcloud.com")
                                    || content_lower.contains("music.apple.com");

                                if has_music_link {
                                    let http = ctx.http.clone();
                                    let msg_id = new_message.id;
                                    let channel_id = new_message.channel_id;
                                    tokio::spawn(async move {
                                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                        let mut flags = serenity::MessageFlags::empty();
                                        flags.insert(serenity::MessageFlags::SUPPRESS_EMBEDS);
                                        let builder = serenity::EditMessage::new().flags(flags);
                                        if let Err(e) =
                                            channel_id.edit_message(&http, msg_id, builder).await
                                        {
                                            tracing::debug!(
                                                "Failed to suppress embeds on user message: {:?}",
                                                e
                                            );
                                        }
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                    Ok(())
                })
            },
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                info!(target: "start", "Slash commands registered globally");

                let guild_players = Arc::new(DashMap::new());

                start_empty_room_monitor(
                    guild_players.clone(),
                    ctx.http.clone(),
                    config_clone.clone(),
                    ctx.clone(),
                );
                
                start_stale_guild_cleanup(
                    guild_players.clone(),
                );

                Ok(Data {
                    config: arc_swap::ArcSwap::new(config_clone),
                    database: database_clone,
                    guild_players,
                    http_client,
                    start_time,
                })
            })
        })
        .build();

    let intents = serenity::GatewayIntents::GUILDS
        | serenity::GatewayIntents::GUILD_VOICE_STATES
        | serenity::GatewayIntents::GUILD_MESSAGES
        | serenity::GatewayIntents::MESSAGE_CONTENT;

    let songbird_config = songbird::Config::default()
        .use_softclip(false)
        .preallocated_tracks(2);
    let mut cache_settings = serenity::cache::Settings::default();
    cache_settings.max_messages = 0;
    let mut client = serenity::ClientBuilder::new(&config.bot.token, intents)
        .framework(framework)
        .cache_settings(cache_settings)
        .register_songbird_from_config(songbird_config)
        .await?;

    info!(
        target: "start",
        display_name = %config.bot.display_name,
        "Serenya is ready"
    );

    #[cfg(unix)]
    let sigterm_future = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .unwrap()
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let sigterm_future = std::future::pending::<()>();

    tokio::select! {
        result = client.start() => {
            if let Err(err) = result {
                error!(%err, "Client exited with error");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!(target: "shutdown", "Shutdown signal received (ctrl+c)");
        }
        _ = sigterm_future => {
            info!(target: "shutdown", "Shutdown signal received (SIGTERM)");
        }
    }

    shutdown(cancel_token, auto_save_handle, &database).await;
    Ok(())
}

async fn shutdown(
    cancel_token: CancellationToken,
    auto_save_handle: tokio::task::JoinHandle<()>,
    database: &DatabaseManager,
) {
    info!(target: "shutdown", "Initiating graceful shutdown...");

    cancel_token.cancel();

    if let Err(err) = auto_save_handle.await {
        error!(%err, "Auto-save task panicked during shutdown");
    }

    if let Err(err) = database.shutdown().await {
        error!(%err, "Failed to save database during shutdown");
    }

    info!(target: "shutdown", "Serenya shut down gracefully");

    logging::webhook::shutdown().await;
}

/// Appends the `bin/` subdirectory (relative to CWD) to the process PATH.
fn configure_path() {
    if let Ok(cwd) = std::env::current_dir() {
        let bin_dir = cwd.join("bin");
        if bin_dir.exists() {
            let mut paths = vec![bin_dir];
            if let Some(path_var) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&path_var));
            }
            if let Ok(new_path) = std::env::join_paths(paths) {
                unsafe {
                    std::env::set_var("PATH", new_path);
                }
            }
        }
    }
}

fn init_tracing(logging: &config::LoggingSection) {
    use crate::logging::MakeRedactingWriter;
    use tracing::Level;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter_str = match logging.level.to_lowercase().as_str() {
        "error" => "error,songbird=error,serenity=error,hyper=error,reqwest=error",
        "warn" => "warn,serenya=warn,songbird=warn,serenity=warn,hyper=warn,reqwest=warn",
        "info" => "info,serenya=info,songbird=info,serenity=warn,hyper=info,reqwest=info",
        "debug" => "info,serenya=debug,songbird=info,serenity=warn,hyper=info,reqwest=info",
        "trace" => "info,serenya=trace,songbird=info,serenity=warn,hyper=info,reqwest=info",
        _ => "info,serenya=debug,songbird=info,serenity=warn,hyper=info,reqwest=info",
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(env_filter_str));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(MakeRedactingWriter);

    let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);

    if logging.webhook_enabled
        && let Some(ref url) = logging.webhook_url
    {
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build webhook http client");
        let min_level = match logging.webhook_min_level.to_lowercase().as_str() {
            "error" => Level::ERROR,
            "warn" => Level::WARN,
            "info" => Level::INFO,
            "debug" => Level::DEBUG,
            "trace" => Level::TRACE,
            _ => Level::INFO,
        };
        let webhook_layer = logging::webhook::WebhookLayer::new(
            url.clone(),
            http_client,
            min_level,
            logging.webhook_plain_text,
        );
        let _ = registry.with(webhook_layer).try_init();
        return;
    }
    let _ = registry.try_init();
}

async fn on_error(error: poise::FrameworkError<'_, Data, utils::Error>) {
    match error {
        poise::FrameworkError::Command { error, ctx, .. } => {
            error!(%error, command = ctx.command().name, "Command error");
            let message =
                if let Some(serenya_err) = error.downcast_ref::<utils::error::SerenyaError>() {
                    match serenya_err {
                        utils::error::SerenyaError::Permission(msg) => {
                            format!("**Permission Denied:** {msg}")
                        }
                        utils::error::SerenyaError::NotFound(msg) => {
                            format!("**Not Found:** {msg}")
                        }
                        utils::error::SerenyaError::Voice(msg) => {
                            format!("**Voice Connection Error:** {msg}")
                        }
                        utils::error::SerenyaError::Queue(msg) => {
                            format!("**Queue Error:** {msg}")
                        }
                        utils::error::SerenyaError::Database(msg) => {
                            format!("**Database Error:** {msg}")
                        }
                        utils::error::SerenyaError::Config(msg) => {
                            format!("**Configuration Error:** {msg}")
                        }
                        other => format!("{other}"),
                    }
                } else {
                    error.to_string()
                };

            let embed = discord::embeds::error_embed(&message);
            let reply = poise::CreateReply::default().embed(embed).ephemeral(true);
            let _ = ctx.send(reply).await;
        }
        poise::FrameworkError::Setup { error, .. } => {
            error!(%error, "Failed to start bot");
        }
        other => {
            if let Err(err) = poise::builtins::on_error(other).await {
                error!(%err, "Unhandled framework error");
            }
        }
    }
}

/// Background task that periodically cleans up guild players that have been disconnected
/// for extended periods to prevent unbounded memory growth.
fn start_stale_guild_cleanup(
    guild_players: Arc<DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<core::GuildPlayer>>>>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600)); // Check every hour
        loop {
            interval.tick().await;
            let now = std::time::Instant::now();
            let mut stale_guilds = Vec::new();

            // Find guilds with no voice connection and idle for 30 minutes
            for entry in guild_players.iter() {
                let guild_id = *entry.key();
                let player_lock = entry.value().clone();
                
                if let Ok(player) = player_lock.try_read() {
                    // If bot is not in voice and queue is empty and idle, mark for removal
                    if player.voice_channel.is_none()
                        && player.queue.is_empty()
                        && player.playback_status == core::PlaybackStatus::Idle
                    {
                        if let Some(empty_since) = player.empty_since {
                            if now.duration_since(empty_since).as_secs() >= 1800 {
                                stale_guilds.push(guild_id);
                            }
                        }
                    }
                }
            }

            // Remove stale entries
            for guild_id in stale_guilds {
                if guild_players.remove(&guild_id).is_some() {
                    audio::runtime::cleanup_guild(guild_id.get());
                    tracing::debug!(guild_id = %guild_id, "Removed stale guild player from memory");
                }
            }
        }
    });
}

fn start_empty_room_monitor(
    guild_players: Arc<DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<core::GuildPlayer>>>>,
    http: Arc<serenity::Http>,
    config: Arc<BotConfig>,
    serenity_ctx: serenity::Context,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let now = std::time::Instant::now();
            // Phase 1: Collect guild IDs without holding the shard-lock across .await
            let guild_ids: Vec<_> = guild_players.iter().map(|e| *e.key()).collect();
            let mut to_clear = Vec::new();

            // Phase 2: Check each guild individually — get() locks only one shard at a time
            for guild_id in guild_ids {
                let player_lock = match guild_players.get(&guild_id) {
                    Some(p) => p.value().clone(),
                    None => continue,
                };
                let player = player_lock.read().await;
                if let Some(empty_since) = player.empty_since
                    && now.duration_since(empty_since).as_secs() >= 10800
                    && (!player.queue.is_empty()
                        || player.playback_status != core::PlaybackStatus::Idle)
                {
                    to_clear.push((guild_id, player.announce_channel));
                }
            }

            for (guild_id, announce_channel) in to_clear {
                let player_lock_opt = guild_players.get(&guild_id).map(|p| p.value().clone());
                if let Some(player_lock) = player_lock_opt {
                    let stay = config.playback.stay_in_voice;

                    {
                        let mut player = player_lock.write().await;
                        player.reset();
                        if !stay {
                            player.voice_channel = None;
                            player.announce_channel = None;
                        } else {
                            // Keep voice_channel/announce_channel but mark empty_since
                            // so we don't re-trigger until someone joins again
                            player.empty_since = Some(now);
                        }
                    }

                    if stay {
                        // stay_in_voice = true: only clear queue, keep the voice connection
                        audio::runtime::cleanup_guild(guild_id.get());
                        info!(guild_id = %guild_id, "Cleared queue after 3 hours of empty room (staying in voice)");
                    } else {
                        // stay_in_voice = false: fully disconnect
                        guild_players.remove(&guild_id);
                        if let Some(manager) = songbird::get(&serenity_ctx).await {
                            let _ = manager.remove(guild_id).await;
                        }
                        audio::runtime::cleanup_guild(guild_id.get());
                        info!(guild_id = %guild_id, "Disconnected after 3 hours of empty room");
                    }

                    if let Some(channel) = announce_channel {
                        let description = if stay {
                            "Đã 3 tiếng không có ai trong phòng, hàng chờ (queue) đã tự động được dọn dẹp để tiết kiệm tài nguyên."
                        } else {
                            "Đã 3 tiếng không có ai trong phòng, bot đã tự động rời kênh thoại để tiết kiệm tài nguyên."
                        };
                        let embed = serenity::CreateEmbed::new()
                            .description(description)
                            .color(0xED4245);
                        let _ = channel
                            .send_message(&http, serenity::CreateMessage::new().embed(embed))
                            .await;
                    }
                }
            }
        }
    });
}

async fn handle_voice_state_update(
    ctx: &serenity::Context,
    _old: &Option<serenity::VoiceState>,
    new: &serenity::VoiceState,
    data: &Data,
) -> Result<(), utils::Error> {
    let guild_id = match new.guild_id {
        Some(g) => g,
        None => return Ok(()),
    };

    let player_lock = match data.guild_players.get(&guild_id) {
        Some(p) => p.value().clone(),
        None => return Ok(()),
    };

    // Read necessary info first under read lock
    let (bot_channel_id, queue_is_empty, playback_status) = {
        let player = player_lock.read().await;
        (
            player.voice_channel,
            player.queue.is_empty(),
            player.playback_status,
        )
    };

    let bot_channel_id = match bot_channel_id {
        Some(c) => c,
        None => {
            // If the bot has left the voice channel and queue is empty, remove player memory
            if queue_is_empty && playback_status == core::PlaybackStatus::Idle {
                data.guild_players.remove(&guild_id);
                audio::runtime::cleanup_guild(guild_id.get());
                info!(
                    guild_id = %guild_id,
                    "Bot is not in voice and queue is empty, removed GuildPlayer"
                );
            }
            return Ok(());
        }
    };

    let bot_id = ctx.cache.current_user().id;

    // Count human members in the voice channel (without holding lock)
    let mut human_count = 0;
    if let Some(guild) = ctx.cache.guild(guild_id) {
        for state in guild.voice_states.values() {
            if state.channel_id == Some(bot_channel_id) && state.user_id != bot_id {
                let is_bot = if let Some(user) = ctx.cache.user(state.user_id) {
                    user.bot
                } else if let Some(member) = guild.members.get(&state.user_id) {
                    member.user.bot
                } else {
                    false
                };

                if !is_bot {
                    human_count += 1;
                }
            }
        }
    }

    // Update empty_since and auto-pause if playing (acquire write lock ONLY when needed)
    if human_count == 0 {
        let mut player = player_lock.write().await;
        
        // Only set empty_since if it's not already set
        if player.empty_since.is_none() {
            player.empty_since = Some(std::time::Instant::now());
        }

        // Auto-pause if playing
        if player.playback_status == core::PlaybackStatus::Playing
            && let Some(ref handle) = player.current_track_handle
        {
            let should_announce = if let Err(e) = handle.pause() {
                error!("Failed to auto-pause track in empty channel: {:?}", e);
                false
            } else {
                player.playback_status = core::PlaybackStatus::Paused;
                info!(
                    guild_id = %guild_id,
                    channel_id = %bot_channel_id,
                    "Playback auto-paused because voice channel is empty"
                );
                true
            };

            let announce_channel = player.announce_channel;
            drop(player); // Release the write lock before sending HTTP request

            if should_announce && let Some(ch) = announce_channel {
                let embed = serenity::CreateEmbed::new()
                    .description("Không có ai trong room nên âm nhạc sẽ tạm dừng `s.resume` để tiếp tục từ chỗ đã stop")
                    .color(0x5865F2);
                let _ = ch
                    .send_message(&ctx.http, serenity::CreateMessage::new().embed(embed))
                    .await;
            }
        }
    } else {
        // Someone joined - clear the empty_since flag
        let mut player = player_lock.write().await;
        if player.empty_since.is_some() {
            player.empty_since = None;
        }
    }

    Ok(())
}
