use std::time::Duration;

use poise::serenity_prelude as serenity;

use crate::audio::{ResolvedInput, resolve_input};
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

    let embed = now_playing_embed(track, elapsed, None, &ctx.data().config());
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
            crate::audio::extract_stream_url_for_guild(guild_id.get(), &selected_track.url, &ctx.data().http_client).await?;
        let eight_d_enabled = player.eight_d_enabled;
        let mut call = call_lock.lock().await;
        let source = crate::audio::source::create_stream_input(
            Some(selected_track.url.clone()),
            &resolved_url,
            eight_d_enabled,
        )
        .await?;
        let handle = call.play_input(source);

        let playback_ctx = crate::audio::events::PlaybackContext {
            guild_id,
            database: std::sync::Arc::clone(&ctx.data().database),
            guild_players: std::sync::Arc::clone(&ctx.data().guild_players),
            http_client: ctx.data().http_client.clone(),
            serenity_ctx: ctx.serenity_context().clone(),
            config: ctx.data().config(),
        };

        let _ = handle.add_event(
            songbird::Event::Track(songbird::TrackEvent::End),
            crate::audio::events::TrackEndHandler {
                ctx: playback_ctx.clone(),
            },
        );
        let _ = handle.add_event(
            songbird::Event::Track(songbird::TrackEvent::Error),
            crate::audio::events::TrackErrorHandler {
                ctx: playback_ctx,
            },
        );
        player.current_track_handle = Some(handle);

        let msg = if selected_track.url.starts_with("http") {
            format!(
                "🎶 **Now Playing:** [{}]({})",
                selected_track.title, selected_track.url
            )
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

    let mut tracks = match resolve_input(
        &query,
        ctx.author().id.get(),
        &ctx.data().database,
        &ctx.data().http_client,
    )
    .await?
    {
        ResolvedInput::SearchResults(tracks) => tracks,
        ResolvedInput::Track(track) => vec![*track],
        ResolvedInput::Playlist(tracks) => tracks,
    };
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
        .timeout(std::time::Duration::from_secs(60));

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

        let selected_track = tracks.remove(selected_idx);
        let mut selected_tracks = if is_metadata_search_option(&selected_track) {
            resolve_input(
                &selected_track.url,
                ctx.author().id.get(),
                &ctx.data().database,
                &ctx.data().http_client,
            )
            .await?
            .into_tracks_or_top()
        } else {
            vec![selected_track]
        };
        let mut selected_track = selected_tracks
            .drain(..)
            .next()
            .ok_or_else(|| SerenyaError::Audio("No playable track resolved.".into()))?;
        selected_track.requester_id = ctx.author().id;
        selected_track.requester_name = Some(ctx.author().name.clone());

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

fn is_metadata_search_option(track: &Track) -> bool {
    track.source_provider.starts_with("Deezer")
        || track.source_provider.starts_with("Spotify")
        || track.source_provider.starts_with("Apple Music")
}
