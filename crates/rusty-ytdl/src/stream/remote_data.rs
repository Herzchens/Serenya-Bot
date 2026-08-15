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

    pub fn byte_range_bounds(&self) -> Option<(u64, u64)> {
        let range = self.1.as_ref()?;
        Some((range.offset.unwrap_or(0), range.length))
    }

    pub fn byte_range_string(&self) -> Option<String> {
        let (start, length) = self.byte_range_bounds()?;
        let end = start.saturating_add(length.saturating_sub(1));

        Some(format!("bytes={}-{}", start, end))
    }

    /// Fetch this segment and return (bytes, final url)
    pub async fn fetch(
        &self,
        client: &reqwest_middleware::ClientWithMiddleware,
    ) -> Result<(Vec<u8>, url::Url), VideoError> {
        let requested_byte_range = self.byte_range_bounds();

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
        let status = resp.status();
        if !status.is_success() {
            return Err(VideoError::BodyCannotParsed);
        }
        let final_url = resp.url().clone();
        let bytes = resp.bytes().await?;
        let bytes = if status == reqwest::StatusCode::OK {
            if let Some((range_start, range_length)) = requested_byte_range {
                let range_start = usize::try_from(range_start).map_err(|_| {
                    VideoError::DownloadError(
                        "remote byte-range start does not fit this platform".to_string(),
                    )
                })?;
                let range_length = usize::try_from(range_length).map_err(|_| {
                    VideoError::DownloadError(
                        "remote byte-range length does not fit this platform".to_string(),
                    )
                })?;
                let range_end = range_start.checked_add(range_length).ok_or_else(|| {
                    VideoError::DownloadError("remote byte-range end overflow".to_string())
                })?;
                if range_end > bytes.len() {
                    return Err(VideoError::DownloadError(format!(
                        "server ignored Range but returned only {} bytes for requested bytes={}-{}",
                        bytes.len(),
                        range_start,
                        range_end.saturating_sub(1)
                    )));
                }
                bytes[range_start..range_end].to_vec()
            } else {
                bytes.to_vec()
            }
        } else {
            bytes.to_vec()
        };

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

#[cfg(test)]
mod ignored_map_range_response_tests {
    use super::RemoteData;
    use m3u8_rs::ByteRange;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read initialization-map range request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    fn client() -> reqwest_middleware::ClientWithMiddleware {
        reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build()
    }

    #[tokio::test]
    async fn ignored_initialization_map_range_emits_only_requested_subrange() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ignored initialization-map range server");
        let address = listener
            .local_addr()
            .expect("read ignored initialization-map range address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept map request");
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("Range: bytes=2-3")),
                "RemoteData must request the declared initialization-map sub-range; got {request:?}"
            );
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
                )
                .await
                .expect("write full ignored-range initialization resource");
        });

        let data = RemoteData::new(
            url::Url::parse(&format!("http://{address}/init.mp4"))
                .expect("parse local initialization URL"),
            Some(ByteRange {
                length: 2,
                offset: Some(2),
            }),
        );

        let (bytes, _) = data
            .fetch(&client())
            .await
            .expect("fetch initialization map");
        server
            .await
            .expect("ignored initialization-map range server should join");

        assert_eq!(
            bytes,
            b"cd",
            "EXT-X-MAP BYTERANGE declares only bytes 2-3 as the initialization section; a server returning the whole resource must not make the client emit the whole resource"
        );
    }

    #[tokio::test]
    async fn partial_initialization_map_response_remains_unchanged() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind partial initialization-map server");
        let address = listener.local_addr().expect("read partial map address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept partial map request");
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("Range: bytes=2-3")),
                "ranged initialization-map control must send Range; got {request:?}"
            );
            socket
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 2\r\nContent-Range: bytes 2-3/6\r\nConnection: close\r\n\r\ncd",
                )
                .await
                .expect("write partial initialization-map response");
        });

        let data = RemoteData::new(
            url::Url::parse(&format!("http://{address}/init.mp4"))
                .expect("parse partial initialization URL"),
            Some(ByteRange {
                length: 2,
                offset: Some(2),
            }),
        );
        let (bytes, _) = data
            .fetch(&client())
            .await
            .expect("fetch partial initialization map");
        server.await.expect("partial map server should join");

        assert_eq!(
            bytes, b"cd",
            "a real 206 body is already the requested range"
        );
    }

    #[tokio::test]
    async fn ordinary_remote_data_200_still_returns_full_body() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ordinary RemoteData server");
        let address = listener
            .local_addr()
            .expect("read ordinary RemoteData address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept ordinary request");
            let request = read_request(&mut socket).await;
            assert!(
                !request.to_ascii_lowercase().contains("\r\nrange:"),
                "ordinary RemoteData must not invent a Range header; got {request:?}"
            );
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
                )
                .await
                .expect("write ordinary full response");
        });

        let data = RemoteData::new(
            url::Url::parse(&format!("http://{address}/file.bin"))
                .expect("parse ordinary RemoteData URL"),
            None,
        );
        let (bytes, _) = data
            .fetch(&client())
            .await
            .expect("fetch ordinary RemoteData");
        server.await.expect("ordinary server should join");

        assert_eq!(
            bytes, b"abcdef",
            "non-ranged 200 behavior must remain unchanged"
        );
    }
}
