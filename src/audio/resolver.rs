use crate::audio::providers::{
    AppleMusicProvider, DeezerProvider, DirectUrlProvider, ExternalTrackMeta, MetadataProvider,
    SoundCloudProvider, SpotifyProvider, YouTubeMusicProvider, YouTubeProvider,
};
use crate::audio::ranking::{
    TrackCandidate, adjust_single_word_score, contains_unrequested_variant,
    jaro_winkler_similarity, score_candidates,
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
    Track(Track),
    SearchResults(Vec<Track>),
}

impl ResolvedInput {
    pub fn into_tracks_or_top(self) -> Vec<Track> {
        match self {
            ResolvedInput::Playlist(tracks) => tracks,
            ResolvedInput::Track(track) => vec![track],
            ResolvedInput::SearchResults(mut tracks) => {
                if tracks.is_empty() {
                    vec![]
                } else {
                    vec![tracks.remove(0)] // just the top candidate
                }
            }
        }
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

fn query_tokens(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| part.len() > 1)
        .map(|part| part.to_lowercase())
        .collect()
}

fn token_overlap(query: &str, candidate_text: &str) -> f64 {
    let query_parts = query_tokens(query);
    if query_parts.is_empty() {
        return 0.0;
    }

    let candidate_tokens = query_tokens(candidate_text);
    let matched = query_parts
        .iter()
        .filter(|token| candidate_tokens.iter().any(|candidate| candidate == *token))
        .count();
    matched as f64 / query_parts.len() as f64
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

    for provider in ["Deezer", "Spotify", "Apple Music"] {
        let query = query.to_owned();
        let client = http_client.clone();
        join_set.spawn(async move {
            let start = std::time::Instant::now();
            let result = match provider {
                "Deezer" => DeezerProvider.search(&query, &client).await,
                "Spotify" => SpotifyProvider.search(&query, &client).await,
                "Apple Music" => AppleMusicProvider.search(&query, &client).await,
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
        "YouTube Music" => 0.08,
        "YouTube" => 0.03,
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
) -> Vec<(TrackCandidate, f64)> {
    let mut scored = score_candidates(
        candidates,
        search_query,
        expected_title,
        expected_artist,
        expected_duration,
    );

    for (candidate, score) in &mut scored {
        *score = (*score + get_priority_boost(&candidate.source, search_query)).clamp(0.0, 1.0);
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
            SearchProviderKind::SoundCloud,
        ],
        search_query,
        expected_title,
        expected_artist,
        expected_duration,
        http_client,
    )
    .await?;

    if all_scored.is_empty() && !crate::audio::runtime::is_youtube_degraded() {
        let yt = YouTubeProvider;
        match yt.search_fallback_ytdl(search_query).await {
            Ok(candidates) => {
                all_scored.extend(score_provider_candidates(
                    candidates,
                    search_query,
                    expected_title,
                    expected_artist,
                    expected_duration,
                ));
            }
            Err(err) => {
                tracing::warn!(query = %search_query, %err, "ytsearch fallback failed");
            }
        }
    }

    all_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(all_scored)
}

async fn collect_search_results(
    query: &str,
    user_id: u64,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    let mut scored = search_trusted_metadata_candidates(query, http_client).await;

    scored.extend(
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
        )
        .await?,
    );

    if !crate::audio::runtime::is_youtube_degraded() {
        let yt = YouTubeProvider;
        match yt.search_fallback_ytdl(query).await {
            Ok(candidates) => {
                scored.extend(score_provider_candidates(
                    candidates, query, query, None, None,
                ));
            }
            Err(err) => {
                tracing::debug!(query, %err, "ytsearch select-list fallback failed");
            }
        }
    }

    scored.sort_by(|a, b| {
        source_priority(&a.0.source)
            .cmp(&source_priority(&b.0.source))
            .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
    });

    let tracks = scored
        .into_iter()
        .take(25)
        .map(|(candidate, score)| Track {
            title: candidate_display_title(&candidate),
            url: candidate.url,
            duration: candidate.duration,
            requester_id: serenity::UserId::new(user_id),
            requester_name: "".to_owned(),
            source_type: SourceType::Search,
            resolved_url: None,
            thumbnail: candidate.thumbnail,
            source_provider: format!("{} • {:.0}%", candidate.source, score * 100.0),
        })
        .collect();

    Ok(tracks)
}

fn source_priority(source: &str) -> usize {
    match source {
        "Deezer" => 0,
        "Spotify" => 1,
        "SoundCloud" => 2,
        "Apple Music" => 3,
        "YouTube Music" => 4,
        "YouTube" => 5,
        "yt-dlp" => 6,
        _ => 7,
    }
}

#[allow(dead_code)]
fn has_confident_candidate(scored: &[(TrackCandidate, f64)], query: &str) -> bool {
    let settings = crate::audio::runtime::settings();
    scored
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(candidate, score)| {
            *score >= settings.auto_pick_threshold
                && !contains_unrequested_variant(&candidate.title, query)
        })
        .unwrap_or(false)
}

async fn run_provider_batch(
    providers: &[SearchProviderKind],
    search_query: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
    expected_duration: Option<Duration>,
    http_client: &reqwest::Client,
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

            let scored = score_provider_candidates(
                candidates,
                search_query,
                expected_title,
                expected_artist,
                expected_duration,
            );

            if let Some((candidate, top_score)) = scored.first() {
                if *top_score >= settings.auto_pick_threshold
                    && !contains_unrequested_variant(&candidate.title, search_query)
                {
                    perfect_found = true;
                    all_scored.extend(scored);
                    cancel_token.cancel();
                    join_set.abort_all();
                    break;
                }
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
        // 1. Check user-owned playlist exact match
        if let Some(playlist) = db.get_user_playlist(user_id, query_trimmed).await {
            let mut tracks = Vec::new();
            for t in playlist.tracks {
                tracks.push(Track {
                    title: t.title,
                    url: t.url,
                    duration: t.duration_secs.map(Duration::from_secs),
                    requester_id: serenity::UserId::new(user_id),
                    requester_name: "".to_owned(),
                    source_type: SourceType::Playlist,
                    resolved_url: None,
                    thumbnail: None,
                    source_provider: "Playlist".to_owned(),
                });
            }
            return Ok(ResolvedInput::Playlist(tracks));
        }

        // 2. Check for Spotify links (playlist/album/artist)
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
            if let Some(id) = extract_spotify_id(query_trimmed, "spotify.com/playlist/") {
                let tracks = resolve_spotify_playlist(&id, limit, user_id, http_client).await?;
                return Ok(ResolvedInput::Playlist(tracks));
            } else {
                return Err(SerenyaError::Audio(
                    "Failed to extract Spotify playlist ID.".into(),
                ));
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
            if let Some(id) = extract_spotify_id(query_trimmed, "spotify.com/album/") {
                let tracks = resolve_spotify_album(&id, limit, user_id, http_client).await?;
                return Ok(ResolvedInput::Playlist(tracks));
            } else {
                return Err(SerenyaError::Audio(
                    "Failed to extract Spotify album ID.".into(),
                ));
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
            if let Some(id) = extract_spotify_id(query_trimmed, "spotify.com/artist/") {
                let tracks =
                    resolve_spotify_artist_top_tracks(&id, limit, user_id, http_client).await?;
                return Ok(ResolvedInput::Playlist(tracks));
            } else {
                return Err(SerenyaError::Audio(
                    "Failed to extract Spotify artist ID.".into(),
                ));
            }
        }

        // 3. Check Cache
        if query_trimmed.starts_with("http://") || query_trimmed.starts_with("https://") {
            if let Some(mut cached_track) =
                crate::audio::source::cache_get_url_metadata(query_trimmed).await
            {
                cached_track.requester_id = serenity::UserId::new(user_id);
                tracing::info!(query = %query_trimmed, cache = "hit", "cache_hit");
                return Ok(ResolvedInput::Track(cached_track));
            }
        } else if let Some(mut cached_track) =
            crate::audio::source::cache_get_metadata(query_trimmed).await
        {
            cached_track.requester_id = serenity::UserId::new(user_id);
            tracing::info!(query = %query_trimmed, cache = "hit", "cache_hit");
            return Ok(ResolvedInput::Track(cached_track));
        }

        tracing::info!(query = %query_trimmed, cache = "miss", "cache_miss");

        // Instantiate providers
        let spotify_provider = SpotifyProvider;
        let apple_provider = AppleMusicProvider;
        let deezer_provider = DeezerProvider;
        let youtube_provider = YouTubeProvider;
        let direct_provider = DirectUrlProvider;

        // 4. Resolve metadata or play directly
        if spotify_provider.supports(query_trimmed) {
            let meta = spotify_provider
                .resolve_metadata(query_trimmed, http_client)
                .await?;
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
                    track.clone(),
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
                    track.clone(),
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
                    track.clone(),
                )
                .await;
            }
            Ok(res)
        } else if youtube_provider.supports(query_trimmed) {
            let mut tracks = youtube_provider
                .load(query_trimmed, user_id, http_client)
                .await?;
            if let Some(track) = tracks.first_mut() {
                track.requester_id = serenity::UserId::new(user_id);
                crate::audio::source::cache_set_url_metadata(
                    query_trimmed.to_owned(),
                    track.clone(),
                )
                .await;
                Ok(ResolvedInput::Track(track.clone()))
            } else {
                Err(SerenyaError::Audio(
                    "Failed to load YouTube track".to_owned(),
                ))
            }
        } else if direct_provider.supports(query_trimmed) {
            let mut tracks = direct_provider.load(query_trimmed, user_id).await?;
            if let Some(track) = tracks.first_mut() {
                track.requester_id = serenity::UserId::new(user_id);
                crate::audio::source::cache_set_url_metadata(
                    query_trimmed.to_owned(),
                    track.clone(),
                )
                .await;
                Ok(ResolvedInput::Track(track.clone()))
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

    let scored = perform_parallel_search(
        &search_query,
        &meta.title,
        meta.artist.as_deref(),
        meta.duration,
        http_client,
    )
    .await?;

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
    forced_thumbnail: Option<String>,
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
    let variant_conflict = contains_unrequested_variant(&top_cand.title, &variant_context);

    let low_confidence = *top_score < settings.auto_pick_threshold || variant_conflict;

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
                title: candidate_display_title(&cand),
                url: cand.url,
                duration: cand.duration,
                requester_id: serenity::UserId::new(user_id),
                requester_name: "".to_owned(),
                source_type: SourceType::Search,
                resolved_url: None,
                thumbnail: forced_thumbnail.clone().or(cand.thumbnail),
                source_provider: format!("{} • {:.0}%", cand.source, score * 100.0),
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
            title: forced_title.unwrap_or_else(|| candidate_display_title(top_cand)),
            url: top_cand.url.clone(),
            duration: forced_duration.or(top_cand.duration),
            requester_id: serenity::UserId::new(user_id),
            requester_name: "".to_owned(),
            source_type: SourceType::Search,
            resolved_url: None,
            thumbnail: forced_thumbnail.or_else(|| top_cand.thumbnail.clone()),
            source_provider: selected_provider,
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

        Ok(ResolvedInput::Track(track))
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

#[allow(dead_code)]
fn parse_spotify_track_json(track_val: &serde_json::Value, user_id: u64) -> Option<Track> {
    let name = track_val.get("name")?.as_str()?.to_owned();
    let id = track_val.get("id")?.as_str()?;
    let duration_ms = track_val.get("duration_ms")?.as_u64()?;

    let mut artists_vec = Vec::new();
    if let Some(artists) = track_val.get("artists").and_then(|v| v.as_array()) {
        for a in artists {
            if let Some(a_name) = a.get("name").and_then(|v| v.as_str()) {
                artists_vec.push(a_name.to_owned());
            }
        }
    }
    let _artist_str = if artists_vec.is_empty() {
        "".to_owned()
    } else {
        artists_vec.join(", ")
    };

    let thumbnail = track_val
        .get("album")
        .and_then(|v| v.get("images"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    let track_url = format!("https://open.spotify.com/track/{}", id);

    Some(Track {
        title: name,
        url: track_url,
        duration: Some(Duration::from_millis(duration_ms)),
        requester_id: serenity::UserId::new(user_id),
        requester_name: "".to_owned(),
        source_type: SourceType::Url,
        resolved_url: None,
        thumbnail,
        source_provider: "Spotify".to_owned(),
    })
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
            // Build a display title including artist (subtitle) for better identification
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
                title: display_title,
                url: track_url,
                duration: embed_track.duration.map(Duration::from_millis),
                requester_id: serenity::UserId::new(user_id),
                requester_name: "".to_owned(),
                source_type: SourceType::Url,
                resolved_url: None,
                thumbnail: entity_thumbnail.clone(),
                source_provider: "Spotify".to_owned(),
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

async fn resolve_spotify_playlist_api(
    playlist_id: &str,
    limit: usize,
    user_id: u64,
    client_id: &str,
    client_secret: &str,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::info!("Requesting Spotify API access token for playlist resolution...");
    let token = crate::audio::providers::get_spotify_access_token(
        http_client,
        client_id,
        client_secret,
        Duration::from_secs(5),
    )
    .await?;
    tracing::info!("Spotify API access token retrieved successfully.");

    let mut tracks = Vec::new();
    let mut offset = 0;

    while tracks.len() < limit {
        let chunk_limit = (limit - tracks.len()).min(100);
        let url = format!(
            "https://api.spotify.com/v1/playlists/{}/tracks?limit={}&offset={}",
            playlist_id, chunk_limit, offset
        );

        tracing::info!(
            "Fetching Spotify playlist tracks: offset = {}, limit = {} (total resolved so far: {})",
            offset,
            chunk_limit,
            tracks.len()
        );

        let response = http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| {
                SerenyaError::Audio(format!(
                    "Failed to send Spotify playlist API request: {}",
                    e
                ))
            })?;

        let status = response.status();
        if !status.is_success() {
            let err_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read response body".to_string());
            tracing::error!(
                "Spotify playlist API request failed with status: {}, body: {}",
                status,
                err_body
            );
            return Err(SerenyaError::Audio(format!(
                "Spotify playlist API returned status: {}, body: {}",
                status, err_body
            )));
        }

        let body: serde_json::Value = response.json().await.map_err(|e| {
            SerenyaError::Audio(format!("Failed to parse Spotify playlist API JSON: {}", e))
        })?;

        let items = match body.get("items").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => {
                tracing::warn!("Spotify playlist response missing 'items' array or it is empty.");
                break;
            }
        };

        if items.is_empty() {
            tracing::info!("No more items found in Spotify playlist response.");
            break;
        }

        let items_len = items.len();
        for item in items {
            let Some(track_val) = item.get("track") else {
                continue;
            };
            if track_val.is_null() {
                continue;
            }

            let Some(name) = track_val.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let duration_ms = track_val
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let mut artists_vec = Vec::new();
            if let Some(artists) = track_val.get("artists").and_then(|v| v.as_array()) {
                for a in artists {
                    if let Some(a_name) = a.get("name").and_then(|v| v.as_str()) {
                        artists_vec.push(a_name.to_owned());
                    }
                }
            }
            let artist_str = if artists_vec.is_empty() {
                "".to_owned()
            } else {
                artists_vec.join(", ")
            };

            let thumbnail = track_val
                .get("album")
                .and_then(|v| v.get("images"))
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|img| img.get("url"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned());

            let search_query = if artist_str.is_empty() {
                name.to_owned()
            } else {
                format!("{} - {}", artist_str, name)
            };
            let track_url = format!("ytsearch1:{}", search_query);

            tracks.push(Track {
                title: name.to_owned(),
                url: track_url,
                duration: Some(Duration::from_millis(duration_ms)),
                requester_id: serenity::UserId::new(user_id),
                requester_name: "".to_owned(),
                source_type: SourceType::Url,
                resolved_url: None,
                thumbnail,
                source_provider: "Spotify".to_owned(),
            });

            if tracks.len() >= limit {
                break;
            }
        }

        tracing::info!(
            "Successfully processed {} tracks from current batch.",
            items_len
        );

        if body.get("next").and_then(|v| v.as_str()).is_none() {
            tracing::info!("Spotify API response does not have a next page link.");
            break;
        }

        offset += chunk_limit;
    }

    tracing::info!(
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
    tracing::info!(
        "Resolving Spotify playlist ID: {} (limit: {})",
        playlist_id,
        limit
    );
    if let Some(config) = crate::audio::runtime::spotify_settings() {
        if let (Some(client_id), Some(client_secret)) = (&config.client_id, &config.client_secret) {
            tracing::info!(
                "Spotify API credentials found. Attempting resolution via Spotify Web API..."
            );
            match resolve_spotify_playlist_api(
                playlist_id,
                limit,
                user_id,
                client_id,
                client_secret,
                http_client,
            )
            .await
            {
                Ok(tracks) => {
                    tracing::info!(
                        "Successfully resolved {} tracks via Spotify Web API",
                        tracks.len()
                    );
                    return Ok(tracks);
                }
                Err(err) => {
                    tracing::warn!(
                        "Spotify Web API playlist resolution failed ({:?}). Falling back to Spotify embed scraper...",
                        err
                    );
                }
            }
        } else {
            tracing::warn!(
                "Spotify API credentials missing in config. Falling back to Spotify embed scraper."
            );
        }
    } else {
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
    client_id: &str,
    client_secret: &str,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::info!("Requesting Spotify API access token for album resolution...");
    let token = crate::audio::providers::get_spotify_access_token(
        http_client,
        client_id,
        client_secret,
        Duration::from_secs(5),
    )
    .await?;
    tracing::info!("Spotify API access token retrieved successfully.");

    let mut tracks = Vec::new();

    let url = format!("https://api.spotify.com/v1/albums/{}", album_id);
    tracing::info!("Fetching Spotify album details: {}", url);
    let response = http_client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| {
            SerenyaError::Audio(format!("Failed to send Spotify album API request: {}", e))
        })?;

    let status = response.status();
    if !status.is_success() {
        let err_body = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read response body".to_string());
        tracing::error!(
            "Spotify album API request failed with status: {}, body: {}",
            status,
            err_body
        );
        return Err(SerenyaError::Audio(format!(
            "Spotify album API returned status: {}, body: {}",
            status, err_body
        )));
    }

    let body: serde_json::Value = response.json().await.map_err(|e| {
        SerenyaError::Audio(format!("Failed to parse Spotify album API JSON: {}", e))
    })?;

    let thumbnail = body
        .get("images")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    let tracks_obj = body
        .get("tracks")
        .ok_or_else(|| SerenyaError::Audio("Missing tracks field in Spotify album JSON".into()))?;

    let mut next_url = tracks_obj
        .get("next")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    let items = tracks_obj
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| SerenyaError::Audio("Missing items in Spotify album tracks JSON".into()))?;

    tracing::info!(
        "Processing first page of Spotify album tracks (items: {})",
        items.len()
    );
    for item in items {
        let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let duration_ms = item
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let mut artists_vec = Vec::new();
        if let Some(artists) = item.get("artists").and_then(|v| v.as_array()) {
            for a in artists {
                if let Some(a_name) = a.get("name").and_then(|v| v.as_str()) {
                    artists_vec.push(a_name.to_owned());
                }
            }
        }
        let artist_str = if artists_vec.is_empty() {
            "".to_owned()
        } else {
            artists_vec.join(", ")
        };

        let search_query = if artist_str.is_empty() {
            name.to_owned()
        } else {
            format!("{} - {}", artist_str, name)
        };
        let track_url = format!("ytsearch1:{}", search_query);

        tracks.push(Track {
            title: name.to_owned(),
            url: track_url,
            duration: Some(Duration::from_millis(duration_ms)),
            requester_id: serenity::UserId::new(user_id),
            requester_name: "".to_owned(),
            source_type: SourceType::Url,
            resolved_url: None,
            thumbnail: thumbnail.clone(),
            source_provider: "Spotify".to_owned(),
        });

        if tracks.len() >= limit {
            tracing::info!(
                "Reached track limit of {} while parsing first album page.",
                limit
            );
            return Ok(tracks);
        }
    }

    while let Some(url) = next_url {
        if tracks.len() >= limit {
            break;
        }

        tracing::info!(
            "Fetching next page of Spotify album tracks: {} (total resolved so far: {})",
            url,
            tracks.len()
        );
        let response = http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| {
                SerenyaError::Audio(format!(
                    "Failed to send Spotify album pagination request: {}",
                    e
                ))
            })?;

        let status = response.status();
        if !status.is_success() {
            let err_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read response body".to_string());
            tracing::error!(
                "Spotify album pagination API request failed with status: {}, body: {}",
                status,
                err_body
            );
            return Err(SerenyaError::Audio(format!(
                "Spotify album pagination API returned status: {}, body: {}",
                status, err_body
            )));
        }

        let body: serde_json::Value = response.json().await.map_err(|e| {
            SerenyaError::Audio(format!(
                "Failed to parse Spotify album pagination JSON: {}",
                e
            ))
        })?;

        let items = match body.get("items").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => break,
        };

        if items.is_empty() {
            break;
        }

        let items_len = items.len();
        for item in items {
            let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let duration_ms = item
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let mut artists_vec = Vec::new();
            if let Some(artists) = item.get("artists").and_then(|v| v.as_array()) {
                for a in artists {
                    if let Some(a_name) = a.get("name").and_then(|v| v.as_str()) {
                        artists_vec.push(a_name.to_owned());
                    }
                }
            }
            let artist_str = if artists_vec.is_empty() {
                "".to_owned()
            } else {
                artists_vec.join(", ")
            };

            let search_query = if artist_str.is_empty() {
                name.to_owned()
            } else {
                format!("{} - {}", artist_str, name)
            };
            let track_url = format!("ytsearch1:{}", search_query);

            tracks.push(Track {
                title: name.to_owned(),
                url: track_url,
                duration: Some(Duration::from_millis(duration_ms)),
                requester_id: serenity::UserId::new(user_id),
                requester_name: "".to_owned(),
                source_type: SourceType::Url,
                resolved_url: None,
                thumbnail: thumbnail.clone(),
                source_provider: "Spotify".to_owned(),
            });

            if tracks.len() >= limit {
                break;
            }
        }
        tracing::info!(
            "Processed {} tracks from album pagination batch.",
            items_len
        );

        next_url = body
            .get("next")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());
    }

    tracing::info!(
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
    tracing::info!(
        "Resolving Spotify album ID: {} (limit: {})",
        album_id,
        limit
    );
    if let Some(config) = crate::audio::runtime::spotify_settings() {
        if let (Some(client_id), Some(client_secret)) = (&config.client_id, &config.client_secret) {
            tracing::info!(
                "Spotify API credentials found. Attempting resolution via Spotify Web API..."
            );
            match resolve_spotify_album_api(
                album_id,
                limit,
                user_id,
                client_id,
                client_secret,
                http_client,
            )
            .await
            {
                Ok(tracks) => {
                    tracing::info!(
                        "Successfully resolved {} tracks via Spotify Web API",
                        tracks.len()
                    );
                    return Ok(tracks);
                }
                Err(err) => {
                    tracing::warn!(
                        "Spotify Web API album resolution failed ({:?}). Falling back to Spotify embed scraper...",
                        err
                    );
                }
            }
        } else {
            tracing::warn!(
                "Spotify API credentials missing in config. Falling back to Spotify embed scraper."
            );
        }
    } else {
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
    tracing::info!(
        "Attempting Spotify artist fallback scraping for ID: {}",
        artist_id
    );
    let url = format!("https://open.spotify.com/embed/artist/{}", artist_id);
    let tracks = resolve_spotify_embed_fallback(&url, limit, user_id, http_client).await?;
    tracing::info!(
        "Successfully resolved {} tracks via Spotify artist fallback scraper",
        tracks.len()
    );
    Ok(tracks)
}

async fn resolve_spotify_artist_top_tracks_api(
    artist_id: &str,
    limit: usize,
    user_id: u64,
    client_id: &str,
    client_secret: &str,
    http_client: &reqwest::Client,
) -> Result<Vec<Track>, SerenyaError> {
    tracing::info!("Requesting Spotify API access token for artist top tracks resolution...");
    let token = crate::audio::providers::get_spotify_access_token(
        http_client,
        client_id,
        client_secret,
        Duration::from_secs(5),
    )
    .await?;
    tracing::info!("Spotify API access token retrieved successfully.");

    let market = crate::audio::runtime::spotify_settings()
        .map(|cfg| cfg.market.clone())
        .unwrap_or_else(|| "VN".to_owned());

    let url = format!(
        "https://api.spotify.com/v1/artists/{}/top-tracks?market={}",
        artist_id, market
    );
    tracing::info!("Fetching Spotify artist top tracks: {}", url);

    let response = http_client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| {
            SerenyaError::Audio(format!(
                "Failed to send Spotify artist top tracks request: {}",
                e
            ))
        })?;

    let status = response.status();
    if !status.is_success() {
        let err_body = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read response body".to_string());
        tracing::error!(
            "Spotify artist top tracks API request failed with status: {}, body: {}",
            status,
            err_body
        );
        return Err(SerenyaError::Audio(format!(
            "Spotify artist top-tracks API returned status: {}, body: {}",
            status, err_body
        )));
    }

    let body: serde_json::Value = response.json().await.map_err(|e| {
        SerenyaError::Audio(format!(
            "Failed to parse Spotify artist top tracks JSON: {}",
            e
        ))
    })?;

    let tracks_arr = body
        .get("tracks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            SerenyaError::Audio("Missing tracks field in Spotify artist top tracks JSON".into())
        })?;

    tracing::info!(
        "Processing Spotify artist top tracks (items: {})",
        tracks_arr.len()
    );
    let mut tracks = Vec::new();
    for track_val in tracks_arr.iter().take(limit) {
        let Some(name) = track_val.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let duration_ms = track_val
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let mut artists_vec = Vec::new();
        if let Some(artists) = track_val.get("artists").and_then(|v| v.as_array()) {
            for a in artists {
                if let Some(a_name) = a.get("name").and_then(|v| v.as_str()) {
                    artists_vec.push(a_name.to_owned());
                }
            }
        }
        let artist_str = if artists_vec.is_empty() {
            "".to_owned()
        } else {
            artists_vec.join(", ")
        };

        let thumbnail = track_val
            .get("album")
            .and_then(|v| v.get("images"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|img| img.get("url"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());

        let search_query = if artist_str.is_empty() {
            name.to_owned()
        } else {
            format!("{} - {}", artist_str, name)
        };
        let track_url = format!("ytsearch1:{}", search_query);

        tracks.push(Track {
            title: name.to_owned(),
            url: track_url,
            duration: Some(Duration::from_millis(duration_ms)),
            requester_id: serenity::UserId::new(user_id),
            requester_name: "".to_owned(),
            source_type: SourceType::Url,
            resolved_url: None,
            thumbnail,
            source_provider: "Spotify".to_owned(),
        });
    }

    tracing::info!(
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
    tracing::info!(
        "Resolving Spotify artist ID: {} (limit: {})",
        artist_id,
        limit
    );
    if let Some(config) = crate::audio::runtime::spotify_settings() {
        if let (Some(client_id), Some(client_secret)) = (&config.client_id, &config.client_secret) {
            tracing::info!(
                "Spotify API credentials found. Attempting resolution via Spotify Web API..."
            );
            match resolve_spotify_artist_top_tracks_api(
                artist_id,
                limit,
                user_id,
                client_id,
                client_secret,
                http_client,
            )
            .await
            {
                Ok(tracks) => {
                    tracing::info!(
                        "Successfully resolved {} tracks via Spotify Web API",
                        tracks.len()
                    );
                    return Ok(tracks);
                }
                Err(err) => {
                    tracing::warn!(
                        "Spotify Web API artist top tracks resolution failed ({:?}). Falling back to Spotify embed scraper...",
                        err
                    );
                }
            }
        } else {
            tracing::warn!(
                "Spotify API credentials missing in config. Falling back to Spotify embed scraper."
            );
        }
    } else {
        tracing::warn!(
            "Spotify settings missing in config. Falling back to Spotify embed scraper."
        );
    }
    resolve_spotify_artist_top_tracks_fallback(artist_id, limit, user_id, http_client).await
}

pub async fn resolve_ytsearch_track(
    track: &mut Track,
    http_client: &reqwest::Client,
) -> Result<(), SerenyaError> {
    if !track.url.starts_with("ytsearch1:") {
        return Ok(());
    }

    let query = &track.url["ytsearch1:".len()..];
    tracing::info!(query, "Resolving ytsearch1 query lazily to YouTube URL");

    let scored =
        perform_parallel_search(query, &track.title, None, track.duration, http_client).await?;

    if let Some((best_candidate, score)) = scored.into_iter().next() {
        tracing::info!(
            query,
            resolved_url = %best_candidate.url,
            score,
            "Successfully resolved ytsearch1 to real YouTube URL"
        );
        track.url = best_candidate.url;
        if track.thumbnail.is_none() {
            track.thumbnail = best_candidate.thumbnail;
        }
        Ok(())
    } else {
        let mut candidates = YouTubeProvider.search(query, http_client).await?;
        if candidates.is_empty() {
            if let Ok(ytdl_candidates) = YouTubeProvider.search_fallback_ytdl(query).await {
                candidates = ytdl_candidates;
            }
        }

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
            track.url = candidate.url;
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
