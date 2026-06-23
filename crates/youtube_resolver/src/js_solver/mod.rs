mod cache;
mod extract;
mod runtime;

pub use cache::{get_or_fetch_player_functions, sha1_hash};
pub use runtime::{decrypt_format_url, solve_n_throttle, solve_signature};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_player_function_extraction() -> Result<(), Box<dyn std::error::Error>> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        let session = crate::get_or_fetch_session(&client).await?;
        let (decipher_js, ncode_js) = get_or_fetch_player_functions(&client, &session.player_url)
            .await
            .map_err(std::io::Error::other)?;

        println!(
            "Resolved player URL: {}, decipher bytes: {}, ncode bytes: {}",
            session.player_url,
            decipher_js.len(),
            ncode_js.len()
        );
        assert!(session.player_url.contains("base.js"));

        let direct_url = "https://example.com/videoplayback?expire=1&n=plain";
        let decrypted =
            decrypt_format_url(&client, &session.player_url, Some(direct_url), None, None)
                .await
                .map_err(std::io::Error::other)?;
        assert!(decrypted.starts_with("https://example.com/videoplayback"));
        Ok(())
    }
}
