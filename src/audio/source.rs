use crate::core::Track;
use crate::utils::SerenyaError;
use arc_swap::ArcSwap;
use moka::future::Cache;
use serde::Deserialize;
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

fn build_query_cache() -> Cache<String, Track> {
    Cache::builder()
        .max_capacity(2048)
        .time_to_live(Duration::from_secs(
            crate::audio::runtime::settings().query_cache_ttl_seconds,
        ))
        .build()
}

fn build_metadata_cache() -> Cache<String, Track> {
    Cache::builder()
        .max_capacity(4096)
        .time_to_live(Duration::from_secs(
            crate::audio::runtime::settings().metadata_cache_ttl_seconds,
        ))
        .build()
}

fn build_stream_cache() -> Cache<String, String> {
    Cache::builder()
        .max_capacity(4096)
        .time_to_live(Duration::from_secs(
            crate::audio::runtime::settings().stream_cache_ttl_seconds,
        ))
        .build()
}

static QUERY_CACHE: LazyLock<ArcSwap<Cache<String, Track>>> =
    LazyLock::new(|| ArcSwap::from_pointee(build_query_cache()));

static METADATA_CACHE: LazyLock<ArcSwap<Cache<String, Track>>> =
    LazyLock::new(|| ArcSwap::from_pointee(build_metadata_cache()));

static STREAM_CACHE: LazyLock<ArcSwap<Cache<String, String>>> =
    LazyLock::new(|| ArcSwap::from_pointee(build_stream_cache()));

/// Shared HTTP client for internal network calls (Invidious, Piped, OEmbed, etc.).
/// Avoids creating a new TLS session per track resolve — reuses connection pool.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(16)
        .tcp_keepalive(Duration::from_secs(60))
        .gzip(true)
        .brotli(true)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::error!(%err, "failed to build shared reqwest client; using default client");
            reqwest::Client::new()
        }
    }
});

pub async fn cache_get_metadata(query: &str) -> Option<Track> {
    let cache = QUERY_CACHE.load();
    if let Some(track) = cache.get(query).await {
        tracing::debug!(query, cache = "query", "cache hit");
        return Some(track);
    }
    tracing::debug!(query, cache = "query", "cache miss/expired");
    None
}

pub async fn cache_set_metadata(query: String, track: Track) {
    QUERY_CACHE.load().insert(query, track).await;
}

pub async fn cache_get_url_metadata(url: &str) -> Option<Track> {
    let cache = METADATA_CACHE.load();
    if let Some(track) = cache.get(url).await {
        tracing::debug!(url, cache = "metadata", "cache hit");
        return Some(track);
    }
    tracing::debug!(url, cache = "metadata", "cache miss/expired");
    None
}

pub async fn cache_set_url_metadata(url: String, track: Track) {
    METADATA_CACHE.load().insert(url, track).await;
}

pub async fn cache_get_stream(url: &str) -> Option<youtube_resolver::ResolvedStream> {
    let cache = STREAM_CACHE.load();
    if let Some(stream_json) = cache.get(url).await
        && let Ok(stream) = serde_json::from_str::<youtube_resolver::ResolvedStream>(&stream_json)
    {
        tracing::debug!(url, cache = "stream", "cache hit");
        return Some(stream);
    }
    tracing::debug!(url, cache = "stream", "cache miss/expired");
    None
}

pub async fn cache_invalidate_stream(url: &str) {
    STREAM_CACHE.load().invalidate(url).await;
    SOUNDCLOUD_STREAM_CACHE.invalidate(url).await;
}

pub async fn cache_set_stream(url: String, stream: &youtube_resolver::ResolvedStream) {
    if let Ok(json) = serde_json::to_string(stream) {
        STREAM_CACHE.load().insert(url, json).await;
    }
}

fn url_encode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[derive(Deserialize)]
struct OEmbedResponse {
    title: String,
    thumbnail_url: Option<String>,
}

pub async fn resolve_youtube_oembed(
    video_url: &str,
    http_client: &reqwest::Client,
) -> Result<(String, Option<String>), SerenyaError> {
    let oembed_url = format!(
        "https://www.youtube.com/oembed?url={}&format=json",
        url_encode(video_url)
    );
    let val = http_client
        .get(&oembed_url)
        .send()
        .await
        .map_err(|e| SerenyaError::Audio(format!("failed to fetch YouTube oEmbed: {}", e)))?
        .json::<OEmbedResponse>()
        .await
        .map_err(|e| SerenyaError::Audio(format!("failed to parse YouTube oEmbed JSON: {}", e)))?;

    Ok((val.title, val.thumbnail_url))
}

fn is_youtube_url(url: &str) -> bool {
    url.contains("youtube.com/") || url.contains("youtu.be/")
}

fn extract_youtube_video_id(url: &str) -> Option<&str> {
    if let Some(pos) = url.find("v=") {
        let rest = &url[pos + 2..];
        let end = rest.find('&').unwrap_or(rest.len());
        Some(&rest[..end])
    } else if let Some(pos) = url.find("youtu.be/") {
        let rest = &url[pos + 9..];
        let end = rest.find('?').unwrap_or(rest.len());
        Some(&rest[..end])
    } else {
        None
    }
}

async fn resolve_via_invidious_or_piped(
    video_id: &str,
    client: &reqwest::Client,
) -> Option<String> {
    #[derive(Deserialize)]
    struct InvidiousFormat {
        #[serde(rename = "type")]
        format_type: Option<String>,
        url: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct InvidiousResponse {
        adaptive_formats: Option<Vec<InvidiousFormat>>,
    }

    let piped_url = format!("https://pipedapi.kavin.rocks/streams/{}", video_id);
    let invidious_url = format!("https://yewtu.be/api/v1/videos/{}", video_id);

    let piped_fut = async {
        let resp = tokio::time::timeout(Duration::from_secs(4), client.get(&piped_url).send())
            .await
            .ok()?
            .ok()?;
        let val = resp.json::<serde_json::Value>().await.ok()?;
        val.get("audioStreams")?
            .as_array()?
            .first()?
            .get("url")?
            .as_str()
            .map(|s| s.to_owned())
    };

    let invidious_fut = async {
        let resp = tokio::time::timeout(Duration::from_secs(4), client.get(&invidious_url).send())
            .await
            .ok()?
            .ok()?;
        let val = resp.json::<InvidiousResponse>().await.ok()?;
        val.adaptive_formats?.into_iter().find_map(|f| {
            let t = f.format_type.unwrap_or_default();
            if t.starts_with("audio/") { f.url } else { None }
        })
    };

    // Race both — return whichever resolves first with a valid URL
    tokio::pin!(piped_fut);
    tokio::pin!(invidious_fut);

    tokio::select! {
        biased;
        result = &mut piped_fut => {
            if result.is_some() { return result; }
            invidious_fut.await
        }
        result = &mut invidious_fut => {
            if result.is_some() { return result; }
            piped_fut.await
        }
    }
}

pub async fn extract_stream_url_for_guild(
    guild_id: u64,
    track_url: &str,
) -> Result<youtube_resolver::ResolvedStream, SerenyaError> {
    let _guild_permit = crate::audio::runtime::acquire_guild_resolve(guild_id).await?;
    extract_stream_url_inner(track_url).await
}

pub async fn prefetch_stream_url_for_guild(
    guild_id: u64,
    track_url: &str,
) -> Result<Option<youtube_resolver::ResolvedStream>, SerenyaError> {
    if is_youtube_url(track_url) && crate::audio::runtime::is_youtube_degraded() {
        tracing::info!(
            guild_id,
            "Skipping prefetch while YouTube resolver is degraded"
        );
        return Ok(None);
    }

    let Some(_guild_permit) = crate::audio::runtime::try_acquire_guild_resolve(guild_id) else {
        tracing::debug!(
            guild_id,
            "Skipping prefetch because a guild resolve is already running"
        );
        return Ok(None);
    };

    let timeout = crate::audio::runtime::prefetch_timeout();
    match tokio::time::timeout(timeout, extract_stream_url_inner(track_url)).await {
        Ok(result) => result.map(Some),
        Err(_) => Err(SerenyaError::Audio(format!(
            "prefetch stream resolution timed out after {timeout:?}"
        ))),
    }
}

async fn run_ytdlp_stream_resolution(
    track_url: &str,
    youtube_url: bool,
    negative_key: &str,
) -> Result<youtube_resolver::ResolvedStream, SerenyaError> {
    if youtube_url {
        return Err(SerenyaError::Audio(
            "Python yt-dlp stream fallback is disabled for YouTube".to_owned(),
        ));
    }

    let ytdlp_args = vec![
        "--ignore-config".to_owned(),
        "--no-warnings".to_owned(),
        "--socket-timeout".to_owned(),
        "5".to_owned(),
        "--no-check-formats".to_owned(),
        "--no-playlist".to_owned(),
        "-g".to_owned(),
        "-f".to_owned(),
        "bestaudio".to_owned(),
        track_url.to_owned(),
    ];

    let output = crate::audio::runtime::run_ytdlp(
        "stream resolution",
        ytdlp_args,
        crate::audio::runtime::yt_dlp_timeout(),
        youtube_url,
        Some(negative_key.to_owned()),
    )
    .await?;

    let stream_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stream_url.is_empty() {
        crate::audio::runtime::remember_negative(
            negative_key.to_owned(),
            "yt-dlp returned an empty stream URL".to_owned(),
        )
        .await;
        return Err(SerenyaError::Audio(
            "yt-dlp returned an empty stream URL".to_owned(),
        ));
    }

    Ok(youtube_resolver::ResolvedStream {
        url: stream_url,
        client_kind: "WEB".to_string(),
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36".to_string(),
        expires_at: None,
        mime_type: None,
        bitrate: None,
        resolve_source: "yt-dlp".to_string(),
    })
}

static SOUNDCLOUD_STREAM_CACHE: LazyLock<Cache<String, String>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(4096)
        .time_to_live(Duration::from_secs(300)) // 5 minutes
        .build()
});

static SOUNDCLOUD_SEMAPHORE: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(4));

async fn resolve_soundcloud_stream_url(
    track_url: &str,
) -> Result<youtube_resolver::ResolvedStream, SerenyaError> {
    // 1. Check cache
    if let Some(stream_json) = SOUNDCLOUD_STREAM_CACHE.get(track_url).await
        && let Ok(stream) = serde_json::from_str::<youtube_resolver::ResolvedStream>(&stream_json)
    {
        tracing::debug!(track_url, "SoundCloud stream cache hit");
        return Ok(stream);
    }

    // 2. Concurrency control via Semaphore
    let _permit = SOUNDCLOUD_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| SerenyaError::Audio("SoundCloud semaphore is closed".to_owned()))?;

    tracing::info!(track_url, "Resolving SoundCloud stream URL natively...");

    // 3. Resolve URL to track metadata with retry and exponential backoff
    let metadata = fetch_track_metadata_with_backoff(track_url).await?;

    // 4. Select the best transcoding
    let media = metadata.media.ok_or_else(|| {
        SerenyaError::Audio("SoundCloud track is missing media transcodings".to_owned())
    })?;

    let transcoding = select_best_transcoding(&media.transcodings).ok_or_else(|| {
        SerenyaError::Audio("No supported transcodings found for SoundCloud track".to_owned())
    })?;

    let transcoding_url = transcoding.url.clone();
    tracing::debug!(transcoding_url, "Selected SoundCloud transcoding");

    // 5. Query transcoding URL to get direct playable URL
    let stream_url = fetch_stream_url_with_backoff(&transcoding_url).await?;

    let stream = youtube_resolver::ResolvedStream {
        url: stream_url,
        client_kind: "SOUNDCLOUD".to_string(),
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36".to_string(),
        expires_at: None,
        mime_type: transcoding.format.mime_type.clone(),
        bitrate: None,
        resolve_source: "soundcloud".to_string(),
    };

    // 6. Cache the result for 5 minutes (SoundCloud signed URLs expire quickly)
    if let Ok(json) = serde_json::to_string(&stream) {
        SOUNDCLOUD_STREAM_CACHE
            .insert(track_url.to_owned(), json)
            .await;
    }

    Ok(stream)
}

fn select_best_transcoding(
    transcodings: &[crate::audio::providers::SoundCloudTranscoding],
) -> Option<&crate::audio::providers::SoundCloudTranscoding> {
    // 1. Check for Opus HLS
    if let Some(t) = transcodings.iter().find(|t| {
        t.format.protocol == "hls"
            && t.format
                .mime_type
                .as_ref()
                .map(|m| m.contains("opus"))
                .unwrap_or(false)
    }) {
        return Some(t);
    }

    // 2. Check for AAC HLS
    if let Some(t) = transcodings.iter().find(|t| {
        t.format.protocol == "hls"
            && t.format
                .mime_type
                .as_ref()
                .map(|m| m.contains("mp4a") || m.contains("mpegurl"))
                .unwrap_or(false)
    }) {
        return Some(t);
    }

    // 3. Check for any HLS
    if let Some(t) = transcodings.iter().find(|t| t.format.protocol == "hls") {
        return Some(t);
    }

    // 4. Check for Progressive
    if let Some(t) = transcodings
        .iter()
        .find(|t| t.format.protocol == "progressive")
    {
        return Some(t);
    }

    None
}

async fn fetch_track_metadata_with_backoff(
    track_url: &str,
) -> Result<crate::audio::providers::SoundCloudTrackMetadata, SerenyaError> {
    let mut final_url = track_url.to_owned();
    if track_url.contains("on.soundcloud.com/")
        && let Ok(res) = HTTP_CLIENT.head(track_url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36")
            .send()
            .await
        {
            final_url = res.url().as_str().to_owned();
            tracing::info!("Redirected shortened SoundCloud stream URL: {} -> {}", track_url, final_url);
        }

    let mut attempt = 0;
    let max_attempts = 3;
    let url_enc = crate::audio::providers::url_encode(&final_url);

    loop {
        let res = crate::audio::providers::send_soundcloud_request(&HTTP_CLIENT, |cid| {
            format!(
                "https://api-v2.soundcloud.com/resolve?url={}&client_id={}",
                url_enc, cid
            )
        })
        .await;

        match res {
            Ok(resp) => {
                if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    attempt += 1;
                    if attempt >= max_attempts {
                        return Err(SerenyaError::Audio(
                            "SoundCloud rate limited (429) after max retries".to_owned(),
                        ));
                    }
                    let delay = calculate_backoff(attempt);
                    tracing::warn!(
                        attempt,
                        delay_ms = delay.as_millis(),
                        "SoundCloud resolve rate-limited. Retrying..."
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }

                let metadata: crate::audio::providers::SoundCloudTrackMetadata =
                    resp.json().await.map_err(|e| {
                        SerenyaError::Audio(format!(
                            "Failed to parse SoundCloud track metadata: {}",
                            e
                        ))
                    })?;
                return Ok(metadata);
            }
            Err(e) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(e);
                }
                let delay = calculate_backoff(attempt);
                tracing::warn!(attempt, delay_ms = delay.as_millis(), err = %e, "SoundCloud resolve failed. Retrying...");
                tokio::time::sleep(delay).await;
            }
        }
    }
}

async fn fetch_stream_url_with_backoff(transcoding_url: &str) -> Result<String, SerenyaError> {
    let mut attempt = 0;
    let max_attempts = 3;

    loop {
        let res = crate::audio::providers::send_soundcloud_request(&HTTP_CLIENT, |cid| {
            format!("{}?client_id={}", transcoding_url, cid)
        })
        .await;

        match res {
            Ok(resp) => {
                if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    attempt += 1;
                    if attempt >= max_attempts {
                        return Err(SerenyaError::Audio(
                            "SoundCloud rate limited (429) on stream resolution after max retries"
                                .to_owned(),
                        ));
                    }
                    let delay = calculate_backoff(attempt);
                    tracing::warn!(
                        attempt,
                        delay_ms = delay.as_millis(),
                        "SoundCloud stream rate-limited. Retrying..."
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }

                #[derive(serde::Deserialize)]
                struct StreamResponse {
                    url: String,
                }

                let stream_res: StreamResponse = resp.json().await.map_err(|e| {
                    SerenyaError::Audio(format!(
                        "Failed to parse SoundCloud stream URL JSON: {}",
                        e
                    ))
                })?;
                return Ok(stream_res.url);
            }
            Err(e) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(e);
                }
                let delay = calculate_backoff(attempt);
                tracing::warn!(attempt, delay_ms = delay.as_millis(), err = %e, "SoundCloud stream resolution failed. Retrying...");
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn calculate_backoff(attempt: u32) -> Duration {
    use rand::Rng;
    let base = 2u64.pow(attempt);
    let jitter = rand::rng().random_range(0..200);
    Duration::from_millis(base * 500 + jitter)
}

async fn extract_stream_url_inner(
    track_url: &str,
) -> Result<youtube_resolver::ResolvedStream, SerenyaError> {
    if let Some(stream) = cache_get_stream(track_url).await {
        tracing::debug!(track_url, stream_url = %stream.url, "stream cache hit");
        return Ok(stream);
    }

    let negative_key = crate::audio::runtime::negative_cache_key("stream", track_url);
    if let Some(entry) = crate::audio::runtime::negative_cache_get(&negative_key).await {
        tracing::info!(track_url, reason = %entry.reason, "negative cache hit");
        return Err(SerenyaError::Audio(format!(
            "Skipping recently failed source: {}",
            entry.reason
        )));
    }

    let youtube_url = is_youtube_url(track_url);

    if youtube_url {
        if let Some(stream) = resolve_youtube_stream_native(track_url).await {
            tracing::info!(track_url, stream_url = %stream.url, "native stream resolution succeeded");
            cache_set_stream(track_url.to_owned(), &stream).await;
            return Ok(stream);
        }
        crate::audio::runtime::remember_negative(
            negative_key,
            "native YouTube stream resolution failed without yt-dlp fallback".to_owned(),
        )
        .await;
        return Err(SerenyaError::Audio(
            "native YouTube stream resolution failed".to_owned(),
        ));
    }

    if track_url.contains("soundcloud.com/") {
        match resolve_soundcloud_stream_url(track_url).await {
            Ok(stream) => {
                cache_set_stream(track_url.to_owned(), &stream).await;
                return Ok(stream);
            }
            Err(e) => {
                crate::audio::runtime::remember_negative(
                    negative_key.clone(),
                    format!("SoundCloud native resolution failed: {e}"),
                )
                .await;
                return Err(e);
            }
        }
    }

    // Non-YouTube URL fallback.
    let res = run_ytdlp_stream_resolution(track_url, youtube_url, &negative_key).await;
    if let Ok(ref stream) = res {
        tracing::info!(track_url, stream_url = %stream.url, "yt-dlp stream resolution succeeded");
        cache_set_stream(track_url.to_owned(), stream).await;
    }
    res
}

fn is_direct_stream_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("googlevideo.com") || lower.contains("googleusercontent.com")
}

async fn resolve_youtube_stream_native(
    track_url: &str,
) -> Option<youtube_resolver::ResolvedStream> {
    // 1. Try our custom youtube_resolver (direct Google stream via ANDROID/IOS mobile clients)
    if let Some(video_id) = extract_youtube_video_id(track_url) {
        let ctx = youtube_resolver::ResolveContext::default();
        let resolver_future = youtube_resolver::resolve_best_audio_stream(video_id, &ctx);
        if let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(5), resolver_future).await
        {
            if is_direct_stream_url(&stream.url) {
                tracing::debug!(track_url, stream_url = %stream.url, "youtube_resolver resolved direct stream");
                return Some(stream);
            }
            tracing::debug!(url = %stream.url, "rejecting non-direct stream URL from youtube_resolver");
        } else {
            tracing::debug!(
                track_url,
                "youtube_resolver stream resolution failed or timed out"
            );
        }
    }

    // 2. Fallback to Invidious/Piped Proxy
    if let Some(video_id) = extract_youtube_video_id(track_url)
        && let Some(url) = resolve_via_invidious_or_piped(video_id, &HTTP_CLIENT).await
    {
        if is_direct_stream_url(&url) {
            tracing::debug!(track_url, stream_url = %url, "invidious/piped resolved direct stream");
            return Some(youtube_resolver::ResolvedStream {
                url,
                client_kind: "WEB".to_string(),
                user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36".to_string(),
                expires_at: None,
                mime_type: None,
                bitrate: None,
                resolve_source: "invidious".to_string(),
            });
        }
        tracing::debug!(url = %url, "rejecting non-direct stream URL from proxies");
    }

    None
}

pub fn create_stream_input(
    original_url: Option<String>,
    stream: youtube_resolver::ResolvedStream,
    eight_d_enabled: bool,
) -> Result<songbird::input::Input, SerenyaError> {
    // Always use ffmpeg for robust playback — handles reconnection, headers,
    // and various URL types better than songbird's built-in HttpRequest
    create_ffmpeg_stream_input(original_url, &stream, None, eight_d_enabled)
}

pub fn create_ffmpeg_stream_input(
    original_url: Option<String>,
    stream: &youtube_resolver::ResolvedStream,
    seek: Option<Duration>,
    eight_d_enabled: bool,
) -> Result<songbird::input::Input, SerenyaError> {
    let mut args = vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
        "-reconnect".to_owned(),
        "1".to_owned(),
        "-reconnect_streamed".to_owned(),
        "1".to_owned(),
        "-reconnect_on_network_error".to_owned(),
        "1".to_owned(),
        "-reconnect_delay_max".to_owned(),
        "5".to_owned(),
        "-multiple_requests".to_owned(),
        "1".to_owned(),
        "-rw_timeout".to_owned(),
        "15000000".to_owned(), // 15s timeout for stalled reads/writes (in microseconds)
    ];

    if let Some(position) = seek {
        args.push("-ss".to_owned());
        args.push(position.as_secs().to_string());
    }

    let is_youtube = stream.url.contains("youtube.com")
        || stream.url.contains("googlevideo.com")
        || stream.url.contains("youtu.be");
    let is_soundcloud = stream.client_kind == "SOUNDCLOUD" || stream.url.contains("soundcloud");

    let mut headers = String::new();

    if is_youtube {
        let ua = if !stream.user_agent.is_empty() {
            &stream.user_agent
        } else {
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36"
        };
        args.push(ua.to_owned());
    } else {
        headers.push_str("User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36\r\n");
    }

    let is_web_youtube_client = stream.client_kind.is_empty()
        || stream.client_kind == "WEB"
        || stream.client_kind == "WEB_SAFARI";
    if is_youtube && is_web_youtube_client {
        args.push("-headers".to_owned());
        args.push(
            "Referer: https://www.youtube.com/\r\nOrigin: https://www.youtube.com\r\n".to_owned(),
        );
    } else if is_soundcloud {
        args.push("-headers".to_owned());
        args.push(
            "Referer: https://soundcloud.com/\r\nOrigin: https://soundcloud.com\r\n".to_owned(),
        );
    }

    args.extend([
        "-i".to_owned(),
        stream.url.to_owned(),
        "-vn".to_owned(),
        "-ar".to_owned(),
        "48000".to_owned(),
        "-ac".to_owned(),
        "2".to_owned(),
    ]);

    if eight_d_enabled {
        args.push("-af".to_owned());
        args.push("apulsator=hz=0.08:amount=0.85,aecho=0.8:0.88:40:0.4".to_owned());
    }

    args.extend([
        "-f".to_owned(),
        "wav".to_owned(),
        "-acodec".to_owned(),
        "pcm_s16le".to_owned(),
        "pipe:1".to_owned(),
    ]);

    let mut child = Command::new("ffmpeg")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| SerenyaError::Audio(format!("Failed to spawn ffmpeg: {e}")))?;

    // Log ffmpeg stderr in background for diagnostics and detect 403 Forbidden
    if let Some(stderr) = child.stderr.take() {
        let pid = child.id();
        let rt_handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut buf = String::new();
            let mut reader = std::io::BufReader::new(stderr);
            while let Ok(bytes) = reader.read_line(&mut buf) {
                if bytes == 0 {
                    break;
                }

                if buf.contains("403 Forbidden") || buf.contains("Server returned 403 Forbidden") {
                    tracing::warn!(
                        "FFmpeg encountered 403 Forbidden! Invalidating cache and killing process..."
                    );

                    if let Some(url) = original_url.clone() {
                        rt_handle.spawn(async move {
                            cache_invalidate_stream(&url).await;
                        });
                    }

                    // Kill the ffmpeg process cross-platform
                    #[cfg(target_os = "windows")]
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/PID", &pid.to_string()])
                        .spawn();
                    #[cfg(not(target_os = "windows"))]
                    let _ = std::process::Command::new("kill")
                        .args(["-9", &pid.to_string()])
                        .spawn();

                    break;
                }

                buf.clear();
            }
        });
    }

    let child_container: songbird::input::ChildContainer = child.into();
    Ok(child_container.into())
}

/// Rebuilds all caches with fresh TTL values from current settings.
/// Called by /reload to ensure config changes actually take effect.
pub fn clear_caches() -> (usize, usize) {
    let q_len = QUERY_CACHE.load().entry_count() as usize;
    let m_len = METADATA_CACHE.load().entry_count() as usize;
    let s_len = STREAM_CACHE.load().entry_count() as usize;
    // Rebuild with current TTL from settings (not the frozen initial values)
    QUERY_CACHE.store(Arc::new(build_query_cache()));
    METADATA_CACHE.store(Arc::new(build_metadata_cache()));
    STREAM_CACHE.store(Arc::new(build_stream_cache()));
    (q_len + m_len, s_len)
}
