use super::extract::extract_player_functions;
use moka::future::Cache;
use sha1::{Digest, Sha1};
use std::sync::LazyLock;
use std::time::Duration;

static FUNCTIONS_CACHE: LazyLock<Cache<String, (String, String)>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(128)
        .time_to_live(Duration::from_secs(24 * 3600))
        .build()
});

pub(super) static N_CACHE: LazyLock<Cache<String, String>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(4096)
        .time_to_live(Duration::from_secs(12 * 3600))
        .build()
});

pub fn sha1_hash(input: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub async fn get_or_fetch_player_functions(
    http_client: &reqwest::Client,
    player_url: &str,
) -> Result<(String, String), String> {
    let url_hash = sha1_hash(player_url);
    if let Some(funcs) = FUNCTIONS_CACHE.get(&url_hash).await {
        return Ok(funcs);
    }

    tracing::info!(
        player_url,
        "Fetching player JS to extract solver functions..."
    );
    let response = http_client
        .get(player_url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .send()
        .await
        .map_err(|e| format!("Failed to download player JS: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Failed to read player JS text: {e}"))?;

    let funcs = extract_player_functions(&response);
    if funcs.0.is_empty() && funcs.1.is_empty() {
        tracing::warn!(
            player_url,
            "Could not extract decipher or n-transform functions from player JS"
        );
    }

    FUNCTIONS_CACHE.insert(url_hash, funcs.clone()).await;
    Ok(funcs)
}
