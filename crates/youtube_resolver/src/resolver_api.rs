use crate::{
    BaseInnerTubeClient, InnerTubeClient, ResolveContext, ResolveError, ResolvedStream,
    create_android_client, create_android_vr_client, create_ios_client, create_tvhtml5_client,
    create_web_safari_client, format_selector, get_or_fetch_session, js_solver,
    resolve_best_audio_stream_rusty_ytdl, stream_probe,
};

pub async fn probe_resolved_stream_health(
    http_client: &reqwest::Client,
    stream: &ResolvedStream,
    bytes_to_probe: usize,
    min_speed_kbps: f64,
) -> Result<stream_probe::ProbeResult, stream_probe::ProbeError> {
    stream_probe::probe_stream_health(
        http_client,
        &stream.url,
        &stream.user_agent,
        &stream.client_kind,
        bytes_to_probe,
        min_speed_kbps,
    )
    .await
}

fn ordered_clients() -> Vec<BaseInnerTubeClient> {
    vec![
        create_android_vr_client(),
        create_tvhtml5_client(None),
        create_web_safari_client(),
        create_ios_client(None),
        create_android_client(None),
    ]
}

fn client_is_allowed(context: &ResolveContext, client_name: &str) -> bool {
    context.excluded_client_kind.as_deref() != Some(client_name)
}

pub async fn resolve_best_audio_stream_via_api(
    video_id: &str,
    context: &ResolveContext,
) -> Result<ResolvedStream, ResolveError> {
    let http_client = &context.http_client;
    let player_url = get_or_fetch_session(http_client).await?.player_url;
    let clients = ordered_clients();
    let mut last_err =
        ResolveError::NotPlayable("All Innertube clients failed to resolve stream".to_string());

    for client in clients {
        if !client_is_allowed(context, client.name()) {
            tracing::info!(
                client = client.name(),
                video_id,
                "Skipping Innertube client excluded for this retry"
            );
            continue;
        }
        tracing::debug!(
            client = client.name(),
            video_id,
            "Attempting to resolve stream with client"
        );
        match try_client(http_client, &player_url, &client, video_id, context).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

pub async fn resolve_best_audio_stream(
    video_id: &str,
    context: &ResolveContext,
) -> Result<ResolvedStream, ResolveError> {
    if let Ok(stream) = resolve_best_audio_stream_via_api(video_id, context).await {
        return Ok(stream);
    }

    let stream = resolve_best_audio_stream_rusty_ytdl(video_id, context).await?;
    if !client_is_allowed(context, &stream.client_kind) {
        return Err(ResolveError::NotPlayable(format!(
            "Fallback resolver returned client {} excluded for this retry",
            stream.client_kind
        )));
    }
    Ok(stream)
}

async fn try_client(
    http_client: &reqwest::Client,
    player_url: &str,
    client: &dyn InnerTubeClient,
    video_id: &str,
    context: &ResolveContext,
) -> Result<ResolvedStream, ResolveError> {
    let player_res = client.player(video_id, context).await.map_err(|err| {
        tracing::warn!(client = client.name(), error = %err, "InnerTube player API error");
        err
    })?;
    let formats = player_res
        .streaming_data
        .and_then(|data| data.adaptive_formats)
        .ok_or_else(|| {
            tracing::warn!(
                client = client.name(),
                "Player response contains no streaming data"
            );
            ResolveError::NotPlayable(format!(
                "Client {} returned player response with no streaming data",
                client.name()
            ))
        })?;
    let best_format = format_selector::select_best_audio(&formats).ok_or_else(|| {
        tracing::warn!(
            client = client.name(),
            "No suitable audio formats found for client"
        );
        ResolveError::NotPlayable(format!(
            "Client {} returned player response but no suitable audio formats found",
            client.name()
        ))
    })?;
    let decrypted_url = js_solver::decrypt_format_url(
        http_client,
        player_url,
        best_format.url.as_deref(),
        best_format.signature_cipher.as_deref(),
        best_format.cipher.as_deref(),
    )
    .await
    .map_err(|err| {
        tracing::warn!(
            client = client.name(),
            error = %err,
            "Failed to decrypt format URL. Rotating to next client..."
        );
        ResolveError::NotPlayable(format!(
            "Client {} failed to decrypt format URL: {}",
            client.name(),
            err
        ))
    })?;
    validate_stream(http_client, client, decrypted_url, &best_format).await
}

async fn validate_stream(
    http_client: &reqwest::Client,
    client: &dyn InnerTubeClient,
    decrypted_url: String,
    best_format: &rusty_ytdl::StreamingDataFormat,
) -> Result<ResolvedStream, ResolveError> {
    let user_agent = client.user_agent();
    let probe = stream_probe::probe_stream_health(
        http_client,
        &decrypted_url,
        &user_agent,
        client.name(),
        102400,
        50.0,
    )
    .await
    .map_err(|err| {
        tracing::warn!(
            client = client.name(),
            error = %err,
            "Stream probe failed. Rotating to next client..."
        );
        ResolveError::NotPlayable(format!(
            "Client {} resolved URL but stream probe failed: {}",
            client.name(),
            err
        ))
    })?;
    tracing::info!(
        client = client.name(),
        speed = format!("{:.2} KB/s", probe.speed_kbps),
        "Successfully probed and validated stream URL"
    );
    Ok(ResolvedStream {
        url: decrypted_url,
        client_kind: client.name().to_string(),
        user_agent,
        expires_at: None,
        mime_type: best_format.mime_type.as_ref().map(|m| m.mime.to_string()),
        bitrate: best_format.bitrate,
        resolve_source: format!("api_client_{}", client.name().to_lowercase()),
    })
}

#[cfg(test)]
mod retry_client_tests {
    use super::{client_is_allowed, ordered_clients};
    use crate::{InnerTubeClient, ResolveContext};

    #[test]
    fn retry_exclusion_skips_only_the_failed_client() {
        let android_vr_context = ResolveContext {
            excluded_client_kind: Some("ANDROID_VR".to_owned()),
            ..Default::default()
        };
        assert!(!client_is_allowed(&android_vr_context, "ANDROID_VR"));
        assert!(client_is_allowed(&android_vr_context, "TVHTML5"));
        assert!(client_is_allowed(&android_vr_context, "WEB_SAFARI"));

        let tv_context = ResolveContext {
            excluded_client_kind: Some("TVHTML5".to_owned()),
            ..Default::default()
        };
        assert!(!client_is_allowed(&tv_context, "TVHTML5"));
    }

    #[test]
    fn native_client_order_prefers_tv_before_token_sensitive_fallbacks() {
        let names = ordered_clients()
            .iter()
            .map(|client| client.name())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["ANDROID_VR", "TVHTML5", "WEB_SAFARI", "IOS", "ANDROID"]
        );
    }
}
