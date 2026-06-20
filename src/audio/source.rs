use crate::core::Track;
use crate::utils::SerenyaError;
use moka::future::Cache;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct TimedCacheEntry<T> {
    value: T,
    inserted_at: Instant,
}

impl<T> TimedCacheEntry<T> {
    fn new(value: T) -> Self {
        Self {
            value,
            inserted_at: Instant::now(),
        }
    }

    fn is_fresh(&self, ttl_seconds: u64) -> bool {
        self.inserted_at.elapsed().as_secs() <= ttl_seconds
    }
}

static QUERY_CACHE: LazyLock<Cache<String, TimedCacheEntry<Track>>> =
    LazyLock::new(|| Cache::builder().max_capacity(2048).build());

static METADATA_CACHE: LazyLock<Cache<String, TimedCacheEntry<Track>>> =
    LazyLock::new(|| Cache::builder().max_capacity(4096).build());

static STREAM_CACHE: LazyLock<Cache<String, TimedCacheEntry<String>>> =
    LazyLock::new(|| Cache::builder().max_capacity(4096).build());

pub async fn cache_get_metadata(query: &str) -> Option<Track> {
    let entry = QUERY_CACHE.get(query).await?;
    if entry.is_fresh(crate::audio::runtime::settings().query_cache_ttl_seconds) {
        tracing::debug!(query, cache = "query", "cache hit");
        return Some(entry.value);
    }
    QUERY_CACHE.invalidate(query).await;
    tracing::debug!(query, cache = "query", "cache expired");
    None
}

pub async fn cache_set_metadata(query: String, track: Track) {
    QUERY_CACHE.insert(query, TimedCacheEntry::new(track)).await;
}

pub async fn cache_get_url_metadata(url: &str) -> Option<Track> {
    let entry = METADATA_CACHE.get(url).await?;
    if entry.is_fresh(crate::audio::runtime::settings().metadata_cache_ttl_seconds) {
        tracing::debug!(url, cache = "metadata", "cache hit");
        return Some(entry.value);
    }
    METADATA_CACHE.invalidate(url).await;
    tracing::debug!(url, cache = "metadata", "cache expired");
    None
}

pub async fn cache_set_url_metadata(url: String, track: Track) {
    METADATA_CACHE
        .insert(url, TimedCacheEntry::new(track))
        .await;
}

pub async fn cache_get_stream(url: &str) -> Option<String> {
    let entry = STREAM_CACHE.get(url).await?;
    if entry.is_fresh(crate::audio::runtime::settings().stream_cache_ttl_seconds) {
        tracing::debug!(url, cache = "stream", "cache hit");
        return Some(entry.value);
    }
    STREAM_CACHE.invalidate(url).await;
    tracing::debug!(url, cache = "stream", "cache expired");
    None
}

pub async fn cache_set_stream(url: String, stream_url: String) {
    STREAM_CACHE
        .insert(url, TimedCacheEntry::new(stream_url))
        .await;
}

fn url_encode(s: &str) -> String {
    let mut encoded = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
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
        .json::<serde_json::Value>()
        .await
        .map_err(|e| SerenyaError::Audio(format!("failed to parse YouTube oEmbed JSON: {}", e)))?;

    let title = val
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SerenyaError::Audio("Missing title in oEmbed response".to_owned()))?;
    let thumbnail = val
        .get("thumbnail_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    Ok((title.to_owned(), thumbnail))
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
    // 1. Try Piped API
    let piped_url = format!("https://pipedapi.kavin.rocks/streams/{}", video_id);
    if let Ok(Ok(resp)) =
        tokio::time::timeout(Duration::from_secs(4), client.get(&piped_url).send()).await
    {
        if let Ok(val) = resp.json::<serde_json::Value>().await {
            if let Some(audio_streams) = val.get("audioStreams").and_then(|a| a.as_array()) {
                if let Some(best) = audio_streams
                    .first()
                    .and_then(|s| s.get("url"))
                    .and_then(|u| u.as_str())
                {
                    return Some(best.to_owned());
                }
            }
        }
    }

    // 2. Try Invidious API
    let invidious_url = format!("https://yewtu.be/api/v1/videos/{}", video_id);
    if let Ok(Ok(resp)) =
        tokio::time::timeout(Duration::from_secs(4), client.get(&invidious_url).send()).await
    {
        if let Ok(val) = resp.json::<serde_json::Value>().await {
            if let Some(adaptive_formats) = val.get("adaptiveFormats").and_then(|a| a.as_array()) {
                for format in adaptive_formats {
                    let type_str = format.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if type_str.starts_with("audio/") {
                        if let Some(url) = format.get("url").and_then(|u| u.as_str()) {
                            return Some(url.to_owned());
                        }
                    }
                }
            }
        }
    }

    None
}

pub async fn extract_stream_url_for_guild(
    guild_id: u64,
    track_url: &str,
) -> Result<String, SerenyaError> {
    let _guild_permit = crate::audio::runtime::acquire_guild_resolve(guild_id).await?;
    extract_stream_url_inner(track_url).await
}

pub async fn prefetch_stream_url_for_guild(
    guild_id: u64,
    track_url: &str,
) -> Result<Option<String>, SerenyaError> {
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
) -> Result<String, SerenyaError> {
    let output = crate::audio::runtime::run_ytdlp(
        "stream resolution",
        vec![
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
        ],
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

    Ok(stream_url)
}

async fn extract_stream_url_inner(track_url: &str) -> Result<String, SerenyaError> {
    if let Some(stream_url) = cache_get_stream(track_url).await {
        tracing::debug!(track_url, stream_url = %stream_url, "stream cache hit");
        return Ok(stream_url);
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

    if youtube_url && !crate::audio::runtime::is_youtube_degraded() {
        if let Some(stream_url) = resolve_youtube_stream_native(track_url).await {
            tracing::info!(track_url, stream_url = %stream_url, "native stream resolution succeeded");
            cache_set_stream(track_url.to_owned(), stream_url.clone()).await;
            return Ok(stream_url);
        }
        tracing::debug!(track_url, "native stream resolution failed, falling back to yt-dlp");
    }

    // Non-YouTube URL or fallback when native resolution fails
    let res = run_ytdlp_stream_resolution(track_url, youtube_url, &negative_key).await;
    if let Ok(ref stream_url) = res {
        tracing::info!(track_url, stream_url = %stream_url, "yt-dlp stream resolution succeeded");
        cache_set_stream(track_url.to_owned(), stream_url.clone()).await;
    }
    res
}

async fn resolve_via_rusty_ytdl(track_url: &str) -> Result<String, SerenyaError> {
    use rusty_ytdl::{Video, VideoOptions, VideoSearchOptions, choose_format};

    let video = Video::new(track_url)
        .map_err(|e| SerenyaError::Audio(format!("rusty_ytdl init failed: {e}")))?;

    let video_info = video
        .get_info()
        .await
        .map_err(|e| SerenyaError::Audio(format!("rusty_ytdl get_info failed: {e}")))?;

    let video_options = VideoOptions {
        filter: VideoSearchOptions::Audio,
        ..Default::default()
    };

    let format = choose_format(&video_info.formats, &video_options)
        .map_err(|e| SerenyaError::Audio(format!("rusty_ytdl choose_format failed: {e}")))?;

    Ok(format.url.clone())
}

fn is_direct_stream_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("googlevideo.com") || lower.contains("googleusercontent.com")
}

async fn resolve_youtube_stream_native(track_url: &str) -> Option<String> {
    let mut join_set = tokio::task::JoinSet::new();
    let track_url_owned = track_url.to_owned();

    join_set.spawn(async move {
        match resolve_via_rusty_ytdl(&track_url_owned).await {
            Ok(url) => {
                tracing::debug!(track_url = %track_url_owned, stream_url = %url, "rusty_ytdl resolved");
                Some(url)
            }
            Err(err) => {
                tracing::debug!(track_url = %track_url_owned, %err, "rusty_ytdl stream resolution failed");
                None
            }
        }
    });

    if let Some(video_id) = extract_youtube_video_id(track_url).map(str::to_owned) {
        join_set.spawn(async move {
            let client = reqwest::Client::new();
            resolve_via_invidious_or_piped(&video_id, &client).await
        });
    }

    let deadline = Duration::from_secs(4);
    let started = Instant::now();
    while started.elapsed() < deadline {
        let remaining = deadline.saturating_sub(started.elapsed());
        match tokio::time::timeout(remaining, join_set.join_next()).await {
            Ok(Some(Ok(Some(url)))) => {
                // Only accept direct Google video URLs — reject piped/invidious proxy URLs
                // which are unreliable and often broken
                if is_direct_stream_url(&url) {
                    join_set.abort_all();
                    while join_set.join_next().await.is_some() {}
                    return Some(url);
                }
                tracing::debug!(url = %url, "rejecting non-direct stream URL from native resolver");
            }
            Ok(Some(Ok(None))) | Ok(Some(Err(_))) => {}
            Ok(None) | Err(_) => break,
        }
    }

    join_set.abort_all();
    while join_set.join_next().await.is_some() {}
    None
}

pub fn create_stream_input(
    _http_client: reqwest::Client,
    stream_url: String,
    eight_d_enabled: bool,
) -> Result<songbird::input::Input, SerenyaError> {
    // Always use ffmpeg for robust playback — handles reconnection, headers,
    // and various URL types better than songbird's built-in HttpRequest
    create_ffmpeg_stream_input(&stream_url, None, eight_d_enabled)
}

pub fn create_ffmpeg_stream_input(
    stream_url: &str,
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
        "-reconnect_delay_max".to_owned(),
        "5".to_owned(),
    ];

    if let Some(position) = seek {
        args.push("-ss".to_owned());
        args.push(position.as_secs().to_string());
    }

    args.extend([
        "-user_agent".to_owned(),
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36".to_owned(),
        "-headers".to_owned(),
        "Referer: https://www.youtube.com/\r\nOrigin: https://www.youtube.com\r\n".to_owned(),
        "-i".to_owned(),
        stream_url.to_owned(),
        "-vn".to_owned(),
        "-ar".to_owned(),
        "48000".to_owned(),
        "-ac".to_owned(),
        "2".to_owned(),
    ]);

    if eight_d_enabled {
        args.push("-af".to_owned());
        args.push("apulsator=hz=0.08:amount=0.85,freeverb=room_size=0.5:damp=0.5:wet_level=0.33:dry_level=1.0:width=1.0".to_owned());
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

    // Log ffmpeg stderr in background for diagnostics
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            let mut reader = std::io::BufReader::new(stderr);
            if reader.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                tracing::warn!(ffmpeg_stderr = %buf, "ffmpeg stderr output");
            }
        });
    }

    let child_container: songbird::input::ChildContainer = child.into();
    Ok(child_container.into())
}

pub fn clear_caches() -> (usize, usize) {
    let q_len = QUERY_CACHE.entry_count() as usize;
    let m_len = METADATA_CACHE.entry_count() as usize;
    let s_len = STREAM_CACHE.entry_count() as usize;
    QUERY_CACHE.invalidate_all();
    METADATA_CACHE.invalidate_all();
    STREAM_CACHE.invalidate_all();
    (q_len + m_len, s_len)
}
