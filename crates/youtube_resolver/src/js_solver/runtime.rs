use super::cache::{N_CACHE, get_or_fetch_player_functions, sha1_hash};
use boa_engine::{Context, Source};

pub fn solve_signature(decipher_js: &str, encrypted_sig: &str) -> Result<String, String> {
    if decipher_js.is_empty() {
        return Ok(encrypted_sig.to_string());
    }
    run_js_function(decipher_js, encrypted_sig, "decipher")
}

pub async fn solve_n_throttle(
    http_client: &reqwest::Client,
    player_url: &str,
    n_input: &str,
) -> Result<String, String> {
    if n_input.is_empty() {
        return Ok(String::new());
    }

    let cache_key = format!("{}_{}", sha1_hash(player_url), n_input);
    if let Some(cached_n) = N_CACHE.get(&cache_key).await {
        return Ok(cached_n);
    }

    let (_, ncode_js) = get_or_fetch_player_functions(http_client, player_url).await?;
    if ncode_js.is_empty() {
        return Ok(n_input.to_string());
    }

    let n_output = run_js_function(&ncode_js, n_input, "ncode")?;
    N_CACHE.insert(cache_key, n_output.clone()).await;
    Ok(n_output)
}

pub async fn decrypt_format_url(
    http_client: &reqwest::Client,
    player_url: &str,
    format_url: Option<&str>,
    sig_cipher: Option<&str>,
    cipher: Option<&str>,
) -> Result<String, String> {
    let mut final_url = if let Some(cipher_str) = sig_cipher.or(cipher) {
        decode_signature_cipher(http_client, player_url, cipher_str).await?
    } else if let Some(url_str) = format_url {
        url_str.to_string()
    } else {
        return Err("No stream URL or signature cipher found in format".to_string());
    };

    if let Ok(mut parsed) = url::Url::parse(&final_url)
        && let Some(n_val) = parsed
            .query_pairs()
            .find(|(key, _)| key == "n")
            .map(|(_, value)| value.into_owned())
        && let Ok(solved_n) = solve_n_throttle(http_client, player_url, &n_val).await
    {
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(key, value)| {
                if key == "n" {
                    (key.into_owned(), solved_n.clone())
                } else {
                    (key.into_owned(), value.into_owned())
                }
            })
            .collect();
        parsed.query_pairs_mut().clear().extend_pairs(pairs);
        final_url = parsed.to_string();
    }

    Ok(final_url)
}

async fn decode_signature_cipher(
    http_client: &reqwest::Client,
    player_url: &str,
    cipher_str: &str,
) -> Result<String, String> {
    let (decipher_js, _) = get_or_fetch_player_functions(http_client, player_url).await?;
    let params = url::form_urlencoded::parse(cipher_str.as_bytes());
    let mut url_val = String::new();
    let mut s_val = String::new();
    let mut sp_val = "sig".to_string();

    for (key, value) in params {
        match key.as_ref() {
            "url" => url_val = value.into_owned(),
            "s" => s_val = value.into_owned(),
            "sp" => sp_val = value.into_owned(),
            _ => {}
        }
    }

    if url_val.is_empty() {
        return Err("Signature cipher is missing URL parameter".to_string());
    }
    if !s_val.is_empty() && decipher_js.is_empty() {
        return Err(
            "Signature cipher is present but no decipher function was extracted".to_string(),
        );
    }

    let decrypted_sig = solve_signature(&decipher_js, &s_val)?;
    let mut parsed_url =
        url::Url::parse(&url_val).map_err(|e| format!("Invalid URL in cipher: {e}"))?;
    parsed_url
        .query_pairs_mut()
        .append_pair(&sp_val, &decrypted_sig);
    Ok(parsed_url.to_string())
}

fn run_js_function(js_source: &str, input: &str, label: &str) -> Result<String, String> {
    let mut context = Context::default();
    context
        .eval(Source::from_bytes(js_source.as_bytes()))
        .map_err(|e| format!("JS evaluation error during {label} setup: {e}"))?;
    let func_name = js_function_name(js_source)
        .ok_or_else(|| format!("Could not determine {label} function name"))?;
    let js_call = format!("{func_name}(\"{input}\")");
    let result = context
        .eval(Source::from_bytes(js_call.as_bytes()))
        .map_err(|e| format!("JS execution error during {label} call: {e}"))?;
    result
        .as_string()
        .and_then(|js_str| js_str.to_std_string().ok())
        .ok_or_else(|| format!("{label} function returned non-string value"))
}

fn js_function_name(js_source: &str) -> Option<&str> {
    js_source
        .split('=')
        .next()
        .and_then(|s| s.split("var ").last())
        .map(str::trim)
}
