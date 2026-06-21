use crate::utils::{Context, Error, SerenyaError};
use poise::serenity_prelude as serenity;
use std::time::Duration;

#[derive(serde::Deserialize, Debug)]
struct LrcResult {
    #[serde(rename = "trackName")]
    track_name: String,
    #[serde(rename = "artistName")]
    artist_name: String,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
}

async fn fetch_lyrics(
    client: &reqwest::Client,
    query_str: &str,
) -> Result<Option<LrcResult>, SerenyaError> {
    let url = "https://lrclib.net/api/search";
    let response = client
        .get(url)
        .header(
            "User-Agent",
            "SerenyaBot/1.0 (https://github.com/Herzchens/Serenya-Bot)",
        )
        .query(&[("q", query_str)])
        .send()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Failed to contact lyrics API: {e}")))?;

    let results: Vec<LrcResult> = response
        .json()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Failed to parse lyrics: {e}")))?;

    let matched = results.into_iter().find(|r| {
        if let Some(lyrics) = &r.plain_lyrics {
            !lyrics.trim().is_empty()
        } else {
            false
        }
    });

    Ok(matched)
}

fn chunk_lyrics(lyrics: &str) -> Vec<String> {
    let mut pages = Vec::new();
    let mut current_page = String::new();
    for line in lyrics.lines() {
        if current_page.len() + line.len() + 1 > 1800 {
            pages.push(current_page.clone());
            current_page.clear();
        }
        current_page.push_str(line);
        current_page.push('\n');
    }
    if !current_page.is_empty() {
        pages.push(current_page);
    }
    pages
}

/// Search for lyrics for the current song or a specific query.
#[poise::command(slash_command, prefix_command)]
pub async fn lyrics(
    ctx: Context<'_>,
    #[description = "Song title or query to search for"] query: Option<String>,
) -> Result<(), Error> {
    ctx.defer().await?;

    let query_str = if let Some(q) = query {
        q
    } else {
        let guild_id = ctx.guild_id().ok_or_else(|| {
            SerenyaError::Config("This command can only be used in a server.".into())
        })?;
        let player_lock = ctx
            .data()
            .guild_players
            .get(&guild_id)
            .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
        let player = player_lock.read().await;
        let track = player.now_playing.as_ref().ok_or_else(|| {
            SerenyaError::Voice("Nothing is currently playing. Please specify a query.".into())
        })?;
        track.title.clone()
    };

    if let Some(matched) = fetch_lyrics(&ctx.data().http_client, &query_str).await? {
        if let Some(lyrics_str) = matched.plain_lyrics {
            let pages = chunk_lyrics(&lyrics_str);
            if pages.is_empty() {
                ctx.say("❌ Lyrics for this song are empty.").await?;
                return Ok(());
            }

            if pages.len() == 1 {
                let embed = serenity::CreateEmbed::new()
                    .title(format!(
                        "🎤 Lyrics: {} - {}",
                        matched.track_name, matched.artist_name
                    ))
                    .description(&pages[0])
                    .color(0x5865F2);
                ctx.send(poise::CreateReply::default().embed(embed)).await?;
            } else {
                crate::discord::pagination::paginate_lyrics(
                    ctx,
                    &matched.track_name,
                    &matched.artist_name,
                    &pages,
                )
                .await?;
            }
            return Ok(());
        }
    }

    ctx.say(format!("❌ No lyrics found for query: **{query_str}**"))
        .await?;
    Ok(())
}

/// Display detailed information about the currently playing song.
#[poise::command(slash_command, prefix_command)]
pub async fn songinfo(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| SerenyaError::Config("This command can only be used in a server.".into()))?;
    let player_lock = ctx
        .data()
        .guild_players
        .get(&guild_id)
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
    let player = player_lock.read().await;

    let track = player
        .now_playing
        .as_ref()
        .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?;

    let mut elapsed = Duration::from_secs(0);
    if let Some(handle) = &player.current_track_handle {
        if let Ok(info) = handle.get_info().await {
            elapsed = player.seek_offset + info.position;
        }
    }

    let loop_str = match player.loop_mode {
        crate::core::loop_mode::LoopMode::Off => "Disabled",
        crate::core::loop_mode::LoopMode::Track => "Track Loop",
        crate::core::loop_mode::LoopMode::Queue => "Queue Loop",
    };

    let duration_str = track
        .duration
        .map(crate::discord::embeds::format_duration)
        .unwrap_or_else(|| "Live".to_string());
    let elapsed_str = crate::discord::embeds::format_duration(elapsed);

    let title_val = if track.url.starts_with("http") {
        format!("[{}]({})", track.title, track.url)
    } else {
        track.title.clone()
    };

    let lyrics_status = match fetch_lyrics(&ctx.data().http_client, &track.title).await {
        Ok(Some(_)) => "✅ Available",
        _ => "❌ Not Available",
    };

    let source = track.clean_source();
    let provider_emoji = crate::discord::embeds::get_provider_emoji(&track, &ctx.data().config());

    let mut embed = serenity::CreateEmbed::new()
        .title("ℹ️ Detailed Song Information")
        .field("Title", title_val, false)
        .field(
            "Requested By",
            format!("👤 **{}**", track.requester_name),
            true,
        )
        .field(
            "Duration",
            format!("⏱️ **{} / {}**", elapsed_str, duration_str),
            true,
        )
        .field("Source", format!("{} **{}**", provider_emoji, source), true)
        .field("Loop State", format!("🔁 **{}**", loop_str), true)
        .field(
            "Playback Status",
            format!("🎵 **{:?}**", player.playback_status),
            true,
        )
        .field("Lyrics Status", format!("🎤 **{}**", lyrics_status), true)
        .color(0x5865F2);

    if let Some(ref thumb) = track.thumbnail {
        embed = embed.thumbnail(thumb.to_string());
    }

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
