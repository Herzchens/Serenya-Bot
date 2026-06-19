use poise::serenity_prelude as serenity;

use crate::utils::{Context, Error};

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
            let updated = playlist
                .updated_at
                .split('T')
                .next()
                .unwrap_or(&playlist.updated_at);
            desc.push_str(&format!(
                "• **{}** ({} tracks, updated {})\n",
                name,
                playlist.tracks.len(),
                updated
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
