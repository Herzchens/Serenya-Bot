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
    pub thumbnail: Option<String>,
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
            thumbnail: og_image,
        })
    }
}

struct SpotifyToken {
    access_token: String,
    expires_at: std::time::Instant,
}

static SPOTIFY_TOKEN_CACHE: std::sync::OnceLock<tokio::sync::Mutex<Option<SpotifyToken>>> =
    std::sync::OnceLock::new();

pub(crate) async fn get_spotify_access_token(
    http_client: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    timeout: std::time::Duration,
) -> Result<String, SerenyaError> {
    let cache_lock = SPOTIFY_TOKEN_CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
    let mut cache = cache_lock.lock().await;

    let now = std::time::Instant::now();
    if let Some(ref token) = *cache {
        if token.expires_at > now + std::time::Duration::from_secs(60) {
            return Ok(token.access_token.clone());
        }
    }

    let token_response = tokio::time::timeout(timeout, async {
        http_client
            .post("https://accounts.spotify.com/api/token")
            .basic_auth(client_id, Some(client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
    })
    .await
    .map_err(|_| SerenyaError::Audio("Spotify token request timed out".to_owned()))?
    .map_err(|e| SerenyaError::Audio(format!("Spotify token request failed: {e}")))?;

    let status = token_response.status();
    if !status.is_success() {
        let err_body = token_response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read response body".to_string());
        tracing::error!(
            "Spotify token request failed with status: {}, body: {}",
            status,
            err_body
        );
        return Err(SerenyaError::Audio(format!(
            "Spotify token request failed with status: {}, body: {}",
            status, err_body
        )));
    }

    let token_json = token_response
        .json::<serde_json::Value>()
        .await
        .map_err(|e| SerenyaError::Audio(format!("Spotify token JSON parse failed: {e}")))?;

    let access_token = token_json
        .get("access_token")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            SerenyaError::Audio("Spotify token response missing access_token".to_owned())
        })?
        .to_owned();

    let expires_in = token_json
        .get("expires_in")
        .and_then(|value| value.as_u64())
        .unwrap_or(3600);

    crate::logging::register_secret_to_redact(&access_token);

    *cache = Some(SpotifyToken {
        access_token: access_token.clone(),
        expires_at: now + std::time::Duration::from_secs(expires_in),
    });

    Ok(access_token)
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
        let client_id = match config.client_id {
            Some(ref val) if !val.trim().is_empty() => val.clone(),
            _ => return Ok(Vec::new()),
        };
        let client_secret = match config.client_secret {
            Some(ref val) if !val.trim().is_empty() => val.clone(),
            _ => return Ok(Vec::new()),
        };

        let settings = crate::audio::runtime::settings();
        let timeout = crate::audio::runtime::duration_from_millis(settings.spotify_timeout_ms);

        let access_token =
            get_spotify_access_token(http_client, &client_id, &client_secret, timeout).await?;

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

        let val = response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| SerenyaError::Audio(format!("Spotify search JSON parse failed: {e}")))?;
        let items = val
            .pointer("/tracks/items")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();

        let mut candidates = Vec::new();
        for item in items {
            let Some(title) = item.get("name").and_then(|value| value.as_str()) else {
                continue;
            };
            let artists = item
                .get("artists")
                .and_then(|value| value.as_array())
                .map(|artists| {
                    artists
                        .iter()
                        .filter_map(|artist| artist.get("name").and_then(|value| value.as_str()))
                        .collect::<Vec<&str>>()
                        .join(", ")
                })
                .filter(|artists| !artists.is_empty())
                .unwrap_or_else(|| "Unknown Artist".to_owned());
            let duration = item
                .get("duration_ms")
                .and_then(|value| value.as_u64())
                .map(Duration::from_millis);
            let url = item
                .pointer("/external_urls/spotify")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_owned();
            let thumbnail = item
                .pointer("/album/images/0/url")
                .and_then(|value| value.as_str())
                .map(|value| value.to_owned());
            let popularity = item.get("popularity").and_then(|value| value.as_u64());

            candidates.push(TrackCandidate {
                source: "Spotify".to_owned(),
                title: title.to_owned(),
                artist: artists,
                duration,
                popularity,
                is_official: true,
                is_topic_channel: false,
                url,
                thumbnail,
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
                    thumbnail,
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
                thumbnail,
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
                        thumbnail,
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
            thumbnail,
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
                        thumbnail,
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
            thumbnail,
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

        let val: serde_json::Value = serde_json::from_str(json_str)
            .map_err(|e| SerenyaError::Audio(format!("Invalid JSON in ytInitialData: {}", e)))?;

        let contents = val.pointer("/contents/twoColumnSearchResultsRenderer/primaryContents/sectionListRenderer/contents/0/itemSectionRenderer/contents")
            .ok_or_else(|| SerenyaError::Audio("Failed to extract itemSectionRenderer contents".to_owned()))?;

        let arr = contents.as_array().ok_or_else(|| {
            SerenyaError::Audio("ItemSectionRenderer contents is not an array".to_owned())
        })?;

        let mut candidates = Vec::new();
        for item in arr {
            if let Some(video) = item.get("videoRenderer") {
                let video_id = video.get("videoId").and_then(|v| v.as_str());
                let title = video.pointer("/title/runs/0/text").and_then(|v| v.as_str());
                let duration_str = video
                    .pointer("/lengthText/simpleText")
                    .and_then(|v| v.as_str());
                let channel = video
                    .pointer("/ownerText/runs/0/text")
                    .and_then(|v| v.as_str());
                let thumbnail = video
                    .pointer("/thumbnail/thumbnails/0/url")
                    .and_then(|v| v.as_str());

                let views_str = video
                    .pointer("/viewCountText/simpleText")
                    .and_then(|v| v.as_str());
                let views = views_str.and_then(|s| {
                    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
                    digits.parse::<u64>().ok()
                });

                let mut is_verified_artist = false;
                let mut is_verified = false;
                if let Some(badges) = video.get("ownerBadges").and_then(|b| b.as_array()) {
                    for badge in badges {
                        if let Some(style) = badge
                            .pointer("/metadataBadgeRenderer/style")
                            .and_then(|s| s.as_str())
                        {
                            if style == "BADGE_STYLE_TYPE_VERIFIED_ARTIST" {
                                is_verified_artist = true;
                            } else if style == "BADGE_STYLE_TYPE_VERIFIED" {
                                is_verified = true;
                            }
                        }
                    }
                }

                if let (Some(id), Some(t)) = (video_id, title) {
                    let duration = duration_str.and_then(parse_simple_duration);
                    let channel_name = channel.unwrap_or("Unknown Artist");
                    let is_topic =
                        channel_name.ends_with(" - Topic") || channel_name.ends_with("- Topic");

                    candidates.push(TrackCandidate {
                        source: "YouTube".to_owned(),
                        title: t.to_owned(),
                        artist: channel_name.to_owned(),
                        duration,
                        popularity: views,
                        is_official: is_verified_artist || is_verified,
                        is_topic_channel: is_topic,
                        url: format!("https://www.youtube.com/watch?v={}", id),
                        thumbnail: thumbnail.map(|s| s.to_owned()),
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
// SoundCloud Provider (using yt-dlp scsearch)
// ----------------------------------------------------
pub struct SoundCloudProvider;

#[async_trait]
impl MetadataProvider for SoundCloudProvider {
    fn supports(&self, input: &str) -> bool {
        input.contains("soundcloud.com/")
    }

    async fn search(
        &self,
        query: &str,
        _http_client: &reqwest::Client,
    ) -> Result<Vec<TrackCandidate>, SerenyaError> {
        let settings = crate::audio::runtime::settings();
        let output = crate::audio::runtime::run_ytdlp(
            "SoundCloud search",
            vec![
                "--flat-playlist".to_owned(),
                "--dump-single-json".to_owned(),
                format!("scsearch5:{query}"),
            ],
            crate::audio::runtime::duration_from_millis(settings.soundcloud_timeout_ms),
            false,
            Some(crate::audio::runtime::negative_cache_key("scsearch", query)),
        )
        .await?;

        let search_result: YtDlpSearchResult =
            serde_json::from_slice(&output.stdout).map_err(|e| {
                SerenyaError::Audio(format!("Failed to parse SoundCloud results: {}", e))
            })?;

        let entries = search_result.entries.unwrap_or_default();
        let mut candidates = Vec::new();
        for entry in entries {
            if let Some(id) = entry.id {
                candidates.push(TrackCandidate {
                    source: "SoundCloud".to_owned(),
                    title: entry.title.unwrap_or_else(|| "Unknown Title".to_string()),
                    artist: "SoundCloud Artist".to_owned(),
                    duration: entry.duration.map(|d| Duration::from_secs(d as u64)),
                    popularity: None,
                    is_official: false,
                    is_topic_channel: false,
                    url: format!("https://api.soundcloud.com/tracks/{}", id),
                    thumbnail: None,
                });
            }
        }

        Ok(candidates)
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
