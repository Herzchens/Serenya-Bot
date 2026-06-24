use crate::{ResolveError, SessionData};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

static SESSION_CACHE: LazyLock<Mutex<Option<(SessionData, Instant)>>> =
    LazyLock::new(|| Mutex::new(None));

pub async fn get_or_fetch_session(
    http_client: &reqwest::Client,
) -> Result<SessionData, ResolveError> {
    let mut cache = SESSION_CACHE.lock().await;
    if let Some((ref data, fetched_at)) = *cache {
        if fetched_at.elapsed() < Duration::from_secs(6 * 3600) {
            return Ok(data.clone());
        }
    }

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
    let data = SessionData {
        visitor_data,
        sts,
        player_url,
    };

    *cache = Some((data.clone(), Instant::now()));
    Ok(data)
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
