use poise::serenity_prelude as serenity;

use crate::audio::{ResolvedInput, TrackEndHandler, TrackErrorHandler, resolve_input};
use crate::core::{GuildPlayer, PlaybackStatus, Track};
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
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    // 1. Defer immediately to prevent Discord interaction timeout (3s deadline)
    ctx.defer().await?;

    // 2. Check if user is in a voice channel
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

    // 3. Resolve input + auto-join voice channel in parallel
    let user_id = ctx.author().id.get();
    let db_ref = &ctx.data().database;
    let http_ref = &ctx.data().http_client;
    let (call_result, resolved) = tokio::join!(
        async {
            if let Some(call) = manager.get(guild_id) {
                Ok::<_, SerenyaError>(call)
            } else {
                let call = manager.join(guild_id, user_channel_id).await.map_err(|e| {
                    SerenyaError::Voice(format!("Failed to join voice channel: {}", e))
                })?;
                let _ = crate::audio::quality::apply_bitrate(ctx, guild_id, user_channel_id).await;
                Ok(call)
            }
        },
        resolve_input(&query, user_id, db_ref, http_ref)
    );
    let call_lock = call_result?;
    let resolved = resolved?;

    match resolved {
        ResolvedInput::Playlist(tracks) => {
            enqueue_and_play_resolved(ctx, guild_id, user_channel_id, call_lock, tracks).await?;
        }
        ResolvedInput::Track(track) => {
            enqueue_and_play_resolved(ctx, guild_id, user_channel_id, call_lock, vec![track])
                .await?;
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

                let selected_track = candidates.remove(selected_idx);

                let _ = interaction
                    .create_response(
                        &ctx.serenity_context().http,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .content("⏳ Resolving selected track...")
                                .components(vec![]),
                        ),
                    )
                    .await;

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

                enqueue_and_play_resolved(ctx, guild_id, user_channel_id, call_lock, tracks)
                    .await?;
            } else {
                let _ = msg_inner
                    .edit(
                        &ctx.serenity_context().http,
                        serenity::EditMessage::new()
                            .content("⏱️ Play selection timed out.")
                            .components(vec![]),
                    )
                    .await;
            }
        }
    }

    Ok(())
}

fn is_metadata_search_option(track: &Track) -> bool {
    track.source_provider.starts_with("Deezer")
        || track.source_provider.starts_with("Spotify")
        || track.source_provider.starts_with("Apple Music")
}

pub(crate) async fn enqueue_and_play_resolved(
    ctx: Context<'_>,
    guild_id: serenity::GuildId,
    user_channel_id: serenity::ChannelId,
    call_lock: std::sync::Arc<tokio::sync::Mutex<songbird::Call>>,
    mut tracks: Vec<Track>,
) -> Result<(), Error> {
    if tracks.is_empty() {
        ctx.say("No tracks found to play.").await?;
        return Ok(());
    }
    let requested_track_count = tracks.len();
    let show_queue_after_enqueue = requested_track_count > 1;

    // Get/Create guild player
    let player_lock = ctx
        .data()
        .guild_players
        .entry(guild_id)
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::RwLock::new(GuildPlayer::new())))
        .clone();

    let mut player = player_lock.write().await;
    player.voice_channel = Some(user_channel_id);
    player.announce_channel = Some(ctx.channel_id());

    let config = ctx.data().config();
    let max_queue_size = config.playback.max_queue_size;

    if player.playback_status == PlaybackStatus::Idle && player.now_playing.is_none() {
        // Play first track immediately
        let mut first_track = tracks.remove(0);
        let requester_name = ctx.author().name.clone();
        first_track.requester_name = requester_name.clone();

        // Fix: set requester_name for all remaining tracks before queuing
        for t in &mut tracks {
            t.requester_name = requester_name.clone();
        }

        player.now_playing = Some(first_track.clone());
        player.playback_status = PlaybackStatus::Playing;

        // Queue remaining tracks (if any)
        let added = player.queue.push_batch(tracks, max_queue_size)?;

        // Spawn background play resolution task
        let player_lock_clone = player_lock.clone();
        let call_lock_clone = call_lock.clone();
        let http_client_clone = ctx.data().http_client.clone();
        let database_clone = ctx.data().database.clone();
        let guild_players_clone = ctx.data().guild_players.clone();
        let serenity_ctx_clone = ctx.serenity_context().clone();
        let config_clone = ctx.data().config();
        let first_track_clone = first_track.clone();

        tokio::spawn(async move {
            let original_url = first_track_clone.url.clone();
            let mut current_track = first_track_clone;

            if current_track.url.starts_with("ytsearch1:") {
                if let Err(e) = crate::audio::resolver::resolve_ytsearch_track(
                    &mut current_track,
                    &http_client_clone,
                )
                .await
                {
                    tracing::error!("Failed to resolve Spotify track search: {:?}", e);
                } else {
                    // Update player's now_playing field with the resolved track
                    let mut player = player_lock_clone.write().await;
                    if player.playback_status == PlaybackStatus::Playing {
                        if let Some(ref mut np) = player.now_playing {
                            if np.url == original_url {
                                *np = current_track.clone();
                            }
                        }
                    }
                }
            }

            // 1. Resolve stream URL in background
            let stream_res =
                crate::audio::extract_stream_url_for_guild(guild_id.get(), &current_track.url)
                    .await;

            // 2. Race condition check: check if player was reset/stopped/skipped while resolving
            {
                let player = player_lock_clone.read().await;
                if player.playback_status == PlaybackStatus::Idle || player.now_playing.is_none() {
                    tracing::info!(
                        "Player was stopped or reset while resolving stream URL, aborting playback"
                    );
                    return;
                }
                if let Some(ref current) = player.now_playing {
                    if current.url != current_track.url && current.url != original_url {
                        tracing::info!(
                            "Track was skipped or changed while resolving stream URL, aborting playback"
                        );
                        return;
                    }
                }
            }

            // 3. Play the input
            let resolved_url = match stream_res {
                Ok(resolved_url) => resolved_url,
                Err(e) => {
                    tracing::warn!(
                        guild_id = %guild_id,
                        track = %current_track.title,
                        "Failed to resolve stream URL: {:?}",
                        e
                    );

                    let announce_channel = {
                        let mut player = player_lock_clone.write().await;
                        player.consecutive_errors += 1;
                        if player
                            .now_playing
                            .as_ref()
                            .map(|current| current.url.as_str())
                            == Some(current_track.url.as_str())
                        {
                            player.now_playing = None;
                            player.current_track_handle = None;
                            player.playback_status = PlaybackStatus::Idle;
                        }
                        player.announce_channel
                    };

                    if let Some(channel) = announce_channel {
                        let _ = channel
                            .say(
                                &serenity_ctx_clone.http,
                                format!(
                                    "⚠️ Could not resolve **{}**. Trying the next track.",
                                    current_track.title
                                ),
                            )
                            .await;
                    }

                    if let Err(next_err) = crate::audio::events::play_next(
                        guild_id,
                        std::sync::Arc::clone(&database_clone),
                        std::sync::Arc::clone(&guild_players_clone),
                        http_client_clone.clone(),
                        serenity_ctx_clone.clone(),
                        config_clone.clone(),
                        None,
                        true,
                    )
                    .await
                    {
                        tracing::error!(
                            guild_id = %guild_id,
                            "Failed to continue after stream resolution error: {:?}",
                            next_err
                        );
                    }
                    return;
                }
            };

            let eight_d_enabled = {
                let player = player_lock_clone.read().await;
                player.eight_d_enabled
            };
            let source = match crate::audio::source::create_stream_input(
                resolved_url.clone(),
                eight_d_enabled,
            ) {
                Ok(source) => source,
                Err(err) => {
                    tracing::error!(guild_id = %guild_id, %err, "Failed to create audio input");
                    return;
                }
            };

            let mut call = call_lock_clone.lock().await;
            let handle = call.play_input(source);

            // 4. Register event handlers
            let _ = handle.add_event(
                songbird::Event::Track(songbird::TrackEvent::End),
                TrackEndHandler {
                    guild_id,
                    database: database_clone.clone(),
                    guild_players: guild_players_clone.clone(),
                    http_client: http_client_clone.clone(),
                    serenity_ctx: serenity_ctx_clone.clone(),
                    config: config_clone.clone(),
                },
            );
            let _ = handle.add_event(
                songbird::Event::Track(songbird::TrackEvent::Error),
                TrackErrorHandler {
                    guild_id,
                    database: database_clone.clone(),
                    guild_players: guild_players_clone.clone(),
                    http_client: http_client_clone.clone(),
                    serenity_ctx: serenity_ctx_clone.clone(),
                    config: config_clone.clone(),
                },
            );

            // 5. Update track handle in player
            let mut player = player_lock_clone.write().await;
            // Check race condition again
            if player.playback_status == PlaybackStatus::Playing {
                if let Some(ref mut current) = player.now_playing {
                    if current.url == current_track.url || current.url == original_url {
                        current.resolved_url = Some(resolved_url);
                        player.current_track_handle = Some(handle);
                        crate::audio::events::schedule_prefetch(
                            guild_id,
                            guild_players_clone.clone(),
                            current_track.duration,
                            http_client_clone.clone(),
                        );
                        
                        let announce_channel = player.announce_channel;
                        let track_for_ann = current_track.clone();
                        let db_for_ann = database_clone.clone();
                        let ctx_for_ann = serenity_ctx_clone.clone();
                        let cfg_for_ann = config_clone.clone();

                        tokio::spawn(async move {
                            let announce_setting = db_for_ann
                                .get_guild_settings(guild_id.get())
                                .await
                                .announce_track;

                            if announce_setting {
                                if let Some(channel) = announce_channel {
                                    let embed = crate::discord::embeds::now_playing_announce_embed(
                                        &track_for_ann,
                                        &cfg_for_ann,
                                    );
                                    let _ = channel
                                        .send_message(
                                            &ctx_for_ann.http,
                                            serenity::CreateMessage::new()
                                                .embed(embed)
                                                .flags(serenity::MessageFlags::SUPPRESS_NOTIFICATIONS),
                                        )
                                        .await;
                                }
                            }
                        });

                        return;
                    }
                }
            }
            // If check failed, stop the handle
            let _ = handle.stop();
        });

        // Drop player lock BEFORE sending response to allow background task to start immediately!
        drop(player);

        if show_queue_after_enqueue {
            let queue_tracks = queue_snapshot(&player_lock).await;
            crate::discord::pagination::paginate_queue(ctx, &queue_tracks, "🎶 Current Queue").await?;
        } else if added > 0 {
            let mut embed = crate::discord::embeds::minimal_track_added_embed(&first_track, &ctx.data().config());
            embed = embed.footer(serenity::CreateEmbedFooter::new(format!(
                "Enqueued {} other tracks.",
                added
            )));
            let reply = poise::CreateReply::default().embed(embed);
            ctx.send(reply).await?;
        } else {
            let embed = crate::discord::embeds::minimal_track_added_embed(&first_track, &ctx.data().config());
            let reply = poise::CreateReply::default().embed(embed);
            ctx.send(reply).await?;
        }
    } else {
        // Enqueue all tracks
        let track_count = tracks.len();
        let first_title = tracks.first().map(|t| t.title.clone()).unwrap_or_default();

        // Populate requester names
        let requester_name = ctx.author().name.clone();
        for t in &mut tracks {
            t.requester_name = requester_name.clone();
        }

        let added = player.queue.push_batch(tracks, max_queue_size)?;

        if added == 0 {
            let embed =
                crate::discord::embeds::error_embed("Queue is full! Could not add any tracks.");
            let reply = poise::CreateReply::default().embed(embed);
            ctx.send(reply).await?;
        } else if show_queue_after_enqueue {
            let mut queue_tracks = Vec::new();
            if let Some(ref np) = player.now_playing {
                queue_tracks.push(np.clone());
            }
            queue_tracks.extend(player.queue.iter().cloned());
            drop(player);
            crate::discord::pagination::paginate_queue(ctx, &queue_tracks, "🎶 Current Queue").await?;
        } else if added == 1 && track_count == 1 {
            let queue_pos = player.queue.len();
            if let Some(track) = player.queue.get(queue_pos - 1) {
                let embed = crate::discord::embeds::track_added_embed(track, queue_pos, &ctx.data().config());
                let reply = poise::CreateReply::default().embed(embed);
                ctx.send(reply).await?;
            } else {
                ctx.say(format!("📝 **Enqueued:** {}", first_title)).await?;
            }
        } else {
            let embed = serenity::CreateEmbed::new()
                .title("📝 Tracks Enqueued")
                .description(format!(
                    "Successfully enqueued **{}** tracks to the queue.",
                    added
                ))
                .color(0x57F287);
            let reply = poise::CreateReply::default().embed(embed);
            ctx.send(reply).await?;
        }

        // Trigger prefetching in case it's the next track
        let gp_clone = ctx.data().guild_players.clone();
        let http_client_clone = ctx.data().http_client.clone();
        tokio::spawn(async move {
            crate::audio::events::trigger_prefetch(guild_id, gp_clone, http_client_clone).await;
        });
    }

    Ok(())
}

async fn queue_snapshot(
    player_lock: &std::sync::Arc<tokio::sync::RwLock<GuildPlayer>>,
) -> Vec<Track> {
    let player = player_lock.read().await;
    let mut tracks = Vec::new();
    if let Some(ref np) = player.now_playing {
        tracks.push(np.clone());
    }
    tracks.extend(player.queue.iter().cloned());
    tracks
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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let mut player = player_lock.write().await;
    if player.now_playing.is_none() {
        ctx.say("Nothing is currently playing.").await?;
        return Ok(());
    }

    let author_id = ctx.author().id;
    let owner_id = ctx.data().config().bot.owner;
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
        player.skip_forced = true;
        if let Some(ref handle) = player.current_track_handle {
            let _ = handle.stop();
        } else {
            drop(player);
            crate::audio::events::play_next(
                guild_id,
                std::sync::Arc::clone(&ctx.data().database),
                std::sync::Arc::clone(&ctx.data().guild_players),
                ctx.data().http_client.clone(),
                ctx.serenity_context().clone(),
                ctx.data().config(),
                None,
                true,
            )
            .await?;
        }
    }

    Ok(())
}
