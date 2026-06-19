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

fn format_tracks_list(playlist: &crate::database::models::UserPlaylist) -> String {
    let mut track_list = String::new();
    for (i, t) in playlist.tracks.iter().enumerate() {
        if i < 10 {
            let dur_str = t
                .duration_secs
                .map(|s| crate::discord::embeds::format_duration(Duration::from_secs(s)))
                .unwrap_or_else(|| "Live".to_string());
            track_list.push_str(&format!("**{}.** {} | `{}`\n", i + 1, t.title, dur_str));
        }
    }

    if playlist.tracks.len() > 10 {
        track_list.push_str(&format!(
            "*...and {} more track(s)*",
            playlist.tracks.len() - 10
        ));
    } else if playlist.tracks.is_empty() {
        track_list = "*No tracks in this playlist.*".to_string();
    }
    track_list
}

/// Show detailed information about a playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn info(
    ctx: Context<'_>,
    #[autocomplete = "super::autocomplete_playlist"]
    #[description = "Playlist name"]
    name: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;

    let playlist = db
        .get_user_playlist(user_id, &name)
        .await
        .ok_or_else(|| SerenyaError::NotFound(format!("Playlist '{name}' not found.")))?;

    let mut total_duration = Duration::from_secs(0);
    for t in &playlist.tracks {
        if let Some(secs) = t.duration_secs {
            total_duration += Duration::from_secs(secs);
        }
    }

    let track_list = format_tracks_list(&playlist);
    let duration_str = crate::discord::embeds::format_duration(total_duration);
    let created = playlist
        .created_at
        .split('T')
        .next()
        .unwrap_or(&playlist.created_at);
    let updated = playlist
        .updated_at
        .split('T')
        .next()
        .unwrap_or(&playlist.updated_at);

    let embed = serenity::CreateEmbed::new()
        .title(format!("📁 Playlist: {}", name))
        .field("Total Tracks", playlist.tracks.len().to_string(), true)
        .field("Total Duration", duration_str, true)
        .field("Created At", created.to_string(), true)
        .field("Updated At", updated.to_string(), true)
        .field("Tracks", track_list, false)
        .color(0xFEE75C);

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
