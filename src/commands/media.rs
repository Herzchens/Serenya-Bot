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

fn score_lyric_results(results: Vec<LrcResult>, query_str: &str) -> Option<LrcResult> {
    // Normalize and clean query
    let mut clean_query = crate::audio::ranking::clean_title(query_str);
    clean_query = clean_query.replace(" | ", " - ").replace(" by ", " - ");

    let query_norm = crate::audio::ranking::normalize_string(&clean_query);

    let mut scored_results: Vec<(LrcResult, f64)> = results
        .into_iter()
        .filter(|r| {
            if let Some(lyrics) = &r.plain_lyrics {
                !lyrics.trim().is_empty()
            } else {
                false
            }
        })
        .map(|r| {
            let r_track_norm = crate::audio::ranking::normalize_string(&r.track_name);
            let r_artist_norm = crate::audio::ranking::normalize_string(&r.artist_name);

            let score = if clean_query.contains(" - ") {
                let parts: Vec<&str> = clean_query.split(" - ").collect();
                let left_norm = crate::audio::ranking::normalize_string(parts[0].trim());
                let right_norm = crate::audio::ranking::normalize_string(parts[1].trim());

                // Scenario 1: Left is Title, Right is Artist
                let mut title_sim_1 =
                    crate::audio::ranking::jaro_winkler_similarity(&r_track_norm, &left_norm);
                let mut artist_sim_1 =
                    crate::audio::ranking::jaro_winkler_similarity(&r_artist_norm, &right_norm);
                if left_norm.contains(&r_track_norm) || r_track_norm.contains(&left_norm) {
                    title_sim_1 = title_sim_1.max(0.95);
                }
                if right_norm.contains(&r_artist_norm) || r_artist_norm.contains(&right_norm) {
                    artist_sim_1 = artist_sim_1.max(0.95);
                }
                let score_1 = title_sim_1 * 0.6 + artist_sim_1 * 0.4;

                // Scenario 2: Left is Artist, Right is Title
                let mut title_sim_2 =
                    crate::audio::ranking::jaro_winkler_similarity(&r_track_norm, &right_norm);
                let mut artist_sim_2 =
                    crate::audio::ranking::jaro_winkler_similarity(&r_artist_norm, &left_norm);
                if right_norm.contains(&r_track_norm) || r_track_norm.contains(&right_norm) {
                    title_sim_2 = title_sim_2.max(0.95);
                }
                if left_norm.contains(&r_artist_norm) || r_artist_norm.contains(&left_norm) {
                    artist_sim_2 = artist_sim_2.max(0.95);
                }
                let score_2 = title_sim_2 * 0.6 + artist_sim_2 * 0.4;

                let split_score = score_1.max(score_2);

                // If there is an artist mismatch in both scenarios, penalize heavily
                let best_artist_sim = artist_sim_1.max(artist_sim_2);
                if best_artist_sim < 0.70 {
                    split_score * 0.3
                } else {
                    split_score
                }
            } else {
                let mut title_sim =
                    crate::audio::ranking::jaro_winkler_similarity(&r_track_norm, &query_norm);
                if query_norm.contains(&r_track_norm) || r_track_norm.contains(&query_norm) {
                    title_sim = title_sim.max(0.95);
                }
                let artist_bonus =
                    if !r_artist_norm.is_empty() && query_norm.contains(&r_artist_norm) {
                        0.20
                    } else {
                        0.0
                    };
                title_sim * 0.8 + artist_bonus
            };

            (r, score)
        })
        .collect();

    scored_results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    if let Some((best_result, best_score)) = scored_results.into_iter().next() {
        tracing::info!(
            "Lyric search match: {} - {} (score: {:.3}) for query: {}",
            best_result.track_name,
            best_result.artist_name,
            best_score,
            query_str
        );
        if best_score >= 0.70 {
            return Some(best_result);
        }
    }

    None
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

    Ok(score_lyric_results(results, query_str))
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
    #[description = "Song title or query to search for"]
    #[rest]
    query: Option<String>,
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
            .map(|r| r.value().clone())
            .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;
        let player = player_lock.read().await;
        let track = player.now_playing.as_ref().ok_or_else(|| {
            SerenyaError::Voice("Nothing is currently playing. Please specify a query.".into())
        })?;
        track.title.to_string()
    };

    if let Some(matched) = fetch_lyrics(&ctx.data().http_client, &query_str).await?
        && let Some(lyrics_str) = matched.plain_lyrics
    {
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
        .map(|r| r.value().clone())
        .ok_or_else(|| SerenyaError::NotFound("No player active in this server.".into()))?;

    let (track, current_track_handle, seek_offset, loop_mode, playback_status) = {
        let player = player_lock.read().await;
        let track = player
            .now_playing
            .clone()
            .ok_or_else(|| SerenyaError::Voice("Nothing is currently playing.".into()))?;
        (
            track,
            player.current_track_handle.clone(),
            player.seek_offset,
            player.loop_mode,
            player.playback_status,
        )
    };

    let mut elapsed = Duration::from_secs(0);
    if let Some(ref handle) = current_track_handle
        && let Ok(info) = handle.get_info().await
    {
        elapsed = seek_offset + info.position;
    }

    let loop_str = match loop_mode {
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
        track.title.to_string()
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
            format!(
                "👤 **{}**",
                track.requester_name.as_deref().unwrap_or("Unknown")
            ),
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
            format!("🎵 **{:?}**", playback_status),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lyric_exact_match_artist() {
        let results = vec![
            LrcResult {
                track_name: "Nếu mai mình chia tay".to_string(),
                artist_name: "Kaisoul".to_string(),
                plain_lyrics: Some(" lyrics kaisoul ".to_string()),
            },
            LrcResult {
                track_name: "Nếu Mai Chia Tay".to_string(),
                artist_name: "Monstar".to_string(),
                plain_lyrics: Some(" lyrics monstar ".to_string()),
            },
        ];

        let matched = score_lyric_results(results, "Nếu Mai Chia Tay - Monstar").unwrap();
        assert_eq!(matched.artist_name, "Monstar");
    }

    #[test]
    fn test_lyric_artist_mismatch_rejected() {
        let results = vec![LrcResult {
            track_name: "Nếu mai mình chia tay".to_string(),
            artist_name: "Kaisoul".to_string(),
            plain_lyrics: Some(" lyrics kaisoul ".to_string()),
        }];

        let matched = score_lyric_results(results, "Nếu Mai Chia Tay - Monstar");
        assert!(
            matched.is_none(),
            "Should reject Kaisoul lyrics when Monstar was requested"
        );
    }
}
