use std::time::Duration;

use poise::serenity_prelude as serenity;

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
            Ok(info) => player.seek_offset + info.position,
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

async fn search_ytdl(query: &str, user_id: serenity::UserId) -> Result<Vec<Track>, SerenyaError> {
    let settings = crate::audio::runtime::settings();
    let output = crate::audio::runtime::run_ytdlp(
        "manual search",
        vec![
            "--flat-playlist".to_owned(),
            "--dump-single-json".to_owned(),
            format!("ytsearch5:{query}"),
        ],
        crate::audio::runtime::duration_from_millis(settings.ytsearch_timeout_ms),
        true,
        Some(crate::audio::runtime::negative_cache_key("ytsearch", query)),
    )
    .await?;

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
                requester_id: user_id,
                requester_name: String::new(),
                source_type: crate::core::SourceType::Search,
                resolved_url: None,
                thumbnail: None,
                source_provider: "YouTube".to_owned(),
            });
        }
    }

    Ok(tracks)
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
    let config = ctx.data().config();

    if player.playback_status == crate::core::PlaybackStatus::Idle && player.now_playing.is_none() {
        player.now_playing = Some(selected_track.clone());
        player.playback_status = crate::core::PlaybackStatus::Playing;

        let manager = songbird::get(ctx.serenity_context())
            .await
            .ok_or_else(|| SerenyaError::Voice("Songbird not initialized.".into()))?;
        let call_lock = manager
            .get(guild_id)
            .ok_or_else(|| SerenyaError::Voice("Not connected to a voice channel.".into()))?;
        let resolved_url =
            crate::audio::extract_stream_url_for_guild(guild_id.get(), &selected_track.url).await?;
        let mut call = call_lock.lock().await;
        let source: songbird::input::Input =
            songbird::input::HttpRequest::new(ctx.data().http_client.clone(), resolved_url).into();
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
        let _ = handle.add_event(
            songbird::Event::Track(songbird::TrackEvent::Error),
            crate::audio::events::TrackErrorHandler {
                guild_id,
                database: ctx.data().database.clone(),
                guild_players: ctx.data().guild_players.clone(),
                http_client: ctx.data().http_client.clone(),
                serenity_ctx: ctx.serenity_context().clone(),
            },
        );
        player.current_track_handle = Some(handle);

        let msg = if selected_track.url.starts_with("http") {
            format!("🎶 **Now Playing:** [{}]({})", selected_track.title, selected_track.url)
        } else {
            format!("🎶 **Now Playing:** {}", selected_track.title)
        };
        Ok(msg)
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

    let mut tracks = search_ytdl(&query, ctx.author().id).await?;
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
