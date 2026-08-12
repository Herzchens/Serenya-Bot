use crate::{ResolveError, SessionData};
use std::future::Future;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

struct SessionStore {
    cache: RwLock<Option<(SessionData, Instant)>>,
    refresh: Mutex<()>,
    ttl: Duration,
}

impl SessionStore {
    fn new(ttl: Duration) -> Self {
        Self {
            cache: RwLock::new(None),
            refresh: Mutex::new(()),
            ttl,
        }
    }

    async fn cached(&self) -> Option<SessionData> {
        let cache = self.cache.read().await;
        cache
            .as_ref()
            .and_then(|(data, fetched_at)| (fetched_at.elapsed() < self.ttl).then(|| data.clone()))
    }

    async fn get_or_fetch<F, Fut>(&self, fetch: F) -> Result<SessionData, ResolveError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<SessionData, ResolveError>>,
    {
        if let Some(data) = self.cached().await {
            return Ok(data);
        }

        let _refresh_guard = self.refresh.lock().await;
        if let Some(data) = self.cached().await {
            return Ok(data);
        }

        let data = fetch().await?;
        *self.cache.write().await = Some((data.clone(), Instant::now()));
        Ok(data)
    }
}

static SESSION_STORE: LazyLock<SessionStore> =
    LazyLock::new(|| SessionStore::new(Duration::from_secs(6 * 3600)));

pub async fn get_or_fetch_session(
    http_client: &reqwest::Client,
) -> Result<SessionData, ResolveError> {
    SESSION_STORE
        .get_or_fetch(|| fetch_session(http_client))
        .await
}

async fn fetch_session(http_client: &reqwest::Client) -> Result<SessionData, ResolveError> {
    let body = http_client
        .get("https://www.youtube.com/watch?v=dQw4w9WgXcQ&hl=en")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .send()
        .await?
        .text()
        .await?;

    let visitor_data = rusty_ytdl::get_visitor_data(&body)
        .unwrap_or_else(|_| "CgtyckVza05NMXhtOCiV8-m_BjIKCgJWThIEGgAgSw==".to_string());
    let sts = rusty_ytdl::get_ytconfig(&body)
        .ok()
        .and_then(|ytcfg| ytcfg.sts)
        .unwrap_or(19950);
    let player_url = normalize_player_url(
        extract_player_url_path(&body)
            .unwrap_or_else(|| "/s/player/9b27514a/player_ias.vflset/en_US/base.js".to_string()),
    );
    Ok(SessionData {
        visitor_data,
        sts,
        player_url,
    })
}

fn normalize_player_url(path: String) -> String {
    if path.starts_with("https://") {
        path
    } else {
        format!("https://www.youtube.com{path}")
    }
}

fn extract_player_url_path(body: &str) -> Option<String> {
    let patterns = [
        r#""jsUrl"\s*:\s*"([^"]+base\.js[^"]*)""#,
        r#""PLAYER_JS_URL"\s*:\s*"([^"]+base\.js[^"]*)""#,
        r#"<script[^>]+src="([^"]+base\.js[^"]*)""#,
        r#"/s/player/[a-zA-Z0-9-_]+/player_ias\.vflset/[a-zA-Z0-9-_]+/base\.js"#,
    ];

    for pattern in patterns {
        let Ok(re) = regex::Regex::new(pattern) else {
            continue;
        };
        if let Some(caps) = re.captures(body)
            && let Some(matched) = caps.get(1).or_else(|| caps.get(0))
        {
            return Some(matched.as_str().replace(r"\/", "/"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::SessionStore;
    use crate::SessionData;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn session() -> SessionData {
        SessionData {
            visitor_data: "visitor".to_owned(),
            sts: 12345,
            player_url: "https://www.youtube.com/s/player/test/base.js".to_owned(),
        }
    }

    #[tokio::test]
    async fn concurrent_cache_misses_share_one_refresh() {
        let store = Arc::new(SessionStore::new(Duration::from_secs(60)));
        let fetches = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();

        for _ in 0..20 {
            let store = Arc::clone(&store);
            let fetches = Arc::clone(&fetches);
            tasks.push(tokio::spawn(async move {
                store
                    .get_or_fetch(|| {
                        let fetches = Arc::clone(&fetches);
                        async move {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(25)).await;
                            Ok(session())
                        }
                    })
                    .await
                    .unwrap()
            }));
        }

        for task in tasks {
            assert_eq!(task.await.unwrap().visitor_data, "visitor");
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cached_session_does_not_enter_refresh_path_again() {
        let store = SessionStore::new(Duration::from_secs(60));
        let fetches = AtomicUsize::new(0);

        for _ in 0..2 {
            store
                .get_or_fetch(|| async {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    Ok(session())
                })
                .await
                .unwrap();
        }

        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }
}
