use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct SessionData {
    pub visitor_data: String,
    pub sts: u64,
    pub player_url: String,
}

#[derive(Debug, Clone)]
pub struct ResolveContext {
    pub visitor_data: Option<String>,
    pub user_agent_override: Option<String>,
    pub language: Option<String>,
    pub region: Option<String>,
    pub timeout: Duration,
    pub trace_id: Option<String>,
    pub excluded_client_kind: Option<String>,
    pub http_client: reqwest::Client,
}

static DEFAULT_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

impl Default for ResolveContext {
    fn default() -> Self {
        let http_client = DEFAULT_CLIENT.get_or_init(reqwest::Client::new).clone();
        Self {
            visitor_data: None,
            user_agent_override: None,
            language: Some("en".to_string()),
            region: Some("US".to_string()),
            timeout: Duration::from_secs(5),
            trace_id: None,
            excluded_client_kind: None,
            http_client,
        }
    }
}

impl ResolveContext {
    pub fn new(http_client: reqwest::Client) -> Self {
        Self {
            visitor_data: None,
            user_agent_override: None,
            language: Some("en".to_string()),
            region: Some("US".to_string()),
            timeout: Duration::from_secs(5),
            trace_id: None,
            excluded_client_kind: None,
            http_client,
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedStream {
    pub url: String,
    pub client_kind: String,
    pub user_agent: String,
    pub expires_at: Option<u64>,
    pub mime_type: Option<String>,
    pub bitrate: Option<u64>,
    pub resolve_source: String,
}
