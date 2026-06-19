use std::time::Duration;

use poise::serenity_prelude as serenity;
use songbird::input::YoutubeDl;

use crate::core::Track;
use crate::discord::embeds::now_playing_embed;
use crate::utils::{Context, Error, SerenyaError};

#[derive(serde::Deserialize, Debug)]
struct YtDlpSearchResult {
    entries: Option<Vec<YtDlpEntry>>,
}

#[derive(serde::Deserialize, Debug)]
struct YtDlpEntry {
    title: Option<String>,
    id: Option<String>,
    duration: Option<f64>,
}

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
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let player = player_lock.read().await;

    let track = match player.now_playing.as_ref() {
        Some(t) => t,
        None => {
            ctx.say("Nothing is currently playing.").await?;
            return Ok(());
        }
    };

    let elapsed = if let Some(ref handle) = player.current_track_handle {
        match handle.get_info().await {
            Ok(info) => info.position,
            Err(_) => Duration::from_secs(0),
        }
    } else {
        Duration::from_secs(0)
    };

    let embed = now_playing_embed(track, elapsed, None);
    let reply = poise::CreateReply::default().embed(embed);
    ctx.send(reply).await?;
    Ok(())
}

async fn search_ytdl(query: &str) -> Result<Vec<Track>, SerenyaError> {
    let output = tokio::process::Command::new("yt-dlp")
        .args([
            "--flat-playlist",
            "--dump-single-json",
            &format!("ytsearch5:{}", query),
        ])
        .output()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Failed to execute yt-dlp: {}", e)))?;

    if !output.status.success() {
        let err_msg = String::from_utf8_lossy(&output.stderr);
        return Err(SerenyaError::Audio(format!("yt-dlp error: {}", err_msg)));
    }

    let search_result: YtDlpSearchResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| SerenyaError::Audio(format!("Failed to parse search results: {}", e)))?;

    let entries = search_result.entries.unwrap_or_default();
    let mut tracks = Vec::new();
    for entry in entries {
        if let Some(id) = entry.id {
            tracks.push(Track {
                title: entry.title.unwrap_or_else(|| "Unknown Title".to_string()),
                url: format!("https://www.youtube.com/watch?v={}", id),
                duration: entry
                    .duration
                    .map(|d| std::time::Duration::from_secs(d as u64)),
                requester_id: serenity::UserId::new(0),
                requester_name: String::new(),
                source_type: crate::core::SourceType::Search,
            });
        }
    }

    Ok(tracks)
}

fn build_search_menu(ctx_id: u64, tracks: &[Track]) -> serenity::CreateSelectMenu {
    let mut options = Vec::new();
    for (i, track) in tracks.iter().enumerate() {
        let label = if track.title.len() > 100 {
            format!("{}...", &track.title[..97])
        } else {
            track.title.clone()
        };
        let duration_str = track
            .duration
            .map(crate::discord::embeds::format_duration)
            .unwrap_or_else(|| "Live".to_string());
        options.push(
            serenity::CreateSelectMenuOption::new(label, i.to_string())
                .description(format!("Duration: {}", duration_str)),
        );
    }

    serenity::CreateSelectMenu::new(
        format!("{}_search", ctx_id),
        serenity::CreateSelectMenuKind::String { options },
    )
    .placeholder("Select a track to play...")
}

async fn enqueue_selected_track(
    ctx: Context<'_>,
    guild_id: serenity::GuildId,
    selected_track: Track,
) -> Result<String, Error> {
    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active.".into()))?;

    let mut player = player_lock.write().await;
    let config = &ctx.data().config;

    if player.playback_status == crate::core::PlaybackStatus::Idle && player.now_playing.is_none() {
        player.now_playing = Some(selected_track.clone());
        player.playback_status = crate::core::PlaybackStatus::Playing;

        let manager = songbird::get(ctx.serenity_context())
            .await
            .ok_or_else(|| SerenyaError::Voice("Songbird not initialized.".into()))?;
        let call_lock = manager
            .get(guild_id)
            .ok_or_else(|| SerenyaError::Voice("Not connected to a voice channel.".into()))?;
        let mut call = call_lock.lock().await;
        let source: songbird::input::Input =
            YoutubeDl::new(ctx.data().http_client.clone(), selected_track.url.clone()).into();
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

        Ok(format!("🎶 **Now Playing:** {}", selected_track.title))
    } else {
        let max_queue_size = config.playback.max_queue_size;
        player.queue.push(selected_track.clone(), max_queue_size)?;
        Ok(format!("📝 **Enqueued:** {}", selected_track.title))
    }
}

/// Search for a song and pick from the top 5 results.
#[poise::command(
    slash_command,
    prefix_command,
    check = "crate::discord::checks::require_same_voice_channel"
)]
pub async fn search(
    ctx: Context<'_>,
    #[description = "Search query"] query: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;

    ctx.defer().await?;

    let mut tracks = search_ytdl(&query).await?;
    if tracks.is_empty() {
        ctx.say("No search results found.").await?;
        return Ok(());
    }

    let select_menu = build_search_menu(ctx.id(), &tracks);
    let components = vec![serenity::CreateActionRow::SelectMenu(select_menu)];
    let reply = poise::CreateReply::default()
        .content("🔍 Select a track to play:")
        .components(components);

    let msg = ctx.send(reply).await?;
    let mut msg_inner = msg.into_message().await?;

    let collector = serenity::ComponentInteractionCollector::new(ctx.serenity_context())
        .author_id(ctx.author().id)
        .message_id(msg_inner.id)
        .timeout(std::time::Duration::from_secs(30));

    if let Some(interaction) = collector.next().await {
        let selected_idx_str = match &interaction.data.kind {
            serenity::ComponentInteractionDataKind::StringSelect { values } => values
                .first()
                .ok_or_else(|| SerenyaError::Audio("No selection received.".into()))?,
            _ => return Err(SerenyaError::Audio("Invalid interaction type.".into()).into()),
        };
        let selected_idx: usize = selected_idx_str
            .parse()
            .map_err(|_| SerenyaError::Audio("Invalid selection index.".into()))?;

        let mut selected_track = tracks.remove(selected_idx);
        selected_track.requester_id = ctx.author().id;
        selected_track.requester_name = ctx.author().name.clone();

        let response_content = enqueue_selected_track(ctx, guild_id, selected_track).await?;

        let _ = interaction
            .create_response(
                &ctx.serenity_context().http,
                serenity::CreateInteractionResponse::UpdateMessage(
                    serenity::CreateInteractionResponseMessage::new()
                        .content(response_content)
                        .components(vec![]),
                ),
            )
            .await;
    } else {
        let _ = msg_inner
            .edit(
                &ctx.serenity_context().http,
                serenity::EditMessage::new()
                    .content("⏱️ Search selection timed out.")
                    .components(vec![]),
            )
            .await;
    }

    Ok(())
}
