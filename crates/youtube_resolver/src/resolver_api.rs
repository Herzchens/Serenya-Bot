use crate::{
    BaseInnerTubeClient, InnerTubeClient, ResolveContext, ResolveError, ResolvedStream,
    create_tvhtml5_client, create_visionos_client, format_selector, get_or_fetch_session,
    js_solver, stream_probe,
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
    // Anonymous path: prefer clients that current yt-dlp does not mark as
    // requiring a GVS PO token for ordinary HTTPS playback.
    //
    // POT-sensitive clients remain defined for a future token-provider path,
    // but probing them without a valid token only burns requests and increases
    // the chance of triggering YouTube's anti-bot controls.
    vec![create_tvhtml5_client(None), create_visionos_client()]
}

fn client_is_allowed(context: &ResolveContext, client_name: &str) -> bool {
    context.excluded_client_kind.as_deref() != Some(client_name)
}

fn client_requires_gvs_pot(client_name: &str) -> bool {
    matches!(
        client_name,
        "IOS" | "ANDROID" | "ANDROID_VR" | "WEB" | "WEB_SAFARI" | "MWEB"
    )
}

fn is_googlevideo_stream_url(stream_url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(stream_url) else {
        return false;
    };

    if parsed.scheme() != "https" {
        return false;
    }

    let Some(host) = parsed.host_str() else {
        return false;
    };

    host == "googlevideo.com" || host.ends_with(".googlevideo.com")
}

fn stream_has_gvs_pot(stream_url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(stream_url) else {
        return false;
    };

    parsed
        .query_pairs()
        .any(|(key, value)| key == "pot" && !value.is_empty())
}

fn validate_gvs_token_requirement(client_name: &str, stream_url: &str) -> Result<(), ResolveError> {
    if is_googlevideo_stream_url(stream_url)
        && client_requires_gvs_pot(client_name)
        && !stream_has_gvs_pot(stream_url)
    {
        tracing::warn!(
            client = client_name,
            "Rejecting Googlevideo stream from GVS PO-token-sensitive client without a token"
        );

        return Err(ResolveError::NotPlayable(format!(
            "Client {client_name} returned Googlevideo stream without required GVS PO token"
        )));
    }

    Ok(())
}

async fn resolve_best_audio_stream_with_clients(
    http_client: &reqwest::Client,
    player_url: &str,
    clients: &[&dyn InnerTubeClient],
    video_id: &str,
    context: &ResolveContext,
) -> Result<ResolvedStream, ResolveError> {
    let mut last_err =
        ResolveError::NotPlayable("All Innertube clients failed to resolve stream".to_string());

    for client in clients {
        let client = *client;

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

        match try_client(http_client, player_url, client, video_id, context).await {
            Ok(stream) => return Ok(stream),
            Err(err) if err.is_not_a_bot_gate() => {
                // An explicit anti-bot gate is session-level evidence. Trying
                // another anonymous client would only spend another request and
                // could overwrite the signal that must trigger cooldown.
                return Err(err);
            }
            Err(err) => last_err = err,
        }
    }

    Err(last_err)
}

pub async fn resolve_best_audio_stream_via_api(
    video_id: &str,
    context: &ResolveContext,
) -> Result<ResolvedStream, ResolveError> {
    let http_client = &context.http_client;
    let player_url = get_or_fetch_session(http_client).await?.player_url;
    let clients = ordered_clients();
    let client_refs = clients
        .iter()
        .map(|client| client as &dyn InnerTubeClient)
        .collect::<Vec<_>>();

    resolve_best_audio_stream_with_clients(
        http_client,
        &player_url,
        &client_refs,
        video_id,
        context,
    )
    .await
}

pub async fn resolve_best_audio_stream(
    video_id: &str,
    context: &ResolveContext,
) -> Result<ResolvedStream, ResolveError> {
    // The anonymous native path is intentionally restricted to ordered_clients()
    // (TVHTML5 -> VISIONOS). The vendored rusty_ytdl fallback internally falls
    // back to an old Android client, which would bypass that policy.
    resolve_best_audio_stream_via_api(video_id, context).await
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
    validate_gvs_token_requirement(client.name(), &decrypted_url)?;

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
mod rotation_access_gate_tests {
    use super::resolve_best_audio_stream_with_clients;
    use crate::{InnerTubeClient, ResolveContext, ResolveError};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone, Copy)]
    enum FakeFailure {
        BotGate,
        Ordinary,
    }

    struct FailingClient {
        name: &'static str,
        failure: FakeFailure,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl InnerTubeClient for FailingClient {
        fn name(&self) -> &'static str {
            self.name
        }

        fn client_name(&self) -> &'static str {
            self.name
        }

        fn client_version(&self) -> String {
            "test".to_owned()
        }

        fn user_agent(&self) -> String {
            "serenya-test".to_owned()
        }

        async fn player(
            &self,
            _video_id: &str,
            _context: &ResolveContext,
        ) -> Result<crate::PlayerResponse, ResolveError> {
            self.calls.fetch_add(1, Ordering::SeqCst);

            Err(match self.failure {
                FakeFailure::BotGate => ResolveError::ApiError {
                    status: Some("LOGIN_REQUIRED".to_owned()),
                    reason: Some("Sign in to confirm you're not a bot".to_owned()),
                },
                FakeFailure::Ordinary => {
                    ResolveError::NotPlayable("ordinary later-client failure".to_owned())
                }
            })
        }
    }

    #[tokio::test]
    async fn explicit_bot_gate_aborts_remaining_client_rotation() {
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));

        let first = FailingClient {
            name: "TVHTML5",
            failure: FakeFailure::BotGate,
            calls: Arc::clone(&first_calls),
        };

        let second = FailingClient {
            name: "VISIONOS",
            failure: FakeFailure::Ordinary,
            calls: Arc::clone(&second_calls),
        };

        let clients: Vec<&dyn InnerTubeClient> = vec![&first, &second];
        let context = ResolveContext::default();

        let err = resolve_best_audio_stream_with_clients(
            &context.http_client,
            "https://example.invalid/player.js",
            &clients,
            "test-video-id",
            &context,
        )
        .await
        .expect_err("both fake clients intentionally fail");

        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            second_calls.load(Ordering::SeqCst),
            0,
            "client rotation continued after explicit anti-bot gate"
        );
        assert!(
            err.is_not_a_bot_gate(),
            "explicit anti-bot gate was overwritten by a later ordinary client error"
        );
    }
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
    fn anonymous_client_order_avoids_pot_sensitive_clients() {
        let names = ordered_clients()
            .iter()
            .map(|client| client.name())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["TVHTML5", "VISIONOS"]);
    }
}

#[cfg(test)]
mod gvs_token_requirement_tests {
    use super::validate_gvs_token_requirement;

    const NO_POT: &str = "https://rr1---sn.example.googlevideo.com/videoplayback?itag=251&c=IOS";

    const WITH_POT: &str =
        "https://rr1---sn.example.googlevideo.com/videoplayback?itag=251&c=IOS&pot=opaque-token";

    const EMPTY_POT: &str =
        "https://rr1---sn.example.googlevideo.com/videoplayback?itag=251&c=IOS&pot=";

    #[test]
    fn ios_googlevideo_without_gvs_pot_is_rejected() {
        assert!(
            validate_gvs_token_requirement("IOS", NO_POT).is_err(),
            "BUG #13: IOS Googlevideo without GVS POT must not be accepted as playable"
        );
    }

    #[test]
    fn ios_googlevideo_with_non_empty_gvs_pot_remains_eligible() {
        assert!(validate_gvs_token_requirement("IOS", WITH_POT).is_ok());
    }

    #[test]
    fn empty_gvs_pot_is_equivalent_to_missing_token() {
        assert!(
            validate_gvs_token_requirement("IOS", EMPTY_POT).is_err(),
            "an empty pot query parameter must not satisfy the GVS token requirement"
        );
    }

    #[test]
    fn other_gvs_pot_sensitive_clients_without_token_are_rejected() {
        for client in ["ANDROID", "WEB", "WEB_SAFARI"] {
            assert!(
                validate_gvs_token_requirement(client, NO_POT).is_err(),
                "{client} Googlevideo stream without GVS POT must be rejected"
            );
        }
    }

    #[test]
    fn android_vr_without_pot_is_rejected_by_gvs_guard() {
        assert!(
            validate_gvs_token_requirement("ANDROID_VR", NO_POT).is_err(),
            "current Android VR playback is POT-sensitive and must not be used tokenless"
        );
    }

    #[test]
    fn tvhtml5_without_pot_is_not_blocked_by_gvs_guard() {
        assert!(validate_gvs_token_requirement("TVHTML5", NO_POT).is_ok());
    }

    #[test]
    fn googlevideo_lookalike_host_is_not_treated_as_googlevideo() {
        assert!(
            validate_gvs_token_requirement(
                "IOS",
                "https://googlevideo.com.attacker.example/audio?itag=251"
            )
            .is_ok()
        );
    }

    #[test]
    fn unrelated_non_googlevideo_url_is_not_blocked_by_ios_guard() {
        assert!(validate_gvs_token_requirement("IOS", "https://media.example/audio.webm").is_ok());
    }
}
