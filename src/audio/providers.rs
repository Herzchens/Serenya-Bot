use crate::audio::ranking::TrackCandidate;
use crate::core::{SourceType, Track};
use crate::utils::SerenyaError;
use async_trait::async_trait;
use poise::serenity_prelude as serenity;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ExternalTrackMeta {
    pub title: String,
    pub artist: Option<String>,
    pub duration: Option<Duration>,
    pub thumbnail: Option<std::sync::Arc<str>>,
}

#[async_trait]
pub trait MetadataProvider: Send + Sync {
    fn supports(&self, input: &str) -> bool;
    async fn search(
        &self,
        query: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError>;
}

// ----------------------------------------------------
// Utilities
// ----------------------------------------------------
pub fn url_encode(s: &str) -> String {
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

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn extract_meta(html: &str, property: &str) -> Option<String> {
    let p_idx = html.find(property)?;
    let tag_start = html[..p_idx].rfind('<')?;
    let tag_end = p_idx + html[p_idx..].find('>')?;
    let tag = &html[tag_start..tag_end];

    let content_pat = "content=";
    let c_idx = tag.find(content_pat)?;
    let after_content = &tag[c_idx + content_pat.len()..];
    let quote_char = after_content.chars().next()?;
    if quote_char == '"' || quote_char == '\'' {
        let rest = &after_content[1..];
        let end_idx = rest.find(quote_char)?;
        Some(decode_html_entities(&rest[..end_idx]))
    } else {
        None
    }
}

fn parse_simple_duration(s: &str) -> Option<Duration> {
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
    } else if parts.len() == 1 {
        secs = parts[0].parse::<u64>().ok()?;
    }
    Some(Duration::from_secs(secs))
}

// ----------------------------------------------------
// Spotify Metadata Provider
// ----------------------------------------------------
pub struct SpotifyProvider;

impl SpotifyProvider {
    pub async fn resolve_metadata(
        &self,
        url: &str,
        http_client: &reqwest::Client,
    ) -> Result<ExternalTrackMeta, SerenyaError> {
        let html_fut = http_client
            .get(url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            )
            .send();

        // 10s Timeout guard
        let response = tokio::time::timeout(Duration::from_secs(10), html_fut)
            .await
            .map_err(|_| SerenyaError::Audio("Timeout fetching Spotify page".to_owned()))?
            .map_err(|e| SerenyaError::Audio(format!("failed to fetch Spotify page: {}", e)))?;

        let html = response
            .text()
            .await
            .map_err(|e| SerenyaError::Audio(format!("failed to read Spotify page: {}", e)))?;

        let og_title = extract_meta(&html, "og:title")
            .ok_or_else(|| SerenyaError::Audio("Could not parse Spotify track title".to_owned()))?;

        let og_desc = extract_meta(&html, "og:description");
        let og_image = extract_meta(&html, "og:image");

        let artist = og_desc
            .as_ref()
            .and_then(|desc| desc.find("·").map(|pos| desc[..pos].trim().to_owned()));

        let duration = extract_meta(&html, "music:duration")
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);

        Ok(ExternalTrackMeta {
            title: og_title,
            artist,
            duration,
            thumbnail: og_image.map(std::sync::Arc::from),
        })
    }
}

struct SpotifyToken {
    access_token: String,
    client_id: String,
    expires_at: std::time::Instant,
    cookie_hash: u64,
}

static SPOTIFY_TOKEN_CACHE: std::sync::OnceLock<tokio::sync::Mutex<Option<SpotifyToken>>> =
    std::sync::OnceLock::new();

const SPOTIFY_WEB_TOKEN_URL: &str = "https://open.spotify.com/api/token";
const SPOTIFY_WEB_HOME_URL: &str = "https://open.spotify.com/";
const SPOTIFY_WEB_PRODUCT_TYPE: &str = "web-player";
const SPOTIFY_WEB_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36";
const SPOTIFY_TOTP_VERSION: &str = "61";
const SPOTIFY_TOTP_CIPHER: [u8; 26] = [
    44, 55, 47, 42, 70, 40, 34, 114, 76, 74, 50, 111, 120, 97, 75, 76, 94, 102, 43, 69, 49, 120,
    118, 80, 64, 78,
];

#[derive(Clone, Debug)]
pub(crate) struct SpotifySessionInfo {
    pub access_token: String,
    pub client_id: String,
}

struct SpotifyClientTokenCache {
    client_token: String,
    client_version: String,
    device_id: String,
    expires_at: std::time::Instant,
}

static SPOTIFY_CLIENT_TOKEN_CACHE: std::sync::OnceLock<tokio::sync::Mutex<Option<SpotifyClientTokenCache>>> =
    std::sync::OnceLock::new();

pub(crate) struct SpotifyClientTokenInfo {
    pub client_token: String,
    pub client_version: String,
    pub device_id: String,
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in input.chars() {
        if c.is_whitespace() || c == '=' {
            continue;
        }
        let val = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    Some(bytes)
}

pub(crate) async fn get_spotify_session_info(
    http_client: &reqwest::Client,
    timeout: std::time::Duration,
) -> Result<SpotifySessionInfo, SerenyaError> {
    let sp_dc = crate::audio::runtime::spotify_settings()
        .and_then(|config| config.sp_dc)
        .filter(|cookie| !cookie.trim().is_empty())
        .ok_or_else(|| SerenyaError::Audio("Spotify sp_dc cookie is not configured.".to_owned()))?;

    let cookie_hash = spotify_cookie_hash(&sp_dc);
    let cache_lock = SPOTIFY_TOKEN_CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
    let now = std::time::Instant::now();
    {
        let cache = cache_lock.lock().await;
        if let Some(ref token) = *cache {
            if token.cookie_hash == cookie_hash
                && token.expires_at > now + std::time::Duration::from_secs(60)
            {
                return Ok(SpotifySessionInfo {
                    access_token: token.access_token.clone(),
                    client_id: token.client_id.clone(),
                });
            }
        }
    }

    let token_json = fetch_spotify_web_token(http_client, timeout, &sp_dc).await?;

    let access_token = token_json
        .get("accessToken")
        .or_else(|| token_json.get("access_token"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            SerenyaError::Audio("Spotify web token response missing accessToken".to_owned())
        })?
        .to_owned();

    let client_id = token_json
        .get("clientId")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            SerenyaError::Audio("Spotify web token response missing client_id".to_owned())
        })?
        .to_owned();

    let expires_at = token_json
        .get("accessTokenExpirationTimestampMs")
        .and_then(|value| value.as_u64())
        .and_then(|timestamp_ms| {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let delta_ms = timestamp_ms as i64 - now_ms;
            u64::try_from(delta_ms.max(0)).ok()
        })
        .map(std::time::Duration::from_millis)
        .unwrap_or_else(|| {
            std::time::Duration::from_secs(
                token_json
                    .get("expires_in")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(3600),
            )
        });

    crate::logging::register_secret_to_redact(&access_token);

    let mut cache = cache_lock.lock().await;
    *cache = Some(SpotifyToken {
        access_token: access_token.clone(),
        client_id: client_id.clone(),
        expires_at: now + expires_at,
        cookie_hash,
    });

    Ok(SpotifySessionInfo {
        access_token,
        client_id,
    })
}

pub(crate) async fn get_spotify_client_token_info(
    http_client: &reqwest::Client,
    client_id: &str,
    timeout: std::time::Duration,
) -> Result<SpotifyClientTokenInfo, SerenyaError> {
    let cache_lock = SPOTIFY_CLIENT_TOKEN_CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
    let now = std::time::Instant::now();
    {
        let cache = cache_lock.lock().await;
        if let Some(ref cached) = *cache {
            if cached.expires_at > now {
                return Ok(SpotifyClientTokenInfo {
                    client_token: cached.client_token.clone(),
                    client_version: cached.client_version.clone(),
                    device_id: cached.device_id.clone(),
                });
            }
        }
    }

    tracing::info!("Fetching Spotify homepage to extract clientVersion and sp_t cookie...");
    let response = tokio::time::timeout(timeout, async {
        http_client
            .get(SPOTIFY_WEB_HOME_URL)
            .header(reqwest::header::USER_AGENT, SPOTIFY_WEB_USER_AGENT)
            .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .send()
            .await
    })
    .await
    .map_err(|_| SerenyaError::Audio("Spotify home page request timed out".to_owned()))?
    .map_err(|e| SerenyaError::Audio(format!("Spotify home page request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(SerenyaError::Audio(format!(
            "Spotify homepage returned status: {}",
            response.status()
        )));
    }

    let mut device_id = "some_random_device_id".to_owned();
    for cookie in response.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(cookie_str) = cookie.to_str() {
            if cookie_str.starts_with("sp_t=") {
                if let Some(val) = cookie_str.split(';').next() {
                    if let Some(val_parts) = val.split('=').nth(1) {
                        device_id = val_parts.to_owned();
                        break;
                    }
                }
            }
        }
    }

    let html = response
        .text()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Failed to read Spotify homepage HTML: {e}")))?;

    let client_version = if let Some(start_pos) = html.find("<script id=\"appServerConfig\" type=\"text/plain\">") {
        let tag_len = "<script id=\"appServerConfig\" type=\"text/plain\">".len();
        let content_start = start_pos + tag_len;
        if let Some(end_pos) = html[content_start..].find("</script>") {
            let base64_str = html[content_start..content_start + end_pos].trim();
            if let Some(decoded_bytes) = base64_decode(base64_str) {
                if let Ok(decoded_str) = String::from_utf8(decoded_bytes) {
                    if let Ok(config_json) = serde_json::from_str::<serde_json::Value>(&decoded_str) {
                        config_json
                            .get("clientVersion")
                            .and_then(|v| v.as_str())
                            .map(|v| v.to_owned())
                            .unwrap_or_else(|| "1.2.93.427.ge5dd628d".to_owned())
                    } else {
                        "1.2.93.427.ge5dd628d".to_owned()
                    }
                } else {
                    "1.2.93.427.ge5dd628d".to_owned()
                }
            } else {
                "1.2.93.427.ge5dd628d".to_owned()
            }
        } else {
            "1.2.93.427.ge5dd628d".to_owned()
        }
    } else {
        "1.2.93.427.ge5dd628d".to_owned()
    };

    tracing::info!("Requesting client token from Spotify...");

    let ct_url = "https://clienttoken.spotify.com/v1/clienttoken";
    let ct_payload = serde_json::json!({
        "client_data": {
            "client_version": client_version,
            "client_id": client_id,
            "js_sdk_data": {
                "device_brand": "unknown",
                "device_model": "unknown",
                "os": "windows",
                "os_version": "NT 10.0",
                "device_id": device_id,
                "device_type": "computer"
            }
        }
    });

    let ct_response = tokio::time::timeout(timeout, async {
        http_client
            .post(ct_url)
            .json(&ct_payload)
            .header(reqwest::header::USER_AGENT, SPOTIFY_WEB_USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
    })
    .await
    .map_err(|_| SerenyaError::Audio("Spotify client token request timed out".to_owned()))?
    .map_err(|e| SerenyaError::Audio(format!("Spotify client token request failed: {e}")))?;

    if !ct_response.status().is_success() {
        return Err(SerenyaError::Audio(format!(
            "Spotify client token request failed with status: {}",
            ct_response.status()
        )));
    }

    let ct_data: serde_json::Value = ct_response
        .json()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Failed to parse Spotify client token JSON: {e}")))?;

    let client_token = ct_data
        .pointer("/granted_token/token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SerenyaError::Audio("Spotify client token response missing token".to_owned()))?
        .to_owned();

    crate::logging::register_secret_to_redact(&client_token);

    let mut cache = cache_lock.lock().await;
    *cache = Some(SpotifyClientTokenCache {
        client_token: client_token.clone(),
        client_version: client_version.clone(),
        device_id: device_id.clone(),
        expires_at: now + std::time::Duration::from_secs(3600),
    });

    Ok(SpotifyClientTokenInfo {
        client_token,
        client_version,
        device_id,
    })
}

pub(crate) async fn get_spotify_access_token(
    http_client: &reqwest::Client,
    timeout: std::time::Duration,
) -> Result<String, SerenyaError> {
    let session = get_spotify_session_info(http_client, timeout).await?;
    Ok(session.access_token)
}

async fn fetch_spotify_web_token(
    http_client: &reqwest::Client,
    timeout: std::time::Duration,
    sp_dc: &str,
) -> Result<serde_json::Value, SerenyaError> {
    let local_time = chrono::Utc::now().timestamp();
    match request_spotify_web_token_reasons(http_client, timeout, sp_dc, local_time).await {
        Ok(token) => return Ok(token),
        Err(err) => {
            tracing::warn!(
                "Spotify web token request with local clock failed; retrying with Spotify server time: {:?}",
                err
            );
        }
    }

    let server_time = fetch_spotify_server_time(http_client, timeout).await?;
    request_spotify_web_token_reasons(http_client, timeout, sp_dc, server_time).await
}

async fn request_spotify_web_token_reasons(
    http_client: &reqwest::Client,
    timeout: std::time::Duration,
    sp_dc: &str,
    timestamp_seconds: i64,
) -> Result<serde_json::Value, SerenyaError> {
    let mut last_error = None;
    for reason in ["transport", "init"] {
        match request_spotify_web_token(http_client, timeout, sp_dc, reason, timestamp_seconds)
            .await
        {
            Ok(token) if spotify_access_token_from_json(&token).is_some() => return Ok(token),
            Ok(_) => {
                last_error = Some(SerenyaError::Audio(format!(
                    "Spotify web token response missing accessToken for reason={reason}"
                )));
            }
            Err(err) => {
                last_error = Some(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        SerenyaError::Audio("Spotify web token request did not return a token".to_owned())
    }))
}

async fn request_spotify_web_token(
    http_client: &reqwest::Client,
    timeout: std::time::Duration,
    sp_dc: &str,
    reason: &str,
    timestamp_seconds: i64,
) -> Result<serde_json::Value, SerenyaError> {
    let totp = spotify_totp_at(timestamp_seconds)?;
    let params = [
        ("reason", reason.to_owned()),
        ("productType", SPOTIFY_WEB_PRODUCT_TYPE.to_owned()),
        ("totp", totp.clone()),
        ("totpServer", totp),
        ("totpVer", SPOTIFY_TOTP_VERSION.to_owned()),
    ];

    let response = tokio::time::timeout(timeout, async {
        http_client
            .get(SPOTIFY_WEB_TOKEN_URL)
            .query(&params)
            .header(reqwest::header::COOKIE, format!("sp_dc={sp_dc}"))
            .header(reqwest::header::USER_AGENT, SPOTIFY_WEB_USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::REFERER, SPOTIFY_WEB_HOME_URL)
            .header("App-Platform", "WebPlayer")
            .send()
            .await
    })
    .await
    .map_err(|_| SerenyaError::Audio("Spotify web token request timed out".to_owned()))?
    .map_err(|e| SerenyaError::Audio(format!("Spotify web token request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        tracing::warn!(reason, %status, "Spotify web token request failed");
        return Err(SerenyaError::Audio(format!(
            "Spotify web token request failed with status {status}"
        )));
    }

    response
        .json::<serde_json::Value>()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Spotify web token JSON parse failed: {e}")))
}

async fn fetch_spotify_server_time(
    http_client: &reqwest::Client,
    timeout: std::time::Duration,
) -> Result<i64, SerenyaError> {
    let response = tokio::time::timeout(timeout, async {
        http_client
            .head(SPOTIFY_WEB_HOME_URL)
            .header(reqwest::header::USER_AGENT, SPOTIFY_WEB_USER_AGENT)
            .send()
            .await
    })
    .await
    .map_err(|_| SerenyaError::Audio("Spotify server time request timed out".to_owned()))?
    .map_err(|e| SerenyaError::Audio(format!("Spotify server time request failed: {e}")))?;

    let date_header = response
        .headers()
        .get(reqwest::header::DATE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            SerenyaError::Audio("Spotify server time response missing Date".to_owned())
        })?;
    chrono::DateTime::parse_from_rfc2822(date_header)
        .map(|date| date.timestamp())
        .map_err(|e| SerenyaError::Audio(format!("Spotify server time parse failed: {e}")))
}

fn spotify_totp_at(timestamp_seconds: i64) -> Result<String, SerenyaError> {
    use hmac::{Hmac, Mac};
    use sha1::Sha1;

    let mut secret_material = String::with_capacity(SPOTIFY_TOTP_CIPHER.len() * 3);
    for (index, encoded) in SPOTIFY_TOTP_CIPHER.iter().copied().enumerate() {
        let xor_key = u8::try_from((index % 33) + 9)
            .map_err(|e| SerenyaError::Audio(format!("Spotify TOTP key conversion failed: {e}")))?;
        secret_material.push_str(&(encoded ^ xor_key).to_string());
    }

    let counter = u64::try_from(timestamp_seconds.max(0))
        .map_err(|e| SerenyaError::Audio(format!("Spotify TOTP timestamp failed: {e}")))?
        / 30;
    let mut mac = Hmac::<Sha1>::new_from_slice(secret_material.as_bytes())
        .map_err(|e| SerenyaError::Audio(format!("Spotify TOTP init failed: {e}")))?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = digest
        .last()
        .map(|byte| usize::from(byte & 0x0f))
        .ok_or_else(|| SerenyaError::Audio("Spotify TOTP digest is empty".to_owned()))?;
    let window = digest
        .get(offset..offset + 4)
        .ok_or_else(|| SerenyaError::Audio("Spotify TOTP digest window invalid".to_owned()))?;
    let binary = (u32::from(window[0] & 0x7f) << 24)
        | (u32::from(window[1]) << 16)
        | (u32::from(window[2]) << 8)
        | u32::from(window[3]);

    Ok(format!("{:06}", binary % 1_000_000))
}

fn spotify_access_token_from_json(token_json: &serde_json::Value) -> Option<&str> {
    token_json
        .get("accessToken")
        .or_else(|| token_json.get("access_token"))
        .and_then(|value| value.as_str())
}

fn spotify_cookie_hash(cookie: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cookie.hash(&mut hasher);
    hasher.finish()
}

#[async_trait]
impl MetadataProvider for SpotifyProvider {
    fn supports(&self, input: &str) -> bool {
        input.contains("open.spotify.com/track/")
    }

    async fn search(
        &self,
        query: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        let config = match crate::audio::runtime::spotify_settings() {
            Some(cfg) if cfg.enabled && cfg.enable_text_search => cfg,
            _ => return Ok(Vec::new()),
        };
        if config
            .sp_dc
            .as_ref()
            .map(|cookie| cookie.trim().is_empty())
            .unwrap_or(true)
        {
            return Ok(Vec::new());
        }

        let settings = crate::audio::runtime::settings();
        let timeout = crate::audio::runtime::duration_from_millis(settings.spotify_timeout_ms);

        let access_token = get_spotify_access_token(http_client, timeout).await?;

        let market = if config.market.trim().is_empty() {
            "US"
        } else {
            &config.market
        };
        let search_url = format!(
            "https://api.spotify.com/v1/search?type=track&limit=10&market={}&q={}",
            market,
            url_encode(query)
        );
        let response = tokio::time::timeout(timeout, async {
            http_client
                .get(&search_url)
                .bearer_auth(access_token)
                .send()
                .await
        })
        .await
        .map_err(|_| SerenyaError::Audio("Spotify search timed out".to_owned()))?
        .map_err(|e| SerenyaError::Audio(format!("Spotify search failed: {e}")))?;

        #[derive(serde::Deserialize)]
        struct SpotifySearchResponse {
            tracks: Option<SpotifyTracks>,
        }
        #[derive(serde::Deserialize)]
        struct SpotifyTracks {
            items: Option<Vec<SpotifyTrackItem>>,
        }
        #[derive(serde::Deserialize)]
        struct SpotifyTrackItem {
            name: Option<String>,
            artists: Option<Vec<SpotifyArtist>>,
            duration_ms: Option<u64>,
            external_urls: Option<SpotifyExternalUrls>,
            album: Option<SpotifyAlbum>,
            popularity: Option<u64>,
        }
        #[derive(serde::Deserialize)]
        struct SpotifyArtist {
            name: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct SpotifyExternalUrls {
            spotify: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct SpotifyAlbum {
            images: Option<Vec<SpotifyImage>>,
        }
        #[derive(serde::Deserialize)]
        struct SpotifyImage {
            url: Option<String>,
        }

        let val: SpotifySearchResponse = response
            .json()
            .await
            .map_err(|e| SerenyaError::Audio(format!("Spotify search JSON parse failed: {e}")))?;

        let items = val.tracks.and_then(|t| t.items).unwrap_or_default();

        let mut candidates = Vec::new();
        for item in items {
            let Some(title) = item.name else {
                continue;
            };
            
            let artist_str = item.artists
                .map(|artists| {
                    artists
                        .into_iter()
                        .filter_map(|artist| artist.name)
                        .collect::<Vec<String>>()
                        .join(", ")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Unknown Artist".to_owned());
                
            let duration = item.duration_ms.map(Duration::from_millis);
            let url = item.external_urls.and_then(|u| u.spotify).unwrap_or_default();
            let thumbnail = item.album
                .and_then(|a| a.images)
                .and_then(|mut images| if !images.is_empty() { images.remove(0).url } else { None });

            candidates.push(TrackCandidate {
                source: "Spotify".to_owned(),
                title,
                artist: artist_str,
                duration,
                popularity: item.popularity,
                is_official: true,
                is_topic_channel: false,
                url,
                thumbnail: thumbnail.map(std::sync::Arc::from),
            });
        }

        Ok(candidates)
    }
}

// ----------------------------------------------------
// Apple Music Metadata Provider
// ----------------------------------------------------
pub struct AppleMusicProvider;

impl AppleMusicProvider {
    pub async fn resolve_metadata(
        &self,
        url: &str,
        http_client: &reqwest::Client,
    ) -> Result<ExternalTrackMeta, SerenyaError> {
        let parsed_url = reqwest::Url::parse(url)
            .map_err(|e| SerenyaError::Audio(format!("Invalid Apple Music URL: {}", e)))?;

        let id_str = parsed_url
            .query_pairs()
            .find(|(k, _)| k == "i")
            .map(|(_, v)| v.into_owned())
            .or_else(|| {
                parsed_url
                    .path_segments()
                    .and_then(|mut s| s.next_back())
                    .map(|s| s.to_owned())
            })
            .ok_or_else(|| {
                SerenyaError::Audio("Could not extract ID from Apple Music URL".to_owned())
            })?;

        let lookup_url = format!("https://itunes.apple.com/lookup?id={}&entity=song", id_str);

        // 10s Timeout guard
        let response =
            tokio::time::timeout(Duration::from_secs(10), http_client.get(&lookup_url).send())
                .await
                .map_err(|_| SerenyaError::Audio("Timeout fetching iTunes lookup".to_owned()))?
                .map_err(|e| {
                    SerenyaError::Audio(format!("failed to fetch iTunes lookup: {}", e))
                })?;

        let val = response.json::<serde_json::Value>().await.map_err(|e| {
            SerenyaError::Audio(format!("failed to parse iTunes lookup JSON: {}", e))
        })?;

        if let Some(results) = val.get("results").and_then(|r| r.as_array()) {
            let track_item = results
                .iter()
                .find(|item| item.get("wrapperType").and_then(|w| w.as_str()) == Some("track"))
                .or_else(|| results.first());

            if let Some(item) = track_item {
                let title = item
                    .get("trackName")
                    .and_then(|t| t.as_str())
                    .or_else(|| item.get("collectionName").and_then(|c| c.as_str()))
                    .ok_or_else(|| {
                        SerenyaError::Audio("Could not find title in iTunes response".to_owned())
                    })?
                    .to_owned();

                let artist = item
                    .get("artistName")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_owned());

                let duration = item
                    .get("trackTimeMillis")
                    .and_then(|d| d.as_u64())
                    .map(Duration::from_millis);

                let thumbnail = item
                    .get("artworkUrl100")
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_owned());

                return Ok(ExternalTrackMeta {
                    title,
                    artist,
                    duration,
                    thumbnail: thumbnail.map(std::sync::Arc::from),
                });
            }
        }

        Err(SerenyaError::Audio(
            "No results found in iTunes lookup".to_owned(),
        ))
    }
}

#[async_trait]
impl MetadataProvider for AppleMusicProvider {
    fn supports(&self, input: &str) -> bool {
        input.contains("music.apple.com/")
    }

    async fn search(
        &self,
        query: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        let settings = crate::audio::runtime::settings();
        let timeout = crate::audio::runtime::duration_from_millis(settings.apple_music_timeout_ms);
        let search_url = format!(
            "https://itunes.apple.com/search?entity=song&limit=5&term={}",
            url_encode(query)
        );
        let response = tokio::time::timeout(timeout, async {
            http_client
                .get(&search_url)
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
                )
                .send()
                .await
        })
        .await
        .map_err(|_| SerenyaError::Audio("Apple Music search timed out".to_owned()))?
        .map_err(|e| SerenyaError::Audio(format!("Apple Music search failed: {e}")))?;

        let val = response.json::<serde_json::Value>().await.map_err(|e| {
            SerenyaError::Audio(format!("Apple Music search JSON parse failed: {e}"))
        })?;
        let results = val
            .get("results")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();

        let mut candidates = Vec::new();
        for item in results {
            let Some(title) = item.get("trackName").and_then(|value| value.as_str()) else {
                continue;
            };
            let artist = item
                .get("artistName")
                .and_then(|value| value.as_str())
                .unwrap_or("Unknown Artist")
                .to_owned();
            let duration = item
                .get("trackTimeMillis")
                .and_then(|value| value.as_u64())
                .map(Duration::from_millis);
            let url = item
                .get("trackViewUrl")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_owned();
            let thumbnail = item
                .get("artworkUrl100")
                .and_then(|value| value.as_str())
                .map(|value| value.to_owned());

            candidates.push(TrackCandidate {
                source: "Apple Music".to_owned(),
                title: title.to_owned(),
                artist,
                duration,
                popularity: None,
                is_official: true,
                is_topic_channel: false,
                url,
                thumbnail: thumbnail.map(std::sync::Arc::from),
            });
        }

        Ok(candidates)
    }
}

// ----------------------------------------------------
// Deezer Metadata Provider
// ----------------------------------------------------
pub struct DeezerProvider;

impl DeezerProvider {
    pub async fn resolve_metadata(
        &self,
        url: &str,
        http_client: &reqwest::Client,
    ) -> Result<ExternalTrackMeta, SerenyaError> {
        let parsed_url = reqwest::Url::parse(url)
            .map_err(|e| SerenyaError::Audio(format!("Invalid Deezer URL: {}", e)))?;

        let id_str = parsed_url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .ok_or_else(|| {
                SerenyaError::Audio("Could not extract ID from Deezer URL".to_owned())
            })?;

        // 1. Try public Deezer API
        let api_url = format!("https://api.deezer.com/track/{}", id_str);

        let response_fut = http_client
            .get(&api_url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .send();

        // 10s Timeout guard
        let response = tokio::time::timeout(Duration::from_secs(10), response_fut).await;

        if let Ok(Ok(res)) = response {
            if let Ok(val) = res.json::<serde_json::Value>().await {
                if let Some(title) = val.get("title").and_then(|t| t.as_str()) {
                    let artist = val
                        .pointer("/artist/name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_owned());
                    let duration = val
                        .get("duration")
                        .and_then(|d| d.as_u64())
                        .map(Duration::from_secs);
                    let thumbnail = val
                        .pointer("/album/cover_medium")
                        .and_then(|c| c.as_str())
                        .map(|s| s.to_owned());

                    return Ok(ExternalTrackMeta {
                        title: title.to_owned(),
                        artist,
                        duration,
                        thumbnail: thumbnail.map(std::sync::Arc::from),
                    });
                }
            }
        }

        // 2. Fallback to HTML scrape
        let scrape_fut = http_client
            .get(url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            )
            .send();

        let scrape_res = tokio::time::timeout(Duration::from_secs(10), scrape_fut)
            .await
            .map_err(|_| SerenyaError::Audio("Timeout fetching Deezer page HTML".to_owned()))?
            .map_err(|e| SerenyaError::Audio(format!("failed to fetch Deezer page: {}", e)))?;

        let html = scrape_res
            .text()
            .await
            .map_err(|e| SerenyaError::Audio(format!("failed to read Deezer page: {}", e)))?;

        let og_title = extract_meta(&html, "og:title")
            .ok_or_else(|| SerenyaError::Audio("Could not parse Deezer track title".to_owned()))?;

        let parts: Vec<&str> = og_title.split(" - ").collect();
        let (title, artist) = if parts.len() >= 2 {
            (parts[0].to_owned(), Some(parts[1].to_owned()))
        } else {
            (og_title, None)
        };

        let duration = extract_meta(&html, "music:duration")
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);

        let thumbnail = extract_meta(&html, "og:image");

        Ok(ExternalTrackMeta {
            title,
            artist,
            duration,
            thumbnail: thumbnail.map(std::sync::Arc::from),
        })
    }
}

#[async_trait]
impl MetadataProvider for DeezerProvider {
    fn supports(&self, input: &str) -> bool {
        input.contains("deezer.com/")
    }

    async fn search(
        &self,
        query: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        let api_url = format!("https://api.deezer.com/search?q={}", url_encode(query));

        let fut = http_client
            .get(&api_url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .send();

        let settings = crate::audio::runtime::settings();
        let timeout = crate::audio::runtime::duration_from_millis(settings.deezer_timeout_ms);
        let response = tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| SerenyaError::Audio("Deezer search timeout".to_owned()))?
            .map_err(|e| SerenyaError::Audio(format!("Deezer search failed: {}", e)))?;

        let val = response.json::<serde_json::Value>().await.map_err(|e| {
            SerenyaError::Audio(format!("Failed to parse Deezer search JSON: {}", e))
        })?;

        let mut candidates = Vec::new();
        if let Some(data) = val.get("data").and_then(|d| d.as_array()) {
            for item in data {
                if let Some(title) = item.get("title").and_then(|t| t.as_str()) {
                    let artist = item
                        .pointer("/artist/name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("Unknown Artist")
                        .to_owned();
                    let duration = item
                        .get("duration")
                        .and_then(|d| d.as_u64())
                        .map(Duration::from_secs);
                    let url = item
                        .get("link")
                        .and_then(|l| l.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let thumbnail = item
                        .pointer("/album/cover_medium")
                        .and_then(|c| c.as_str())
                        .map(|s| s.to_owned());
                    let rank = item.get("rank").and_then(|r| r.as_u64());

                    candidates.push(TrackCandidate {
                        source: "Deezer".to_owned(),
                        title: title.to_owned(),
                        artist,
                        duration,
                        popularity: rank,
                        is_official: true,
                        is_topic_channel: false,
                        url,
                        thumbnail: thumbnail.map(std::sync::Arc::from),
                    });
                }
            }
        }

        Ok(candidates)
    }
}

// ----------------------------------------------------
// YouTube Provider (Scraper & yt-dlp fallback)
// ----------------------------------------------------
#[derive(serde::Deserialize, Debug)]
struct YtDlpSearchResult {
    entries: Option<Vec<YtDlpEntry>>,
}

#[derive(serde::Deserialize, Debug)]
struct YtDlpEntry {
    title: Option<String>,
    id: Option<String>,
    duration: Option<f64>,
}

pub struct YouTubeProvider;

impl YouTubeProvider {
    pub async fn load(
        &self,
        url: &str,
        user_id: u64,
        http_client: &reqwest::Client,
    ) -> Result<Vec<Track>, SerenyaError> {
        let (title, thumbnail) =
            match crate::audio::source::resolve_youtube_oembed(url, http_client).await {
                Ok(res) => res,
                Err(_) => ("YouTube Track".to_owned(), None),
            };
        Ok(vec![Track {
            title,
            url: url.to_owned(),
            duration: None,
            requester_id: serenity::UserId::new(user_id),
            requester_name: "".to_owned(),
            source_type: SourceType::Url,
            resolved_url: None,
            thumbnail: thumbnail.map(std::sync::Arc::from),
            source_provider: "YouTube".to_owned(),
        }])
    }
    async fn search_scrape(
        &self,
        query: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        let encoded = url_encode(query);
        let url = format!(
            "https://www.youtube.com/results?search_query={}&sp=EgIQAQ%253D%253D",
            encoded
        );

        let scrape_fut = http_client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/115.0.0.0 Safari/537.36")
            .send();

        // 10s Timeout guard
        let response = tokio::time::timeout(Duration::from_secs(10), scrape_fut)
            .await
            .map_err(|_| SerenyaError::Audio("YouTube search scrape timeout".to_owned()))?
            .map_err(|e| SerenyaError::Audio(format!("failed to fetch YouTube page: {}", e)))?;

        let html = response
            .text()
            .await
            .map_err(|e| SerenyaError::Audio(format!("failed to read YouTube page: {}", e)))?;

        let search_str = "ytInitialData = ";
        let pos = html
            .find(search_str)
            .ok_or_else(|| SerenyaError::Audio("Missing ytInitialData".to_owned()))?;

        let start = pos + search_str.len();
        let rest = &html[start..];
        let end_pos = rest
            .find(";</script>")
            .or_else(|| rest.find("</script>"))
            .unwrap_or(rest.len());
        let json_str = rest[..end_pos].trim().trim_end_matches(';');

        #[derive(serde::Deserialize)]
        struct YtInitialData {
            contents: Option<YtContents>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct YtContents {
            two_column_search_results_renderer: Option<YtTwoColumn>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct YtTwoColumn {
            primary_contents: Option<YtPrimaryContents>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct YtPrimaryContents {
            section_list_renderer: Option<YtSectionList>,
        }
        #[derive(serde::Deserialize)]
        struct YtSectionList {
            contents: Option<Vec<YtSectionListContent>>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct YtSectionListContent {
            item_section_renderer: Option<YtItemSection>,
        }
        #[derive(serde::Deserialize)]
        struct YtItemSection {
            contents: Option<Vec<YtItemSectionContent>>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct YtItemSectionContent {
            video_renderer: Option<YtVideoRenderer>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct YtVideoRenderer {
            video_id: Option<String>,
            title: Option<YtText>,
            length_text: Option<YtSimpleText>,
            owner_text: Option<YtText>,
            thumbnail: Option<YtThumbnails>,
            view_count_text: Option<YtSimpleText>,
            owner_badges: Option<Vec<YtBadge>>,
        }
        #[derive(serde::Deserialize)]
        struct YtText {
            runs: Option<Vec<YtRun>>,
        }
        #[derive(serde::Deserialize)]
        struct YtRun {
            text: Option<String>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct YtSimpleText {
            simple_text: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct YtThumbnails {
            thumbnails: Option<Vec<YtThumbnail>>,
        }
        #[derive(serde::Deserialize)]
        struct YtThumbnail {
            url: Option<String>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct YtBadge {
            metadata_badge_renderer: Option<YtBadgeRenderer>,
        }
        #[derive(serde::Deserialize)]
        struct YtBadgeRenderer {
            style: Option<String>,
        }

        let val: YtInitialData = serde_json::from_str(json_str)
            .map_err(|e| SerenyaError::Audio(format!("Invalid JSON in ytInitialData: {}", e)))?;

        let contents = val.contents
            .and_then(|c| c.two_column_search_results_renderer)
            .and_then(|t| t.primary_contents)
            .and_then(|p| p.section_list_renderer)
            .and_then(|s| s.contents)
            .and_then(|mut c| if !c.is_empty() { Some(c.remove(0)) } else { None })
            .and_then(|i| i.item_section_renderer)
            .and_then(|i| i.contents)
            .ok_or_else(|| SerenyaError::Audio("Failed to extract itemSectionRenderer contents".to_owned()))?;

        let mut candidates = Vec::new();
        for item in contents {
            if let Some(video) = item.video_renderer {
                let video_id = video.video_id;
                let title = video.title.and_then(|t| t.runs).and_then(|mut r| if !r.is_empty() { r.remove(0).text } else { None });
                let duration_str = video.length_text.and_then(|t| t.simple_text);
                let channel = video.owner_text.and_then(|t| t.runs).and_then(|mut r| if !r.is_empty() { r.remove(0).text } else { None });
                let thumbnail = video.thumbnail.and_then(|t| t.thumbnails).and_then(|mut t| if !t.is_empty() { t.remove(0).url } else { None });
                
                let views_str = video.view_count_text.and_then(|t| t.simple_text);
                let views = views_str.and_then(|s| {
                    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
                    digits.parse::<u64>().ok()
                });

                let mut is_verified_artist = false;
                let mut is_verified = false;
                if let Some(badges) = video.owner_badges {
                    for badge in badges {
                        if let Some(style) = badge.metadata_badge_renderer.and_then(|r| r.style) {
                            if style == "BADGE_STYLE_TYPE_VERIFIED_ARTIST" {
                                is_verified_artist = true;
                            } else if style == "BADGE_STYLE_TYPE_VERIFIED" {
                                is_verified = true;
                            }
                        }
                    }
                }

                if let (Some(id), Some(t)) = (video_id, title) {
                    let duration = duration_str.and_then(|s| parse_simple_duration(&s));
                    let channel_name = channel.unwrap_or_else(|| "Unknown Artist".to_owned());
                    let is_topic = channel_name.ends_with(" - Topic") || channel_name.ends_with("- Topic");

                    candidates.push(TrackCandidate {
                        source: "YouTube".to_owned(),
                        title: t,
                        artist: channel_name,
                        duration,
                        popularity: views,
                        is_official: is_verified_artist || is_verified,
                        is_topic_channel: is_topic,
                        url: format!("https://www.youtube.com/watch?v={}", id),
                        thumbnail: thumbnail.map(std::sync::Arc::from),
                    });

                    if candidates.len() >= 7 {
                        break;
                    }
                }
            }
        }

        Ok(candidates)
    }

    pub(crate) async fn search_fallback_ytdl(
        &self,
        query: &str,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        let settings = crate::audio::runtime::settings();
        let output = crate::audio::runtime::run_ytdlp(
            "ytsearch fallback",
            vec![
                "--flat-playlist".to_owned(),
                "--dump-single-json".to_owned(),
                format!("ytsearch5:{query}"),
            ],
            crate::audio::runtime::duration_from_millis(settings.ytsearch_timeout_ms),
            true,
            Some(crate::audio::runtime::negative_cache_key("ytsearch", query)),
        )
        .await?;

        let search_result: YtDlpSearchResult = serde_json::from_slice(&output.stdout)
            .map_err(|e| SerenyaError::Audio(format!("Failed to parse yt-dlp results: {}", e)))?;

        let entries = search_result.entries.unwrap_or_default();
        let mut candidates = Vec::new();
        for entry in entries {
            if let Some(id) = entry.id {
                candidates.push(TrackCandidate {
                    source: "yt-dlp".to_owned(),
                    title: entry.title.unwrap_or_else(|| "Unknown Title".to_string()),
                    artist: "Unknown Artist".to_owned(),
                    duration: entry.duration.map(|d| Duration::from_secs(d as u64)),
                    popularity: None,
                    is_official: false,
                    is_topic_channel: false,
                    url: format!("https://www.youtube.com/watch?v={}", id),
                    thumbnail: None,
                });
            }
        }

        Ok(candidates)
    }
}

#[async_trait]
impl MetadataProvider for YouTubeProvider {
    fn supports(&self, input: &str) -> bool {
        input.contains("youtube.com/") || input.contains("youtu.be/")
    }

    async fn search(
        &self,
        query: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        self.search_scrape(query, http_client).await
    }
}

// ----------------------------------------------------
// YouTube Music Provider (Prefers official/topic tracks)
// ----------------------------------------------------
pub struct YouTubeMusicProvider;

#[async_trait]
impl MetadataProvider for YouTubeMusicProvider {
    fn supports(&self, _input: &str) -> bool {
        false // URL loading falls back to normal YouTubeProvider
    }

    async fn search(
        &self,
        query: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        // Search YouTube, but rewrite sources to "YouTube Music" and filter/prefer official audios
        let yt = YouTubeProvider;
        let candidates = yt.search_scrape(query, http_client).await?;

        let mut ytm_candidates = Vec::new();
        for mut c in candidates {
            c.source = "YouTube Music".to_owned();

            // Prefer Official Audio / Topic / VEVO indicators
            let channel_lower = c.artist.to_lowercase();
            let title_lower = c.title.to_lowercase();
            let is_vevo = channel_lower.contains("vevo");
            let is_official_audio =
                title_lower.contains("official audio") || title_lower.contains("official lyric");

            if c.is_official || c.is_topic_channel || is_vevo || is_official_audio {
                c.is_official = true;
            }
            ytm_candidates.push(c);
        }

        Ok(ytm_candidates)
    }
}

// ----------------------------------------------------
// SoundCloud Provider (using api-v2.soundcloud.com)
// ----------------------------------------------------
use std::sync::LazyLock;
use tokio::sync::{RwLock, Mutex};
use std::time::Instant;

#[derive(serde::Deserialize, Debug, Clone)]
pub struct SoundCloudTrackUser {
    pub username: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct SoundCloudTranscodingFormat {
    pub protocol: String,
    pub mime_type: Option<String>,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct SoundCloudTranscoding {
    pub url: String,
    pub format: SoundCloudTranscodingFormat,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct SoundCloudMedia {
    pub transcodings: Vec<SoundCloudTranscoding>,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct SoundCloudTrackMetadata {
    pub id: i64,
    pub title: Option<String>,
    pub duration: Option<u64>, // milliseconds
    pub permalink_url: Option<String>,
    pub user: Option<SoundCloudTrackUser>,
    pub media: Option<SoundCloudMedia>,
    pub artwork_url: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
struct SoundCloudSearchResponse {
    collection: Vec<SoundCloudTrackMetadata>,
}

struct ClientIdState {
    client_id: String,
    obtained_at: Instant,
}

static SOUNDCLOUD_STATE: LazyLock<RwLock<Option<ClientIdState>>> = LazyLock::new(|| RwLock::new(None));
static SOUNDCLOUD_REFRESH_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub async fn get_or_fetch_client_id(http_client: &reqwest::Client) -> Result<String, SerenyaError> {
    // 1. Read lock check
    {
        let read_guard = SOUNDCLOUD_STATE.read().await;
        if let Some(ref state) = *read_guard {
            if state.obtained_at.elapsed() < Duration::from_secs(3600) {
                return Ok(state.client_id.clone());
            }
        }
    }

    // 2. Write lock check (Double Checked Locking under Mutex)
    let _refresh_permit = SOUNDCLOUD_REFRESH_MUTEX.lock().await;
    {
        let mut write_guard = SOUNDCLOUD_STATE.write().await;
        if let Some(ref state) = *write_guard {
            if state.obtained_at.elapsed() < Duration::from_secs(3600) {
                return Ok(state.client_id.clone());
            }
        }

        tracing::info!("SoundCloud client_id is expired or None, fetching fresh one...");
        let url = "https://soundcloud.com";
        let res = http_client.get(url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36")
            .send()
            .await
            .map_err(|e| SerenyaError::Audio(format!("Failed to fetch SoundCloud homepage for client_id: {}", e)))?;

        let html = res.text().await
            .map_err(|e| SerenyaError::Audio(format!("Failed to read SoundCloud homepage body: {}", e)))?;

        let client_id = if let Some(cap) = regex_extract_client_id(&html) {
            cap
        } else {
            return Err(SerenyaError::Audio("Failed to extract client_id from SoundCloud hydration data".to_owned()));
        };

        tracing::info!("Successfully extracted new SoundCloud client_id: {}", client_id);
        *write_guard = Some(ClientIdState {
            client_id: client_id.clone(),
            obtained_at: Instant::now(),
        });
        Ok(client_id)
    }
}

fn regex_extract_client_id(html: &str) -> Option<String> {
    let start_pat = "window.__sc_hydration";
    if let Some(pos) = html.find(start_pat) {
        let rest = &html[pos..];
        if let Some(end_pos) = rest.find(";</script>") {
            let json_str = &rest[..end_pos];
            if let Some(eq_pos) = json_str.find('=') {
                let json_data_str = json_str[eq_pos + 1..].trim();
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_data_str) {
                    if let Some(arr) = parsed.as_array() {
                        for val in arr {
                            if val.get("hydratable").and_then(|h| h.as_str()) == Some("apiClient") {
                                if let Some(client_id) = val.get("data").and_then(|d| d.get("id")).and_then(|id| id.as_str()) {
                                    return Some(client_id.to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

pub async fn send_soundcloud_request(
    http_client: &reqwest::Client,
    url_factory: impl Fn(&str) -> String,
) -> Result<reqwest::Response, SerenyaError> {
    let mut client_id = get_or_fetch_client_id(http_client).await?;
    let url = url_factory(&client_id);

    let res = http_client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36")
        .send()
        .await
        .map_err(|e| SerenyaError::Audio(format!("SoundCloud request failed: {}", e)))?;

    if res.status() == reqwest::StatusCode::UNAUTHORIZED || res.status() == reqwest::StatusCode::FORBIDDEN {
        tracing::warn!("SoundCloud request unauthorized (401/403). Clearing client_id and retrying once...");
        {
            let mut write_guard = SOUNDCLOUD_STATE.write().await;
            *write_guard = None;
        }
        
        client_id = get_or_fetch_client_id(http_client).await?;
        let retry_url = url_factory(&client_id);
        
        let retry_res = http_client.get(&retry_url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36")
            .send()
            .await
            .map_err(|e| SerenyaError::Audio(format!("SoundCloud retry request failed: {}", e)))?;
            
        if retry_res.status() == reqwest::StatusCode::UNAUTHORIZED || retry_res.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(SerenyaError::Audio("SoundCloud request unauthorized even after client_id refresh".to_owned()));
        }
        
        Ok(retry_res)
    } else {
        Ok(res)
    }
}

pub struct SoundCloudProvider;

#[async_trait]
impl MetadataProvider for SoundCloudProvider {
    fn supports(&self, input: &str) -> bool {
        input.contains("soundcloud.com/")
    }

    async fn search(
        &self,
        query: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        let query_enc = url_encode(query);
        let res = send_soundcloud_request(http_client, |cid| {
            format!("https://api-v2.soundcloud.com/search/tracks?q={}&client_id={}&limit=5", query_enc, cid)
        }).await?;

        let search_result: SoundCloudSearchResponse = res.json().await.map_err(|e| {
            SerenyaError::Audio(format!("Failed to parse SoundCloud search response: {}", e))
        })?;

        let mut candidates = Vec::new();
        for entry in search_result.collection {
            let artist = entry.user.as_ref().map(|u| u.username.clone()).unwrap_or_else(|| "SoundCloud Artist".to_owned());
            candidates.push(TrackCandidate {
                source: "SoundCloud".to_owned(),
                title: entry.title.unwrap_or_else(|| "Unknown Title".to_string()),
                artist,
                duration: entry.duration.map(|d| Duration::from_millis(d)),
                popularity: None,
                is_official: false,
                is_topic_channel: false,
                url: entry.permalink_url.unwrap_or_else(|| format!("https://api.soundcloud.com/tracks/{}", entry.id)),
                thumbnail: entry.artwork_url.map(std::sync::Arc::from),
            });
        }

        Ok(candidates)
    }
}

impl SoundCloudProvider {
    pub async fn load(
        &self,
        url: &str,
        user_id: u64,
        http_client: &reqwest::Client,
    ) -> Result<Vec<Track>, SerenyaError> {
        let mut final_url = url.to_owned();
        if url.contains("on.soundcloud.com/") {
            if let Ok(res) = http_client.head(url)
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36")
                .send()
                .await 
            {
                final_url = res.url().as_str().to_owned();
                tracing::info!("Redirected shortened SoundCloud URL: {} -> {}", url, final_url);
            }
        }

        let url_enc = url_encode(&final_url);
        let res = send_soundcloud_request(http_client, |cid| {
            format!("https://api-v2.soundcloud.com/resolve?url={}&client_id={}", url_enc, cid)
        }).await?;

        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(SerenyaError::Audio("SoundCloud URL not found".to_owned()));
        }

        let resolved: serde_json::Value = res.json().await.map_err(|e| {
            SerenyaError::Audio(format!("Failed to parse SoundCloud resolve response: {}", e))
        })?;

        let kind = resolved.get("kind").and_then(|k| k.as_str()).unwrap_or("track");

        if kind == "playlist" || kind == "system-playlist" {
            let tracks_arr = resolved.get("tracks").and_then(|t| t.as_array()).ok_or_else(|| {
                SerenyaError::Audio("SoundCloud playlist is missing 'tracks' field".to_owned())
            })?;

            let mut track_ids = Vec::new();
            for t in tracks_arr {
                if let Some(id) = t.get("id").and_then(|id| id.as_i64()) {
                    track_ids.push(id);
                }
            }

            if track_ids.is_empty() {
                return Ok(Vec::new());
            }

            let mut tracks = Vec::new();
            for chunk in track_ids.chunks(50) {
                let ids_str: Vec<String> = chunk.iter().map(|id| id.to_string()).collect();
                let query_ids = ids_str.join(",");
                
                let bulk_res = send_soundcloud_request(http_client, |cid| {
                    format!("https://api-v2.soundcloud.com/tracks?ids={}&client_id={}", query_ids, cid)
                }).await?;
                
                let bulk_tracks: Vec<SoundCloudTrackMetadata> = bulk_res.json().await.map_err(|e| {
                    SerenyaError::Audio(format!("Failed to parse SoundCloud bulk tracks response: {}", e))
                })?;
                
                for meta in bulk_tracks {
                    let duration = meta.duration.map(|d| Duration::from_millis(d));
                    let track_url = meta.permalink_url.clone().unwrap_or_else(|| format!("https://api.soundcloud.com/tracks/{}", meta.id));
                    
                    tracks.push(Track {
                        title: meta.title.unwrap_or_else(|| "SoundCloud Track".to_owned()),
                        url: track_url,
                        duration,
                        requester_id: serenity::UserId::new(user_id),
                        requester_name: "".to_owned(),
                        source_type: SourceType::Playlist,
                        resolved_url: None,
                        thumbnail: meta.artwork_url.map(std::sync::Arc::from),
                        source_provider: "SoundCloud".to_owned(),
                    });
                }
            }
            Ok(tracks)
        } else {
            let meta: SoundCloudTrackMetadata = serde_json::from_value(resolved).map_err(|e| {
                SerenyaError::Audio(format!("Failed to parse SoundCloud track metadata: {}", e))
            })?;

            let duration = meta.duration.map(|d| Duration::from_millis(d));

            Ok(vec![Track {
                title: meta.title.unwrap_or_else(|| "SoundCloud Track".to_owned()),
                url: final_url.to_owned(),
                duration,
                requester_id: serenity::UserId::new(user_id),
                requester_name: "".to_owned(),
                source_type: SourceType::Url,
                resolved_url: None,
                thumbnail: meta.artwork_url.map(std::sync::Arc::from),
                source_provider: "SoundCloud".to_owned(),
            }])
        }
    }
}

// ----------------------------------------------------
// Direct URL Provider (Standard Direct Link resolutions)
// ----------------------------------------------------
pub struct DirectUrlProvider;

impl DirectUrlProvider {
    pub fn supports(&self, input: &str) -> bool {
        input.starts_with("http://") || input.starts_with("https://")
    }

    pub async fn load(&self, input: &str, user_id: u64) -> Result<Vec<Track>, SerenyaError> {
        Ok(vec![Track {
            title: input.to_owned(),
            url: input.to_owned(),
            duration: None,
            requester_id: serenity::UserId::new(user_id),
            requester_name: "".to_owned(),
            source_type: SourceType::Url,
            resolved_url: None,
            thumbnail: None,
            source_provider: "Direct Link".to_owned(),
        }])
    }
}
