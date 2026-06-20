use crate::core::Track;
use crate::utils::{Context, Error, SerenyaError};
use poise::serenity_prelude as serenity;

async fn enqueue_playlist_tracks(
    ctx: Context<'_>,
    guild_id: serenity::GuildId,
    user_channel_id: serenity::ChannelId,
    mut tracks: Vec<Track>,
) -> Result<(), Error> {
    let player_lock = ctx
        .data()
        .guild_players
        .entry(guild_id)
        .or_insert_with(|| {
            std::sync::Arc::new(tokio::sync::RwLock::new(crate::core::GuildPlayer::new()))
        })
        .clone();

    let mut player = player_lock.write().await;
    player.voice_channel = Some(user_channel_id);
    player.announce_channel = Some(ctx.channel_id());

    let max_queue_size = ctx.data().config().playback.max_queue_size;

    if player.playback_status == crate::core::PlaybackStatus::Idle && player.now_playing.is_none() {
        let mut first = tracks.remove(0);

        // Resolve first track search query synchronously so we have a real URL for the embed!
        if first.url.starts_with("ytsearch1:") {
            if let Err(e) =
                crate::audio::resolver::resolve_ytsearch_track(&mut first, &ctx.data().http_client)
                    .await
            {
                tracing::error!("Failed to resolve Spotify track search: {:?}", e);
            }
        }

        player.now_playing = Some(first.clone());
        player.playback_status = crate::core::PlaybackStatus::Playing;

        let manager = songbird::get(ctx.serenity_context())
            .await
            .ok_or_else(|| SerenyaError::Voice("Songbird not initialized.".into()))?;
        let call_lock = if let Some(call) = manager.get(guild_id) {
            call
        } else {
            manager
                .join(guild_id, user_channel_id)
                .await
                .map_err(|e| SerenyaError::Voice(format!("Failed to join voice channel: {}", e)))?
        };
        let resolved_url =
            crate::audio::extract_stream_url_for_guild(guild_id.get(), &first.url).await?;
        let mut call = call_lock.lock().await;
        let source: songbird::input::Input =
            songbird::input::HttpRequest::new(ctx.data().http_client.clone(), resolved_url).into();
        let handle = call.play_input(source);

        let _ = handle.add_event(
            songbird::Event::Track(songbird::TrackEvent::End),
            crate::audio::events::TrackEndHandler {
                guild_id,
                database: ctx.data().database.clone(),
                guild_players: ctx.data().guild_players.clone(),
                http_client: ctx.data().http_client.clone(),
                serenity_ctx: ctx.serenity_context().clone(),
            },
        );
        player.current_track_handle = Some(handle);

        let added = player.queue.push_batch(tracks, max_queue_size)?;
        let mut embed = crate::discord::embeds::now_playing_announce_embed(&first);
        if added > 0 {
            embed = embed.footer(serenity::CreateEmbedFooter::new(format!(
                "Enqueued {} other tracks.",
                added
            )));
        }
        let reply = poise::CreateReply::default().embed(embed);
        ctx.send(reply).await?;
    } else {
        let added = player.queue.push_batch(tracks, max_queue_size)?;
        ctx.say(format!(
            "📝 **Enqueued {} tracks** from the playlist.",
            added
        ))
        .await?;
    }

    Ok(())
}

/// Play all tracks in a playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn play(
    ctx: Context<'_>,
    #[autocomplete = "super::autocomplete_playlist"]
    #[description = "Playlist name"]
    #[rest]
    name: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let user_channel_id = {
        let guild = ctx
            .guild()
            .ok_or_else(|| SerenyaError::NotFound("Guild not found".into()))?;
        guild
            .voice_states
            .get(&ctx.author().id)
            .and_then(|state| state.channel_id)
            .ok_or_else(|| SerenyaError::Voice("You must be in a voice channel.".into()))?
    };

    ctx.defer().await?;

    let user_id = ctx.author().id.get();
    let playlist = ctx
        .data()
        .database
        .get_user_playlist(user_id, &name)
        .await
        .ok_or_else(|| SerenyaError::NotFound(format!("Playlist '{}' not found.", name)))?;

    if playlist.tracks.is_empty() {
        ctx.say("This playlist is empty.").await?;
        return Ok(());
    }

    let mut tracks = Vec::new();
    for t in playlist.tracks {
        tracks.push(Track {
            title: t.title,
            url: t.url,
            duration: t.duration_secs.map(std::time::Duration::from_secs),
            requester_id: ctx.author().id,
            requester_name: ctx.author().name.clone(),
            source_type: crate::core::SourceType::Playlist,
            resolved_url: None,
            thumbnail: None,
            source_provider: "Playlist".to_owned(),
        });
    }

    enqueue_playlist_tracks(ctx, guild_id, user_channel_id, tracks).await?;
    Ok(())
}
