use poise::serenity_prelude as serenity;

use crate::audio::{ResolvedInput, resolve_input};
use crate::core::{GuildPlayer, PlaybackStatus, Track};
use crate::discord::embeds::QueueTrackSnapshot;
use crate::utils::{Context, Error, SerenyaError};

/// Play a song or playlist.
#[poise::command(slash_command, prefix_command, aliases("p"))]
pub async fn play(
    ctx: Context<'_>,
    #[autocomplete = "crate::commands::playlist::autocomplete_playlist"]
    #[description = "Search query, URL, or playlist name"]
    #[rest]
    query: String,
) -> Result<(), Error> {
    tracing::info!(query = %query, "Play invoked");
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    ctx.defer().await?;

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

    let user_id = ctx.author().id.get();
    let resolved = resolve_input(
        &query,
        user_id,
        &ctx.data().database,
        &ctx.data().http_client,
    )
    .await?;

    match resolved {
        ResolvedInput::Playlist(tracks) => {
            ensure_play_voice(ctx, guild_id, user_channel_id).await?;
            enqueue_and_play_resolved(ctx, guild_id, user_channel_id, tracks).await?;
        }
        ResolvedInput::Track(track) => {
            ensure_play_voice(ctx, guild_id, user_channel_id).await?;
            enqueue_and_play_resolved(ctx, guild_id, user_channel_id, vec![*track]).await?;
        }
        ResolvedInput::SearchResults(mut candidates) => {
            let select_menu = crate::commands::info::build_search_menu(ctx.id(), &candidates);
            let components = vec![serenity::CreateActionRow::SelectMenu(select_menu)];
            let reply = poise::CreateReply::default()
                .content("🔎 Select a track to play:")
                .components(components);

            let msg = ctx.send(reply).await?;
            let mut msg_inner = msg.into_message().await?;

            let collector = serenity::ComponentInteractionCollector::new(ctx.serenity_context())
                .author_id(ctx.author().id)
                .message_id(msg_inner.id)
                .timeout(std::time::Duration::from_secs(60));

            if let Some(interaction) = collector.next().await {
                let selected_idx_str = match &interaction.data.kind {
                    serenity::ComponentInteractionDataKind::StringSelect { values } => values
                        .first()
                        .ok_or_else(|| SerenyaError::Audio("No selection received.".into()))?,
                    _ => return Err(SerenyaError::Audio("Invalid interaction type.".into()).into()),
                };
                let selected_idx: usize = selected_idx_str
                    .parse()
                    .map_err(|_| SerenyaError::Audio("Invalid selection index.".into()))?;
                if selected_idx >= candidates.len() {
                    return Err(SerenyaError::Audio("Selection index out of range.".into()).into());
                }

                let selected_track = candidates.remove(selected_idx);
                interaction
                    .create_response(
                        &ctx.serenity_context().http,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .content("⏳ Resolving selected track...")
                                .components(vec![]),
                        ),
                    )
                    .await?;

                let tracks = if is_metadata_search_option(&selected_track) {
                    resolve_input(
                        &selected_track.url,
                        user_id,
                        &ctx.data().database,
                        &ctx.data().http_client,
                    )
                    .await?
                    .into_tracks_or_top()
                } else {
                    vec![selected_track]
                };

                ensure_play_voice(ctx, guild_id, user_channel_id).await?;
                enqueue_and_play_resolved(ctx, guild_id, user_channel_id, tracks).await?;
            } else {
                msg_inner
                    .edit(
                        &ctx.serenity_context().http,
                        serenity::EditMessage::new()
                            .content("⏱️ Play selection timed out.")
                            .components(vec![]),
                    )
                    .await?;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingVoiceAction {
    Join,
    Reuse,
    RejectDifferentChannel,
}

fn existing_voice_action(bot_channel: Option<u64>, user_channel: u64) -> ExistingVoiceAction {
    match bot_channel {
        None => ExistingVoiceAction::Join,
        Some(channel) if channel == user_channel => ExistingVoiceAction::Reuse,
        Some(_) => ExistingVoiceAction::RejectDifferentChannel,
    }
}

async fn join_then_configure_voice<J, JFut, C, CFut, E>(join: J, configure: C) -> Result<(), E>
where
    J: FnOnce() -> JFut,
    JFut: std::future::Future<Output = Result<(), E>>,
    C: FnOnce() -> CFut,
    CFut: std::future::Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    join().await?;
    if let Err(err) = configure().await {
        tracing::warn!(%err, "Failed to apply voice bitrate after successful join");
    }
    Ok(())
}

pub(crate) async fn ensure_play_voice(
    ctx: Context<'_>,
    guild_id: serenity::GuildId,
    user_channel_id: serenity::ChannelId,
) -> Result<(), Error> {
    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| SerenyaError::Voice("Songbird manager not initialized.".into()))?
        .clone();

    let existing_channel = if let Some(call_lock) = manager.get(guild_id) {
        let call = call_lock.lock().await;
        call.current_channel().map(|channel| channel.0.get())
    } else {
        None
    };

    match existing_voice_action(existing_channel, user_channel_id.get()) {
        ExistingVoiceAction::Reuse => Ok(()),
        ExistingVoiceAction::RejectDifferentChannel => Err(SerenyaError::Voice(
            "Bot is already connected to a different voice channel in this server. Use /join from your channel to move it explicitly."
                .into(),
        )
        .into()),
        ExistingVoiceAction::Join => {
            let join_manager = manager.clone();
            join_then_configure_voice(
                move || async move {
                    join_manager
                        .join(guild_id, user_channel_id)
                        .await
                        .map(|_| ())
                        .map_err(|err| -> Error {
                            SerenyaError::Voice(format!("Failed to join voice channel: {err}")).into()
                        })
                },
                move || async move {
                    crate::audio::quality::apply_bitrate(ctx, guild_id, user_channel_id).await
                },
            )
            .await
        }
    }
}

fn is_metadata_search_option(track: &Track) -> bool {
    track.source_provider.starts_with("Deezer")
        || track.source_provider.starts_with("Spotify")
        || track.source_provider.starts_with("Apple Music")
}

#[derive(Debug)]
struct EnqueuePreparation {
    start_playback: bool,
    added_to_queue: usize,
    first_track: Track,
}

fn prepare_enqueue(
    player: &mut GuildPlayer,
    mut tracks: Vec<Track>,
    max_queue_size: usize,
    requester_name: std::sync::Arc<str>,
) -> Result<EnqueuePreparation, SerenyaError> {
    if tracks.is_empty() {
        return Err(SerenyaError::Queue("No tracks found to enqueue.".into()));
    }

    for track in &mut tracks {
        track.requester_name = Some(requester_name.clone());
    }

    let can_start = player.now_playing.is_none()
        && matches!(
            player.playback_status,
            PlaybackStatus::Idle | PlaybackStatus::Stopped
        );

    if can_start {
        let first_track = tracks.remove(0);
        player.now_playing = Some(first_track.clone());
        player.current_track_handle = None;
        player.playback_status = PlaybackStatus::Idle;
        let added_to_queue = player.queue.push_batch(tracks, max_queue_size)?;
        Ok(EnqueuePreparation {
            start_playback: true,
            added_to_queue,
            first_track,
        })
    } else {
        let first_track = tracks[0].clone();
        let added_to_queue = player.queue.push_batch(tracks, max_queue_size)?;
        Ok(EnqueuePreparation {
            start_playback: false,
            added_to_queue,
            first_track,
        })
    }
}

pub(crate) async fn enqueue_and_play_resolved(
    ctx: Context<'_>,
    guild_id: serenity::GuildId,
    user_channel_id: serenity::ChannelId,
    tracks: Vec<Track>,
) -> Result<(), Error> {
    if tracks.is_empty() {
        ctx.say("No tracks found to play.").await?;
        return Ok(());
    }

    let requested_track_count = tracks.len();
    let show_queue_after_enqueue = requested_track_count > 1;
    let player_lock = ctx
        .data()
        .guild_players
        .entry(guild_id)
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::RwLock::new(GuildPlayer::new())))
        .clone();

    let preparation = {
        let mut player = player_lock.write().await;
        player.voice_channel = Some(user_channel_id);
        player.announce_channel = Some(ctx.channel_id());
        let requester_name: std::sync::Arc<str> = std::sync::Arc::from(ctx.author().name.as_str());
        prepare_enqueue(
            &mut player,
            tracks,
            ctx.data().config().playback.max_queue_size,
            requester_name,
        )?
    };

    if preparation.start_playback {
        let playback_ctx = crate::audio::events::PlaybackContext {
            guild_id,
            database: std::sync::Arc::clone(&ctx.data().database),
            guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
            http_client: ctx.data().http_client.clone(),
            serenity_ctx: ctx.serenity_context().clone(),
            config: std::sync::Arc::clone(&ctx.data().config),
        };
        let player_lock_for_error = player_lock.clone();
        tokio::spawn(async move {
            if let Err(err) = crate::audio::events::play_next(playback_ctx, None, false).await {
                tracing::error!(guild_id = %guild_id, %err, "Failed to start playback");
                let mut player = player_lock_for_error.write().await;
                if player.current_track_handle.is_none()
                    && player.playback_status == PlaybackStatus::Idle
                {
                    if let Some(mut failed) = player.now_playing.take() {
                        failed.resolved_url = None;
                        player.previous_track = Some(failed);
                    }
                    player.failure_state.reset();
                }
            }
        });
    } else if preparation.added_to_queue > 0 {
        let gp_clone = ctx.data().guild_players.clone();
        let http_client_clone = ctx.data().http_client.clone();
        tokio::spawn(async move {
            crate::audio::events::trigger_prefetch(guild_id, gp_clone, http_client_clone).await;
        });
    }

    if preparation.added_to_queue == 0 && !preparation.start_playback {
        let embed = crate::discord::embeds::error_embed("Queue is full! Could not add any tracks.");
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    if show_queue_after_enqueue {
        let queue_tracks = queue_snapshot(&player_lock).await;
        crate::discord::pagination::paginate_queue(ctx, &queue_tracks, "🎶 Current Queue").await?;
    } else if preparation.start_playback {
        let mut embed = crate::discord::embeds::minimal_track_added_embed(
            &preparation.first_track,
            &ctx.data().config(),
        );
        if preparation.added_to_queue > 0 {
            embed = embed.footer(serenity::CreateEmbedFooter::new(format!(
                "Enqueued {} other tracks.",
                preparation.added_to_queue
            )));
        }
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
    } else {
        let queue_pos = {
            let player = player_lock.read().await;
            player.queue.len()
        };
        let embed = crate::discord::embeds::track_added_embed(
            &preparation.first_track,
            queue_pos,
            &ctx.data().config(),
        );
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
    }

    Ok(())
}

async fn queue_snapshot(
    player_lock: &std::sync::Arc<tokio::sync::RwLock<GuildPlayer>>,
) -> Vec<QueueTrackSnapshot> {
    let player = player_lock.read().await;
    let mut tracks = Vec::new();
    if let Some(ref np) = player.now_playing {
        tracks.push(QueueTrackSnapshot::from(np));
    }
    tracks.extend(player.queue.iter().map(QueueTrackSnapshot::from));
    tracks
}

enum PauseOutcome {
    NotPlaying,
    PausedSuccessfully,
    NoTrackPlaying,
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
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let outcome = {
        let mut player = player_lock.write().await;
        if player.playback_status != PlaybackStatus::Playing {
            PauseOutcome::NotPlaying
        } else if let Some(ref handle) = player.current_track_handle {
            handle
                .pause()
                .map_err(|e| SerenyaError::Audio(format!("Failed to pause track: {}", e)))?;
            player.playback_status = PlaybackStatus::Paused;
            PauseOutcome::PausedSuccessfully
        } else {
            PauseOutcome::NoTrackPlaying
        }
    };

    let embed = match outcome {
        PauseOutcome::NotPlaying => crate::discord::embeds::playback_status_embed(
            "❌ Error",
            "Playback is not currently active.",
            0xED4245,
        ),
        PauseOutcome::PausedSuccessfully => {
            crate::discord::embeds::playback_status_embed("⏸️ Pause", "Paused playback.", 0x5865F2)
        }
        PauseOutcome::NoTrackPlaying => crate::discord::embeds::playback_status_embed(
            "❌ Error",
            "No track is currently playing.",
            0xED4245,
        ),
    };

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

enum ResumeOutcome {
    NotPaused,
    ResumedSuccessfully,
    NoTrackPaused,
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
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let outcome = {
        let mut player = player_lock.write().await;
        if player.playback_status != PlaybackStatus::Paused {
            ResumeOutcome::NotPaused
        } else if let Some(ref handle) = player.current_track_handle {
            handle
                .play()
                .map_err(|e| SerenyaError::Audio(format!("Failed to resume track: {}", e)))?;
            player.playback_status = PlaybackStatus::Playing;
            ResumeOutcome::ResumedSuccessfully
        } else {
            ResumeOutcome::NoTrackPaused
        }
    };

    let embed = match outcome {
        ResumeOutcome::NotPaused => crate::discord::embeds::playback_status_embed(
            "❌ Error",
            "Playback is not currently paused.",
            0xED4245,
        ),
        ResumeOutcome::ResumedSuccessfully => crate::discord::embeds::playback_status_embed(
            "▶️ Resume",
            "Resumed playback.",
            0x5865F2,
        ),
        ResumeOutcome::NoTrackPaused => crate::discord::embeds::playback_status_embed(
            "❌ Error",
            "No track is currently paused.",
            0xED4245,
        ),
    };

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
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
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    crate::audio::events::finalize_interrupted_playback_stats(
        ctx.data().database.as_ref(),
        guild_id,
        &player_lock,
    )
    .await;

    let handle_opt = {
        let mut player = player_lock.write().await;
        let vc = player.voice_channel;
        let ac = player.announce_channel;
        let handle = player.current_track_handle.take();

        player.reset();

        player.voice_channel = vc;
        player.announce_channel = ac;
        player.playback_status = PlaybackStatus::Stopped;
        handle
    };

    if let Some(ref handle) = handle_opt {
        let _ = handle.stop();
    }

    let embed = crate::discord::embeds::queue_stopped_embed();
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Helper to count VC users and perform vote skip logic.
async fn process_vote_skip(
    ctx: Context<'_>,
    player_lock: &std::sync::Arc<tokio::sync::RwLock<GuildPlayer>>,
    guild: &serenity::Guild,
) -> Result<bool, Error> {
    let (current_votes, required_votes) = {
        let mut player = player_lock.write().await;
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
        (player.skip_votes.len(), required_votes)
    };

    if current_votes >= required_votes {
        Ok(true)
    } else {
        let embed = crate::discord::embeds::playback_status_embed(
            "📥 Vote Skip",
            &format!(
                "Vote skip recorded! ({} / {} votes needed)",
                current_votes, required_votes
            ),
            0x5865F2,
        );
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        Ok(false)
    }
}

/// Helper to handle requester absence checks and skip timers.
async fn check_requester_absence(
    ctx: Context<'_>,
    player_lock: &std::sync::Arc<tokio::sync::RwLock<GuildPlayer>>,
    track_requester_id: Option<serenity::UserId>,
    guild: &serenity::Guild,
) -> Result<bool, Error> {
    let (requester_in_vc, timer_status) = {
        let player = player_lock.read().await;
        let requester_in_vc = if let Some(req_id) = track_requester_id {
            if let Some(user_state) = guild.voice_states.get(&req_id) {
                user_state.channel_id == player.voice_channel
            } else {
                false
            }
        } else {
            false
        };
        (requester_in_vc, player.requester_absence_timer)
    };

    if !requester_in_vc {
        if let Some(timer) = timer_status {
            if timer.elapsed().as_secs() > 60 {
                Ok(true)
            } else {
                let remaining = 60 - timer.elapsed().as_secs();
                let embed = crate::discord::embeds::playback_status_embed(
                    "⏳ Skip Timer",
                    &format!(
                        "The original requester is not in the VC. Skip will unlock for everyone in {}s.",
                        remaining
                    ),
                    0xFEE75C,
                );
                ctx.send(poise::CreateReply::default().embed(embed)).await?;
                Ok(false)
            }
        } else {
            {
                let mut player = player_lock.write().await;
                player.requester_absence_timer = Some(std::time::Instant::now());
            }
            let embed = crate::discord::embeds::playback_status_embed(
                "⏳ Skip Timer",
                "The original requester is not in the VC. A 60-second skip timer has been started.",
                0xFEE75C,
            );
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            Ok(false)
        }
    } else {
        process_vote_skip(ctx, player_lock, guild).await
    }
}

/// Skip the current track.
#[poise::command(
    slash_command,
    prefix_command,
    aliases("s"),
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
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let player = player_lock.write().await;
    if player.now_playing.is_none() {
        drop(player);
        let embed = crate::discord::embeds::playback_status_embed(
            "❌ Error",
            "Nothing is currently playing.",
            0xED4245,
        );
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    let author_id = ctx.author().id;
    let owner_id = ctx.data().config().bot.owner;
    let track_requester_id = player.now_playing.as_ref().map(|t| t.requester_id);

    let can_skip = author_id.get() == owner_id || Some(author_id) == track_requester_id;

    // Drop write lock before checking requester absence or executing skip (which awaits and gets its own locks)
    drop(player);

    let approved = if can_skip {
        true
    } else {
        let guild = ctx
            .guild()
            .ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?
            .clone();
        check_requester_absence(ctx, &player_lock, track_requester_id, &guild).await?
    };

    if approved {
        let mut player = player_lock.write().await;
        if player.now_playing.is_none() {
            drop(player);
            let embed = crate::discord::embeds::playback_status_embed(
                "❌ Error",
                "Nothing is currently playing.",
                0xED4245,
            );
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            return Ok(());
        }

        player.skip_forced = true;
        let handle_opt = player.current_track_handle.clone();

        drop(player);

        let embed =
            crate::discord::embeds::playback_status_embed("⏭️ Skip", "Skipping track...", 0x5865F2);
        let playback_ctx = crate::audio::events::PlaybackContext {
            guild_id,
            database: std::sync::Arc::clone(&ctx.data().database),
            guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
            http_client: ctx.data().http_client.clone(),
            serenity_ctx: ctx.serenity_context().clone(),
            config: std::sync::Arc::clone(&ctx.data().config),
        };
        crate::commands::control::run_control_transition(
            async {
                ctx.send(poise::CreateReply::default().embed(embed)).await?;
                Ok::<(), Error>(())
            },
            async move {
                if let Some(handle) = handle_opt {
                    let _ = handle.stop();
                } else {
                    crate::audio::events::play_next(playback_ctx, None, true).await?;
                }
                Ok::<(), Error>(())
            },
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::{ExistingVoiceAction, existing_voice_action, prepare_enqueue};
    use crate::core::{GuildPlayer, PlaybackStatus, SourceType, Track};
    use poise::serenity_prelude as serenity;
    use std::sync::Arc;

    fn track(title: &str) -> Track {
        Track {
            title: title.into(),
            url: format!("https://example.com/{title}").into(),
            duration: Some(std::time::Duration::from_secs(180)),
            requester_id: serenity::UserId::new(1),
            requester_name: None,
            source_type: SourceType::Url,
            resolved_url: None,
            thumbnail: None,
            source_provider: Arc::from("test"),
        }
    }

    #[test]
    fn existing_voice_reuses_only_the_same_channel() {
        assert_eq!(existing_voice_action(None, 20), ExistingVoiceAction::Join);
        assert_eq!(
            existing_voice_action(Some(20), 20),
            ExistingVoiceAction::Reuse
        );
        assert_eq!(
            existing_voice_action(Some(10), 20),
            ExistingVoiceAction::RejectDifferentChannel
        );
    }

    #[test]
    fn idle_enqueue_never_claims_playing_before_a_handle_exists() {
        let mut player = GuildPlayer::new();
        let prepared = prepare_enqueue(
            &mut player,
            vec![track("one"), track("two")],
            100,
            Arc::from("requester"),
        )
        .unwrap();

        assert!(prepared.start_playback);
        assert!(player.now_playing.is_some());
        assert_eq!(player.queue.len(), 1);
        assert_eq!(player.playback_status, PlaybackStatus::Idle);
        assert!(player.current_track_handle.is_none());
    }

    #[test]
    fn active_enqueue_only_extends_the_waiting_queue() {
        let mut player = GuildPlayer::new();
        player.now_playing = Some(track("current"));
        player.playback_status = PlaybackStatus::Playing;

        let prepared = prepare_enqueue(
            &mut player,
            vec![track("next")],
            100,
            Arc::from("requester"),
        )
        .unwrap();

        assert!(!prepared.start_playback);
        assert_eq!(
            player.now_playing.as_ref().unwrap().title.as_ref(),
            "current"
        );
        assert_eq!(player.queue.len(), 1);
    }

    #[test]
    fn advancing_to_the_next_track_stays_idle_until_a_handle_exists() {
        let mut player = GuildPlayer::new();
        player.queue.push(track("next"), 100).unwrap();
        player.playback_status = PlaybackStatus::Playing;
        player.advance_queue();

        assert!(player.now_playing.is_some());
        assert_eq!(player.playback_status, PlaybackStatus::Idle);
        assert!(player.current_track_handle.is_none());
    }
}

#[cfg(test)]
mod post_join_voice_configuration_tests {
    use super::join_then_configure_voice;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[tokio::test]
    async fn bitrate_failure_after_successful_join_is_nonfatal() {
        let joined = Arc::new(AtomicBool::new(false));
        let configured = Arc::new(AtomicBool::new(false));
        let joined_for_task = Arc::clone(&joined);
        let configured_for_task = Arc::clone(&configured);

        let result = join_then_configure_voice(
            move || async move {
                joined_for_task.store(true, Ordering::SeqCst);
                Ok::<(), &'static str>(())
            },
            move || async move {
                configured_for_task.store(true, Ordering::SeqCst);
                Err::<(), &'static str>("guild cache unavailable")
            },
        )
        .await;

        assert!(
            joined.load(Ordering::SeqCst),
            "voice join must have completed"
        );
        assert!(
            configured.load(Ordering::SeqCst),
            "bitrate configuration must run after a successful join"
        );
        assert_eq!(
            result,
            Ok(()),
            "bitrate tuning is auxiliary; a failure after a successful join must not abort /play and leave the connection orphaned"
        );
    }

    #[tokio::test]
    async fn join_failure_remains_fatal_and_skips_bitrate_configuration() {
        let configured = Arc::new(AtomicBool::new(false));
        let configured_for_task = Arc::clone(&configured);

        let result = join_then_configure_voice(
            || async { Err::<(), &'static str>("join failed") },
            move || async move {
                configured_for_task.store(true, Ordering::SeqCst);
                Ok::<(), &'static str>(())
            },
        )
        .await;

        assert_eq!(result, Err("join failed"));
        assert!(
            !configured.load(Ordering::SeqCst),
            "bitrate configuration must not run when the voice join itself failed"
        );
    }
}
