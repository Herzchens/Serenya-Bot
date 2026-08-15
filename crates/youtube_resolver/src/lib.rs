#![allow(clippy::collapsible_if)]

pub use rusty_ytdl::PlayerResponse;

mod clients;
pub mod format_selector;
mod innertube;
pub mod js_solver;
mod resolver_api;
mod rusty_resolver;
mod session;
pub mod stream_probe;
mod types;

pub use clients::{
    create_android_client, create_android_vr_client, create_ios_client, create_tvhtml5_client,
    create_visionos_client, create_web_safari_client,
};
pub use innertube::{BaseInnerTubeClient, InnerTubeClient};
pub use resolver_api::{
    probe_resolved_stream_health, resolve_best_audio_stream, resolve_best_audio_stream_via_api,
};
pub use rusty_resolver::resolve_best_audio_stream_rusty_ytdl;
pub use session::get_or_fetch_session;
pub use types::{ResolveContext, ResolveError, ResolvedStream, SessionData};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "live-tests")]
    #[tokio::test]
    async fn test_android_vr_resolve() {
        let client = create_android_vr_client();
        let ctx = ResolveContext::default();
        let res = client.player("dQw4w9WgXcQ", &ctx).await;
        assert!(res.is_ok());
        assert!(res.unwrap().streaming_data.is_some());
    }

    #[test]
    fn web_safari_client_identity_is_explicit() {
        let client = create_web_safari_client();
        assert_eq!(client.name(), "WEB_SAFARI");
        assert_eq!(client.client_name(), "WEB");
        assert!(client.user_agent().contains("Safari"));
    }

    #[test]
    fn visionos_client_identity_is_explicit() {
        let client = create_visionos_client();

        assert_eq!(client.name(), "VISIONOS");
        assert_eq!(client.client_name(), "VISIONOS");
        assert_eq!(client.client_version(), "1.02");
        assert!(
            client.user_agent().contains("Safari"),
            "VISIONOS client must retain its Safari-style user agent"
        );
    }

    #[cfg(feature = "live-tests")]
    #[tokio::test]
    async fn test_resolve_best_audio_stream() {
        let ctx = ResolveContext::default();
        let res = resolve_best_audio_stream("dQw4w9WgXcQ", &ctx).await;
        if let Err(e) = &res {
            println!("resolve_best_audio_stream failed: {:?}", e);
        }
        let stream = res.unwrap();
        println!(
            "resolve_best_audio_stream succeeded! url starts with: {}",
            &stream.url[..std::cmp::min(stream.url.len(), 60)]
        );
        assert!(stream.url.contains("googlevideo.com"));
    }
}
