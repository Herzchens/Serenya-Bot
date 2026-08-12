#![allow(dead_code)]

use m3u8_rs::ByteRange;
use reqwest::header::{self, HeaderMap};

use super::hashable_byte_range::HashableByteRange;
use crate::VideoError;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RemoteData(url::Url, Option<HashableByteRange>);

impl RemoteData {
    pub fn new(url: impl Into<url::Url>, byte_range: Option<ByteRange>) -> Self {
        let url: url::Url = url.into();
        Self(url, byte_range.map(HashableByteRange::new))
    }

    pub fn url(&self) -> &url::Url {
        &self.0
    }

    pub fn byte_range_string(&self) -> Option<String> {
        let start = self.1.as_ref()?.offset.unwrap_or(0);
        let end = start.saturating_add(self.1.as_ref()?.length.saturating_sub(1));

        Some(format!("bytes={}-{}", start, end))
    }

    /// Fetch this segment and return (bytes, final url)
    pub async fn fetch(
        &self,
        client: &reqwest_middleware::ClientWithMiddleware,
    ) -> Result<(Vec<u8>, url::Url), VideoError> {
        // Add byte range headers if needed
        let mut header_map = HeaderMap::new();
        if let Some(ref range) = self.byte_range_string() {
            header_map.insert(
                header::RANGE,
                header::HeaderValue::from_str(range)
                    .unwrap_or(header::HeaderValue::from_str("").unwrap()),
            );
        }

        let ua = crate::utils::get_user_agent_for_url(self.url().as_str());
        header_map.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(ua).unwrap(),
        );

        // Fetch data
        let resp = client
            .get(self.url().clone())
            .headers(header_map)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(VideoError::BodyCannotParsed);
        }
        let final_url = resp.url().clone();
        let bytes = resp.bytes().await?.into_iter().collect();

        Ok((bytes, final_url))
    }
}

#[cfg(test)]
mod byte_range_tests {
    use super::RemoteData;
    use m3u8_rs::ByteRange;

    fn remote(range: ByteRange) -> RemoteData {
        RemoteData::new(
            url::Url::parse("https://example.invalid/segment.ts")
                .expect("test segment URL must parse"),
            Some(range),
        )
    }

    #[test]
    fn normal_byte_range_formats_expected_header() {
        let data = remote(ByteRange {
            length: 4,
            offset: Some(10),
        });
        assert_eq!(data.byte_range_string().as_deref(), Some("bytes=10-13"));
    }

    #[test]
    fn byte_range_end_overflow_is_saturated_without_panicking() {
        let data = remote(ByteRange {
            length: 2,
            offset: Some(u64::MAX),
        });
        let result = std::panic::catch_unwind(|| data.byte_range_string());
        assert!(
            result.is_ok(),
            "malformed/extreme HLS byte ranges must not panic the stream"
        );
        assert_eq!(
            result
                .expect("byte range formatting must return normally")
                .as_deref(),
            Some("bytes=18446744073709551615-18446744073709551615"),
            "range end should clamp instead of wrapping"
        );
    }

    #[test]
    fn large_byte_range_length_does_not_wrap_end() {
        let data = remote(ByteRange {
            length: u64::MAX,
            offset: Some(2),
        });
        let result = std::panic::catch_unwind(|| data.byte_range_string());
        assert!(result.is_ok(), "range length overflow must not panic");
        assert_eq!(
            result
                .expect("byte range formatting must return normally")
                .as_deref(),
            Some("bytes=2-18446744073709551615"),
            "range end should clamp at u64::MAX"
        );
    }
}
