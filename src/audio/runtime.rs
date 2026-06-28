use arc_swap::{ArcSwap, ArcSwapOption};
use std::process::Output;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use moka::future::Cache;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::ResolverSection;
use crate::utils::SerenyaError;

const YOUTUBE_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug)]
pub struct NegativeCacheEntry {
    pub reason: String,
}

fn check_ytdlp_available() -> bool {
    std::process::Command::new("yt-dlp")
        .arg("--version")
        .output()
        .is_ok()
        || std::path::Path::new("yt-dlp.exe").exists()
        || std::path::Path::new("yt-dlp").exists()
}

fn build_negative_cache(settings: &ResolverSection) -> Cache<String, NegativeCacheEntry> {
    Cache::builder()
        .max_capacity(settings.negative_cache_max_capacity as u64)
        .time_to_live(Duration::from_secs(settings.negative_cache_ttl_seconds))
        .build()
}

struct ResolverRuntime {
    settings: ArcSwap<ResolverSection>,
    spotify_settings: ArcSwapOption<crate::config::SpotifySection>,
    ytdlp_semaphore: RwLock<Arc<Semaphore>>,
    soundcloud_semaphore: RwLock<Arc<Semaphore>>,
    guild_resolve_semaphores: DashMap<u64, Arc<Semaphore>>,
    negative_cache: ArcSwap<Cache<String, NegativeCacheEntry>>,
    youtube_degraded_until: RwLock<Option<Instant>>,
    max_playlist_import: std::sync::atomic::AtomicUsize,
    ytdlp_fallback_active: std::sync::atomic::AtomicBool,
    spotify_embed_fallback_active: std::sync::atomic::AtomicBool,
}

impl ResolverRuntime {
    fn new() -> Self {
        let settings = ResolverSection::default();
        let ytdlp_active = settings.enable_ytdlp_youtube_fallback && check_ytdlp_available();
        let spotify_active = settings.enable_spotify_embed_fallback;
        Self {
            ytdlp_semaphore: RwLock::new(Arc::new(Semaphore::new(settings.max_concurrent_ytdlp))),
            soundcloud_semaphore: RwLock::new(Arc::new(Semaphore::new(
                settings.max_concurrent_soundcloud,
            ))),
            guild_resolve_semaphores: DashMap::new(),
            negative_cache: ArcSwap::from_pointee(build_negative_cache(&settings)),
            settings: ArcSwap::from_pointee(settings),
            spotify_settings: ArcSwapOption::const_empty(),
            youtube_degraded_until: RwLock::new(None),
            max_playlist_import: std::sync::atomic::AtomicUsize::new(100),
            ytdlp_fallback_active: std::sync::atomic::AtomicBool::new(ytdlp_active),
            spotify_embed_fallback_active: std::sync::atomic::AtomicBool::new(spotify_active),
        }
    }
}

static RESOLVER_RUNTIME: LazyLock<ResolverRuntime> = LazyLock::new(ResolverRuntime::new);

pub fn configure(
    settings: &ResolverSection,
    spotify: &crate::config::SpotifySection,
    max_playlist_import: usize,
) {
    RESOLVER_RUNTIME.settings.store(Arc::new(settings.clone()));
    RESOLVER_RUNTIME
        .spotify_settings
        .store(Some(Arc::new(spotify.clone())));
    RESOLVER_RUNTIME
        .max_playlist_import
        .store(max_playlist_import, std::sync::atomic::Ordering::Relaxed);

    let ytdlp_active = settings.enable_ytdlp_youtube_fallback && check_ytdlp_available();
    RESOLVER_RUNTIME
        .ytdlp_fallback_active
        .store(ytdlp_active, std::sync::atomic::Ordering::Relaxed);

    let spotify_active = settings.enable_spotify_embed_fallback;
    RESOLVER_RUNTIME
        .spotify_embed_fallback_active
        .store(spotify_active, std::sync::atomic::Ordering::Relaxed);

    if let Ok(mut semaphore) = RESOLVER_RUNTIME.ytdlp_semaphore.write() {
        *semaphore = Arc::new(Semaphore::new(settings.max_concurrent_ytdlp));
    }
    if let Ok(mut semaphore) = RESOLVER_RUNTIME.soundcloud_semaphore.write() {
        *semaphore = Arc::new(Semaphore::new(settings.max_concurrent_soundcloud));
    }
    RESOLVER_RUNTIME.guild_resolve_semaphores.clear();
    RESOLVER_RUNTIME
        .negative_cache
        .store(Arc::new(build_negative_cache(settings)));
}

pub fn cleanup_guild(guild_id: u64) {
    RESOLVER_RUNTIME.guild_resolve_semaphores.remove(&guild_id);
}

pub fn negative_cache_entry_count() -> u64 {
    RESOLVER_RUNTIME.negative_cache.load().entry_count()
}

pub fn settings() -> Arc<ResolverSection> {
    RESOLVER_RUNTIME.settings.load().clone()
}

pub fn spotify_settings() -> Option<Arc<crate::config::SpotifySection>> {
    RESOLVER_RUNTIME.spotify_settings.load().clone()
}

pub fn is_ytdlp_fallback_active() -> bool {
    RESOLVER_RUNTIME
        .ytdlp_fallback_active
        .load(std::sync::atomic::Ordering::Relaxed)
}

pub fn is_spotify_embed_fallback_active() -> bool {
    RESOLVER_RUNTIME
        .spotify_embed_fallback_active
        .load(std::sync::atomic::Ordering::Relaxed)
}

pub fn max_playlist_import() -> usize {
    RESOLVER_RUNTIME
        .max_playlist_import
        .load(std::sync::atomic::Ordering::Relaxed)
}

pub fn duration_from_millis(ms: u64) -> Duration {
    Duration::from_millis(ms.max(1))
}

pub fn yt_dlp_timeout() -> Duration {
    Duration::from_secs(settings().yt_dlp_timeout_seconds.max(1))
}

pub fn prefetch_timeout() -> Duration {
    Duration::from_secs(settings().prefetch_timeout_seconds.max(1))
}

pub async fn acquire_guild_resolve(guild_id: u64) -> Result<OwnedSemaphorePermit, SerenyaError> {
    guild_resolve_semaphore(guild_id)
        .acquire_owned()
        .await
        .map_err(|_| SerenyaError::Audio("guild resolver limiter is closed".into()))
}

pub fn try_acquire_guild_resolve(guild_id: u64) -> Option<OwnedSemaphorePermit> {
    guild_resolve_semaphore(guild_id).try_acquire_owned().ok()
}

fn guild_resolve_semaphore(guild_id: u64) -> Arc<Semaphore> {
    RESOLVER_RUNTIME
        .guild_resolve_semaphores
        .entry(guild_id)
        .or_insert_with(|| {
            Arc::new(Semaphore::new(
                settings().max_concurrent_resolves_per_guild.max(1),
            ))
        })
        .clone()
}

async fn acquire_ytdlp() -> Result<OwnedSemaphorePermit, SerenyaError> {
    let semaphore = RESOLVER_RUNTIME
        .ytdlp_semaphore
        .read()
        .map(|semaphore| semaphore.clone())
        .map_err(|_| SerenyaError::Audio("yt-dlp limiter is poisoned".into()))?;

    semaphore
        .acquire_owned()
        .await
        .map_err(|_| SerenyaError::Audio("yt-dlp limiter is closed".into()))
}

pub async fn acquire_soundcloud_resolve() -> Result<OwnedSemaphorePermit, SerenyaError> {
    let semaphore = RESOLVER_RUNTIME
        .soundcloud_semaphore
        .read()
        .map(|semaphore| semaphore.clone())
        .map_err(|_| SerenyaError::Audio("SoundCloud limiter is poisoned".into()))?;

    semaphore
        .acquire_owned()
        .await
        .map_err(|_| SerenyaError::Audio("SoundCloud limiter is closed".into()))
}

pub async fn run_ytdlp(
    context: &'static str,
    args: Vec<String>,
    timeout_duration: Duration,
    youtube_sensitive: bool,
    negative_cache_key: Option<String>,
) -> Result<Output, SerenyaError> {
    if youtube_sensitive && is_youtube_degraded() {
        return Err(youtube_degraded_error());
    }

    let _permit = acquire_ytdlp().await?;
    let started = Instant::now();
    let mut command = tokio::process::Command::new("yt-dlp");
    command.args(&args).kill_on_drop(true);
    #[cfg(windows)]
    {
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW);
    }

    let output = tokio::time::timeout(timeout_duration, command.output())
        .await
        .map_err(|_| {
            SerenyaError::Audio(format!("{context} timed out after {timeout_duration:?}"))
        })?
        .map_err(|err| {
            SerenyaError::Audio(format!("Failed to execute yt-dlp for {context}: {err}"))
        })?;

    let elapsed_ms = started.elapsed().as_millis();
    tracing::debug!(
        context,
        elapsed_ms,
        status = %output.status,
        "yt-dlp subprocess finished"
    );

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if youtube_sensitive && contains_youtube_rate_limit(&stderr) {
            mark_youtube_degraded(YOUTUBE_RATE_LIMIT_COOLDOWN);
        }
        if should_negative_cache(&stderr)
            && let Some(key) = negative_cache_key
        {
            remember_negative(key, summarize_stderr(&stderr)).await;
        }
        return Err(SerenyaError::Audio(format!(
            "yt-dlp {context} error: {}",
            summarize_stderr(&stderr)
        )));
    }

    Ok(output)
}

pub fn is_youtube_degraded() -> bool {
    let now = Instant::now();
    let degraded_until = match RESOLVER_RUNTIME.youtube_degraded_until.read() {
        Ok(guard) => *guard,
        Err(_) => return false,
    };

    match degraded_until {
        Some(until) if until > now => true,
        Some(_) => {
            clear_youtube_degraded_if_expired();
            false
        }
        None => false,
    }
}

pub fn youtube_degraded_remaining() -> Option<Duration> {
    let now = Instant::now();
    RESOLVER_RUNTIME
        .youtube_degraded_until
        .read()
        .ok()
        .and_then(|guard| guard.and_then(|until| until.checked_duration_since(now)))
}

pub fn youtube_degraded_error() -> SerenyaError {
    let remaining = youtube_degraded_remaining()
        .map(|duration| {
            format!(
                " Try again in about {} minutes.",
                duration.as_secs().div_ceil(60)
            )
        })
        .unwrap_or_default();

    SerenyaError::Audio(format!(
        "YouTube is temporarily rate-limited, so Serenya is avoiding new yt-dlp requests.{remaining}"
    ))
}

pub fn mark_youtube_degraded(cooldown: Duration) {
    let until = Instant::now() + cooldown;
    if let Ok(mut degraded_until) = RESOLVER_RUNTIME.youtube_degraded_until.write() {
        *degraded_until = Some(until);
    }
    tracing::warn!(
        reason = "youtube_rate_limit",
        cooldown_seconds = cooldown.as_secs(),
        "resolver degraded"
    );
}

fn clear_youtube_degraded_if_expired() {
    if let Ok(mut degraded_until) = RESOLVER_RUNTIME.youtube_degraded_until.write()
        && degraded_until
            .map(|until| until <= Instant::now())
            .unwrap_or(false)
    {
        *degraded_until = None;
    }
}

pub async fn remember_negative(key: String, reason: String) {
    let entry = NegativeCacheEntry { reason };
    RESOLVER_RUNTIME
        .negative_cache
        .load()
        .insert(key, entry)
        .await;
}

pub async fn negative_cache_get(key: &str) -> Option<NegativeCacheEntry> {
    RESOLVER_RUNTIME.negative_cache.load().get(key).await
}

pub fn negative_cache_key(namespace: &str, id: &str) -> String {
    format!("{namespace}:{id}")
}

pub fn contains_youtube_rate_limit(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("rate-limited by youtube") || lower.contains("rate limited by youtube")
}

pub fn should_negative_cache(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    contains_youtube_rate_limit(&lower)
        || lower.contains("video unavailable")
        || lower.contains("content isn't available")
        || lower.contains("requested format is not available")
        || lower.contains("this video is unavailable")
        || lower.contains("private video")
}

fn summarize_stderr(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.len() <= 400 {
        trimmed.to_owned()
    } else {
        format!("{}...", trimmed.chars().take(400).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_youtube_rate_limit_message() {
        assert!(contains_youtube_rate_limit(
            "The current session has been rate-limited by YouTube for up to an hour."
        ));
    }

    #[test]
    fn detects_negative_cache_errors() {
        assert!(should_negative_cache(
            "ERROR: Video unavailable. This content isn't available, try again later."
        ));
        assert!(should_negative_cache(
            "ERROR: Requested format is not available."
        ));
    }
}
