use poise::serenity_prelude as serenity;

use crate::audio::{TrackEndHandler, resolve_input};
use crate::core::{GuildPlayer, PlaybackStatus};
use crate::utils::{Context, Error, SerenyaError};
use songbird::input::YoutubeDl;

/// Play a song or playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn play(
    ctx: Context<'_>,
    #[autocomplete = "crate::commands::playlist::autocomplete_playlist"]
    #[description = "Search query, URL, or playlist name"]
    query: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    // 1. Check if user is in a voice channel
    let user_channel_id = {
        let guild = ctx
            .guild()
            .ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?;
        guild
            .voice_states
            .get(&ctx.author().id)
            .and_then(|state| state.channel_id)
            .ok_or_else(|| {
                SerenyaError::Voice("You must be in a voice channel to use this command.".into())
            })?
    };

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    // 2. Auto-join voice channel if not already connected
    let call_lock: std::sync::Arc<tokio::sync::Mutex<songbird::Call>> =
        if let Some(call) = manager.get(guild_id) {
            call
        } else {
            manager
                .join(guild_id, user_channel_id)
                .await
                .map_err(|e| SerenyaError::Voice(format!("Failed to join voice channel: {}", e)))?
        };

    // 3. Defer response because metadata lookup can take time
    ctx.defer().await?;

    // 4. Resolve input tracks
    let user_id = ctx.author().id.get();
    let mut tracks = resolve_input(
        &query,
        user_id,
        &ctx.data().database,
        &ctx.data().http_client,
    )
    .await?;

    if tracks.is_empty() {
        ctx.say("No tracks found for the query.").await?;
        return Ok(());
    }

    // 5. Get/Create guild player
    let player_lock = ctx
        .data()
        .guild_players
        .entry(guild_id)
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::RwLock::new(GuildPlayer::new())))
        .clone();

    let mut player = player_lock.write().await;
    player.voice_channel = Some(user_channel_id);
    player.announce_channel = Some(ctx.channel_id());

    // 6. Add to queue or play immediately
    let config = &ctx.data().config;
    let max_queue_size = config.playback.max_queue_size;

    if player.playback_status == PlaybackStatus::Idle && player.now_playing.is_none() {
        // Play first track immediately
        let mut first_track = tracks.remove(0);
        first_track.requester_name = ctx.author().name.clone();

        player.now_playing = Some(first_track.clone());
        player.playback_status = PlaybackStatus::Playing;

        let mut call = call_lock.lock().await;
        let source: songbird::input::Input =
            YoutubeDl::new(ctx.data().http_client.clone(), first_track.url.clone()).into();
        let handle = call.play_input(source);

        // Register event handler
        let _ = handle.add_event(
            songbird::Event::Track(songbird::TrackEvent::End),
            TrackEndHandler {
                guild_id,
                database: ctx.data().database.clone(),
                guild_players: ctx.data().guild_players.clone(),
                http_client: ctx.data().http_client.clone(),
                serenity_ctx: ctx.serenity_context().clone(),
            },
        );

        player.current_track_handle = Some(handle);

        // Queue remaining tracks (if any)
        let added = player.queue.push_batch(tracks, max_queue_size)?;

        if added > 0 {
            ctx.say(format!(
                "🎶 **Now Playing:** {}\nEnqueued {} other tracks.",
                first_track.title, added
            ))
            .await?;
        } else {
            ctx.say(format!("🎶 **Now Playing:** {}", first_track.title))
                .await?;
        }
    } else {
        // Enqueue all tracks
        let track_count = tracks.len();
        let first_title = tracks.first().map(|t| t.title.clone()).unwrap_or_default();

        // Populate requester names
        for t in &mut tracks {
            t.requester_name = ctx.author().name.clone();
        }

        let added = player.queue.push_batch(tracks, max_queue_size)?;

        if added == 0 {
            ctx.say("Queue is full! Could not add any tracks.").await?;
        } else if added == 1 && track_count == 1 {
            ctx.say(format!("📝 **Enqueued:** {}", first_title)).await?;
        } else {
            ctx.say(format!("📝 **Enqueued {} tracks**", added)).await?;
        }
    }

    Ok(())
}

/// Pause the currently playing song.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn pause(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let mut player = player_lock.write().await;
    if player.playback_status != PlaybackStatus::Playing {
        ctx.say("Playback is not currently active.").await?;
        return Ok(());
    }

    if let Some(ref handle) = player.current_track_handle {
        handle
            .pause()
            .map_err(|e| SerenyaError::Audio(format!("Failed to pause track: {}", e)))?;
        player.playback_status = PlaybackStatus::Paused;
        ctx.say("⏸️ Paused playback.").await?;
    } else {
        ctx.say("No track is currently playing.").await?;
    }

    Ok(())
}

/// Resume paused playback.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn resume(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let mut player = player_lock.write().await;
    if player.playback_status != PlaybackStatus::Paused {
        ctx.say("Playback is not currently paused.").await?;
        return Ok(());
    }

    if let Some(ref handle) = player.current_track_handle {
        handle
            .play()
            .map_err(|e| SerenyaError::Audio(format!("Failed to resume track: {}", e)))?;
        player.playback_status = PlaybackStatus::Playing;
        ctx.say("▶️ Resumed playback.").await?;
    } else {
        ctx.say("No track is currently paused.").await?;
    }

    Ok(())
}

/// Stop playback and clear the queue.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let mut player = player_lock.write().await;

    // Reset player state (clears queue and stops the active track)
    let vc = player.voice_channel;
    let ac = player.announce_channel;

    player.reset();

    player.voice_channel = vc;
    player.announce_channel = ac;
    player.playback_status = PlaybackStatus::Stopped;

    ctx.say("⏹️ Stopped playback and cleared the queue.")
        .await?;
    Ok(())
}

/// Helper to count VC users and perform vote skip logic.
async fn process_vote_skip(
    ctx: Context<'_>,
    player: &mut GuildPlayer,
    guild: &serenity::Guild,
) -> Result<bool, Error> {
    let vc_channel_id = player
        .voice_channel
        .ok_or_else(|| SerenyaError::Voice("Bot is not in a voice channel.".into()))?;

    let mut human_count: usize = 0;
    for state in guild.voice_states.values() {
        if state.channel_id == Some(vc_channel_id) {
            let is_bot = ctx
                .cache()
                .user(state.user_id)
                .map(|u| u.bot)
                .unwrap_or(false);
            if !is_bot {
                human_count += 1;
            }
        }
    }

    let required_votes = human_count.div_ceil(2).max(1);
    player.skip_votes.insert(ctx.author().id);
    let current_votes = player.skip_votes.len();

    if current_votes >= required_votes {
        Ok(true)
    } else {
        ctx.say(format!(
            "📥 Vote skip recorded! ({} / {} votes needed)",
            current_votes, required_votes
        ))
        .await?;
        Ok(false)
    }
}

/// Helper to handle requester absence checks and skip timers.
async fn check_requester_absence(
    ctx: Context<'_>,
    player: &mut GuildPlayer,
    track_requester_id: Option<serenity::UserId>,
    guild: &serenity::Guild,
) -> Result<bool, Error> {
    let requester_in_vc = if let Some(req_id) = track_requester_id {
        if let Some(user_state) = guild.voice_states.get(&req_id) {
            user_state.channel_id == player.voice_channel
        } else {
            false
        }
    } else {
        false
    };

    if !requester_in_vc {
        if let Some(timer) = player.requester_absence_timer {
            if timer.elapsed().as_secs() > 60 {
                Ok(true)
            } else {
                let remaining = 60 - timer.elapsed().as_secs();
                ctx.say(format!(
                    "The original requester is not in the VC. Skip will unlock for everyone in {}s.",
                    remaining
                ))
                .await?;
                Ok(false)
            }
        } else {
            player.requester_absence_timer = Some(std::time::Instant::now());
            ctx.say(
                "The original requester is not in the VC. A 60-second skip timer has been started.",
            )
            .await?;
            Ok(false)
        }
    } else {
        process_vote_skip(ctx, player, guild).await
    }
}

/// Skip the current track.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn skip(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let mut player = player_lock.write().await;
    if player.now_playing.is_none() {
        ctx.say("Nothing is currently playing.").await?;
        return Ok(());
    }

    let author_id = ctx.author().id;
    let owner_id = ctx.data().config.bot.owner;
    let track_requester_id = player.now_playing.as_ref().map(|t| t.requester_id);

    let can_skip = author_id.get() == owner_id || Some(author_id) == track_requester_id;
    let approved = if can_skip {
        true
    } else {
        let guild = ctx
            .guild()
            .ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?
            .clone();
        check_requester_absence(ctx, &mut player, track_requester_id, &guild).await?
    };

    if approved {
        ctx.say("⏭️ Skipping track...").await?;
        if let Some(ref handle) = player.current_track_handle {
            let _ = handle.stop();
        } else {
            drop(player);
            crate::audio::events::play_next(
                guild_id,
                &ctx.data().database,
                &ctx.data().guild_players,
                &ctx.data().http_client,
                ctx.serenity_context(),
            )
            .await?;
        }
    }

    Ok(())
}
