use crate::utils::{Context, Error, SerenyaError};

/// Create a new playlist.
#[poise::command(slash_command, prefix_command)]
pub async fn create(
    ctx: Context<'_>,
    #[description = "Playlist name"]
    #[rest]
    name: String,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let db = &ctx.data().database;
    let config = ctx.data().config();

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
