#![allow(clippy::collapsible_if)]
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::json;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use rusty_ytdl::PlayerResponse;

static SESSION_CACHE: Mutex<Option<(String, u64, Instant)>> = Mutex::new(None);

#[derive(Debug, Clone)]
pub struct ResolveContext {
    pub po_token: Option<String>,
    pub visitor_data: Option<String>,
    pub cookies: Option<String>,
    pub user_agent_override: Option<String>,
    pub language: Option<String>,
    pub region: Option<String>,
    pub timeout: Duration,
    pub trace_id: Option<String>,
}

impl Default for ResolveContext {
    fn default() -> Self {
        Self {
            po_token: None,
            visitor_data: None,
            cookies: None,
            user_agent_override: None,
            language: Some("en".to_string()),
            region: Some("US".to_string()),
            timeout: Duration::from_secs(5),
            trace_id: None,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ResolveError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Serialization/Deserialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("API error response: status={status:?}, reason={reason:?}")]
    ApiError {
        status: Option<String>,
        reason: Option<String>,
    },

    #[error("Video not playable: {0}")]
    NotPlayable(String),

    #[error("Request timeout after {0:?}")]
    Timeout(Duration),

    #[error("Unknown error: {0}")]
    Unknown(String),
}

#[async_trait]
pub trait InnerTubeClient: Send + Sync {
    fn name(&self) -> &'static str;
    fn client_name(&self) -> &'static str;
    fn client_version(&self) -> String;
    fn user_agent(&self) -> String;

    async fn player(
        &self,
        video_id: &str,
        context: &ResolveContext,
    ) -> Result<PlayerResponse, ResolveError>;
}

// A base implementation that builds standard Innertube requests
pub struct BaseInnerTubeClient {
    name: &'static str,
    client_name: &'static str,
    client_version: String,
    user_agent: String,
    client_id_header: String, // X-Youtube-Client-Name integer
    payload_client_override: Option<serde_json::Value>,
    payload_context_override: Option<serde_json::Value>,
}

impl BaseInnerTubeClient {
    pub fn new(
        name: &'static str,
        client_name: &'static str,
        client_version: String,
        user_agent: String,
        client_id_header: String,
        payload_client_override: Option<serde_json::Value>,
        payload_context_override: Option<serde_json::Value>,
    ) -> Self {
        Self {
            name,
            client_name,
            client_version,
            user_agent,
            client_id_header,
            payload_client_override,
            payload_context_override,
        }
    }
}

pub async fn get_or_fetch_session(
    http_client: &reqwest::Client,
) -> Result<(String, u64), ResolveError> {
    {
        let cache = SESSION_CACHE.lock().unwrap();
        if let Some((ref visitor_data, sts, fetched_at)) = *cache {
            if fetched_at.elapsed() < Duration::from_secs(6 * 3600) {
                return Ok((visitor_data.clone(), sts));
            }
        }
    }

    // Cold start or expired cache - fetch watch page to extract visitor_data and sts
    let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ&hl=en";
    let res = http_client.get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .send()
        .await?
        .text()
        .await?;

    println!("get_or_fetch_session: HTML length = {}", res.len());
    let visitor_data = match rusty_ytdl::get_visitor_data(&res) {
        Ok(v) => {
            println!("Parsed visitor_data: {}", v);
            v
        }
        Err(e) => {
            println!("Failed to parse visitor_data: {:?}", e);
            "CgtyckVza05NMXhtOCiV8-m_BjIKCgJWThIEGgAgSw==".to_string()
        }
    };

    let sts = match rusty_ytdl::get_ytconfig(&res) {
        Ok(ytcfg) => {
            let s = ytcfg.sts.unwrap_or(19950);
            println!("Parsed sts: {}", s);
            s
        }
        Err(e) => {
            println!("Failed to parse ytconfig sts: {:?}", e);
            19950
        }
    };

    {
        let mut cache = SESSION_CACHE.lock().unwrap();
        *cache = Some((visitor_data.clone(), sts, Instant::now()));
    }

    Ok((visitor_data, sts))
}

#[async_trait]
impl InnerTubeClient for BaseInnerTubeClient {
    fn name(&self) -> &'static str {
        self.name
    }

    fn client_name(&self) -> &'static str {
        self.client_name
    }

    fn client_version(&self) -> String {
        self.client_version.clone()
    }

    fn user_agent(&self) -> String {
        self.user_agent.clone()
    }

    async fn player(
        &self,
        video_id: &str,
        context: &ResolveContext,
    ) -> Result<PlayerResponse, ResolveError> {
        let http_client = reqwest::Client::builder()
            .timeout(context.timeout)
            .build()?;

        let (visitor_data, sts) = if context.visitor_data.is_none() {
            get_or_fetch_session(&http_client).await?
        } else {
            (context.visitor_data.clone().unwrap(), 19950)
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("content-type").unwrap(),
            HeaderValue::from_str("application/json").unwrap(),
        );
        let ua = context
            .user_agent_override
            .clone()
            .unwrap_or_else(|| self.user_agent());
        headers.insert(
            HeaderName::from_str("User-Agent").unwrap(),
            HeaderValue::from_str(&ua).unwrap(),
        );
        headers.insert(
            HeaderName::from_str("X-Youtube-Client-Name").unwrap(),
            HeaderValue::from_str(&self.client_id_header).unwrap(),
        );
        headers.insert(
            HeaderName::from_str("X-Youtube-Client-Version").unwrap(),
            HeaderValue::from_str(&self.client_version()).unwrap(),
        );
        headers.insert(
            HeaderName::from_str("Origin").unwrap(),
            HeaderValue::from_str("https://www.youtube.com").unwrap(),
        );
        headers.insert(
            HeaderName::from_str("Referer").unwrap(),
            HeaderValue::from_str("https://www.youtube.com/").unwrap(),
        );
        headers.insert(
            HeaderName::from_str("X-Goog-Visitor-Id").unwrap(),
            HeaderValue::from_str(&visitor_data).unwrap(),
        );

        if let Some(ref cookies) = context.cookies {
            headers.insert(
                HeaderName::from_str("Cookie").unwrap(),
                HeaderValue::from_str(cookies).unwrap(),
            );
        }

        let hl = context.language.clone().unwrap_or_else(|| "en".to_string());

        let mut client_obj = json!({
            "clientName": self.client_name,
            "clientVersion": self.client_version(),
            "hl": hl,
            "userAgent": context.user_agent_override.clone().unwrap_or_else(|| self.user_agent()),
        });

        // Merge any client payload overrides (like osName, osVersion, deviceModel, etc.)
        if let Some(ref extra) = self.payload_client_override {
            if let Some(obj) = client_obj.as_object_mut() {
                if let Some(extra_obj) = extra.as_object() {
                    for (k, v) in extra_obj {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let mut context_obj = json!({
            "client": client_obj
        });

        // Merge any context level overrides
        if let Some(ref extra) = self.payload_context_override {
            if let Some(obj) = context_obj.as_object_mut() {
                if let Some(extra_obj) = extra.as_object() {
                    for (k, v) in extra_obj {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let mut payload = json!({
            "context": context_obj,
            "videoId": video_id,
            "playbackContext": {
                "contentPlaybackContext": {
                    "signatureTimestamp": sts,
                    "html5Preference": "HTML5_PREF_WANTS"
                }
            }
        });

        // Add serviceIntegrityDimensions if po_token is supplied
        if let Some(ref po_token) = context.po_token {
            if let Some(payload_obj) = payload.as_object_mut() {
                payload_obj.insert(
                    "serviceIntegrityDimensions".to_string(),
                    json!({
                        "poToken": po_token
                    }),
                );
            }
        }

        // Innertube Player API Endpoint
        let url = "https://www.youtube.com/youtubei/v1/player?key=AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";

        let response_text = http_client
            .post(url)
            .headers(headers)
            .json(&payload)
            .send()
            .await?
            .text()
            .await?;

        // Check if the response contains an error block
        let raw_val: serde_json::Value = serde_json::from_str(&response_text)?;
        if let Some(error_block) = raw_val.get("error") {
            let status = error_block
                .get("status")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            let reason = error_block
                .get("message")
                .and_then(|m| m.as_str())
                .map(|m| m.to_string());
            return Err(ResolveError::ApiError { status, reason });
        }

        let player_res: PlayerResponse = serde_json::from_value(raw_val)?;

        // Simple playability status check
        if let Some(ref playability) = player_res.playability_status {
            if let Some(ref status) = playability.status {
                if status != "OK" {
                    return Err(ResolveError::ApiError {
                        status: Some(status.clone()),
                        reason: playability.reason.clone(),
                    });
                }
            }
        }

        Ok(player_res)
    }
}

// ANDROID Client Factory
pub fn create_android_client(version: Option<String>) -> BaseInnerTubeClient {
    let ver = version.unwrap_or_else(|| "20.10.38".to_string());
    BaseInnerTubeClient::new(
        "ANDROID",
        "ANDROID",
        ver.clone(),
        format!(
            "com.google.android.youtube/{} (Linux; U; Android 11) gzip",
            ver
        ),
        "3".to_string(),
        Some(json!({
            "osName": "Android",
            "osVersion": "11",
            "userAgent": format!("com.google.android.youtube/{} (Linux; U; Android 11) gzip", ver)
        })),
        None,
    )
}

// TVHTML5 Client Factory
pub fn create_tvhtml5_client(version: Option<String>) -> BaseInnerTubeClient {
    let ver = version.unwrap_or_else(|| "7.20230522.05.00".to_string());
    BaseInnerTubeClient::new(
        "TVHTML5",
        "TVHTML5",
        ver,
        "Mozilla/5.0 (Chromecast; Google TV) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/90.0.4430.225 Safari/537.36".to_string(),
        "7".to_string(),
        None,
        None,
    )
}

// IOS Client Factory
pub fn create_ios_client(version: Option<String>) -> BaseInnerTubeClient {
    let ver = version.unwrap_or_else(|| "21.02.3".to_string());
    BaseInnerTubeClient::new(
        "IOS",
        "IOS",
        ver.clone(),
        format!(
            "com.google.ios.youtube/{} (iPhone16,2; U; CPU iOS 18_1_0 like Mac OS X;)",
            ver
        ),
        "5".to_string(),
        Some(json!({
            "deviceMake": "Apple",
            "deviceModel": "iPhone16,2",
            "osName": "iPhone",
            "osVersion": "18.1.0",
            "userAgent": format!("com.google.ios.youtube/{} (iPhone16,2; U; CPU iOS 18_1_0 like Mac OS X;)", ver)
        })),
        None,
    )
}

/// Helper to extract the best audio stream URL for a given video ID.
/// Tries the ANDROID client first, and falls back to the IOS client if needed.
/// Prioritizes itags: 251 (Opus), 140 (AAC), 250 (Opus), 249 (Opus), 139 (AAC), then any other audio format.
pub async fn resolve_best_audio_stream(
    video_id: &str,
    context: &ResolveContext,
) -> Result<String, ResolveError> {
    // 1. Try ANDROID client
    let android_client = create_android_client(None);
    match android_client.player(video_id, context).await {
        Ok(player_res) => {
            if let Some(url) = extract_best_audio_url(&player_res) {
                return Ok(url);
            }
        }
        Err(_e) => {
            // Fall through to IOS client
        }
    }

    // 2. Try IOS client
    let ios_client = create_ios_client(None);
    let player_res = ios_client.player(video_id, context).await?;
    if let Some(url) = extract_best_audio_url(&player_res) {
        return Ok(url);
    }

    Err(ResolveError::NotPlayable(
        "No suitable audio streams found in response".to_string(),
    ))
}

fn extract_best_audio_url(player_res: &PlayerResponse) -> Option<String> {
    let sd = player_res.streaming_data.as_ref()?;
    let adaptive = sd.adaptive_formats.as_ref()?;

    // Prioritize formats by itag
    let itag_priority = |itag: u64| -> i32 {
        match itag {
            251 => 10, // Opus 160kbps
            140 => 9,  // AAC 128kbps
            250 => 8,  // Opus 70kbps
            249 => 7,  // Opus 50kbps
            139 => 6,  // AAC 48kbps
            _ => 1,    // Any other audio
        }
    };

    let mut candidate: Option<(String, i32)> = None;

    for format in adaptive {
        let is_audio = format
            .mime_type
            .as_ref()
            .map(|m| m.mime.type_() == mime::AUDIO)
            .unwrap_or(false);
        if !is_audio {
            continue;
        }

        if let Some(ref url) = format.url {
            if url.is_empty() {
                continue;
            }
            let itag = format.itag.unwrap_or(0);
            let priority = itag_priority(itag);

            if let Some((_, best_priority)) = candidate {
                if priority > best_priority {
                    candidate = Some((url.clone(), priority));
                }
            } else {
                candidate = Some((url.clone(), priority));
            }
        }
    }

    candidate.map(|(url, _)| url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_android_resolve() {
        let client = create_android_client(None);
        let ctx = ResolveContext::default();
        let video_id = "dQw4w9WgXcQ";
        let res = client.player(video_id, &ctx).await;
        match &res {
            Ok(player_res) => {
                println!(
                    "Android resolve succeeded! Playability status: {:?}",
                    player_res.playability_status
                );
                if let Some(ref sd) = player_res.streaming_data {
                    let formats_len = sd.formats.as_ref().map(|f| f.len()).unwrap_or(0);
                    let adaptive_len = sd.adaptive_formats.as_ref().map(|f| f.len()).unwrap_or(0);
                    println!(
                        "Formats count: {}, Adaptive formats count: {}",
                        formats_len, adaptive_len
                    );

                    if let Some(ref adaptive) = sd.adaptive_formats {
                        let mut audio_count = 0;
                        for f in adaptive {
                            let is_audio = f
                                .mime_type
                                .as_ref()
                                .map(|m| m.mime.type_() == mime::AUDIO)
                                .unwrap_or(false);
                            if is_audio {
                                audio_count += 1;
                                println!(
                                    "  Audio Format #{} - itag: {:?}, mime: {:?}, bitrate: {:?}, audioQuality: {:?}, sampleRate: {:?}, url starts with: {}",
                                    audio_count,
                                    f.itag,
                                    f.mime_type,
                                    f.bitrate,
                                    f.audio_quality,
                                    f.audio_sample_rate,
                                    f.url
                                        .as_ref()
                                        .map(|u| &u[..std::cmp::min(u.len(), 60)])
                                        .unwrap_or("None")
                                );
                            }
                        }
                        println!("Total Audio formats found: {}", audio_count);
                    }
                }
            }
            Err(e) => {
                println!("Android resolve failed: {:?}", e);
            }
        }
        assert!(res.is_ok());
        let player_res = res.unwrap();
        assert!(player_res.streaming_data.is_some());
    }

    #[tokio::test]
    async fn test_ios_resolve() {
        let client = create_ios_client(None);
        let ctx = ResolveContext::default();
        let video_id = "dQw4w9WgXcQ";
        let res = client.player(video_id, &ctx).await;
        match &res {
            Ok(player_res) => {
                println!(
                    "IOS resolve succeeded! Playability status: {:?}",
                    player_res.playability_status
                );
                if let Some(ref sd) = player_res.streaming_data {
                    let formats_len = sd.formats.as_ref().map(|f| f.len()).unwrap_or(0);
                    let adaptive_len = sd.adaptive_formats.as_ref().map(|f| f.len()).unwrap_or(0);
                    println!(
                        "Formats count: {}, Adaptive formats count: {}",
                        formats_len, adaptive_len
                    );

                    if let Some(ref adaptive) = sd.adaptive_formats {
                        let mut audio_count = 0;
                        for f in adaptive {
                            let is_audio = f
                                .mime_type
                                .as_ref()
                                .map(|m| m.mime.type_() == mime::AUDIO)
                                .unwrap_or(false);
                            if is_audio {
                                audio_count += 1;
                                println!(
                                    "  IOS Audio Format #{} - itag: {:?}, mime: {:?}, bitrate: {:?}, audioQuality: {:?}, sampleRate: {:?}",
                                    audio_count,
                                    f.itag,
                                    f.mime_type,
                                    f.bitrate,
                                    f.audio_quality,
                                    f.audio_sample_rate
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("IOS resolve failed: {:?}", e);
            }
        }
        assert!(res.is_ok());
        let player_res = res.unwrap();
        assert!(player_res.streaming_data.is_some());
    }

    #[tokio::test]
    async fn test_resolve_best_audio_stream() {
        let ctx = ResolveContext::default();
        let video_id = "dQw4w9WgXcQ";
        let res = resolve_best_audio_stream(video_id, &ctx).await;
        match &res {
            Ok(url) => {
                println!(
                    "resolve_best_audio_stream succeeded! url starts with: {}",
                    &url[..std::cmp::min(url.len(), 60)]
                );
            }
            Err(e) => {
                println!("resolve_best_audio_stream failed: {:?}", e);
            }
        }
        assert!(res.is_ok());
        let url = res.unwrap();
        assert!(url.contains("googlevideo.com"));
    }
}
