use std::time::Duration;

use poise::serenity_prelude as serenity;

use crate::core::Track;
use crate::discord::embeds::now_playing_embed;
use crate::utils::{Context, Error, SerenyaError};

/// Show details of the currently playing track.
#[poise::command(
    slash_command,
    prefix_command,
    rename = "nowplaying",
    aliases("np"),
    check = "crate::discord::checks::require_guild"
)]
pub async fn nowplaying(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let (now_playing, current_track_handle, seek_offset) = {
        let player = player_lock.read().await;
        (
            player.now_playing.clone(),
            player.current_track_handle.clone(),
            player.seek_offset,
        )
    };

    let track = match now_playing {
        Some(t) => t,
        None => {
            ctx.say("Nothing is currently playing.").await?;
            return Ok(());
        }
    };

    let elapsed = if let Some(ref handle) = current_track_handle {
        match handle.get_info().await {
            Ok(info) => seek_offset + info.position,
            Err(_) => Duration::from_secs(0),
        }
    } else {
        Duration::from_secs(0)
    };

    let embed = now_playing_embed(&track, elapsed, None, &ctx.data().config());
    let reply = poise::CreateReply::default().embed(embed);
    ctx.send(reply).await?;
    Ok(())
}

pub fn build_search_menu(ctx_id: u64, tracks: &[Track]) -> serenity::CreateSelectMenu {
    let mut options = Vec::new();
    for (i, track) in tracks.iter().enumerate() {
        let label = crate::utils::truncate_chars(&track.title, 97);
        let duration_str = track
            .duration
            .map(crate::discord::embeds::format_duration)
            .unwrap_or_else(|| "Live".to_string());
        let description = format!("{} • {}", track.source_provider, duration_str);
        options.push(
            serenity::CreateSelectMenuOption::new(label, i.to_string())
                .description(description.chars().take(100).collect::<String>()),
        );
    }

    serenity::CreateSelectMenu::new(
        format!("{}_search", ctx_id),
        serenity::CreateSelectMenuKind::String { options },
    )
    .placeholder("Select a track to play...")
}
