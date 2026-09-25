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

#[cfg(not(panic = "unwind"))]
compile_error!("Serenya requires panic=unwind so resolver task panics remain containable");
use tracing::{error, info};

use crate::config::BotConfig;
use crate::database::DatabaseManager;

/// Shared application state accessible from all command handlers.
pub struct Data {
    pub config: Arc<arc_swap::ArcSwap<BotConfig>>,
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

fn normalize_client_start_result<E: std::fmt::Display>(result: Result<(), E>) -> Result<(), E> {
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            error!(%err, "Client exited with error");
            Err(err)
        }
    }
}

fn normalize_shutdown_signal_result<E: std::fmt::Display>(result: Result<(), E>) -> Result<(), E> {
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            error!(%err, "Shutdown signal listener failed");
            Err(err)
        }
    }
}

#[cfg(unix)]
fn normalize_sigterm_registration<T, E>(result: Result<T, E>) -> Result<T, E> {
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GatewayStopOrigin {
    ClientCompletion,
    ExternalControl,
}

async fn quiesce_gateway_for_stop<F, Fut>(origin: GatewayStopOrigin, quiesce: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    if origin == GatewayStopOrigin::ExternalControl {
        quiesce().await;
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    installer::ensure_dependencies().await;

    let config = Arc::new(config::load_config("config.yml").await?);
    let live_config = Arc::new(arc_swap::ArcSwap::new(config.clone()));

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

    let live_config_clone = Arc::clone(&live_config);
    let database_clone = Arc::clone(&database);
    let empty_room_monitor_handle = Arc::new(tokio::sync::Mutex::new(None));
    let empty_room_monitor_handle_for_setup = Arc::clone(&empty_room_monitor_handle);
    let empty_room_monitor_cancel = cancel_token.clone();

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
                            if let Some(player_lock) = data
                                .guild_players
                                .get(&guild_id)
                                .map(|entry| entry.value().clone())
                            {
                                audio::events::finalize_interrupted_playback_stats(
                                    data.database.as_ref(),
                                    guild_id,
                                    &player_lock,
                                )
                                .await;
                            }
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

                let monitor_handle = start_empty_room_monitor(
                    guild_players.clone(),
                    ctx.http.clone(),
                    live_config_clone.clone(),
                    Arc::clone(&database_clone),
                    ctx.clone(),
                    empty_room_monitor_cancel.clone(),
                );
                *empty_room_monitor_handle_for_setup.lock().await = Some(monitor_handle);

                Ok(Data {
                    config: live_config_clone,
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
        let mut signal = normalize_sigterm_registration(tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ))?;
        signal.recv().await;
        Ok::<(), std::io::Error>(())
    };
    #[cfg(not(unix))]
    let sigterm_future = std::future::pending::<Result<(), std::io::Error>>();

    let (run_error, gateway_stop_origin): (Option<Box<dyn std::error::Error>>, GatewayStopOrigin) = tokio::select! {
        result = client.start() => {
            (
                normalize_client_start_result(result)
                    .err()
                    .map(|err| Box::new(err) as Box<dyn std::error::Error>),
                GatewayStopOrigin::ClientCompletion,
            )
        }
        result = tokio::signal::ctrl_c() => {
            match normalize_shutdown_signal_result(result) {
                Ok(()) => {
                    info!(target: "shutdown", "Shutdown signal received (ctrl+c)");
                    (None, GatewayStopOrigin::ExternalControl)
                }
                Err(err) => (Some(Box::new(err)), GatewayStopOrigin::ExternalControl),
            }
        }
        result = sigterm_future => {
            match result {
                Ok(()) => {
                    info!(target: "shutdown", "Shutdown signal received (SIGTERM)");
                    (None, GatewayStopOrigin::ExternalControl)
                }
                Err(err) => {
                    error!(%err, "SIGTERM signal listener failed");
                    (Some(Box::new(err)), GatewayStopOrigin::ExternalControl)
                }
            }
        }
    };

    let shard_manager = client.shard_manager.clone();
    quiesce_gateway_for_stop(gateway_stop_origin, move || async move {
        shard_manager.shutdown_all().await;
    })
    .await;

    // Always flush database/webhook state first, then surface any unexpected
    // client, signal-listener, or final persistence failure to the process.
    let shutdown_result = shutdown(
        cancel_token,
        auto_save_handle,
        &database,
        empty_room_monitor_handle,
    )
    .await;
    if let Some(err) = run_error {
        if let Err(shutdown_err) = &shutdown_result {
            error!(%shutdown_err, "Final database save also failed while handling a prior runtime error");
        }
        return Err(err);
    }
    shutdown_result?;
    Ok(())
}

fn normalize_final_database_shutdown_result<E: std::fmt::Display>(
    result: Result<(), E>,
) -> Result<(), E> {
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            error!(%err, "Failed to save database during shutdown");
            Err(err)
        }
    }
}

async fn synchronize_empty_room_monitor(handle: tokio::task::JoinHandle<()>) {
    if let Err(err) = handle.await {
        error!(%err, "Empty-room monitor task panicked during shutdown");
    }
}

async fn shutdown(
    cancel_token: CancellationToken,
    auto_save_handle: tokio::task::JoinHandle<()>,
    database: &DatabaseManager,
    empty_room_monitor_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
) -> Result<(), utils::error::SerenyaError> {
    info!(target: "shutdown", "Initiating graceful shutdown...");

    cancel_token.cancel();

    if let Some(handle) = empty_room_monitor_handle.lock().await.take() {
        synchronize_empty_room_monitor(handle).await;
    }

    if let Err(err) = auto_save_handle.await {
        error!(%err, "Auto-save task panicked during shutdown");
    }

    let database_result = normalize_final_database_shutdown_result(database.shutdown().await);
    if database_result.is_ok() {
        info!(target: "shutdown", "Serenya shut down gracefully");
    }

    // Flush observability regardless of whether the final database save succeeded.
    logging::webhook::shutdown().await;
    database_result
}

/// Prepends the dependency directory to PATH before auto-install runs.
fn configure_path() {
    installer::configure_dependency_path();
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
            let error_class = error
                .downcast_ref::<utils::error::SerenyaError>()
                .map(utils::error::SerenyaError::class)
                .unwrap_or("Other");
            error!(%error, command = ctx.command().name, error_class, "Command error");
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

fn should_cleanup_empty_room(
    empty_since: Option<std::time::Instant>,
    now: std::time::Instant,
    _stay_in_voice: bool,
    queue_is_empty: bool,
    playback_status: core::PlaybackStatus,
) -> bool {
    let Some(empty_since) = empty_since else {
        return false;
    };
    if now.duration_since(empty_since) < std::time::Duration::from_secs(3 * 60 * 60) {
        return false;
    }
    !queue_is_empty || playback_status != core::PlaybackStatus::Idle
}

async fn empty_room_disconnect_then_cleanup<D, DFut, C, CFut, T, E>(
    _stay_in_voice: bool,
    _disconnect: D,
    cleanup: C,
) -> Result<T, E>
where
    D: FnOnce() -> DFut,
    DFut: std::future::Future<Output = Result<(), E>>,
    C: FnOnce() -> CFut,
    CFut: std::future::Future<Output = T>,
{
    Ok(cleanup().await)
}

fn sticky_voice_policy(_configured_stay_in_voice: bool) -> bool {
    true
}

fn start_empty_room_monitor(
    guild_players: Arc<DashMap<serenity::GuildId, Arc<tokio::sync::RwLock<core::GuildPlayer>>>>,
    http: Arc<serenity::Http>,
    config: Arc<arc_swap::ArcSwap<BotConfig>>,
    database: Arc<DatabaseManager>,
    _serenity_ctx: serenity::Context,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => break,
                _ = interval.tick() => {}
            }
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
                let stay = sticky_voice_policy(config.load().playback.stay_in_voice);
                if should_cleanup_empty_room(
                    player.empty_since,
                    now,
                    stay,
                    player.queue.is_empty(),
                    player.playback_status,
                ) {
                    to_clear.push(guild_id);
                }
            }

            for guild_id in to_clear {
                let player_lock_opt = guild_players.get(&guild_id).map(|p| p.value().clone());
                if let Some(player_lock) = player_lock_opt {
                    let (handle_uuid, still_due) = {
                        let player = player_lock.read().await;
                        let stay = sticky_voice_policy(config.load().playback.stay_in_voice);
                        (
                            player
                                .current_track_handle
                                .as_ref()
                                .map(|handle| handle.uuid()),
                            should_cleanup_empty_room(
                                player.empty_since,
                                std::time::Instant::now(),
                                stay,
                                player.queue.is_empty(),
                                player.playback_status,
                            ),
                        )
                    };
                    if !still_due {
                        continue;
                    }

                    let interrupted_play_time = if let Some(handle_uuid) = handle_uuid {
                        audio::events::interrupted_play_time_for_handle(&player_lock, handle_uuid)
                            .await
                    } else {
                        None
                    };

                    let stay = sticky_voice_policy(config.load().playback.stay_in_voice);
                    let still_due_before_disconnect = {
                        let player = player_lock.read().await;
                        should_cleanup_empty_room(
                            player.empty_since,
                            std::time::Instant::now(),
                            stay,
                            player.queue.is_empty(),
                            player.playback_status,
                        ) && player
                            .current_track_handle
                            .as_ref()
                            .map(|handle| handle.uuid())
                            == handle_uuid
                    };
                    if !still_due_before_disconnect {
                        continue;
                    }

                    let cleanup_player = Arc::clone(&player_lock);
                    let cleanup_players = Arc::clone(&guild_players);
                    let cleanup_database = Arc::clone(&database);
                    let cleanup_http = Arc::clone(&http);
                    let result = empty_room_disconnect_then_cleanup(
                        stay,
                        || async { Ok::<(), String>(()) },
                        move || async move {
                            let (announce_channel, record_play_time) = {
                                let mut player = cleanup_player.write().await;
                                if !should_cleanup_empty_room(
                                    player.empty_since,
                                    std::time::Instant::now(),
                                    stay,
                                    player.queue.is_empty(),
                                    player.playback_status,
                                ) || player
                                    .current_track_handle
                                    .as_ref()
                                    .map(|handle| handle.uuid())
                                    != handle_uuid
                                {
                                    return None;
                                }

                                let record_play_time = match (handle_uuid, interrupted_play_time) {
                                    (Some(handle_uuid), Some(play_time))
                                        if player.failure_state.claim_terminal(handle_uuid) =>
                                    {
                                        Some(play_time)
                                    }
                                    _ => None,
                                };
                                let announce_channel = player.announce_channel;
                                player.reset();
                                if !stay {
                                    player.voice_channel = None;
                                    player.announce_channel = None;
                                } else {
                                    player.empty_since = Some(std::time::Instant::now());
                                }
                                (announce_channel, record_play_time)
                            };

                            if let Some(play_time) = record_play_time {
                                audio::events::record_guild_playback_stats(
                                    cleanup_database.as_ref(),
                                    guild_id,
                                    play_time,
                                    false,
                                )
                                .await;
                            }

                            if stay {
                                audio::runtime::cleanup_guild(guild_id.get());
                            } else {
                                cleanup_players.remove(&guild_id);
                                audio::runtime::cleanup_guild(guild_id.get());
                            }

                            Some(announce_channel)
                        },
                    )
                    .await;

                    match result {
                        Ok(Some(announce_channel)) => {
                            if stay {
                                info!(guild_id = %guild_id, "Cleared queue after 3 hours of empty room (staying in voice)");
                            } else {
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
                                    .send_message(
                                        &cleanup_http,
                                        serenity::CreateMessage::new().embed(embed),
                                    )
                                    .await;
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::warn!(
                                guild_id = %guild_id,
                                error = %err,
                                "Empty-room voice disconnect failed"
                            );
                        }
                    }
                }
            }
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BotVoiceUpdate {
    Unchanged,
    Connected(serenity::ChannelId),
    Disconnected,
}

fn classify_bot_voice_update(
    event_user: serenity::UserId,
    bot_user: serenity::UserId,
    channel_id: Option<serenity::ChannelId>,
) -> BotVoiceUpdate {
    if event_user != bot_user {
        BotVoiceUpdate::Unchanged
    } else if let Some(channel_id) = channel_id {
        BotVoiceUpdate::Connected(channel_id)
    } else {
        BotVoiceUpdate::Disconnected
    }
}

fn bot_voice_update_is_current(player: &core::GuildPlayer, expected_generation: u64) -> bool {
    player.bot_voice_generation == expected_generation
}

fn apply_bot_voice_update_state(player: &mut core::GuildPlayer, update: BotVoiceUpdate) -> bool {
    match update {
        BotVoiceUpdate::Unchanged => false,
        BotVoiceUpdate::Connected(channel_id) => {
            player.bot_voice_generation = player.bot_voice_generation.wrapping_add(1);
            player.voice_channel = Some(channel_id);
            player.empty_since = None;
            false
        }
        BotVoiceUpdate::Disconnected => {
            let should_recover = player.voice_channel.is_some();
            player.bot_voice_generation = player.bot_voice_generation.wrapping_add(1);
            should_recover
        }
    }
}

fn voice_room_is_empty(cached_human_count: Option<usize>) -> Option<bool> {
    cached_human_count.map(|human_count| human_count == 0)
}

fn voice_recovery_delay(attempt: u32) -> std::time::Duration {
    let seconds = match attempt {
        0 => 1,
        1 => 2,
        2 => 5,
        3 => 10,
        _ => 30,
    };
    std::time::Duration::from_secs(seconds)
}

fn voice_recovery_is_current(
    player: &core::GuildPlayer,
    expected_generation: u64,
    desired_channel: serenity::ChannelId,
) -> bool {
    player.bot_voice_generation == expected_generation
        && player.voice_channel == Some(desired_channel)
}

fn schedule_voice_recovery(
    ctx: serenity::Context,
    guild_id: serenity::GuildId,
    desired_channel: serenity::ChannelId,
    player_lock: Arc<tokio::sync::RwLock<core::GuildPlayer>>,
    expected_generation: u64,
) {
    tokio::spawn(async move {
        let mut attempt = 0u32;
        loop {
            tokio::time::sleep(voice_recovery_delay(attempt)).await;
            let still_current = {
                let player = player_lock.read().await;
                voice_recovery_is_current(&player, expected_generation, desired_channel)
            };
            if !still_current {
                return;
            }

            let Some(manager) = songbird::get(&ctx).await else {
                tracing::warn!(guild_id = %guild_id, "Songbird manager unavailable during sticky voice recovery");
                attempt = attempt.saturating_add(1);
                continue;
            };

            if let Some(call_lock) = manager.get(guild_id) {
                let current_channel = {
                    let call = call_lock.lock().await;
                    call.current_channel().map(|channel| channel.0.get())
                };
                if current_channel == Some(desired_channel.get()) {
                    return;
                }
            }

            match manager.join(guild_id, desired_channel).await {
                Ok(_) => {
                    info!(guild_id = %guild_id, "Recovered unexpected voice disconnect");
                    return;
                }
                Err(err) => {
                    tracing::warn!(
                        guild_id = %guild_id,
                        error = %err,
                        attempt,
                        "Sticky voice recovery join failed; retrying"
                    );
                }
            }
            attempt = attempt.saturating_add(1);
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

    let bot_id = ctx.cache.current_user().id;
    let bot_update = classify_bot_voice_update(new.user_id, bot_id, new.channel_id);
    let player_lock = match data.guild_players.get(&guild_id) {
        Some(p) => p.value().clone(),
        None if matches!(bot_update, BotVoiceUpdate::Connected(_)) => data
            .guild_players
            .entry(guild_id)
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(core::GuildPlayer::new())))
            .clone(),
        None => return Ok(()),
    };
    if bot_update != BotVoiceUpdate::Unchanged {
        let (expected_generation, interrupted_handle_uuid) = {
            let player = player_lock.read().await;
            (
                player.bot_voice_generation,
                player
                    .current_track_handle
                    .as_ref()
                    .map(|handle| handle.uuid()),
            )
        };
        let interrupted_play_time: Option<std::time::Duration> = None;

        let state_change = {
            let mut player = player_lock.write().await;
            if bot_update == BotVoiceUpdate::Disconnected
                && !bot_voice_update_is_current(&player, expected_generation)
            {
                None
            } else {
                let record_play_time = match (interrupted_handle_uuid, interrupted_play_time) {
                    (Some(handle_uuid), Some(play_time))
                        if player
                            .current_track_handle
                            .as_ref()
                            .map(|handle| handle.uuid())
                            == Some(handle_uuid)
                            && player.failure_state.claim_terminal(handle_uuid) =>
                    {
                        Some(play_time)
                    }
                    _ => None,
                };
                let previous_channel = player.voice_channel;
                let disconnected = apply_bot_voice_update_state(&mut player, bot_update);
                Some((
                    previous_channel,
                    disconnected,
                    record_play_time,
                    player.bot_voice_generation,
                ))
            }
        };

        let Some((previous_channel, disconnected, record_play_time, applied_generation)) =
            state_change
        else {
            tracing::debug!(
                guild_id = %guild_id,
                "Ignored stale bot voice disconnect after a newer voice update"
            );
            return Ok(());
        };

        if let Some(play_time) = record_play_time {
            audio::events::record_guild_playback_stats(
                data.database.as_ref(),
                guild_id,
                play_time,
                false,
            )
            .await;
        }

        if disconnected {
            if let Some(desired_channel) = previous_channel {
                info!(
                    guild_id = %guild_id,
                    "Unexpected bot voice disconnect; preserving GuildPlayer and scheduling recovery"
                );
                schedule_voice_recovery(
                    ctx.clone(),
                    guild_id,
                    desired_channel,
                    player_lock.clone(),
                    applied_generation,
                );
            }
            return Ok(());
        }

        if let BotVoiceUpdate::Connected(channel_id) = bot_update {
            data.guild_players.insert(guild_id, player_lock.clone());
            if previous_channel != Some(channel_id) {
                info!(
                    guild_id = %guild_id,
                    previous_channel = ?previous_channel,
                    channel_id = %channel_id,
                    "Synchronized GuildPlayer after external bot voice-channel move"
                );
            }
        }
    }

    // Read necessary info first under read lock
    let (bot_channel_id, queue_is_empty, playback_status, has_empty_since) = {
        let player = player_lock.read().await;
        (
            player.voice_channel,
            player.queue.is_empty(),
            player.playback_status,
            player.empty_since.is_some(),
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

    // 5. Count human members in the voice channel (without holding lock)
    let cached_human_count = ctx.cache.guild(guild_id).map(|guild| {
        let mut human_count = 0;
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
        human_count
    });

    // 6. Update empty_since and auto-pause if playing (acquire write lock ONLY when needed)
    let room_is_empty = voice_room_is_empty(cached_human_count);
    if room_is_empty == Some(true) {
        let mut player = player_lock.write().await;
        if player.empty_since.is_none() {
            player.empty_since = Some(std::time::Instant::now());
        }

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
    } else if room_is_empty == Some(false) && has_empty_since {
        let mut player = player_lock.write().await;
        player.empty_since = None;
    }

    Ok(())
}

#[cfg(test)]
mod multiguild_tests {
    use super::{
        BotVoiceUpdate, apply_bot_voice_update_state, classify_bot_voice_update,
        should_cleanup_empty_room,
    };
    use crate::core::{GuildPlayer, PlaybackStatus};
    use poise::serenity_prelude::{ChannelId, UserId};
    use std::time::{Duration, Instant};

    #[test]
    fn external_bot_move_updates_player_channel_and_recomputes_empty_state() {
        let mut player = GuildPlayer::new();
        player.voice_channel = Some(ChannelId::new(11));
        player.empty_since = Some(Instant::now());
        let update =
            classify_bot_voice_update(UserId::new(9), UserId::new(9), Some(ChannelId::new(22)));
        assert_eq!(update, BotVoiceUpdate::Connected(ChannelId::new(22)));
        assert!(!apply_bot_voice_update_state(&mut player, update));
        assert_eq!(player.voice_channel, Some(ChannelId::new(22)));
        assert!(player.empty_since.is_none());
    }

    #[test]
    fn unexpected_bot_disconnect_preserves_player_and_requests_recovery() {
        let mut player = GuildPlayer::new();
        player.voice_channel = Some(ChannelId::new(11));
        player.announce_channel = Some(ChannelId::new(33));
        player.playback_status = PlaybackStatus::Paused;
        let update = classify_bot_voice_update(UserId::new(9), UserId::new(9), None);
        assert_eq!(update, BotVoiceUpdate::Disconnected);
        assert!(apply_bot_voice_update_state(&mut player, update));
        assert_eq!(player.voice_channel, Some(ChannelId::new(11)));
        assert_eq!(player.announce_channel, Some(ChannelId::new(33)));
        assert_eq!(player.playback_status, PlaybackStatus::Paused);
    }

    #[test]
    fn another_users_voice_event_does_not_mutate_bot_state() {
        let mut player = GuildPlayer::new();
        player.voice_channel = Some(ChannelId::new(11));
        let update =
            classify_bot_voice_update(UserId::new(8), UserId::new(9), Some(ChannelId::new(22)));
        assert_eq!(update, BotVoiceUpdate::Unchanged);
        assert!(!apply_bot_voice_update_state(&mut player, update));
        assert_eq!(player.voice_channel, Some(ChannelId::new(11)));
    }

    #[test]
    fn idle_empty_player_never_disconnects_when_legacy_stay_flag_is_disabled() {
        let now = Instant::now();
        assert!(!should_cleanup_empty_room(
            Some(now - Duration::from_secs(10800)),
            now,
            false,
            true,
            PlaybackStatus::Idle
        ));
    }

    #[test]
    fn idle_empty_player_stays_when_stay_is_enabled() {
        let now = Instant::now();
        assert!(!should_cleanup_empty_room(
            Some(now - Duration::from_secs(10800)),
            now,
            true,
            true,
            PlaybackStatus::Idle
        ));
    }

    #[test]
    fn active_state_is_cleaned_when_staying_connected() {
        let now = Instant::now();
        assert!(should_cleanup_empty_room(
            Some(now - Duration::from_secs(10800)),
            now,
            true,
            false,
            PlaybackStatus::Paused
        ));
    }

    #[test]
    fn empty_room_timeout_does_not_fire_early() {
        let now = Instant::now();
        assert!(!should_cleanup_empty_room(
            Some(now - Duration::from_secs(10799)),
            now,
            false,
            true,
            PlaybackStatus::Idle
        ));
    }
}

#[cfg(test)]
mod sticky_voice_recovery_tests {
    use super::{sticky_voice_policy, voice_recovery_delay, voice_recovery_is_current};
    use crate::core::GuildPlayer;
    use poise::serenity_prelude::ChannelId;
    use std::time::Duration;

    #[test]
    fn legacy_config_cannot_enable_automatic_voice_leave() {
        assert!(sticky_voice_policy(true));
        assert!(sticky_voice_policy(false));
    }

    #[test]
    fn recovery_backoff_is_bounded() {
        assert_eq!(voice_recovery_delay(0), Duration::from_secs(1));
        assert_eq!(voice_recovery_delay(1), Duration::from_secs(2));
        assert_eq!(voice_recovery_delay(2), Duration::from_secs(5));
        assert_eq!(voice_recovery_delay(3), Duration::from_secs(10));
        assert_eq!(voice_recovery_delay(99), Duration::from_secs(30));
    }

    #[test]
    fn newer_voice_generation_or_channel_move_cancels_stale_recovery() {
        let mut player = GuildPlayer::new();
        player.voice_channel = Some(ChannelId::new(10));
        let generation = player.bot_voice_generation;
        assert!(voice_recovery_is_current(
            &player,
            generation,
            ChannelId::new(10)
        ));
        player.bot_voice_generation = player.bot_voice_generation.wrapping_add(1);
        assert!(!voice_recovery_is_current(
            &player,
            generation,
            ChannelId::new(10)
        ));
        let generation = player.bot_voice_generation;
        player.voice_channel = Some(ChannelId::new(20));
        assert!(!voice_recovery_is_current(
            &player,
            generation,
            ChannelId::new(10)
        ));
    }
}

#[cfg(test)]
mod panic_strategy_tests {
    #[tokio::test]
    async fn spawned_task_panic_is_reported_as_join_error() {
        let result = tokio::spawn(async { panic!("intentional panic isolation test") }).await;
        let error = result.expect_err("task panic should become JoinError");
        assert!(error.is_panic());
    }
}

#[cfg(test)]
mod voice_lifecycle_generation_tests {
    use super::{
        BotVoiceUpdate, apply_bot_voice_update_state, bot_voice_update_is_current,
        should_cleanup_empty_room,
    };
    use crate::core::{GuildPlayer, PlaybackStatus};
    use poise::serenity_prelude as serenity;

    #[test]
    fn newer_connected_update_invalidates_stale_disconnect_generation() {
        let mut player = GuildPlayer::new();
        let stale_generation = player.bot_voice_generation;
        apply_bot_voice_update_state(
            &mut player,
            BotVoiceUpdate::Connected(serenity::ChannelId::new(77)),
        );
        assert!(!bot_voice_update_is_current(&player, stale_generation));
        assert_eq!(player.voice_channel, Some(serenity::ChannelId::new(77)));
    }

    #[test]
    fn listener_return_cancels_a_previously_due_empty_room_cleanup() {
        let now = std::time::Instant::now();
        let old = now - std::time::Duration::from_secs(3 * 60 * 60 + 1);
        assert!(should_cleanup_empty_room(
            Some(old),
            now,
            false,
            true,
            PlaybackStatus::Playing,
        ));
        assert!(!should_cleanup_empty_room(
            None,
            now,
            false,
            true,
            PlaybackStatus::Playing,
        ));
    }
}

#[cfg(test)]
mod voice_room_cache_tests {
    use super::voice_room_is_empty;

    #[test]
    fn missing_guild_cache_is_not_treated_as_empty_room() {
        assert_eq!(voice_room_is_empty(None), None);
    }

    #[test]
    fn cached_room_without_humans_is_empty() {
        assert_eq!(voice_room_is_empty(Some(0)), Some(true));
    }

    #[test]
    fn cached_room_with_humans_is_occupied() {
        assert_eq!(voice_room_is_empty(Some(2)), Some(false));
    }
}

#[cfg(test)]
mod client_exit_status_tests {
    use super::normalize_client_start_result;

    #[test]
    fn client_start_error_reaches_run_outcome() {
        let result = normalize_client_start_result::<&'static str>(Err("gateway failed"));
        assert_eq!(
            result,
            Err("gateway failed"),
            "an unexpected Discord client shutdown must not be converted into a successful process outcome"
        );
    }

    #[test]
    fn clean_client_completion_remains_successful() {
        assert_eq!(
            normalize_client_start_result::<&'static str>(Ok(())),
            Ok(())
        );
    }
}

#[cfg(test)]
mod shutdown_signal_error_tests {
    use super::normalize_shutdown_signal_result;

    #[test]
    fn ctrl_c_listener_error_reaches_run_outcome() {
        let result = normalize_shutdown_signal_result::<&'static str>(Err("listener failed"));
        assert_eq!(
            result,
            Err("listener failed"),
            "a ctrl_c listener error must not be mistaken for a graceful shutdown signal"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sigterm_registration_error_is_propagated_without_panicking() {
        let outcome = std::panic::catch_unwind(|| {
            super::normalize_sigterm_registration::<(), &'static str>(Err("registration failed"))
        });
        assert!(
            outcome.is_ok(),
            "SIGTERM listener registration failure must be returned, not panic"
        );
        assert_eq!(outcome.unwrap(), Err("registration failed"));
    }
}

#[cfg(test)]
mod final_database_shutdown_error_tests {
    use super::normalize_final_database_shutdown_result;

    #[test]
    fn final_database_save_error_reaches_process_outcome() {
        let result = normalize_final_database_shutdown_result::<&'static str>(Err("disk full"));
        assert_eq!(
            result,
            Err("disk full"),
            "a failed final database save must not be converted into a successful process shutdown"
        );
    }

    #[test]
    fn successful_final_database_save_remains_successful() {
        assert_eq!(
            normalize_final_database_shutdown_result::<&'static str>(Ok(())),
            Ok(())
        );
    }
}

#[cfg(test)]
mod empty_room_monitor_shutdown_tests {
    use super::{DatabaseManager, synchronize_empty_room_monitor};
    use std::sync::Arc;
    use std::time::Duration;

    fn temp_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "serenya-monitor-shutdown-{}-{}.yml",
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
    async fn monitor_write_cannot_land_after_final_database_save()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = temp_db_path();
        let manager = Arc::new(DatabaseManager::load(&path).await?);
        let writer = Arc::clone(&manager);

        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            let mut settings = writer.get_guild_settings(4242).await;
            settings.quality = "late-monitor-write".to_owned();
            writer.update_guild_settings(4242, settings).await;
        });

        synchronize_empty_room_monitor(handle).await;
        manager.shutdown().await?;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let reloaded = DatabaseManager::load(&path).await?;
        let persisted = reloaded.get_guild_settings(4242).await;
        assert_eq!(
            persisted.quality, "late-monitor-write",
            "shutdown must quiesce an in-flight monitor before the final database save"
        );

        cleanup(&path).await;
        Ok(())
    }
}

#[cfg(test)]
mod gateway_shutdown_order_tests {
    use super::{GatewayStopOrigin, quiesce_gateway_for_stop};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn external_control_stop_quiesces_gateway_before_persistence() {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        quiesce_gateway_for_stop(GatewayStopOrigin::ExternalControl, move || async move {
            callback_calls.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn completed_client_does_not_request_redundant_gateway_shutdown() {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        quiesce_gateway_for_stop(GatewayStopOrigin::ClientCompletion, move || async move {
            callback_calls.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod empty_room_disconnect_order_tests {
    use super::empty_room_disconnect_then_cleanup;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn empty_room_maintenance_never_calls_disconnect_even_with_legacy_flag_disabled() {
        let disconnected = Arc::new(AtomicBool::new(false));
        let cleaned = Arc::new(AtomicBool::new(false));
        let disconnect_flag = Arc::clone(&disconnected);
        let cleanup_flag = Arc::clone(&cleaned);
        let result = empty_room_disconnect_then_cleanup(
            false,
            move || async move {
                disconnect_flag.store(true, Ordering::SeqCst);
                Ok::<(), &'static str>(())
            },
            move || async move {
                cleanup_flag.store(true, Ordering::SeqCst);
            },
        )
        .await;
        assert_eq!(result, Ok(()));
        assert!(!disconnected.load(Ordering::SeqCst));
        assert!(cleaned.load(Ordering::SeqCst));
    }
}
