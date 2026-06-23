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
    pub guild_players: Arc<
        DashMap<serenity::GuildId, std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>>,
    >,
    pub http_client: reqwest::Client,
    pub start_time: std::time::Instant,
}

impl Data {
    pub fn config(&self) -> Arc<BotConfig> {
        self.config.load().clone()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    configure_path();
    tokio::runtime::Runtime::new().unwrap().block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    installer::ensure_dependencies().await;

    let config = Arc::new(config::load_config("config.yml").await?);

    // Register secrets for redaction
    crate::logging::register_secret_to_redact(&config.bot.token);
    if let Some(ref cookie) = config.spotify.sp_dc {
        crate::logging::register_secret_to_redact(cookie);
    }
    if let Some(ref url) = config.logging.webhook_url {
        crate::logging::register_secret_to_redact(url);
    }
    if let Some(ref url) = config.bot.log_webhook_url {
        crate::logging::register_secret_to_redact(url);
    }

    audio::runtime::configure(&config.resolver, &config.spotify);
    init_tracing(&config.logging);
    info!("Starting Serenya...");
    info!(instance_id = %config.bot.instance_id, "Configuration loaded");

    let database = Arc::new(DatabaseManager::load("database.yml").await?);
    info!("Database loaded");

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
                            let db = &ctx.data.database;
                            let settings = db.get_guild_settings(guild_id.get()).await;
                            if let Some(ref custom_prefix) = settings.prefix {
                                return Ok(Some(custom_prefix.clone()));
                            }
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
                            crate::audio::runtime::cleanup_guild(guild_id.get());
                            data.guild_players.remove(&guild_id);
                            info!(guild_id = %guild_id, "Guild removed — cleaned up runtime state");
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
                info!("Slash commands registered globally");

                let guild_players = std::sync::Arc::new(DashMap::new());

                start_empty_room_monitor(guild_players.clone(), ctx.http.clone());

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

    let intents = serenity::GatewayIntents::non_privileged()
        | serenity::GatewayIntents::MESSAGE_CONTENT
        | serenity::GatewayIntents::GUILD_VOICE_STATES;

    let songbird_config = songbird::Config::default()
        .use_softclip(false)
        .preallocated_tracks(2);
    let mut client = serenity::ClientBuilder::new(&config.bot.token, intents)
        .framework(framework)
        .register_songbird_from_config(songbird_config)
        .await?;

    info!(
        display_name = %config.bot.display_name,
        "Serenya is ready"
    );

    #[cfg(unix)]
    let sigterm_future = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap().recv().await;
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
            info!("Shutdown signal received (ctrl+c)");
        }
        _ = sigterm_future => {
            info!("Shutdown signal received (SIGTERM)");
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
    info!("Initiating graceful shutdown...");

    cancel_token.cancel();

    // Wait for auto-save task to finish
    if let Err(err) = auto_save_handle.await {
        error!(%err, "Auto-save task panicked during shutdown");
    }

    if let Err(err) = database.shutdown().await {
        error!(%err, "Failed to save database during shutdown");
    }

    info!("Serenya shut down gracefully");
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

fn init_tracing(logging: &crate::config::LoggingSection) {
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
            let message = if let Some(serenya_err) =
                error.downcast_ref::<crate::utils::error::SerenyaError>()
            {
                match serenya_err {
                    crate::utils::error::SerenyaError::Permission(msg) => {
                        format!("**Permission Denied:** {msg}")
                    }
                    crate::utils::error::SerenyaError::NotFound(msg) => {
                        format!("**Not Found:** {msg}")
                    }
                    crate::utils::error::SerenyaError::Voice(msg) => {
                        format!("**Voice Connection Error:** {msg}")
                    }
                    crate::utils::error::SerenyaError::Queue(msg) => {
                        format!("**Queue Error:** {msg}")
                    }
                    crate::utils::error::SerenyaError::Database(msg) => {
                        format!("**Database Error:** {msg}")
                    }
                    crate::utils::error::SerenyaError::Config(msg) => {
                        format!("**Configuration Error:** {msg}")
                    }
                    other => format!("{other}"),
                }
            } else {
                error.to_string()
            };

            let embed = crate::discord::embeds::error_embed(&message);
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

fn start_empty_room_monitor(
    guild_players: Arc<
        DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>>,
    >,
    http: Arc<serenity::Http>,
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
                {
                    // 3 hours
                    if !player.queue.is_empty()
                        || player.playback_status != crate::core::PlaybackStatus::Idle
                    {
                        to_clear.push((guild_id, player.announce_channel));
                    }
                }
            }

            for (guild_id, announce_channel) in to_clear {
                if let Some(player_lock) = guild_players.get(&guild_id) {
                    let mut player = player_lock.write().await;
                    player.reset();
                    // Notice we keep empty_since as is (or reset it to Some(now) implicitly?
                    // Actually, reset() clears empty_since, so we should set it back to maintain
                    // the empty state, otherwise it might trigger repeatedly or lose state.
                    // Wait, if we cleared the queue, it's empty, so we don't care if empty_since is reset.
                    // It will be re-evaluated next voice update, or we just leave it as Some(now) to prevent re-clearing.
                    player.empty_since = Some(now);

                    crate::audio::runtime::cleanup_guild(guild_id.get());
                    info!(guild_id = %guild_id, "Cleared queue after 3 hours of empty room");

                    if let Some(channel) = announce_channel {
                        let embed = serenity::CreateEmbed::new()
                            .description("Đã 3 tiếng không có ai trong phòng, hàng chờ (queue) đã tự động được dọn dẹp để tiết kiệm tài nguyên.")
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
) -> Result<(), crate::utils::Error> {
    let guild_id = match new.guild_id {
        Some(g) => g,
        None => return Ok(()),
    };

    // 1. Get the guild player. If there is no player, we don't care.
    let player_lock = match data.guild_players.get(&guild_id) {
        Some(p) => p.value().clone(),
        None => return Ok(()),
    };

    let mut player = player_lock.write().await;

    // 2. We track empty_since regardless of playback status,
    // so we don't return early here anymore.

    // 3. Get the bot's current voice channel
    let bot_channel_id = match player.voice_channel {
        Some(c) => c,
        None => {
            // If the bot has left the voice channel and queue is empty, remove player memory
            if player.queue.is_empty() && player.playback_status == crate::core::PlaybackStatus::Idle {
                drop(player);
                data.guild_players.remove(&guild_id);
                crate::audio::runtime::cleanup_guild(guild_id.get());
                tracing::info!(guild_id = %guild_id, "Bot is not in voice and queue is empty, removed GuildPlayer");
            }
            return Ok(());
        }
    };

    // 4. Get bot user ID
    let bot_id = ctx.cache.current_user().id;

    // 5. Count human members in the voice channel
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

    // 6. Update empty_since and auto-pause if playing
    if human_count == 0 {
        if player.empty_since.is_none() {
            player.empty_since = Some(std::time::Instant::now());
        }

        if player.playback_status == crate::core::PlaybackStatus::Playing
            && let Some(ref handle) = player.current_track_handle
        {
            let should_announce = if let Err(e) = handle.pause() {
                tracing::error!("Failed to auto-pause track in empty channel: {:?}", e);
                false
            } else {
                player.playback_status = crate::core::PlaybackStatus::Paused;
                info!(
                    guild_id = %guild_id,
                    channel_id = %bot_channel_id,
                    "Playback auto-paused because voice channel is empty"
                );
                true
            };

            let announce_channel = player.announce_channel;
            drop(player); // Release the write lock before sending HTTP request

            if should_announce {
                if let Some(ch) = announce_channel {
                    let embed = serenity::CreateEmbed::new()
                            .description("Không có ai trong room nên âm nhạc sẽ tạm dừng `s.resume` để tiếp tục từ chỗ đã stop")
                            .color(0x5865F2);
                    let _ = ch
                        .send_message(&ctx.http, serenity::CreateMessage::new().embed(embed))
                        .await;
                }
            }
        }
    } else {
        player.empty_since = None;
    }

    Ok(())
}
