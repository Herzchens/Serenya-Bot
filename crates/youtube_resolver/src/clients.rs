use crate::BaseInnerTubeClient;
use serde_json::json;

pub fn create_android_vr_client() -> BaseInnerTubeClient {
    BaseInnerTubeClient::new(
        "ANDROID_VR",
        "ANDROID_VR",
        "1.57.2".to_string(),
        "Mozilla/5.0 (Linux; U; Android 10; en-US; Quest 2 Build/QQ3A.200805.001.A1) AppleWebKit/537.36 (KHTML, like Gecko) OculusBrowser/18.1.0.0.30.29 Chrome/89.0.4389.90 VR Safari/537.36".to_string(),
        "91".to_string(),
        Some(json!({
            "osName": "Android",
            "osVersion": "10",
            "deviceMake": "Oculus",
            "deviceModel": "Quest 2"
        })),
        None,
    )
}

pub fn create_web_safari_client() -> BaseInnerTubeClient {
    BaseInnerTubeClient::new(
        "WEB_SAFARI",
        "WEB",
        "2.20240101.00.00".to_string(),
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15".to_string(),
        "1".to_string(),
        Some(json!({
            "browserName": "Safari",
            "browserVersion": "17.0",
            "osName": "Macintosh",
            "osVersion": "10.15.7"
        })),
        None,
    )
}

pub fn create_android_client(version: Option<String>) -> BaseInnerTubeClient {
    let version = version.unwrap_or_else(|| "20.10.38".to_string());
    BaseInnerTubeClient::new(
        "ANDROID",
        "ANDROID",
        version.clone(),
        format!("com.google.android.youtube/{version} (Linux; U; Android 11) gzip"),
        "3".to_string(),
        Some(json!({
            "osName": "Android",
            "osVersion": "11",
            "userAgent": format!("com.google.android.youtube/{version} (Linux; U; Android 11) gzip")
        })),
        None,
    )
}

pub fn create_tvhtml5_client(version: Option<String>) -> BaseInnerTubeClient {
    BaseInnerTubeClient::new(
        "TVHTML5",
        "TVHTML5",
        version.unwrap_or_else(|| "7.20230522.05.00".to_string()),
        "Mozilla/5.0 (Chromecast; Google TV) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/90.0.4430.225 Safari/537.36".to_string(),
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
    let version = version.unwrap_or_else(|| "21.02.3".to_string());
    BaseInnerTubeClient::new(
        "IOS",
        "IOS",
        version.clone(),
        format!("com.google.ios.youtube/{version} (iPhone16,2; U; CPU iOS 18_1_0 like Mac OS X;)"),
        "5".to_string(),
        Some(json!({
            "deviceMake": "Apple",
            "deviceModel": "iPhone16,2",
            "osName": "iPhone",
            "osVersion": "18.1.0",
            "userAgent": format!("com.google.ios.youtube/{version} (iPhone16,2; U; CPU iOS 18_1_0 like Mac OS X;)")
        })),
        None,
    )
}
