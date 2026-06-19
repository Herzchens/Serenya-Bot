use async_trait::async_trait;
use poise::serenity_prelude as serenity;
use songbird::input::YoutubeDl;
use songbird::{Event, EventContext, EventHandler};

use crate::database::DatabaseManager;
use crate::utils::SerenyaError;

pub struct TrackEndHandler {
    pub guild_id: serenity::GuildId,
    pub database: std::sync::Arc<DatabaseManager>,
    pub guild_players: std::sync::Arc<
        dashmap::DashMap<
            serenity::GuildId,
            std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
        >,
    >,
    pub http_client: reqwest::Client,
    pub serenity_ctx: serenity::Context,
}

#[async_trait]
impl EventHandler for TrackEndHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(_) = ctx {
            let guild_id = self.guild_id;
            let database = self.database.clone();
            let guild_players = self.guild_players.clone();
            let http_client = self.http_client.clone();
            let serenity_ctx = self.serenity_ctx.clone();

            tokio::spawn(async move {
                if let Err(e) = play_next(
                    guild_id,
                    &database,
                    &guild_players,
                    &http_client,
                    &serenity_ctx,
                )
                .await
                {
                    tracing::error!("Error in play_next during event handling: {:?}", e);
                }
            });
        }
        None
    }
}

pub async fn play_next(
    guild_id: serenity::GuildId,
    database: &std::sync::Arc<DatabaseManager>,
    guild_players: &std::sync::Arc<
        dashmap::DashMap<
            serenity::GuildId,
            std::sync::Arc<tokio::sync::RwLock<crate::core::GuildPlayer>>,
        >,
    >,
    http_client: &reqwest::Client,
    serenity_ctx: &serenity::Context,
) -> Result<(), SerenyaError> {
    // 1. Get the guild player lock
    let player_lock = guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("Guild player not found".into()))?;

    // 2. Get songbird call manager
    let songbird_manager = songbird::get(serenity_ctx)
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized".into()))?
        .clone();

    let call_lock = songbird_manager
        .get(guild_id)
        .ok_or_else(|| SerenyaError::Voice("Not connected to a voice channel".into()))?;

    // 3. Advance the queue
    let mut player = player_lock.write().await;
    player.advance_queue();

    // 4. Play next track or announce queue end
    if let Some(track) = player.now_playing.clone() {
        let mut call = call_lock.lock().await;

        let source = YoutubeDl::new(http_client.clone(), track.url.clone());
        let handle = call.play_input(source.into());

        // Register end handler
        let _ = handle.add_event(
            songbird::Event::Track(songbird::TrackEvent::End),
            TrackEndHandler {
                guild_id,
                database: database.clone(),
                guild_players: guild_players.clone(),
                http_client: http_client.clone(),
                serenity_ctx: serenity_ctx.clone(),
            },
        );

        player.current_track_handle = Some(handle);
        player.playback_status = crate::core::PlaybackStatus::Playing;

        // Announce track if enabled in database
        let announce_channel = player.announce_channel;
        let announce_setting = database
            .get_guild_settings(guild_id.get())
            .await
            .announce_track;

        if announce_setting {
            if let Some(channel) = announce_channel {
                let track_title = track.title.clone();
                let ctx_clone = serenity_ctx.clone();
                tokio::spawn(async move {
                    let _ = channel
                        .say(
                            &ctx_clone.http,
                            format!("🎶 **Now Playing:** {}", track_title),
                        )
                        .await;
                });
            }
        }
    } else {
        // No more tracks, stop player
        let mut call = call_lock.lock().await;
        call.stop();
        player.current_track_handle = None;
        player.playback_status = crate::core::PlaybackStatus::Idle;

        let announce_channel = player.announce_channel;
        let announce_setting = database
            .get_guild_settings(guild_id.get())
            .await
            .announce_track;

        if announce_setting {
            if let Some(channel) = announce_channel {
                let ctx_clone = serenity_ctx.clone();
                tokio::spawn(async move {
                    let _ = channel.say(&ctx_clone.http, "Queue finished. ⏹️").await;
                });
            }
        }
    }

    Ok(())
}
