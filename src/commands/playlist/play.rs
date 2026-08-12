use crate::core::Track;
use crate::utils::{Context, Error, SerenyaError};

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

    let req_name: std::sync::Arc<str> = std::sync::Arc::from(ctx.author().name.as_str());
    let source_prov: std::sync::Arc<str> = std::sync::Arc::from("Playlist");
    let mut tracks = Vec::new();
    for t in playlist.tracks {
        tracks.push(Track {
            title: t.title.into(),
            url: t.url.into(),
            duration: t.duration_secs.map(std::time::Duration::from_secs),
            requester_id: ctx.author().id,
            requester_name: Some(req_name.clone()),
            source_type: crate::core::SourceType::Playlist,
            resolved_url: None,
            thumbnail: None,
            source_provider: source_prov.clone(),
        });
    }

    crate::commands::playback::ensure_play_voice(ctx, guild_id, user_channel_id).await?;

    crate::commands::playback::enqueue_and_play_resolved(ctx, guild_id, user_channel_id, tracks)
        .await?;
    Ok(())
}
