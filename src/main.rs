mod audio;
mod commands;
mod config;
mod core;
mod database;
mod discord;
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
    pub config: Arc<BotConfig>,
    pub database: Arc<DatabaseManager>,
    pub guild_players: DashMap<serenity::GuildId, ()>,
    pub http_client: reqwest::Client,
    pub start_time: std::time::Instant,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("Fatal error: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), utils::Error> {
    init_tracing();
    info!("Starting Serenya...");

    let config = Arc::new(config::load_config("config.yml")?);
    info!(instance_id = %config.bot.instance_id, "Configuration loaded");

    let database = Arc::new(DatabaseManager::load("database.yml").await?);
    info!("Database loaded");

    let cancel_token = CancellationToken::new();
    let auto_save_handle =
        database.start_auto_save(std::time::Duration::from_secs(30), cancel_token.clone());

    let http_client = reqwest::Client::new();
    let start_time = std::time::Instant::now();

    let config_clone = Arc::clone(&config);
    let database_clone = Arc::clone(&database);

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all_commands(),
            prefix_options: poise::PrefixFrameworkOptions {
                prefix: Some(config.bot.prefix.clone()),
                ..Default::default()
            },
            on_error: |error| Box::pin(on_error(error)),
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                info!("Slash commands registered globally");

                Ok(Data {
                    config: config_clone,
                    database: database_clone,
                    guild_players: DashMap::new(),
                    http_client,
                    start_time,
                })
            })
        })
        .build();

    let intents = serenity::GatewayIntents::non_privileged()
        | serenity::GatewayIntents::MESSAGE_CONTENT
        | serenity::GatewayIntents::GUILD_VOICE_STATES;

    let mut client = serenity::ClientBuilder::new(&config.bot.token, intents)
        .framework(framework)
        .register_songbird()
        .await?;

    info!(
        display_name = %config.bot.display_name,
        "Serenya is ready"
    );

    tokio::select! {
        result = client.start() => {
            if let Err(err) = result {
                error!(%err, "Client exited with error");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received (ctrl+c)");
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

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,serenya=debug,serenity=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

async fn on_error(error: poise::FrameworkError<'_, Data, utils::Error>) {
    match error {
        poise::FrameworkError::Command { error, ctx, .. } => {
            error!(%error, command = ctx.command().name, "Command error");
            let _ = ctx.say(format!("❌ An error occurred: {error}")).await;
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
