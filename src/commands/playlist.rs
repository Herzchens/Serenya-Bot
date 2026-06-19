use poise::serenity_prelude as serenity;
use songbird::input::YoutubeDl;

use crate::core::Track;
use crate::utils::{Context, Error, SerenyaError};

pub async fn autocomplete_playlist(ctx: Context<'_>, partial: &str) -> Vec<String> {
    let user_id = ctx.author().id.get();
    ctx.data()
        .database
        .get_user_playlist_names(user_id)
        .await
        .into_iter()
        .filter(|name| name.to_lowercase().contains(&partial.to_lowercase()))
        .take(25)
        .collect()
}

/// Create a new playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn create(
    ctx: Context<'_>,
    #[description = "Playlist name"] name: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;
    let config = &ctx.data().config;

    let current_playlists = db.get_user_playlist_names(user_id).await;
    if current_playlists.len() >= config.playback.max_user_playlists {
        ctx.say(format!(
            "Limit reached! You cannot have more than {} playlists.",
            config.playback.max_user_playlists
        ))
        .await?;
        return Ok(());
    }

    db.create_playlist(user_id, &name, config.playback.max_user_playlists)
        .await
        .map_err(|e| SerenyaError::Database(format!("Failed to create playlist: {}", e)))?;

    ctx.say(format!("📁 Created playlist **{}**.", name))
        .await?;
    Ok(())
}

async fn save_tracks_to_playlist(
    db: &crate::database::DatabaseManager,
    user_id: u64,
    name: &str,
    tracks: Vec<Track>,
    max_tracks: usize,
    max_import: usize,
) -> Result<usize, Error> {
    let mut added = 0;
    for track in tracks.into_iter().take(max_import) {
        let p_track = crate::database::models::PlaylistTrack {
            title: track.title,
            url: track.url,
            duration_secs: track.duration.map(|d| d.as_secs()),
        };
        db.add_to_playlist(user_id, name, p_track, max_tracks)
            .await?;
        added += 1;
    }
    Ok(added)
}

/// Add a song to a playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn add(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_playlist"]
    #[description = "Playlist name"]
    name: String,
    #[description = "Search query or URL"] query: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;
    let config = &ctx.data().config;

    ctx.defer().await?;
    let tracks = crate::audio::resolve_input(&query, user_id, db, &ctx.data().http_client).await?;
    if tracks.is_empty() {
        ctx.say("No tracks found for the query.").await?;
        return Ok(());
    }

    let playlist = db
        .get_user_playlist(user_id, &name)
        .await
        .ok_or_else(|| SerenyaError::NotFound(format!("Playlist '{}' not found.", name)))?;

    let max_tracks = config.playback.max_tracks_per_user_playlist;
    if playlist.tracks.len() + tracks.len() > max_tracks {
        ctx.say(format!(
            "Playlist limit exceeded! Cannot add {} tracks (max {}).",
            tracks.len(),
            max_tracks
        ))
        .await?;
        return Ok(());
    }

    let added = save_tracks_to_playlist(
        db,
        user_id,
        &name,
        tracks,
        max_tracks,
        config.playback.max_playlist_import,
    )
    .await?;

    ctx.say(format!(
        "📝 Added {} track(s) to playlist **{}**.",
        added, name
    ))
    .await?;
    Ok(())
}

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
    #[autocomplete = "autocomplete_playlist"]
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

/// List all your playlists.
#[poise::command(slash_command, prefix_command)]
pub async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;

    let names = db.get_user_playlist_names(user_id).await;
    if names.is_empty() {
        ctx.say("You don't have any playlists yet. Use `/playlist create` to create one.")
            .await?;
        return Ok(());
    }

    let mut desc = String::new();
    for name in names {
        if let Some(playlist) = db.get_user_playlist(user_id, &name).await {
            desc.push_str(&format!(
                "• **{}** ({} tracks, updated {})\n",
                name,
                playlist.tracks.len(),
                playlist
                    .updated_at
                    .split('T')
                    .next()
                    .unwrap_or(&playlist.updated_at)
            ));
        }
    }

    let embed = serenity::CreateEmbed::new()
        .title("📁 Your Playlists")
        .description(desc)
        .color(0xFEE75C);

    let reply = poise::CreateReply::default().embed(embed);
    ctx.send(reply).await?;
    Ok(())
}

/// Manage your custom playlists.
#[poise::command(
    slash_command,
    prefix_command,
    subcommands("create", "add", "play", "list"),
    aliases("pl"),
    subcommand_required
)]
pub async fn playlist(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}
