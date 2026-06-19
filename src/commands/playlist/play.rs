use poise::serenity_prelude as serenity;
use songbird::input::YoutubeDl;

use crate::core::Track;
use crate::utils::{Context, Error, SerenyaError};

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

    let max_queue_size = ctx.data().config.playback.max_queue_size;

    if player.playback_status == crate::core::PlaybackStatus::Idle && player.now_playing.is_none() {
        let first = tracks.remove(0);
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
        let mut call = call_lock.lock().await;
        let source: songbird::input::Input =
            YoutubeDl::new(ctx.data().http_client.clone(), first.url.clone()).into();
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
        ctx.say(format!(
            "🎶 **Now Playing playlist track:** {}\nEnqueued {} other tracks.",
            first.title, added
        ))
        .await?;
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
        });
    }

    enqueue_playlist_tracks(ctx, guild_id, user_channel_id, tracks).await?;
    Ok(())
}
