use poise::serenity_prelude as serenity;
use std::time::Duration;

use crate::utils::{Context, Error, SerenyaError};

/// Remove a song from a playlist by its index.
#[poise::command(slash_command, prefix_command)]
pub async fn remove(
    ctx: Context<'_>,
    #[autocomplete = "super::autocomplete_playlist"]
    #[description = "Playlist name"]
    name: String,
    #[description = "1-based index of the song to remove"] position: usize,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;

    db.remove_from_playlist(user_id, &name, position).await?;
    ctx.say(format!(
        "🗑️ Removed track #{position} from playlist **{}**.",
        name
    ))
    .await?;
    Ok(())
}

/// Delete a playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn delete(
    ctx: Context<'_>,
    #[autocomplete = "super::autocomplete_playlist"]
    #[description = "Playlist name"]
    #[rest]
    name: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;

    db.delete_playlist(user_id, &name).await?;
    ctx.say(format!("🗑️ Deleted playlist **{}**.", name))
        .await?;
    Ok(())
}

/// Rename a playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn rename(
    ctx: Context<'_>,
    #[autocomplete = "super::autocomplete_playlist"]
    #[description = "Current playlist name"]
    old_name: String,
    #[description = "New playlist name"] new_name: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;

    db.rename_playlist(user_id, &old_name, &new_name).await?;
    ctx.say(format!(
        "📝 Renamed playlist **{}** to **{}**.",
        old_name, new_name
    ))
    .await?;
    Ok(())
}

/// Show detailed information about a playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn info(
    ctx: Context<'_>,
    #[autocomplete = "super::autocomplete_playlist"]
    #[description = "Playlist name"]
    #[rest]
    name: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;

    let playlist = db
        .get_user_playlist(user_id, &name)
        .await
        .ok_or_else(|| SerenyaError::NotFound(format!("Playlist '{name}' not found.")))?;

    if playlist.tracks.is_empty() {
        ctx.say(format!("Playlist **{}** is currently empty.", name))
            .await?;
        return Ok(());
    }

    ctx.defer().await?;

    let req_name: std::sync::Arc<str> = std::sync::Arc::from(ctx.author().name.as_str());
    let source_prov: std::sync::Arc<str> = std::sync::Arc::from("Playlist");
    let tracks: Vec<crate::core::Track> = playlist
        .tracks
        .iter()
        .map(|t| crate::core::Track {
            title: t.title.as_str().into(),
            url: t.url.as_str().into(),
            duration: t.duration_secs.map(Duration::from_secs),
            requester_id: serenity::UserId::new(user_id),
            requester_name: Some(req_name.clone()),
            source_type: crate::core::track::SourceType::Playlist,
            resolved_url: None,
            thumbnail: None,
            source_provider: source_prov.clone(),
        })
        .collect();

    crate::discord::pagination::paginate_queue(ctx, &tracks, &format!("📁 Playlist: {}", name))
        .await?;

    Ok(())
}
