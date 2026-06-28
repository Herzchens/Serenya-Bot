use crate::audio::providers::{
    AppleMusicProvider, DeezerProvider, DirectUrlProvider, ExternalTrackMeta, MetadataProvider,
    SoundCloudProvider, SpotifyProvider, YouTubeMusicProvider, YouTubeProvider,
};
use crate::audio::ranking::{
    MetadataConfidence, TrackCandidate, adjust_single_word_score, contains_unrequested_variant,
    has_critical_risks, jaro_winkler_similarity, score_candidates,
};
use crate::core::{SourceType, Track};
use crate::database::DatabaseManager;
use crate::utils::SerenyaError;
use poise::serenity_prelude as serenity;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum ResolvedInput {
    Playlist(Vec<Track>),
    Track(Box<Track>),
    SearchResults(Vec<Track>),
}

impl ResolvedInput {
    pub fn into_tracks_or_top(self) -> Vec<Track> {
        match self {
            ResolvedInput::Playlist(tracks) => tracks,
            ResolvedInput::Track(track) => vec![*track],
            ResolvedInput::SearchResults(mut tracks) => {
                if tracks.is_empty() {
                    vec![]
                } else {
                    vec![tracks.remove(0)]
                }
            }
        }
    }
}

pub(crate) fn extract_artist_string(artists: Option<&Vec<serde_json::Value>>) -> String {
    let mut artists_vec = Vec::new();
    if let Some(artists) = artists {
        for a in artists {
            if let Some(a_name) = a
                .get("name")
                .or_else(|| a.pointer("/profile/name"))
                .and_then(|v| v.as_str())
            {
                artists_vec.push(a_name.to_owned());
            }
        }
    }
    if artists_vec.is_empty() {
        "".to_owned()
    } else {
        artists_vec.join(", ")
    }
}

#[cfg(test)]
const TRUSTED_METADATA_PICK_THRESHOLD: f64 = 0.68;

#[cfg(test)]
#[derive(Debug, Clone)]
struct TrustedMetadataMatch {
    meta: ExternalTrackMeta,
    source: String,
    score: f64,
}

#[cfg(test)]
fn candidate_to_meta(candidate: &TrackCandidate) -> ExternalTrackMeta {
    ExternalTrackMeta {
        title: candidate.title.clone(),
        artist: Some(candidate.artist.clone()).filter(|artist| artist != "Unknown Artist"),
        duration: candidate.duration,
        thumbnail: candidate.thumbnail.clone(),
    }
}

fn metadata_provider_boost(source: &str) -> f64 {
    match source {
        "Deezer" => 0.08,
        "Apple Music" => 0.06,
        "Spotify" => 0.06,
        _ => 0.0,
    }
}

fn token_overlap(query: &str, candidate_text: &str) -> f64 {
    let mut total_query_tokens = 0;
    let mut matched_tokens = 0;

    let c_tokens: std::collections::HashSet<String> = candidate_text
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| part.len() > 1)
        .map(|s| s.to_ascii_lowercase())
        .collect();

    for q_token in query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| part.len() > 1)
    {
        total_query_tokens += 1;

        if c_tokens.contains(&q_token.to_ascii_lowercase()) {
            matched_tokens += 1;
        }
    }

    if total_query_tokens == 0 {
        return 0.0;
    }

    matched_tokens as f64 / total_query_tokens as f64
}

fn metadata_candidate_score(candidate: &TrackCandidate, query: &str) -> Option<f64> {
    let title_artist = format!("{} {}", candidate.title, candidate.artist);
    let artist_title = format!("{} {}", candidate.artist, candidate.title);
    let similarity = jaro_winkler_similarity(query, &title_artist)
        .max(jaro_winkler_similarity(query, &artist_title))
        .max(jaro_winkler_similarity(query, &candidate.title));
    let overlap = token_overlap(query, &title_artist);
    let popularity = candidate
        .popularity
        .map(|value| ((value as f64).ln() / 18.0).clamp(0.0, 1.0))
        .unwrap_or(0.5);
    let official = if candidate.is_official { 0.05 } else { 0.0 };
    let mut score = (similarity * 0.45
        + overlap * 0.40
        + popularity * 0.07
        + official
        + metadata_provider_boost(&candidate.source))
    .clamp(0.0, 1.0);

    score = adjust_single_word_score(&candidate.title, query, score);

    tracing::debug!(
        provider = %candidate.source,
        candidate_title = %candidate.title,
        candidate_artist = %candidate.artist,
        candidate_duration = ?candidate.duration,
        score,
        "scored trusted metadata candidate"
    );

    Some(score)
}

#[cfg(test)]
fn score_trusted_metadata_candidates(
    candidates: Vec<TrackCandidate>,
    query: &str,
) -> Vec<TrustedMetadataMatch> {
    let mut scored = candidates
        .into_iter()
        .filter_map(|candidate| {
            let score = metadata_candidate_score(&candidate, query)?;
            Some(TrustedMetadataMatch {
                meta: candidate_to_meta(&candidate),
                source: candidate.source,
                score,
            })
        })
        .collect::<Vec<_>>();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

async fn search_trusted_metadata_candidates(
    query: &str,
    http_client: &reqwest::Client,
) -> Vec<(TrackCandidate, f64)> {
    let mut join_set = JoinSet::new();

    for provider in ["Deezer", "Apple Music", "Spotify"] {
        let query = query.to_owned();
        let client = http_client.clone();
        join_set.spawn(async move {
            let start = std::time::Instant::now();
            let result = match provider {
                "Deezer" => DeezerProvider.search(&query, &client).await,
                "Apple Music" => AppleMusicProvider.search(&query, &client).await,
                "Spotify" => SpotifyProvider.search(&query, &client).await,
                _ => Ok(Vec::new()),
            };
            let elapsed_ms = start.elapsed().as_millis();
            if let Err(ref err) = result {
                let kind = match err {
                    SerenyaError::Audio(s) if s.contains("timeout") || s.contains("timed out") => {
                        "timeout"
                    }
                    SerenyaError::Audio(s)
                        if s.contains("rate limit")
                            || s.contains("rate-limit")
                            || s.contains("429") =>
                    {
                        "rate_limit"
                    }
                    SerenyaError::Audio(s) if s.contains("parse") || s.contains("json") => "parse",
                    SerenyaError::Audio(s) if s.contains("api") || s.contains("status") => "api",
                    _ => "network",
                };
                tracing::info!(
                    "provider_failed provider={} kind={} query={} elapsed_ms={}",
                    provider,
                    kind,
                    query,
                    elapsed_ms
                );
            }
            (provider, result)
        });
    }

    let mut scored = Vec::new();
    let timeout = crate::audio::runtime::duration_from_millis(
        crate::audio::runtime::settings().global_search_timeout_ms,
    );
    let _ = tokio::time::timeout(timeout, async {
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((_provider, Ok(candidates))) => {
                    scored.extend(candidates.into_iter().filter_map(|candidate| {
                        let score = metadata_candidate_score(&candidate, query)?;
                        Some((candidate, score))
                    }));
                }
                Ok((provider, Err(err))) => {
                    tracing::debug!(provider, %err, "trusted metadata provider failed");
                }
                Err(err) => {
                    tracing::warn!(%err, "trusted metadata task failed");
                }
            }
        }
    })
    .await;

    join_set.abort_all();
    while join_set.join_next().await.is_some() {}

    scored
}

fn get_priority_boost(source: &str, query: &str) -> f64 {
    let query_lower = query.to_lowercase();
    match source {
        "YouTube Music" => 0.02,
        "YouTube" => -0.02,
        "yt-dlp" => -0.05,
        "SoundCloud" if query_lower.contains("soundcloud") => 0.04,
        "SoundCloud" if query_lower.contains("remix") || query_lower.contains("mix") => 0.02,
        "SoundCloud" => -0.03,
        _ => 0.0,
    }
}

#[derive(Debug, Clone, Copy)]
enum SearchProviderKind {
    YouTubeMusic,
    YouTube,
    SoundCloud,
}

impl SearchProviderKind {
    fn name(self) -> &'static str {
        match self {
            SearchProviderKind::YouTubeMusic => "YouTube Music",
            SearchProviderKind::YouTube => "YouTube",
            SearchProviderKind::SoundCloud => "SoundCloud",
        }
    }

    fn timeout(self) -> Duration {
        let settings = crate::audio::runtime::settings();
        let millis = match self {
            SearchProviderKind::YouTubeMusic => settings.youtube_music_timeout_ms,
            SearchProviderKind::YouTube => settings.youtube_timeout_ms,
            SearchProviderKind::SoundCloud => settings.soundcloud_timeout_ms,
        };
        crate::audio::runtime::duration_from_millis(millis)
    }
}

struct ProviderSearchResult {
    provider: SearchProviderKind,
    elapsed: Duration,
    result: Result<Vec<TrackCandidate>, SerenyaError>,
}

async fn run_provider_search(
    provider: SearchProviderKind,
    query: &str,
    http_client: &reqwest::Client,
) -> Result<Vec<TrackCandidate>, SerenyaError> {
    match provider {
        SearchProviderKind::YouTubeMusic => YouTubeMusicProvider.search(query, http_client).await,
        SearchProviderKind::YouTube => YouTubeProvider.search(query, http_client).await,
        SearchProviderKind::SoundCloud => SoundCloudProvider.search(query, http_client).await,
    }
}

fn score_provider_candidates(
    candidates: Vec<TrackCandidate>,
    search_query: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
    expected_duration: Option<Duration>,
    confidence: MetadataConfidence,
) -> Vec<(TrackCandidate, f64)> {
    let mut scored = score_candidates(
        candidates,
        search_query,
        expected_title,
        expected_artist,
        expected_duration,
        confidence,
    );

    for (candidate, score) in &mut scored {
        *score = (*score + get_priority_boost(&candidate.source, search_query)).clamp(0.0, 1.0);

        // Apply a small priority boost for YouTube/YT Music/YT-DLP lyrics videos
        let is_yt = candidate.source == "YouTube"
            || candidate.source == "YouTube Music"
            || candidate.source == "yt-dlp";
        let title_lower = candidate.title.to_lowercase();
        let is_lyric = title_lower.contains("lyric") || title_lower.contains("lyrics");
        if is_yt && is_lyric {
            *score = (*score + 0.05).clamp(0.0, 1.0);
        }

        tracing::debug!(
            provider = %candidate.source,
            candidate_title = %candidate.title,
            candidate_duration = ?candidate.duration,
            score = *score,
            "scored search candidate"
        );
    }

    scored
}

async fn perform_parallel_search(
    search_query: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
    expected_duration: Option<Duration>,
    http_client: &reqwest::Client,
) -> Result<Vec<(TrackCandidate, f64)>, SerenyaError> {
    let mut all_scored = run_provider_batch(
        &[
            SearchProviderKind::YouTubeMusic,
            SearchProviderKind::YouTube,
        ],
        search_query,
        expected_title,
        expected_artist,
        expected_duration,
        http_client,
        MetadataConfidence::Trusted,
    )
    .await?;

    all_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(all_scored)
}

async fn collect_search_results(
    query: &str,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    // Run trusted metadata (Deezer/Apple/Spotify) and provider batch (YTM/YT/SC)
    // in parallel — halves worst-case latency on the most common code path.
    let (trusted_results, provider_results) = tokio::join!(
        search_trusted_metadata_candidates(query, http_client),
        run_provider_batch(
            &[
                SearchProviderKind::YouTubeMusic,
                SearchProviderKind::YouTube,
                SearchProviderKind::SoundCloud,
            ],
            query,
            query,
            None,
            None,
            http_client,
            MetadataConfidence::Untrusted,
        )
    );

    let mut scored = trusted_results;
    scored.extend(provider_results?);

    scored.sort_by(|a, b| {
        source_priority(&a.0.source)
            .cmp(&source_priority(&b.0.source))
            .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
    });

    let tracks = scored
        .into_iter()
        .take(25)
        .map(|(candidate, score)| Track {
            title: candidate_display_title(&candidate).into(),
            url: candidate.url.into(),
            duration: candidate.duration,
            requester_id: serenity::UserId::new(user_id),
            requester_name: None,
            source_type: SourceType::Search,
            resolved_url: None,
            thumbnail: candidate.thumbnail,
            source_provider: std::sync::Arc::from(format!(
                "{} • {:.0}%",
                candidate.source,
                score * 100.0
            )),
        })
        .collect();

    Ok(tracks)
}

fn source_priority(source: &str) -> usize {
    match source {
        "Deezer" => 0,
        "Apple Music" => 1,
        "Spotify" => 2,
        "SoundCloud" => 3,
        "YouTube Music" => 4,
        "YouTube" => 5,
        "yt-dlp" => 6,
        _ => 7,
    }
}

async fn run_provider_batch(
    providers: &[SearchProviderKind],
    search_query: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
    expected_duration: Option<Duration>,
    http_client: &reqwest::Client,
    confidence: MetadataConfidence,
) -> Result<Vec<(TrackCandidate, f64)>, SerenyaError> {
    let settings = crate::audio::runtime::settings();
    let global_timeout =
        crate::audio::runtime::duration_from_millis(settings.global_search_timeout_ms);
    let cancel_token = CancellationToken::new();
    let mut join_set = JoinSet::new();

    for provider in providers {
        let provider = *provider;
        let query = search_query.to_owned();
        let client = http_client.clone();
        let token = cancel_token.child_token();
        join_set.spawn(async move {
            let started = std::time::Instant::now();
            let result = tokio::select! {
                _ = token.cancelled() => Ok(Vec::new()),
                result = tokio::time::timeout(provider.timeout(), run_provider_search(provider, &query, &client)) => {
                    match result {
                        Ok(result) => result,
                        Err(_) => Err(SerenyaError::Audio(format!(
                            "{} search timed out",
                            provider.name()
                        ))),
                    }
                }
            };
            ProviderSearchResult {
                provider,
                elapsed: started.elapsed(),
                result,
            }
        });
    }

    let mut all_scored = Vec::new();
    let mut perfect_found = false;
    let mut seen_urls = std::collections::HashSet::new();

    let search_result = tokio::time::timeout(global_timeout, async {
        while let Some(joined) = join_set.join_next().await {
            let provider_result = match joined {
                Ok(result) => result,
                Err(err) => {
                    tracing::warn!(%err, "search provider task failed");
                    continue;
                }
            };

            tracing::debug!(
                provider = provider_result.provider.name(),
                elapsed_ms = provider_result.elapsed.as_millis(),
                "search provider finished"
            );

            let candidates = match provider_result.result {
                Ok(candidates) => candidates,
                Err(err) => {
                    let kind = match &err {
                        SerenyaError::Audio(s)
                            if s.contains("timeout") || s.contains("timed out") =>
                        {
                            "timeout"
                        }
                        SerenyaError::Audio(s)
                            if s.contains("rate limit")
                                || s.contains("rate-limit")
                                || s.contains("429") =>
                        {
                            "rate_limit"
                        }
                        SerenyaError::Audio(s) if s.contains("parse") || s.contains("json") => {
                            "parse"
                        }
                        SerenyaError::Audio(s) if s.contains("api") || s.contains("status") => {
                            "api"
                        }
                        _ => "network",
                    };
                    tracing::info!(
                        "provider_failed provider={} kind={} query={} elapsed_ms={}",
                        provider_result.provider.name(),
                        kind,
                        search_query,
                        provider_result.elapsed.as_millis()
                    );
                    tracing::warn!(
                        provider = provider_result.provider.name(),
                        %err,
                        "search provider failed"
                    );
                    continue;
                }
            };

            if candidates.is_empty() {
                continue;
            }

            let mut candidates = candidates;
            candidates.retain(|c| {
                if seen_urls.contains(&c.url) {
                    false
                } else {
                    seen_urls.insert(c.url.clone());
                    true
                }
            });
            if candidates.is_empty() {
                continue;
            }

            let scored = score_provider_candidates(
                candidates,
                search_query,
                expected_title,
                expected_artist,
                expected_duration,
                confidence,
            );

            if let Some((candidate, top_score)) = scored.first()
                && *top_score >= settings.auto_pick_threshold
                && !contains_unrequested_variant(&candidate.title, search_query)
            {
                perfect_found = true;
                all_scored.extend(scored);
                cancel_token.cancel();
                join_set.abort_all();
                break;
            }

            all_scored.extend(scored);
        }
    })
    .await;

    if search_result.is_err() {
        tracing::warn!(
            query = %search_query,
            timeout_ms = settings.global_search_timeout_ms,
            "global search deadline reached"
        );
    }

    if !perfect_found {
        cancel_token.cancel();
        join_set.abort_all();
    }
    while join_set.join_next().await.is_some() {}

    all_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(all_scored)
}

/// Orchestrates track resolution, mirroring, and search ranking.
pub async fn resolve_input(
    query: &str,
    user_id: u64,
    db: &DatabaseManager,
    http_client: &reqwest::Client,
) -> Result<ResolvedInput, SerenyaError> {
    let start_time = std::time::Instant::now();
    let query_trimmed = query.trim();

    let res = async move {
        if let Some(playlist) = db.get_user_playlist(user_id, query_trimmed).await {
            let mut tracks = Vec::new();
            let source_prov: std::sync::Arc<str> = std::sync::Arc::from("Playlist");
            for t in playlist.tracks {
                tracks.push(Track {
                    title: t.title.into(),
                    url: t.url.into(),
                    duration: t.duration_secs.map(Duration::from_secs),
                    requester_id: serenity::UserId::new(user_id),
                    requester_name: None,
                    source_type: SourceType::Playlist,
                    resolved_url: None,
                    thumbnail: None,
                    source_provider: source_prov.clone(),
                });
            }
            return Ok(ResolvedInput::Playlist(tracks));
        }

        if query_trimmed.contains("open.spotify.com/playlist/") {
            let config = crate::audio::runtime::spotify_settings();
            let (enabled, limit) = match config {
                Some(cfg) => (cfg.enable_playlist, cfg.max_playlist_import),
                None => (true, 100),
            };
            if !enabled {
                return Err(SerenyaError::Audio(
                    "Spotify playlist import is disabled in configuration.".into(),
                ));
            }
            return if let Some(id) = extract_spotify_id(query_trimmed, "spotify.com/playlist/") {
                let tracks = resolve_spotify_playlist(&id, limit, user_id, http_client).await?;
                Ok(ResolvedInput::Playlist(tracks))
            } else {
                Err(SerenyaError::Audio(
                    "Failed to extract Spotify playlist ID.".into(),
                ))
            }
        }

        if query_trimmed.contains("open.spotify.com/album/") {
            let config = crate::audio::runtime::spotify_settings();
            let (enabled, limit) = match config {
                Some(cfg) => (cfg.enable_album, cfg.max_album_import),
                None => (true, 100),
            };
            if !enabled {
                return Err(SerenyaError::Audio(
                    "Spotify album import is disabled in configuration.".into(),
                ));
            }
            return if let Some(id) = extract_spotify_id(query_trimmed, "spotify.com/album/") {
                let tracks = resolve_spotify_album(&id, limit, user_id, http_client).await?;
                Ok(ResolvedInput::Playlist(tracks))
            } else {
                Err(SerenyaError::Audio(
                    "Failed to extract Spotify album ID.".into(),
                ))
            }
        }

        if query_trimmed.contains("open.spotify.com/artist/") {
            let config = crate::audio::runtime::spotify_settings();
            let (enabled, limit) = match config {
                Some(cfg) => (cfg.enable_artist_top_tracks, cfg.max_artist_top_tracks),
                None => (true, 20),
            };
            if !enabled {
                return Err(SerenyaError::Audio(
                    "Spotify artist top tracks import is disabled in configuration.".into(),
                ));
            }
            return if let Some(id) = extract_spotify_id(query_trimmed, "spotify.com/artist/") {
                let tracks =
                    resolve_spotify_artist_top_tracks(&id, limit, user_id, http_client).await?;
                Ok(ResolvedInput::Playlist(tracks))
            } else {
                Err(SerenyaError::Audio(
                    "Failed to extract Spotify artist ID.".into(),
                ))
            }
        }

        if query_trimmed.starts_with("http://") || query_trimmed.starts_with("https://") {
            if let Some(mut cached_track) =
                crate::audio::source::cache_get_url_metadata(query_trimmed).await
            {
                cached_track.requester_id = serenity::UserId::new(user_id);
                tracing::debug!(query = %query_trimmed, cache = "hit", "cache_hit");
                return Ok(ResolvedInput::Track(Box::new(cached_track)));
            }
        } else if let Some(mut cached_track) =
            crate::audio::source::cache_get_metadata(query_trimmed).await
        {
            cached_track.requester_id = serenity::UserId::new(user_id);
            tracing::debug!(query = %query_trimmed, cache = "hit", "cache_hit");
            return Ok(ResolvedInput::Track(Box::new(cached_track)));
        }

        tracing::debug!(query = %query_trimmed, cache = "miss", "cache_miss");

        let spotify_provider = SpotifyProvider;
        let apple_provider = AppleMusicProvider;
        let deezer_provider = DeezerProvider;
        let youtube_provider = YouTubeProvider;
        let soundcloud_provider = SoundCloudProvider;
        let direct_provider = DirectUrlProvider;

        if spotify_provider.supports(query_trimmed) {
            let meta =
                if let Some(track_id) = extract_spotify_id(query_trimmed, "spotify.com/track/") {
                    resolve_spotify_track(&track_id, http_client).await?
                } else {
                    spotify_provider
                        .resolve_metadata(query_trimmed, http_client)
                        .await?
                };
            let res = mirror_metadata(
                query_trimmed,
                &meta,
                user_id,
                http_client,
                "Spotify".to_owned(),
            )
            .await?;
            if let ResolvedInput::Track(ref track) = res {
                crate::audio::source::cache_set_url_metadata(
                    query_trimmed.to_owned(),
                    track.as_ref().clone(),
                )
                .await;
            }
            Ok(res)
        } else if apple_provider.supports(query_trimmed) {
            let meta = apple_provider
                .resolve_metadata(query_trimmed, http_client)
                .await?;
            let res = mirror_metadata(
                query_trimmed,
                &meta,
                user_id,
                http_client,
                "Apple Music".to_owned(),
            )
            .await?;
            if let ResolvedInput::Track(ref track) = res {
                crate::audio::source::cache_set_url_metadata(
                    query_trimmed.to_owned(),
                    track.as_ref().clone(),
                )
                .await;
            }
            Ok(res)
        } else if deezer_provider.supports(query_trimmed) {
            let meta = deezer_provider
                .resolve_metadata(query_trimmed, http_client)
                .await?;
            let res = mirror_metadata(
                query_trimmed,
                &meta,
                user_id,
                http_client,
                "Deezer".to_owned(),
            )
            .await?;
            if let ResolvedInput::Track(ref track) = res {
                crate::audio::source::cache_set_url_metadata(
                    query_trimmed.to_owned(),
                    track.as_ref().clone(),
                )
                .await;
            }
            Ok(res)
        } else if youtube_provider.supports(query_trimmed) {
            if rusty_ytdl::search::Playlist::is_playlist(query_trimmed) {
                let limit = crate::audio::runtime::max_playlist_import();
                match resolve_youtube_playlist(query_trimmed, limit, user_id, http_client).await {
                    Ok(tracks) => return Ok(ResolvedInput::Playlist(tracks)),
                    Err(e) => {
                        let has_video_context = query_trimmed.contains("watch?v=") || query_trimmed.contains("youtu.be/");
                        if has_video_context {
                            tracing::warn!("Failed to resolve YouTube playlist, falling back to single track loader: {:?}", e);
                        } else {
                            return Err(e);
                        }
                    }
                }
            }
            let mut tracks = youtube_provider
                .load(query_trimmed, user_id, http_client)
                .await?;
            if !tracks.is_empty() {
                let mut track = tracks.remove(0);
                track.requester_id = serenity::UserId::new(user_id);
                crate::audio::source::cache_set_url_metadata(
                    query_trimmed.to_owned(),
                    track.clone(),
                )
                .await;
                Ok(ResolvedInput::Track(Box::new(track)))
            } else {
                Err(SerenyaError::Audio(
                    "Failed to load YouTube track".to_owned(),
                ))
            }
        } else if soundcloud_provider.supports(query_trimmed) {
            let tracks = soundcloud_provider
                .load(query_trimmed, user_id, http_client)
                .await?;
            if !tracks.is_empty() {
                if tracks.len() > 1 {
                    Ok(ResolvedInput::Playlist(tracks))
                } else {
                    let mut track = tracks[0].clone();
                    track.requester_id = serenity::UserId::new(user_id);
                    crate::audio::source::cache_set_url_metadata(
                        query_trimmed.to_owned(),
                        track.clone(),
                    )
                    .await;
                    Ok(ResolvedInput::Track(Box::new(track)))
                }
            } else {
                Err(SerenyaError::Audio(
                    "Failed to load SoundCloud track".to_owned(),
                ))
            }
        } else if direct_provider.supports(query_trimmed) {
            let mut tracks = direct_provider.load(query_trimmed, user_id).await?;
            if !tracks.is_empty() {
                let mut track = tracks.remove(0);
                track.requester_id = serenity::UserId::new(user_id);
                crate::audio::source::cache_set_url_metadata(
                    query_trimmed.to_owned(),
                    track.clone(),
                )
                .await;
                Ok(ResolvedInput::Track(Box::new(track)))
            } else {
                Err(SerenyaError::Audio(
                    "Failed to load direct URL track".to_owned(),
                ))
            }
        } else {
            tracing::info!(
                query = %query_trimmed,
                "Collecting user-selectable search results from all providers"
            );
            let tracks = collect_search_results(query_trimmed, user_id, http_client).await?;

            if tracks.is_empty() {
                return Err(SerenyaError::Audio("No search candidates found".to_owned()));
            }

            Ok(ResolvedInput::SearchResults(tracks))
        }
    }
    .await;

    let elapsed = start_time.elapsed().as_millis();
    tracing::info!(
        query = %query_trimmed,
        resolver_elapsed_ms = elapsed,
        "resolver_resolved"
    );
    res
}

/// Helper function to perform mirroring of metadata (Spotify/Apple/Deezer) to a playable track.
async fn mirror_metadata(
    original_query: &str,
    meta: &ExternalTrackMeta,
    user_id: u64,
    http_client: &reqwest::Client,
    source_provider: String,
) -> Result<ResolvedInput, SerenyaError> {
    let search_query = if let Some(ref artist) = meta.artist {
        format!("{} - {}", artist, meta.title)
    } else {
        meta.title.clone()
    };

    tracing::info!(
        query = %search_query,
        "Mirroring external metadata to YouTube/SoundCloud/YouTubeMusic search query"
    );

    let mut scored = perform_parallel_search(
        &search_query,
        &meta.title,
        meta.artist.as_deref(),
        meta.duration,
        http_client,
    )
    .await?;

    // If the results are poor (e.g. max score < 0.80) and we searched with an artist,
    // the artist name might have confused the search engine. Fall back to searching just the title.
    if meta.artist.is_some()
        && (scored.is_empty() || scored.first().map(|(_, s)| *s).unwrap_or(0.0) < 0.80)
    {
        tracing::info!(
            query = %meta.title,
            "First mirror search yielded poor results. Retrying with only title."
        );
        let fallback_scored = perform_parallel_search(
            &meta.title,
            &meta.title,
            meta.artist.as_deref(),
            meta.duration,
            http_client,
        )
        .await?;

        if let Some((_, best_new)) = fallback_scored.first()
            && *best_new > scored.first().map(|(_, s)| *s).unwrap_or(0.0)
        {
            scored = fallback_scored;
        }
    }

    if scored.is_empty() {
        return Err(SerenyaError::Audio(format!(
            "Failed to find playable YouTube/SoundCloud candidate for: {}",
            meta.title
        )));
    }

    let display_title = Some(metadata_display_title(meta));

    evaluate_confidence_and_respond(
        original_query,
        scored,
        user_id,
        meta.thumbnail.clone(),
        display_title,
        meta.duration,
        source_provider,
    )
}

fn metadata_display_title(meta: &ExternalTrackMeta) -> String {
    meta.title.clone()
}

/// Evaluates candidate scores, caches the result if confidence is high, and constructs the appropriate ResolvedInput.
fn evaluate_confidence_and_respond(
    original_query: &str,
    scored: Vec<(TrackCandidate, f64)>,
    user_id: u64,
    forced_thumbnail: Option<std::sync::Arc<str>>,
    forced_title: Option<String>,
    forced_duration: Option<Duration>,
    source_provider: String,
) -> Result<ResolvedInput, SerenyaError> {
    let (top_cand, top_score) = &scored[0];
    let settings = crate::audio::runtime::settings();
    let variant_context = forced_title
        .as_deref()
        .map(|title| format!("{original_query} {title}"))
        .unwrap_or_else(|| original_query.to_owned());

    let has_critical = has_critical_risks(
        top_cand,
        original_query,
        forced_title.as_deref().unwrap_or(original_query),
        None,
        MetadataConfidence::Trusted,
    );

    let variant_conflict =
        contains_unrequested_variant(&top_cand.title, &variant_context) || has_critical;

    let mut low_confidence = *top_score < settings.auto_pick_threshold || variant_conflict;

    if !low_confidence && scored.len() > 1 {
        let second_score = scored[1].1;
        let margin = top_score - second_score;
        let required_margin = if *top_score >= 0.96 { 0.05 } else { 0.08 };
        if margin < required_margin {
            tracing::info!(
                top_score = %top_score,
                second_score = %second_score,
                margin = %margin,
                required_margin = %required_margin,
                "Ambiguous match, presenting select menu options"
            );
            low_confidence = true;
        }
    }

    if low_confidence {
        tracing::info!(
            top_score = %top_score,
            original_query = %original_query,
            variant_conflict,
            "Low confidence match, presenting select menu options"
        );
        let mut tracks = Vec::new();
        for (cand, score) in scored.into_iter().take(5) {
            tracks.push(Track {
                title: candidate_display_title(&cand).into(),
                url: cand.url.into(),
                duration: cand.duration,
                requester_id: serenity::UserId::new(user_id),
                requester_name: None,
                source_type: SourceType::Search,
                resolved_url: None,
                thumbnail: forced_thumbnail.clone().or(cand.thumbnail),
                source_provider: std::sync::Arc::from(format!(
                    "{} • {:.0}%",
                    cand.source,
                    score * 100.0
                )),
            });
        }
        Ok(ResolvedInput::SearchResults(tracks))
    } else {
        let selected_provider = if source_provider == top_cand.source.as_str() {
            top_cand.source.clone()
        } else {
            format!("{} -> {}", source_provider, top_cand.source)
        };
        let clean_source = if let Some(pos) = selected_provider.find(" -> ") {
            selected_provider[..pos].trim()
        } else {
            selected_provider.as_str()
        };
        tracing::info!(
            user_id,
            query = %original_query,
            candidate_title = %top_cand.title,
            candidate_duration = ?top_cand.duration,
            score = %top_score,
            selected_source = %clean_source,
            source_chain = %selected_provider,
            cache = "miss",
            "track_resolved"
        );
        tracing::info!(
            top_score = %top_score,
            track = %top_cand.title,
            selected_provider = %top_cand.source,
            "High confidence match, auto-picking"
        );
        let track = Track {
            title: forced_title
                .unwrap_or_else(|| candidate_display_title(top_cand))
                .into(),
            url: top_cand.url.clone().into(),
            duration: forced_duration.or(top_cand.duration),
            requester_id: serenity::UserId::new(user_id),
            requester_name: None,
            source_type: SourceType::Search,
            resolved_url: None,
            thumbnail: forced_thumbnail.or_else(|| top_cand.thumbnail.clone()),
            source_provider: std::sync::Arc::from(selected_provider),
        };

        // Cache the high-confidence search result asynchronously to keep it non-blocking
        let query_str = original_query.to_owned();
        let track_c = track.clone();
        tokio::spawn(async move {
            if query_str.starts_with("http://") || query_str.starts_with("https://") {
                crate::audio::source::cache_set_url_metadata(query_str, track_c).await;
            } else {
                crate::audio::source::cache_set_metadata(query_str, track_c).await;
            }
        });

        Ok(ResolvedInput::Track(Box::new(track)))
    }
}

fn candidate_display_title(candidate: &TrackCandidate) -> String {
    if candidate.artist == "Unknown Artist"
        || candidate.artist == "SoundCloud Artist"
        || candidate
            .title
            .to_lowercase()
            .contains(&candidate.artist.to_lowercase())
    {
        candidate.title.clone()
    } else {
        format!("{} - {}", candidate.artist, candidate.title)
    }
}

fn extract_spotify_id(url: &str, pattern: &str) -> Option<String> {
    if let Some(pos) = url.find(pattern) {
        let start = pos + pattern.len();
        let remaining = &url[start..];
        let end = remaining.find('?').unwrap_or(remaining.len());
        let end = remaining[..end].find('/').unwrap_or(end);
        let id = remaining[..end].trim();
        if !id.is_empty() {
            return Some(id.to_owned());
        }
    }
    None
}

#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)]
struct EmbedTrack {
    uri: String,
    title: String,
    subtitle: Option<String>,
    duration: Option<u64>,
}

#[derive(serde::Deserialize, Debug)]
struct EmbedCoverArtSource {
    url: String,
}

#[derive(serde::Deserialize, Debug)]
struct EmbedCoverArt {
    sources: Vec<EmbedCoverArtSource>,
}

#[derive(serde::Deserialize, Debug)]
struct EmbedEntity {
    #[serde(rename = "trackList")]
    track_list: Option<Vec<EmbedTrack>>,
    #[serde(rename = "coverArt")]
    cover_art: Option<EmbedCoverArt>,
}

#[derive(serde::Deserialize, Debug)]
struct EmbedData {
    entity: Option<EmbedEntity>,
}

#[derive(serde::Deserialize, Debug)]
struct EmbedState {
    data: Option<EmbedData>,
}

#[derive(serde::Deserialize, Debug)]
struct EmbedPageProps {
    state: Option<EmbedState>,
}

#[derive(serde::Deserialize, Debug)]
struct EmbedProps {
    #[serde(rename = "pageProps")]
    page_props: Option<EmbedPageProps>,
}

#[derive(serde::Deserialize, Debug)]
struct NextDataJson {
    props: Option<EmbedProps>,
}

fn extract_next_data(html: &str) -> Option<&str> {
    let patterns = [
        "<script id=\"__NEXT_DATA__\" type=\"application/json\">",
        "<script type=\"application/json\" id=\"__NEXT_DATA__\">",
    ];
    for pattern in patterns {
        if let Some(pos) = html.find(pattern) {
            let start_idx = pos + pattern.len();
            let remaining = &html[start_idx..];
            if let Some(end_idx) = remaining.find("</script>") {
                return Some(&remaining[..end_idx]);
            }
        }
    }
    None
}

async fn resolve_spotify_embed_fallback(
    url: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    let response = http_client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Referer", "https://open.spotify.com/")
        .send()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Failed to send Spotify embed request: {}", e)))?;

    if !response.status().is_success() {
        return Err(SerenyaError::Audio(format!(
            "Spotify embed page returned status: {}",
            response.status()
        )));
    }

    let html = response
        .text()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Failed to read Spotify embed HTML: {}", e)))?;

    let json_str = extract_next_data(&html).ok_or_else(|| {
        SerenyaError::Audio("Failed to extract __NEXT_DATA__ from Spotify embed HTML".into())
    })?;

    let data: NextDataJson = serde_json::from_str(json_str)
        .map_err(|e| SerenyaError::Audio(format!("Failed to parse __NEXT_DATA__ JSON: {}", e)))?;

    let entity = data
        .props
        .and_then(|p| p.page_props)
        .and_then(|pp| pp.state)
        .and_then(|s| s.data)
        .and_then(|d| d.entity)
        .ok_or_else(|| SerenyaError::Audio("Missing entity in Spotify embed JSON".into()))?;

    let track_list = entity.track_list.unwrap_or_default();
    let entity_thumbnail = entity
        .cover_art
        .and_then(|ca| ca.sources.first().map(|src| src.url.clone()));

    let mut tracks = Vec::new();
    for embed_track in track_list.into_iter().take(limit) {
        if embed_track.uri.starts_with("spotify:track:") {
            let display_title = embed_track.title.clone();

            // Use YouTube search instead of Spotify URL — Spotify URLs are DRM-protected
            // and yt-dlp cannot resolve stream URLs from them
            let search_query = if let Some(ref artist) = embed_track.subtitle {
                format!("{} - {}", artist, embed_track.title)
            } else {
                embed_track.title.clone()
            };
            let track_url = format!("ytsearch1:{}", search_query);

            tracks.push(Track {
                title: display_title.into(),
                url: track_url.into(),
                duration: embed_track.duration.map(Duration::from_millis),
                requester_id: serenity::UserId::new(user_id),
                requester_name: None,
                source_type: SourceType::Url,
                resolved_url: None,
                thumbnail: entity_thumbnail.clone().map(std::sync::Arc::from),
                source_provider: std::sync::Arc::from("Spotify"),
            });
        }
    }

    Ok(tracks)
}

async fn resolve_spotify_playlist_fallback(
    playlist_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::info!(
        "Attempting Spotify playlist fallback scraping for ID: {}",
        playlist_id
    );
    let url = format!("https://open.spotify.com/embed/playlist/{}", playlist_id);
    let tracks = resolve_spotify_embed_fallback(&url, limit, user_id, http_client).await?;
    tracing::info!(
        "Successfully resolved {} tracks via Spotify playlist fallback scraper",
        tracks.len()
    );
    Ok(tracks)
}

async fn resolve_spotify_album_fallback(
    album_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::info!(
        "Attempting Spotify album fallback scraping for ID: {}",
        album_id
    );
    let url = format!("https://open.spotify.com/embed/album/{}", album_id);
    let tracks = resolve_spotify_embed_fallback(&url, limit, user_id, http_client).await?;
    tracing::info!(
        "Successfully resolved {} tracks via Spotify album fallback scraper",
        tracks.len()
    );
    Ok(tracks)
}

async fn spotify_partner_post(
    http_client: &reqwest::Client,
    gql_body: &serde_json::Value,
) -> Result<serde_json::Value, SerenyaError> {
    let session_info =
        crate::audio::providers::get_spotify_session_info(http_client, Duration::from_secs(5))
            .await?;

    let client_token_info = crate::audio::providers::get_spotify_client_token_info(
        http_client,
        &session_info.client_id,
        Duration::from_secs(5),
    )
    .await?;

    let sp_dc = crate::audio::runtime::spotify_settings()
        .and_then(|config| config.sp_dc.clone())
        .filter(|cookie| !cookie.trim().is_empty())
        .ok_or_else(|| SerenyaError::Audio("Spotify sp_dc cookie is not configured.".to_owned()))?;

    let mut response = None;
    let mut attempts = 0;
    let mut backoff_ms = 500u64;
    const MAX_BACKOFF_MS: u64 = 5000;
    
    while attempts < 3 {
        attempts += 1;
        match http_client
            .post("https://api-partner.spotify.com/pathfinder/v1/query")
            .json(gql_body)
            .header("Authorization", format!("Bearer {}", session_info.access_token))
            .header("Client-Token", &client_token_info.client_token)
            .header("Spotify-App-Version", &client_token_info.client_version)
            .header("Accept", "application/json")
            .header("Accept-Language", "en")
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36")
            .header("Referer", "https://open.spotify.com/")
            .header("Origin", "https://open.spotify.com")
            .header("Cookie", format!("sp_dc={}; sp_t={}", sp_dc, client_token_info.device_id))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    response = Some(resp);
                    break;
                } else if status.as_u16() == 429 {
                    // Rate limited - check if it's a global or endpoint-specific limit
                    let retry_after = resp
                        .headers()
                        .get("Retry-After")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(2);
                    
                    // Check X-Rate-Limit-* headers to distinguish between limits
                    let is_global_limit = resp
                        .headers()
                        .get("X-Rate-Limit-Status")
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.contains("global"))
                        .unwrap_or(false);
                    
                    tracing::warn!(
                        "Spotify Partner API rate limited (429). {} (attempt {}/3). Retrying after {} seconds...",
                        if is_global_limit { "Global limit" } else { "Endpoint limit" },
                        attempts,
                        retry_after
                    );
                    tokio::time::sleep(Duration::from_secs(retry_after)).await;
                } else {
                    // Log error response body for better debugging
                    let error_body = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "<could not read response body>".to_owned());
                    tracing::warn!(
                        "Spotify Partner API request failed with status: {} (attempt {}/3). Response: {}",
                        status,
                        attempts,
                        error_body
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                }
            }
            Err(e) => {
                tracing::warn!("Spotify Partner API request failed: {} (attempt {}/3)", e, attempts);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
        }
    }

    let response = response.ok_or_else(|| {
        SerenyaError::Audio("Spotify Partner API request failed after 3 attempts.".to_owned())
    })?;

    let body = response.json::<serde_json::Value>().await.map_err(|e| {
        SerenyaError::Audio(format!(
            "Failed to parse Spotify Partner API JSON response: {e}"
        ))
    })?;

    if let Some(errors) = body.get("errors").and_then(|e| e.as_array())
        && !errors.is_empty()
    {
        let first_err = errors[0]
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown GraphQL error");
        return Err(SerenyaError::Audio(format!(
            "Spotify GraphQL error: {}",
            first_err
        )));
    }

    Ok(body)
}

async fn resolve_spotify_playlist_api(
    playlist_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    let mut tracks = Vec::with_capacity(limit);
    let source_prov: std::sync::Arc<str> = std::sync::Arc::from("Spotify");
    let mut offset = 0;
    let mut total_count = None;

    while tracks.len() < limit {
        let chunk_limit = (limit - tracks.len()).min(100);

        tracing::debug!(
            "Fetching Spotify playlist tracks: offset = {}, limit = {} (total resolved so far: {})",
            offset,
            chunk_limit,
            tracks.len()
        );

        let gql_hash = "a65e12194ed5fc443a1cdebed5fabe33ca5b07b987185d63c72483867ad13cb4";
        let gql_body = serde_json::json!({
            "operationName": "fetchPlaylist",
            "variables": {
                "uri": format!("spotify:playlist:{}", playlist_id),
                "offset": offset,
                "limit": chunk_limit,
                "enableWatchFeedEntrypoint": false
            },
            "extensions": {
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": gql_hash
                }
            }
        });

        let body = spotify_partner_post(http_client, &gql_body).await?;

        let playlist_v2 = body.pointer("/data/playlistV2").ok_or_else(|| {
            SerenyaError::Audio("Missing playlistV2 in Spotify GraphQL response".to_owned())
        })?;

        if playlist_v2.get("__typename").and_then(|t| t.as_str()) == Some("NotFound") {
            return Err(SerenyaError::Audio(
                "Spotify playlist not found or access denied".to_owned(),
            ));
        }

        let content = playlist_v2.get("content").ok_or_else(|| {
            SerenyaError::Audio("Missing content in Spotify playlistV2".to_owned())
        })?;

        if total_count.is_none() {
            let tc = content
                .get("totalCount")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            total_count = Some(tc as usize);
            tracing::debug!("Spotify playlist total track count: {}", tc);
        }

        let items = content
            .get("items")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                SerenyaError::Audio("Missing items in Spotify playlist content".to_owned())
            })?;

        if items.is_empty() {
            tracing::debug!("No more items found in Spotify playlist response.");
            break;
        }

        let items_len = items.len();
        for item in items {
            let track_val = item
                .pointer("/itemV2/data")
                .or_else(|| item.pointer("/itemV3/data"));
            let Some(track_val) = track_val else {
                continue;
            };

            let Some(name) = track_val.get("name").and_then(|v| v.as_str()) else {
                continue;
            };

            let duration_ms = track_val
                .pointer("/trackDuration/totalMilliseconds")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    track_val
                        .pointer("/consumptionExperienceTrait/duration/seconds")
                        .and_then(|v| v.as_u64())
                        .map(|s| s * 1000)
                })
                .unwrap_or(0);

            let artist_str = extract_artist_string(
                track_val
                    .pointer("/artists/items")
                    .or_else(|| track_val.pointer("/identityTrait/contributors/items"))
                    .and_then(|v| v.as_array()),
            );

            let thumbnail = track_val
                .pointer("/albumOfTrack/coverArt/sources")
                .or_else(|| {
                    track_val.pointer("/visualIdentityTrait/squareCoverImage/image/data/sources")
                })
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|img| img.get("url"))
                .and_then(|v| v.as_str())
                .map(std::sync::Arc::from);

            let search_query = if artist_str.is_empty() {
                name.to_owned()
            } else {
                format!("{} - {}", artist_str, name)
            };
            let track_url = format!("ytsearch1:{}", search_query);

            tracks.push(Track {
                title: name.into(),
                url: track_url.into(),
                duration: Some(Duration::from_millis(duration_ms)),
                requester_id: serenity::UserId::new(user_id),
                requester_name: None,
                source_type: SourceType::Url,
                resolved_url: None,
                thumbnail,
                source_provider: source_prov.clone(),
            });

            if tracks.len() >= limit {
                break;
            }
        }

        tracing::info!(
            "Successfully processed {} tracks from current batch.",
            items_len
        );

        offset += items_len;
        if let Some(tc) = total_count
            && offset >= tc
        {
            break;
        }
    }

    tracing::debug!(
        "Finished Spotify API playlist resolution. Total resolved: {} tracks",
        tracks.len()
    );
    Ok(tracks)
}

async fn resolve_spotify_playlist(
    playlist_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::debug!(
        "Resolving Spotify playlist ID: {} (limit: {})",
        playlist_id,
        limit
    );
    if playlist_id.starts_with("37i9dQZF") {
        if !crate::audio::runtime::is_spotify_embed_fallback_active() {
            return Err(SerenyaError::Audio(
                "Spotify embed fallback is disabled (required for Spotify-curated playlist)".into(),
            ));
        }
        tracing::debug!(
            "Playlist {} is Spotify-curated. Bypassing Partner API and using embed scraper fallback directly.",
            playlist_id
        );
        return resolve_spotify_playlist_fallback(playlist_id, limit, user_id, http_client).await;
    }
    if let Some(config) = crate::audio::runtime::spotify_settings() {
        if config
            .sp_dc
            .as_ref()
            .map(|cookie| !cookie.trim().is_empty())
            .unwrap_or(false)
        {
            tracing::debug!(
                "Spotify sp_dc cookie found. Attempting resolution via Spotify Web API..."
            );
            match resolve_spotify_playlist_api(playlist_id, limit, user_id, http_client).await {
                Ok(tracks) => {
                    tracing::debug!(
                        "Successfully resolved {} tracks via Spotify Web API",
                        tracks.len()
                    );
                    return Ok(tracks);
                }
                Err(err) => {
                    if !crate::audio::runtime::is_spotify_embed_fallback_active() {
                        return Err(err);
                    }
                    tracing::warn!(
                        "Spotify Web API playlist resolution failed ({:?}). Falling back to Spotify embed scraper...",
                        err
                    );
                }
            }
        } else {
            if !crate::audio::runtime::is_spotify_embed_fallback_active() {
                return Err(SerenyaError::Audio(
                    "Spotify sp_dc cookie missing in config and embed fallback is disabled".into(),
                ));
            }
            tracing::warn!(
                "Spotify sp_dc cookie missing in config. Falling back to Spotify embed scraper."
            );
        }
    } else {
        if !crate::audio::runtime::is_spotify_embed_fallback_active() {
            return Err(SerenyaError::Audio(
                "Spotify settings missing in config and embed fallback is disabled".into(),
            ));
        }
        tracing::warn!(
            "Spotify settings missing in config. Falling back to Spotify embed scraper."
        );
    }
    resolve_spotify_playlist_fallback(playlist_id, limit, user_id, http_client).await
}

async fn resolve_spotify_album_api(
    album_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    let mut tracks = Vec::with_capacity(limit);
    let source_prov: std::sync::Arc<str> = std::sync::Arc::from("Spotify");
    let mut offset = 0;
    let mut total_count = None;

    while tracks.len() < limit {
        let chunk_limit = (limit - tracks.len()).min(100);

        tracing::debug!(
            "Fetching Spotify album tracks: offset = {}, limit = {} (total resolved so far: {})",
            offset,
            chunk_limit,
            tracks.len()
        );

        let gql_hash = "b9bfabef66ed756e5e13f68a942deb60bd4125ec1f1be8cc42769dc0259b4b10";
        let gql_body = serde_json::json!({
            "operationName": "queryAlbumTracks",
            "variables": {
                "uri": format!("spotify:album:{}", album_id),
                "offset": offset,
                "limit": chunk_limit
            },
            "extensions": {
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": gql_hash
                }
            }
        });

        let body = spotify_partner_post(http_client, &gql_body).await?;

        let album_val = body
            .pointer("/data/albumUnion")
            .or_else(|| body.pointer("/data/album"))
            .ok_or_else(|| {
                SerenyaError::Audio("Missing album in Spotify GraphQL response".to_owned())
            })?;

        if album_val.get("__typename").and_then(|t| t.as_str()) == Some("NotFound") {
            return Err(SerenyaError::Audio(
                "Spotify album not found or access denied".to_owned(),
            ));
        }

        let thumbnail = album_val
            .pointer("/coverArt/sources")
            .or_else(|| album_val.pointer("/images"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|img| img.get("url"))
            .and_then(|v| v.as_str())
            .map(std::sync::Arc::from);

        let tracks_val = album_val
            .get("tracks")
            .ok_or_else(|| SerenyaError::Audio("Missing tracks in Spotify album".to_owned()))?;

        if total_count.is_none() {
            let tc = tracks_val
                .get("totalCount")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            total_count = Some(tc as usize);
            tracing::debug!("Spotify album total track count: {}", tc);
        }

        let items = tracks_val
            .get("items")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                SerenyaError::Audio("Missing items in Spotify album tracks".to_owned())
            })?;

        if items.is_empty() {
            break;
        }

        let items_len = items.len();
        for item in items {
            let track_val = item.get("track").unwrap_or(item);
            let Some(name) = track_val.get("name").and_then(|v| v.as_str()) else {
                continue;
            };

            let duration_ms = track_val
                .pointer("/duration/totalMilliseconds")
                .and_then(|v| v.as_u64())
                .or_else(|| track_val.get("duration_ms").and_then(|v| v.as_u64()))
                .unwrap_or(0);

            let artist_str = extract_artist_string(
                track_val
                    .pointer("/artists/items")
                    .or_else(|| track_val.pointer("/profile/name"))
                    .and_then(|v| v.as_array()),
            );

            let search_query = if artist_str.is_empty() {
                name.to_owned()
            } else {
                format!("{} - {}", artist_str, name)
            };
            let track_url = format!("ytsearch1:{}", search_query);

            tracks.push(Track {
                title: name.into(),
                url: track_url.into(),
                duration: Some(Duration::from_millis(duration_ms)),
                requester_id: serenity::UserId::new(user_id),
                requester_name: None,
                source_type: SourceType::Url,
                resolved_url: None,
                thumbnail: thumbnail.clone(),
                source_provider: source_prov.clone(),
            });

            if tracks.len() >= limit {
                break;
            }
        }

        offset += items_len;
        if let Some(tc) = total_count
            && offset >= tc
        {
            break;
        }
    }

    tracing::debug!(
        "Finished Spotify API album resolution. Total resolved: {} tracks",
        tracks.len()
    );
    Ok(tracks)
}

async fn resolve_spotify_album(
    album_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::debug!(
        "Resolving Spotify album ID: {} (limit: {})",
        album_id,
        limit
    );
    if let Some(config) = crate::audio::runtime::spotify_settings() {
        if config
            .sp_dc
            .as_ref()
            .map(|cookie| !cookie.trim().is_empty())
            .unwrap_or(false)
        {
            tracing::debug!(
                "Spotify sp_dc cookie found. Attempting resolution via Spotify Web API..."
            );
            match resolve_spotify_album_api(album_id, limit, user_id, http_client).await {
                Ok(tracks) => {
                    tracing::debug!(
                        "Successfully resolved {} tracks via Spotify Web API",
                        tracks.len()
                    );
                    return Ok(tracks);
                }
                Err(err) => {
                    if !crate::audio::runtime::is_spotify_embed_fallback_active() {
                        return Err(err);
                    }
                    tracing::warn!(
                        "Spotify Web API album resolution failed ({:?}). Falling back to Spotify embed scraper...",
                        err
                    );
                }
            }
        } else {
            if !crate::audio::runtime::is_spotify_embed_fallback_active() {
                return Err(SerenyaError::Audio(
                    "Spotify sp_dc cookie missing in config and embed fallback is disabled".into(),
                ));
            }
            tracing::warn!(
                "Spotify sp_dc cookie missing in config. Falling back to Spotify embed scraper."
            );
        }
    } else {
        if !crate::audio::runtime::is_spotify_embed_fallback_active() {
            return Err(SerenyaError::Audio(
                "Spotify settings missing in config and embed fallback is disabled".into(),
            ));
        }
        tracing::warn!(
            "Spotify settings missing in config. Falling back to Spotify embed scraper."
        );
    }
    resolve_spotify_album_fallback(album_id, limit, user_id, http_client).await
}

async fn resolve_spotify_artist_top_tracks_fallback(
    artist_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::debug!(
        "Attempting Spotify artist fallback scraping for ID: {}",
        artist_id
    );
    let url = format!("https://open.spotify.com/embed/artist/{}", artist_id);
    let tracks = resolve_spotify_embed_fallback(&url, limit, user_id, http_client).await?;
    tracing::debug!(
        "Successfully resolved {} tracks via Spotify artist fallback scraper",
        tracks.len()
    );
    Ok(tracks)
}

async fn resolve_spotify_artist_top_tracks_api(
    artist_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    let gql_hash = "7f86ff63e38c24973a2842b672abe44c910c1973978dc8a4a0cb648edef34527";
    let gql_body = serde_json::json!({
        "operationName": "queryArtistOverview",
        "variables": {
            "uri": format!("spotify:artist:{}", artist_id),
            "locale": "en",
            "includePrerelease": false
        },
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": gql_hash
            }
        }
    });

    let body = spotify_partner_post(http_client, &gql_body).await?;

    let artist_val = body
        .pointer("/data/artistUnion")
        .or_else(|| body.pointer("/data/artist"))
        .ok_or_else(|| {
            SerenyaError::Audio("Missing artist in Spotify GraphQL response".to_owned())
        })?;

    if artist_val.get("__typename").and_then(|t| t.as_str()) == Some("NotFound") {
        return Err(SerenyaError::Audio(
            "Spotify artist not found or access denied".to_owned(),
        ));
    }

    let items = artist_val
        .pointer("/discography/topTracks/items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            SerenyaError::Audio("Missing topTracks items in Spotify artist response".to_owned())
        })?;

    tracing::debug!(
        "Processing Spotify artist top tracks (items: {})",
        items.len()
    );
    let mut tracks = Vec::with_capacity(limit.min(items.len()));
    let source_prov: std::sync::Arc<str> = std::sync::Arc::from("Spotify");
    for item in items.iter().take(limit) {
        let track_val = item.get("track").unwrap_or(item);
        let Some(name) = track_val.get("name").and_then(|v| v.as_str()) else {
            continue;
        };

        let duration_ms = track_val
            .pointer("/duration/totalMilliseconds")
            .and_then(|v| v.as_u64())
            .or_else(|| track_val.get("duration_ms").and_then(|v| v.as_u64()))
            .unwrap_or(0);

        let artist_str = extract_artist_string(
            track_val
                .pointer("/artists/items")
                .or_else(|| track_val.pointer("/profile/name"))
                .and_then(|v| v.as_array()),
        );

        let thumbnail = track_val
            .pointer("/album/coverArt/sources")
            .or_else(|| track_val.pointer("/albumOfTrack/coverArt/sources"))
            .or_else(|| track_val.pointer("/album/images"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|img| img.get("url"))
            .and_then(|v| v.as_str())
            .map(std::sync::Arc::from);

        let search_query = if artist_str.is_empty() {
            name.to_owned()
        } else {
            format!("{} - {}", artist_str, name)
        };
        let track_url = format!("ytsearch1:{}", search_query);

        tracks.push(Track {
            title: name.into(),
            url: track_url.into(),
            duration: Some(Duration::from_millis(duration_ms)),
            requester_id: serenity::UserId::new(user_id),
            requester_name: None,
            source_type: SourceType::Url,
            resolved_url: None,
            thumbnail,
            source_provider: source_prov.clone(),
        });
    }

    tracing::debug!(
        "Finished Spotify API artist top tracks resolution. Total resolved: {} tracks",
        tracks.len()
    );
    Ok(tracks)
}

async fn resolve_spotify_artist_top_tracks(
    artist_id: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::debug!(
        "Resolving Spotify artist ID: {} (limit: {})",
        artist_id,
        limit
    );
    if let Some(config) = crate::audio::runtime::spotify_settings() {
        if config
            .sp_dc
            .as_ref()
            .map(|cookie| !cookie.trim().is_empty())
            .unwrap_or(false)
        {
            tracing::debug!(
                "Spotify sp_dc cookie found. Attempting resolution via Spotify Web API..."
            );
            match resolve_spotify_artist_top_tracks_api(artist_id, limit, user_id, http_client)
                .await
            {
                Ok(tracks) => {
                    tracing::debug!(
                        "Successfully resolved {} tracks via Spotify Web API",
                        tracks.len()
                    );
                    return Ok(tracks);
                }
                Err(err) => {
                    if !crate::audio::runtime::is_spotify_embed_fallback_active() {
                        return Err(err);
                    }
                    tracing::warn!(
                        "Spotify Web API artist top tracks resolution failed ({:?}). Falling back to Spotify embed scraper...",
                        err
                    );
                }
            }
        } else {
            if !crate::audio::runtime::is_spotify_embed_fallback_active() {
                return Err(SerenyaError::Audio(
                    "Spotify sp_dc cookie missing in config and embed fallback is disabled".into(),
                ));
            }
            tracing::warn!(
                "Spotify sp_dc cookie missing in config. Falling back to Spotify embed scraper."
            );
        }
    } else {
        if !crate::audio::runtime::is_spotify_embed_fallback_active() {
            return Err(SerenyaError::Audio(
                "Spotify settings missing in config and embed fallback is disabled".into(),
            ));
        }
        tracing::warn!(
            "Spotify settings missing in config. Falling back to Spotify embed scraper."
        );
    }
    resolve_spotify_artist_top_tracks_fallback(artist_id, limit, user_id, http_client).await
}

async fn resolve_spotify_track_api(
    track_id: &str,
    http_client: &reqwest::Client,
) -> Result<ExternalTrackMeta, SerenyaError> {
    let gql_hash = "612585ae06ba435ad26369870deaae23b5c8800a256cd8a57e08eddc25a37294";
    let gql_body = serde_json::json!({
        "operationName": "getTrack",
        "variables": {
            "uri": format!("spotify:track:{}", track_id)
        },
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": gql_hash
            }
        }
    });

    let body = spotify_partner_post(http_client, &gql_body).await?;

    let track_val = body
        .pointer("/data/trackUnion")
        .or_else(|| body.pointer("/data/track"))
        .ok_or_else(|| {
            SerenyaError::Audio("Missing track in Spotify GraphQL response".to_owned())
        })?;

    if track_val.get("__typename").and_then(|t| t.as_str()) == Some("NotFound") {
        return Err(SerenyaError::Audio(
            "Spotify track not found or access denied".to_owned(),
        ));
    }

    let title = track_val
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SerenyaError::Audio("Missing track title in Spotify response".to_owned()))?
        .to_owned();

    let artist_str = extract_artist_string(
        track_val
            .pointer("/firstArtist/items")
            .or_else(|| track_val.pointer("/artists/items"))
            .and_then(|v| v.as_array()),
    );
    let artist = if artist_str.is_empty() {
        None
    } else {
        Some(artist_str)
    };

    let duration = track_val
        .pointer("/duration/totalMilliseconds")
        .and_then(|v| v.as_u64())
        .or_else(|| track_val.get("duration_ms").and_then(|v| v.as_u64()))
        .map(Duration::from_millis);

    let thumbnail = track_val
        .pointer("/album/coverArt/sources")
        .or_else(|| track_val.pointer("/albumOfTrack/coverArt/sources"))
        .or_else(|| track_val.pointer("/album/images"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(|v| v.as_str())
        .map(std::sync::Arc::from);

    Ok(ExternalTrackMeta {
        title,
        artist,
        duration,
        thumbnail,
    })
}

async fn resolve_spotify_track(
    track_id: &str,
    http_client: &reqwest::Client,
) -> Result<ExternalTrackMeta, SerenyaError> {
    tracing::debug!("Resolving Spotify track ID: {}", track_id);
    if let Some(config) = crate::audio::runtime::spotify_settings() {
        if config
            .sp_dc
            .as_ref()
            .map(|cookie| !cookie.trim().is_empty())
            .unwrap_or(false)
        {
            tracing::debug!(
                "Spotify sp_dc cookie found. Attempting resolution via Spotify Web API..."
            );
            match resolve_spotify_track_api(track_id, http_client).await {
                Ok(meta) => {
                    tracing::debug!(
                        "Successfully resolved track via Spotify Web API: {}",
                        meta.title
                    );
                    return Ok(meta);
                }
                Err(err) => {
                    tracing::warn!(
                        "Spotify Web API track resolution failed ({:?}). Falling back to Spotify embed scraper...",
                        err
                    );
                }
            }
        } else {
            tracing::warn!(
                "Spotify sp_dc cookie missing in config. Falling back to Spotify embed scraper."
            );
        }
    } else {
        tracing::warn!(
            "Spotify settings missing in config. Falling back to Spotify embed scraper."
        );
    }
    let spotify_provider = SpotifyProvider;
    let url = format!("https://open.spotify.com/track/{track_id}");
    spotify_provider.resolve_metadata(&url, http_client).await
}

fn is_instrumental_or_non_vocal(title: &str) -> bool {
    let t = title.to_lowercase();
    t.contains("instrumental")
        || t.contains("karaoke")
        || t.contains("off vocal")
        || t.contains("minus one")
        || t.contains("backing track")
        || t.contains("lofi beat")
        || t.contains("lofi beats")
        || t.contains("piano cover")
        || t.contains("orchestra")
        || t.contains("violin")
        || t.contains("classical")
        || t.contains("bgm")
}

pub async fn resolve_ytsearch_track(
    track: &mut Track,
    http_client: &reqwest::Client,
) -> Result<(), SerenyaError> {
    if !track.url.starts_with("ytsearch1:") {
        return Ok(());
    }

    let raw_query = &track.url["ytsearch1:".len()..];
    let query = raw_query.to_owned();
    tracing::info!(query, "Resolving ytsearch1 query lazily to YouTube URL");

    let mut scored = if is_instrumental_or_non_vocal(&track.title) {
        perform_parallel_search(&query, &track.title, None, track.duration, http_client).await?
    } else {
        let query_lyrics = format!("{} lyrics", query);
        let (normal_res, lyrics_res) = tokio::join!(
            perform_parallel_search(&query, &track.title, None, track.duration, http_client),
            perform_parallel_search(
                &query_lyrics,
                &track.title,
                None,
                track.duration,
                http_client
            )
        );
        let mut combined = normal_res?;
        if let Ok(mut l_res) = lyrics_res {
            combined.append(&mut l_res);
        }
        // Deduplicate by URL
        let mut seen = std::collections::HashSet::new();
        combined.retain(|(candidate, _)| seen.insert(candidate.url.clone()));
        // Re-sort by score descending
        combined.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        combined
    };

    if !scored.is_empty() {
        let (best_candidate, score) = scored.remove(0);
        let settings = crate::audio::runtime::settings();
        if score >= settings.auto_pick_threshold {
            tracing::info!(
                query,
                resolved_url = %best_candidate.url,
                score,
                "Successfully resolved ytsearch1 to real YouTube URL"
            );
        } else {
            tracing::info!(
                query,
                resolved_url = %best_candidate.url,
                score,
                "Resolved ytsearch1 fallback candidate using top ranking score"
            );
        }
        track.url = best_candidate.url.into();
        if track.thumbnail.is_none() {
            track.thumbnail = best_candidate.thumbnail;
        }
        return Ok(());
    }

    // Safety fallback: if parallel search results are completely empty (rare)
    let mut candidates = YouTubeProvider.search(&query, http_client).await?;

    let best_candidate = if let Some(expected) = track.duration {
        candidates.sort_by(|a, b| {
            let diff_a = if let Some(d) = a.duration {
                (d.as_secs_f64() - expected.as_secs_f64()).abs()
            } else {
                3600.0
            };
            let diff_b = if let Some(d) = b.duration {
                (d.as_secs_f64() - expected.as_secs_f64()).abs()
            } else {
                3600.0
            };
            diff_a
                .partial_cmp(&diff_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.into_iter().next()
    } else {
        candidates.into_iter().next()
    };

    if let Some(candidate) = best_candidate {
        tracing::info!(
            query,
            resolved_url = %candidate.url,
            "Resolved ytsearch1 fallback candidate using duration sorting"
        );
        track.url = candidate.url.into();
        if track.thumbnail.is_none() {
            track.thumbnail = candidate.thumbnail;
        }
        Ok(())
    } else {
        Err(SerenyaError::Audio(format!(
            "Could not find search result for lazy query: {}",
            query
        )))
    }
}

fn recursive_find_tracks(
    value: &serde_json::Value,
    tracks: &mut Vec<Track>,
    seen_ids: &mut std::collections::HashSet<String>,
    limit: usize,
    user_id: u64,
    provider_name: &str,
) {
    if tracks.len() >= limit {
        return;
    }

    if let Some(obj) = value.as_object() {
        if let Some(lockup) = obj.get("lockupViewModel") {
            let title = lockup["metadata"]["lockupMetadataViewModel"]["title"]["content"]
                .as_str()
                .unwrap_or("Unknown");
            let vid = lockup["contentId"]
                .as_str()
                .or_else(|| lockup["videoCommand"]["watchEndpoint"]["videoId"].as_str())
                .unwrap_or_default();

            if !vid.is_empty() && seen_ids.insert(vid.to_string()) {
                tracks.push(Track {
                    title: title.into(),
                    url: format!("https://www.youtube.com/watch?v={}", vid).into(),
                    duration: None,
                    requester_id: serenity::UserId::new(user_id),
                    requester_name: None,
                    source_type: SourceType::Playlist,
                    resolved_url: None,
                    thumbnail: None,
                    source_provider: std::sync::Arc::from(provider_name),
                });
            }
        } else if let Some(video) = obj.get("playlistVideoRenderer") {
            let title = video["title"]["runs"][0]["text"]
                .as_str()
                .or_else(|| video["title"]["simpleText"].as_str())
                .unwrap_or("Unknown");
            let vid = video["videoId"].as_str().unwrap_or_default();

            if !vid.is_empty() && seen_ids.insert(vid.to_string()) {
                let duration_str = video["lengthText"]["simpleText"].as_str();
                let duration = duration_str.and_then(|s| {
                    let parts: Vec<&str> = s.split(':').collect();
                    let mut secs = 0u64;
                    if parts.len() == 2 {
                        let mins = parts[0].parse::<u64>().ok()?;
                        let s = parts[1].parse::<u64>().ok()?;
                        secs = mins * 60 + s;
                    } else if parts.len() == 3 {
                        let hrs = parts[0].parse::<u64>().ok()?;
                        let mins = parts[1].parse::<u64>().ok()?;
                        let s = parts[2].parse::<u64>().ok()?;
                        secs = hrs * 3600 + mins * 60 + s;
                    }
                    Some(Duration::from_secs(secs))
                });

                tracks.push(Track {
                    title: title.into(),
                    url: format!("https://www.youtube.com/watch?v={}", vid).into(),
                    duration,
                    requester_id: serenity::UserId::new(user_id),
                    requester_name: None,
                    source_type: SourceType::Playlist,
                    resolved_url: None,
                    thumbnail: video["thumbnail"]["thumbnails"]
                        .as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|t| t["url"].as_str())
                        .map(std::sync::Arc::from),
                    source_provider: std::sync::Arc::from(provider_name),
                });
            }
        } else {
            for (_, val) in obj {
                recursive_find_tracks(val, tracks, seen_ids, limit, user_id, provider_name);
            }
        }
    } else if let Some(arr) = value.as_array() {
        for val in arr {
            recursive_find_tracks(val, tracks, seen_ids, limit, user_id, provider_name);
        }
    }
}

async fn resolve_youtube_playlist(
    url: &str,
    limit: usize,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    let mut normalized_url = url.to_owned();
    if normalized_url.contains("music.youtube.com") {
        normalized_url = normalized_url.replace("music.youtube.com", "www.youtube.com");
    }

    let playlist_url = rusty_ytdl::search::Playlist::get_playlist_url(&normalized_url)
        .ok_or_else(|| SerenyaError::Audio("Invalid YouTube playlist URL".into()))?;

    let provider_name: std::sync::Arc<str> = if url.contains("music.youtube.com") {
        std::sync::Arc::from("YouTube Music")
    } else {
        std::sync::Arc::from("YouTube")
    };

    let mut tracks = Vec::new();
    let mut is_valid_playlist = false;

    let is_album = url.contains("list=OLAK") || url.contains("OLAK5uy_");

    if !is_album {
        let playlist_opts = rusty_ytdl::search::PlaylistSearchOptions {
            limit: limit as u64,
            request_options: Some(rusty_ytdl::RequestOptions {
                client: Some(http_client.clone()),
                ..Default::default()
            }),
            fetch_all: false,
        };
        if let Ok(mut playlist) =
            rusty_ytdl::search::Playlist::get(&playlist_url, Some(&playlist_opts)).await
        {
            playlist.fetch(Some(limit as u64)).await;

            let is_fake_video = playlist.videos.len() == 1
                && playlist
                    .videos
                    .first()
                    .map(|v| v.title == "Videos")
                    .unwrap_or(false);
            let mut valid_videos = Vec::new();
            if !is_fake_video {
                for video in playlist.videos {
                    valid_videos.push(video);
                }
            }

            if !valid_videos.is_empty() {
                is_valid_playlist = true;
                for video in valid_videos {
                    let duration = if video.duration > 0 {
                        Some(Duration::from_millis(video.duration))
                    } else {
                        None
                    };

                    tracks.push(Track {
                        title: video.title.into(),
                        url: video.url.into(),
                        duration,
                        requester_id: serenity::UserId::new(user_id),
                        requester_name: None,
                        source_type: SourceType::Playlist,
                        resolved_url: None,
                        thumbnail: video
                            .thumbnails
                            .first()
                            .map(|t| std::sync::Arc::from(t.url.as_str())),
                        source_provider: provider_name.clone(),
                    });
                }
            }
        }
    }

    // Fallback/Scraper for Album playlists (OLAK...) or failed playlist resolutions
    if !is_valid_playlist {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/115.0.0.0 Safari/537.36".parse().unwrap(),
        );
        headers.insert(
            reqwest::header::COOKIE,
            "SOCS=CAISNQgDEitib3FfaWRlbnRpdHlmcm9udGVuZHVpc2VydmVyXzIwMjMwODI5LjA3X3AxGgJlbiACGgYIgOmgpwY".parse().unwrap(),
        );

        if let Ok(res) = http_client
            .get(&normalized_url)
            .headers(headers)
            .send()
            .await
            && let Ok(html) = res.text().await
            && let Some(start) = html.find("ytInitialData = {")
        {
            let json_str = &html[start + 16..];
            if let Some(end) = json_str.find("};</script>") {
                let json_data = &json_str[..=end];
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_data) {
                    let mut seen_ids = std::collections::HashSet::new();
                    recursive_find_tracks(
                        &v,
                        &mut tracks,
                        &mut seen_ids,
                        limit,
                        user_id,
                        &provider_name,
                    );
                }
            }
        }
    }

    if tracks.is_empty() {
        return Err(SerenyaError::Audio(
            "Failed to retrieve YouTube playlist tracks".into(),
        ));
    }

    Ok(tracks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recursive_find_tracks() {
        let json_data = serde_json::json!({
            "contents": {
                "twoColumnBrowseResultsRenderer": {
                    "tabs": [
                        {
                            "tabRenderer": {
                                "content": {
                                    "sectionListRenderer": {
                                        "contents": [
                                            {
                                                "itemSectionRenderer": {
                                                    "contents": [
                                                        {
                                                            "lockupViewModel": {
                                                                "contentId": "video_id_1",
                                                                "metadata": {
                                                                    "lockupMetadataViewModel": {
                                                                        "title": {
                                                                            "content": "Test Lockup Title"
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        },
                                                        {
                                                            "playlistVideoRenderer": {
                                                                "videoId": "video_id_2",
                                                                "title": {
                                                                    "runs": [
                                                                        { "text": "Test Playlist Title" }
                                                                    ]
                                                                },
                                                                "lengthText": {
                                                                    "simpleText": "3:45"
                                                                }
                                                            }
                                                        },
                                                        // Duplicate ID to test deduplication
                                                        {
                                                            "lockupViewModel": {
                                                                "contentId": "video_id_1",
                                                                "metadata": {
                                                                    "lockupMetadataViewModel": {
                                                                        "title": {
                                                                            "content": "Duplicate Title"
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    ]
                                                }
                                            }
                                        ]
                                    }
                                }
                            }
                        }
                    ]
                }
            }
        });

        let mut tracks = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();
        recursive_find_tracks(&json_data, &mut tracks, &mut seen_ids, 10, 12345, "YouTube");

        assert_eq!(tracks.len(), 2);
        assert_eq!(&*tracks[0].title, "Test Lockup Title");
        assert_eq!(
            &*tracks[0].url,
            "https://www.youtube.com/watch?v=video_id_1"
        );
        assert_eq!(tracks[0].duration, None);
        assert_eq!(&*tracks[0].source_provider, "YouTube");

        assert_eq!(&*tracks[1].title, "Test Playlist Title");
        assert_eq!(
            &*tracks[1].url,
            "https://www.youtube.com/watch?v=video_id_2"
        );
        assert_eq!(tracks[1].duration, Some(Duration::from_secs(225))); // 3 * 60 + 45 = 225
        assert_eq!(&*tracks[1].source_provider, "YouTube");
    }

    #[tokio::test]
    async fn test_live_youtube_album_resolution() {
        let url =
            "https://music.youtube.com/playlist?list=OLAK5uy_nvYrn-HvzbwuVef3BcLzl70t41Warfbpw";
        let http_client = reqwest::Client::new();
        let tracks = resolve_youtube_playlist(url, 10, 12345, &http_client)
            .await
            .expect("Failed to resolve live album playlist");
        assert!(!tracks.is_empty(), "Resolved tracks list is empty");
        assert!(
            tracks[0].title.contains("SỢ HẠNH PHÚC QUÁ NGẮN")
                || tracks[0].title.contains("Sợ Hạnh Phúc Quá Ngắn")
                || tracks[0].title.to_lowercase().contains("hạnh phúc"),
            "Unexpected title: {}",
            tracks[0].title
        );
        assert_eq!(
            &*tracks[0].url,
            "https://www.youtube.com/watch?v=ns878yW7N2c"
        );
        assert_eq!(&*tracks[0].source_provider, "YouTube Music");
    }

    fn metadata_candidate(
        source: &str,
        title: &str,
        artist: &str,
        duration_secs: u64,
        popularity: Option<u64>,
    ) -> TrackCandidate {
        TrackCandidate {
            source: source.to_owned(),
            title: title.to_owned(),
            artist: artist.to_owned(),
            duration: Some(Duration::from_secs(duration_secs)),
            popularity,
            is_official: true,
            is_topic_channel: false,
            url: format!("https://example.com/{source}/{title}"),
            thumbnail: None,
        }
    }

    #[test]
    fn trusted_metadata_prefers_catalog_artist_match() {
        let scored = score_trusted_metadata_candidates(
            vec![
                metadata_candidate("Apple Music", "Clarity", "Zedd", 204, None),
                metadata_candidate(
                    "Deezer",
                    "Clarity",
                    "Zedd, VALORANT, Foxes, BUNT.",
                    204,
                    Some(900_000),
                ),
            ],
            "Clarity Valorant",
        );

        assert_eq!(scored[0].source, "Deezer");
        assert_eq!(
            scored[0].meta.artist.as_deref(),
            Some("Zedd, VALORANT, Foxes, BUNT.")
        );
        assert!(scored[0].score >= TRUSTED_METADATA_PICK_THRESHOLD);
    }

    #[test]
    fn trusted_metadata_accepts_catalog_remix() {
        let scored = score_trusted_metadata_candidates(
            vec![metadata_candidate(
                "Deezer",
                "Clarity (BUNT. Remix)",
                "Zedd, VALORANT, Foxes, BUNT.",
                204,
                Some(900_000),
            )],
            "Clarity Valorant",
        );

        assert_eq!(scored[0].source, "Deezer");
        assert_eq!(scored[0].meta.title, "Clarity (BUNT. Remix)");
        assert!(scored[0].score >= TRUSTED_METADATA_PICK_THRESHOLD);
    }

    #[test]
    fn test_extract_next_data() {
        let html_content = r#"
            <html>
            <head>
                <script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"state":{"data":{"entity":{"trackList":[{"uri":"spotify:track:123","title":"Hello","duration":1000}]}}}}}}</script>
            </head>
            <body></body>
            </html>
        "#;
        let data = extract_next_data(html_content);
        assert!(data.is_some());
        let json_str = data.unwrap();
        let parsed: NextDataJson = serde_json::from_str(json_str).unwrap();
        let entity = parsed
            .props
            .unwrap()
            .page_props
            .unwrap()
            .state
            .unwrap()
            .data
            .unwrap()
            .entity
            .unwrap();
        let tracks = entity.track_list.unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title, "Hello");
        assert_eq!(tracks[0].uri, "spotify:track:123");
        assert_eq!(tracks[0].duration, Some(1000));
    }

    #[tokio::test]
    async fn test_spotify_playlist_resolution() -> Result<(), Box<dyn std::error::Error>> {
        if !std::path::Path::new("config.yml").exists() {
            println!("Skipping test: config.yml not found");
            return Ok(());
        }
        let config = crate::config::load_config("config.yml").await?;
        crate::audio::runtime::configure(
            &config.resolver,
            &config.spotify,
            config.playback.max_playlist_import,
        );

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        let tracks = resolve_spotify_playlist("0rDIyaLtoI8Dpw58rsDspw", 1, 1, &http_client).await?;

        assert!(
            !tracks.is_empty(),
            "Spotify playlist should resolve at least one track"
        );

        for mut track in tracks.into_iter().take(1) {
            resolve_ytsearch_track(&mut track, &http_client).await?;
            assert!(
                track.url.contains("youtube.com/") || track.url.contains("youtu.be/"),
                "Spotify track should resolve to a YouTube URL, got {}",
                track.url
            );

            let stream =
                crate::audio::source::extract_stream_url_for_guild(9_001, &track.url, &http_client)
                    .await?;
            assert!(
                stream.url.contains("googlevideo.com")
                    || stream.url.contains("googleusercontent.com"),
                "native resolver should return a direct Google media URL"
            );

            let probe = youtube_resolver::probe_resolved_stream_health(
                &http_client,
                &stream,
                32 * 1024,
                10.0,
            )
            .await?;

            println!(
                "spotify playlist probe: title={}, client={}, source={}, bytes={}, speed={:.2} KB/s",
                track.title,
                stream.client_kind,
                stream.resolve_source,
                probe.total_bytes,
                probe.speed_kbps
            );
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_spotify_track_resolution() -> Result<(), Box<dyn std::error::Error>> {
        if !std::path::Path::new("config.yml").exists() {
            println!("Skipping test: config.yml not found");
            return Ok(());
        }
        let config = crate::config::load_config("config.yml").await?;
        crate::audio::runtime::configure(
            &config.resolver,
            &config.spotify,
            config.playback.max_playlist_import,
        );

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        // PUBG: BATTLEGROUNDS Hot Drop Anthem (Original Soundtrack) - R.Tee, PUBG
        let meta = resolve_spotify_track("3MZV29Px9MDOrEasd3NIJP", &http_client).await?;
        assert!(
            meta.title.to_lowercase().contains("hot drop anthem")
                || meta.title.to_lowercase().contains("pubg")
        );
        let artist = meta.artist.as_deref().unwrap_or_default().to_lowercase();
        println!(
            "DEBUG RESOLVED META: title='{}', artist='{}'",
            meta.title, artist
        );
        assert!(artist.contains("r.tee") || artist.contains("pubg"));
        Ok(())
    }
}
