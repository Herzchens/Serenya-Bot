use crate::BaseInnerTubeClient;
use serde_json::json;

pub fn create_android_vr_client() -> BaseInnerTubeClient {
    BaseInnerTubeClient::new(
        "ANDROID_VR",
        "ANDROID_VR",
        "1.65.10".to_string(),
        "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip".to_string(),
        "28".to_string(),
        Some(json!({
            "osName": "Android",
            "osVersion": "12L",
            "androidSdkVersion": 32,
            "deviceMake": "Oculus",
            "deviceModel": "Quest 3"
        })),
        None,
    )
}

pub fn create_web_safari_client() -> BaseInnerTubeClient {
    BaseInnerTubeClient::new(
        "WEB_SAFARI",
        "WEB",
        "2.20260708.00.00".to_string(),
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15,gzip(gfe)".to_string(),
        "1".to_string(),
        Some(json!({
            "browserName": "Safari",
            "browserVersion": "15.5",
            "osName": "Macintosh",
            "osVersion": "10.15.7"
        })),
        None,
    )
}

pub fn create_android_client(version: Option<String>) -> BaseInnerTubeClient {
    let version = version.unwrap_or_else(|| "21.26.364".to_string());
    BaseInnerTubeClient::new(
        "ANDROID",
        "ANDROID",
        version.clone(),
        format!("com.google.android.youtube/{version} (Linux; U; Android 11) gzip"),
        "3".to_string(),
        Some(json!({
            "osName": "Android",
            "osVersion": "11",
            "androidSdkVersion": 30,
            "userAgent": format!("com.google.android.youtube/{version} (Linux; U; Android 11) gzip")
        })),
        None,
    )
}

pub fn create_tvhtml5_client(version: Option<String>) -> BaseInnerTubeClient {
    BaseInnerTubeClient::new(
        "TVHTML5",
        "TVHTML5",
        version.unwrap_or_else(|| "7.20260707.07.00".to_string()),
        "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)".to_string(),
        "7".to_string(),
        None,
        None,
    )
}

pub fn create_visionos_client() -> BaseInnerTubeClient {
    BaseInnerTubeClient::new(
        "VISIONOS",
        "VISIONOS",
        "1.02".to_string(),
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15".to_string(),
        "101".to_string(),
        Some(json!({
            "deviceMake": "Apple",
            "deviceModel": "RealityDevice17,1",
            "osName": "visionOS",
            "osVersion": "26.5.23O471"
        })),
        None,
    )
}

pub fn create_ios_client(version: Option<String>) -> BaseInnerTubeClient {
    let version = version.unwrap_or_else(|| "21.26.4".to_string());
    BaseInnerTubeClient::new(
        "IOS",
        "IOS",
        version.clone(),
        format!("com.google.ios.youtube/{version} (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)"),
        "5".to_string(),
        Some(json!({
            "deviceMake": "Apple",
            "deviceModel": "iPhone16,2",
            "osName": "iPhone",
            "osVersion": "18.3.2.22D82",
            "userAgent": format!("com.google.ios.youtube/{version} (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)")
        })),
        None,
    )
}
