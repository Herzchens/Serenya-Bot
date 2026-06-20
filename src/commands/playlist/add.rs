use crate::core::Track;
use crate::utils::{Context, Error, SerenyaError};

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

/// Add songs to a playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn add(
    ctx: Context<'_>,
    #[autocomplete = "super::autocomplete_playlist"]
    #[description = "Playlist name"]
    name: String,
    #[description = "Search query or URLs (separated by spaces or commas)"] query: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;
    let config = ctx.data().config();

    ctx.defer().await?;

    // Split input query by whitespace or commas to support multiple links
    let mut urls = Vec::new();
    for part in query.split(|c: char| c == ',' || c.is_whitespace()) {
        let trimmed = part.trim();
        if !trimmed.is_empty() {
            urls.push(trimmed.to_string());
        }
    }

    if urls.is_empty() {
        ctx.say("Please provide at least one search query or URL.").await?;
        return Ok(());
    }

    let mut all_tracks = Vec::new();
    for url in urls {
        match crate::audio::resolve_input(&url, user_id, db, &ctx.data().http_client).await {
            Ok(resolved) => {
                all_tracks.extend(resolved.into_tracks_or_top());
            }
            Err(e) => {
                tracing::warn!("Failed to resolve input '{}': {:?}", url, e);
            }
        }
    }

    if all_tracks.is_empty() {
        ctx.say("No tracks found for the provided query/queries.").await?;
        return Ok(());
    }

    let playlist = db
        .get_user_playlist(user_id, &name)
        .await
        .ok_or_else(|| SerenyaError::NotFound(format!("Playlist '{}' not found.", name)))?;

    let max_tracks = config.playback.max_tracks_per_user_playlist;
    if playlist.tracks.len() + all_tracks.len() > max_tracks {
        ctx.say(format!(
            "Playlist limit exceeded! Cannot add {} tracks (max {}). Current size: {}.",
            all_tracks.len(),
            max_tracks,
            playlist.tracks.len()
        ))
        .await?;
        return Ok(());
    }

    let added = save_tracks_to_playlist(
        db,
        user_id,
        &name,
        all_tracks,
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
