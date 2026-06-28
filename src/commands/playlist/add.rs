use poise::serenity_prelude as serenity;

use crate::audio::{ResolvedInput, resolve_input};
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
            title: track.title.to_string(),
            url: track.url.to_string(),
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
    #[description = "Search query or URLs (separated by spaces or commas)"]
    #[rest]
    query: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;
    let config = ctx.data().config();

    ctx.defer().await?;

    // Check if input looks like a URL (direct link) or a search query
    let is_url = query.trim().starts_with("http://") || query.trim().starts_with("https://");

    if is_url {
        // URL mode: split by whitespace/commas and resolve each
        let mut urls = Vec::new();
        for part in query.split(|c: char| c == ',' || c.is_whitespace()) {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                urls.push(trimmed.to_string());
            }
        }

        if urls.is_empty() {
            ctx.say("Please provide at least one search query or URL.")
                .await?;
            return Ok(());
        }

        let mut all_tracks = Vec::new();
        for url in urls {
            match resolve_input(&url, user_id, db, &ctx.data().http_client).await {
                Ok(resolved) => {
                    all_tracks.extend(resolved.into_tracks_or_top());
                }
                Err(e) => {
                    tracing::warn!("Failed to resolve input '{}': {:?}", url, e);
                }
            }
        }

        add_tracks_to_playlist(ctx, db, user_id, &name, all_tracks, &config).await
    } else {
        // Search mode: resolve and show interactive selection
        let resolved = resolve_input(&query, user_id, db, &ctx.data().http_client).await?;

        match resolved {
            ResolvedInput::SearchResults(mut candidates) => {
                if candidates.is_empty() {
                    ctx.say("No tracks found for the provided query.").await?;
                    return Ok(());
                }

                let select_menu = crate::commands::info::build_search_menu(ctx.id(), &candidates);
                let components = vec![serenity::CreateActionRow::SelectMenu(select_menu)];
                let reply = poise::CreateReply::default()
                    .content(format!(
                        "🔎 Select a track to add to playlist **{}**:",
                        name
                    ))
                    .components(components);

                let msg = ctx.send(reply).await?;
                let mut msg_inner = msg.into_message().await?;

                let collector =
                    serenity::ComponentInteractionCollector::new(ctx.serenity_context())
                        .author_id(ctx.author().id)
                        .message_id(msg_inner.id)
                        .timeout(std::time::Duration::from_secs(60));

                if let Some(interaction) = collector.next().await {
                    let selected_idx_str = match &interaction.data.kind {
                        serenity::ComponentInteractionDataKind::StringSelect { values } => values
                            .first()
                            .ok_or_else(|| SerenyaError::Audio("No selection received.".into()))?,
                        _ => {
                            return Err(
                                SerenyaError::Audio("Invalid interaction type.".into()).into()
                            );
                        }
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
                                    .content("⏳ Adding selected track...")
                                    .components(vec![]),
                            ),
                        )
                        .await;

                    // If it's a metadata-only provider (Deezer/Spotify/Apple Music),
                    // resolve the actual playable URL
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

                    add_tracks_to_playlist(ctx, db, user_id, &name, tracks, &config).await?;
                } else {
                    let _ = msg_inner
                        .edit(
                            &ctx.serenity_context().http,
                            serenity::EditMessage::new()
                                .content("⏱️ Selection timed out.")
                                .components(vec![]),
                        )
                        .await;
                }

                Ok(())
            }
            // Direct track or playlist result: add immediately
            other => {
                let tracks = other.into_tracks_or_top();
                add_tracks_to_playlist(ctx, db, user_id, &name, tracks, &config).await
            }
        }
    }
}

fn is_metadata_search_option(track: &Track) -> bool {
    track.source_provider.starts_with("Deezer")
        || track.source_provider.starts_with("Spotify")
        || track.source_provider.starts_with("Apple Music")
}

async fn add_tracks_to_playlist(
    ctx: Context<'_>,
    db: &crate::database::DatabaseManager,
    user_id: u64,
    name: &str,
    all_tracks: Vec<Track>,
    config: &crate::config::BotConfig,
) -> Result<(), Error> {
    if all_tracks.is_empty() {
        ctx.say("No tracks found for the provided query/queries.")
            .await?;
        return Ok(());
    }

    let playlist = db
        .get_user_playlist(user_id, name)
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
        name,
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
