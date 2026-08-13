use async_trait::async_trait;
use bytes::{Bytes, BytesMut};

#[cfg(feature = "ffmpeg")]
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};

use crate::constants::{DEFAULT_HEADERS, DEFAULT_MAX_RETRIES};
use crate::stream::streams::Stream;
use crate::structs::{CustomRetryableStrategy, VideoError};

#[cfg(feature = "ffmpeg")]
use crate::structs::FFmpegArgs;

#[cfg(feature = "ffmpeg")]
use super::{FFmpegStream, FFmpegStreamOptions};

fn partial_response_end(
    content_range: &str,
    requested_start: u64,
    requested_end: u64,
    expected_content_length: u64,
    body_len: usize,
) -> Result<u64, VideoError> {
    let body_len = u64::try_from(body_len).map_err(|_| {
        VideoError::DownloadError("partial response body length does not fit u64".to_string())
    })?;
    if body_len == 0 {
        return Err(VideoError::DownloadError(
            "partial response body was empty".to_string(),
        ));
    }

    let range = content_range.strip_prefix("bytes ").ok_or_else(|| {
        VideoError::DownloadError(format!("invalid Content-Range unit: {content_range}"))
    })?;
    let (bounds, total) = range.split_once('/').ok_or_else(|| {
        VideoError::DownloadError(format!("invalid Content-Range: {content_range}"))
    })?;
    let (served_start, served_end) = bounds.split_once('-').ok_or_else(|| {
        VideoError::DownloadError(format!("invalid Content-Range bounds: {content_range}"))
    })?;
    let served_start = served_start.parse::<u64>().map_err(|_| {
        VideoError::DownloadError(format!("invalid Content-Range start: {content_range}"))
    })?;
    let served_end = served_end.parse::<u64>().map_err(|_| {
        VideoError::DownloadError(format!("invalid Content-Range end: {content_range}"))
    })?;
    if served_start != requested_start || served_end < served_start || served_end > requested_end {
        return Err(VideoError::DownloadError(format!(
            "unexpected Content-Range {content_range} for requested bytes={requested_start}-{requested_end}"
        )));
    }
    if total != "*" {
        let total = total.parse::<u64>().map_err(|_| {
            VideoError::DownloadError(format!("invalid Content-Range total: {content_range}"))
        })?;
        if total != expected_content_length {
            return Err(VideoError::DownloadError(format!(
                "Content-Range representation length {total} does not match expected content length {expected_content_length}: {content_range}"
            )));
        }
        if served_end >= total {
            return Err(VideoError::DownloadError(format!(
                "Content-Range end exceeds representation length: {content_range}"
            )));
        }
    }
    let expected_len = served_end
        .checked_sub(served_start)
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| VideoError::DownloadError("partial response length overflow".to_string()))?;
    if expected_len != body_len {
        return Err(VideoError::DownloadError(format!(
            "Content-Range {content_range} describes {expected_len} bytes but body contained {body_len}"
        )));
    }

    Ok(served_end)
}

pub struct NonLiveStreamOptions {
    pub client: Option<reqwest_middleware::ClientWithMiddleware>,
    pub link: String,
    pub content_length: u64,
    pub dl_chunk_size: u64,
    pub start: u64,
    pub end: u64,

    #[cfg(feature = "ffmpeg")]
    pub ffmpeg_args: Option<FFmpegArgs>,
}

pub struct NonLiveStream {
    link: String,
    content_length: u64,
    dl_chunk_size: u64,
    start: RwLock<u64>,
    end: RwLock<u64>,
    start_static: u64,
    end_static: u64,
    chunk_lock: Mutex<()>,

    client: reqwest_middleware::ClientWithMiddleware,

    #[cfg(feature = "ffmpeg")]
    ffmpeg_args: Vec<String>,

    #[cfg(feature = "ffmpeg")]
    ffmpeg_stream: Arc<Mutex<Option<FFmpegStream>>>,
}

impl NonLiveStream {
    pub fn new(options: NonLiveStreamOptions) -> Result<Self, VideoError> {
        if options.dl_chunk_size == 0 {
            return Err(VideoError::DownloadError(
                "download chunk size must be greater than zero".to_string(),
            ));
        }
        let client = if options.client.is_some() {
            options.client.unwrap()
        } else {
            let client = reqwest::Client::builder()
                .build()
                .map_err(VideoError::Reqwest)?;

            let retry_policy = reqwest_retry::policies::ExponentialBackoff::builder()
                .retry_bounds(
                    std::time::Duration::from_millis(1000),
                    std::time::Duration::from_millis(30000),
                )
                .build_with_max_retries(DEFAULT_MAX_RETRIES);
            reqwest_middleware::ClientBuilder::new(client)
                .with(
                    reqwest_retry::RetryTransientMiddleware::new_with_policy_and_strategy(
                        retry_policy,
                        CustomRetryableStrategy,
                    ),
                )
                .build()
        };

        #[cfg(feature = "ffmpeg")]
        {
            let ffmpeg_args = options
                .ffmpeg_args
                .clone()
                .map(|x| x.build())
                .unwrap_or_default();

            let ffmpeg_stream = if !ffmpeg_args.is_empty() {
                Arc::new(Mutex::new(Some(FFmpegStream::new(FFmpegStreamOptions {
                    client: client.clone(),
                    link: options.link.clone(),
                    content_length: options.content_length,
                    dl_chunk_size: options.dl_chunk_size,
                    start: options.start,
                    end: options.end,
                    ffmpeg_args: ffmpeg_args.clone(),
                })?)))
            } else {
                Arc::new(Mutex::new(None))
            };

            Ok(Self {
                client,
                link: options.link,
                content_length: options.content_length,
                dl_chunk_size: options.dl_chunk_size,
                start: RwLock::new(options.start),
                end: RwLock::new(options.end),
                start_static: options.start,
                end_static: options.end,
                chunk_lock: Mutex::new(()),
                ffmpeg_args,
                ffmpeg_stream,
            })
        }

        #[cfg(not(feature = "ffmpeg"))]
        {
            Ok(Self {
                client,
                link: options.link,
                content_length: options.content_length,
                dl_chunk_size: options.dl_chunk_size,
                start: RwLock::new(options.start),
                end: RwLock::new(options.end),
                start_static: options.start,
                end_static: options.end,
                chunk_lock: Mutex::new(()),
            })
        }
    }

    pub fn content_length(&self) -> u64 {
        self.content_length
    }

    async fn end_index(&self) -> u64 {
        *self.end.read().await
    }

    async fn start_index(&self) -> u64 {
        *self.start.read().await
    }
}

#[async_trait]
impl Stream for NonLiveStream {
    async fn chunk(&self) -> Result<Option<Bytes>, VideoError> {
        let _chunk_guard = self.chunk_lock.lock().await;

        #[cfg(feature = "ffmpeg")]
        {
            if !self.ffmpeg_args.is_empty() {
                if let Some(ffmpeg_stream) = &mut *self.ffmpeg_stream.lock().await {
                    // notify to start download task
                    ffmpeg_stream.start_download();

                    if let Some(reciever) = ffmpeg_stream.refined_data_reciever.clone() {
                        let mut reciever = reciever.lock().await;

                        let byte_value = reciever.recv().await;

                        // reset ffmpeg_stream for reuse
                        if byte_value.is_none() {
                            *ffmpeg_stream = FFmpegStream::new(FFmpegStreamOptions {
                                client: self.client.clone(),
                                link: self.link.clone(),
                                content_length: self.content_length,
                                dl_chunk_size: self.dl_chunk_size,
                                start: self.start_static,
                                end: self.end_static,
                                ffmpeg_args: self.ffmpeg_args.clone(),
                            })?;
                        }

                        return Ok(byte_value);
                    }
                }
            }
        }

        let start = self.start_index().await;
        if start >= self.content_length {
            let mut end = self.end.write().await;
            let mut start = self.start.write().await;
            *end = self.end_static;
            *start = self.start_static;
            return Ok(None);
        }

        let end = self
            .end_index()
            .await
            .min(self.content_length.saturating_sub(1));

        let mut headers = DEFAULT_HEADERS.clone();

        headers.insert(
            reqwest::header::RANGE,
            format!("bytes={start}-{end}").parse().unwrap(),
        );

        let ua = crate::utils::get_user_agent_for_url(&self.link);
        headers.insert(reqwest::header::USER_AGENT, ua.parse().unwrap());

        let response = self
            .client
            .get(&self.link)
            .headers(headers)
            .send()
            .await
            .map_err(VideoError::ReqwestMiddleware)?;
        let status = response.status();
        let mut response = response.error_for_status().map_err(VideoError::Reqwest)?;
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(VideoError::DownloadError(format!(
                "unexpected HTTP status {status} for ranged media request"
            )));
        }
        let range_was_ignored = status == reqwest::StatusCode::OK;
        let content_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        let mut buf: BytesMut = BytesMut::new();

        while let Some(chunk) = response.chunk().await.map_err(VideoError::Reqwest)? {
            buf.extend(chunk);
        }

        if range_was_ignored {
            let full_body: Bytes = buf.into();
            let full_body_len = u64::try_from(full_body.len()).map_err(|_| {
                VideoError::DownloadError("full response body length does not fit u64".to_string())
            })?;
            if full_body_len != self.content_length || start > full_body_len {
                return Err(VideoError::DownloadError(format!(
                    "server ignored Range but returned {full_body_len} bytes for a {}-byte representation",
                    self.content_length
                )));
            }
            let start_offset = usize::try_from(start).map_err(|_| {
                VideoError::DownloadError("range start does not fit usize".to_string())
            })?;
            let remaining = full_body.slice(start_offset..);

            let mut next_start = self.start.write().await;
            *next_start = self.content_length;
            let mut next_end = self.end.write().await;
            *next_end = self.content_length.saturating_sub(1);

            return Ok(Some(remaining));
        }

        let body: Bytes = buf.into();
        let served_end = if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let content_range = content_range.as_deref().ok_or_else(|| {
                VideoError::DownloadError("partial response was missing Content-Range".to_string())
            })?;
            partial_response_end(content_range, start, end, self.content_length, body.len())?
        } else {
            end
        };

        let mut next_start = self.start.write().await;
        *next_start = served_end.saturating_add(1);
        let mut next_end = self.end.write().await;
        *next_end = served_end.saturating_add(self.dl_chunk_size);

        Ok(Some(body))
    }

    fn content_length(&self) -> usize {
        self.content_length() as usize
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket.read(&mut buffer).await.expect("read range request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("HTTP request must be UTF-8")
    }

    fn range_header(request: &str) -> String {
        request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("range")
                    .then(|| value.trim().to_owned())
            })
            .expect("request must contain Range header")
    }

    async fn respond(mut socket: TcpStream, body: &[u8], content_range: &str) {
        let response = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: {content_range}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write range response headers");
        socket
            .write_all(body)
            .await
            .expect("write range response body");
    }

    #[tokio::test]
    async fn concurrent_chunk_calls_advance_to_distinct_ranges() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local range server");
        let address = listener.local_addr().expect("read local server address");

        let server = tokio::spawn(async move {
            let (mut first_socket, _) =
                listener.accept().await.expect("accept first range request");
            let first_request = read_request(&mut first_socket).await;
            let first_range = range_header(&first_request);

            let second_early =
                tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
            let (second_range, second_socket) = match second_early {
                Ok(Ok((mut second_socket, _))) => {
                    let second_request = read_request(&mut second_socket).await;
                    let second_range = range_header(&second_request);
                    respond(first_socket, b"test", "bytes 0-3/8").await;
                    (second_range, second_socket)
                }
                Ok(Err(error)) => panic!("accept second range request failed: {error}"),
                Err(_) => {
                    respond(first_socket, b"test", "bytes 0-3/8").await;
                    let (mut second_socket, _) = listener
                        .accept()
                        .await
                        .expect("accept serialized second range request");
                    let second_request = read_request(&mut second_socket).await;
                    let second_range = range_header(&second_request);
                    (second_range, second_socket)
                }
            };
            respond(second_socket, b"xyz", "bytes 4-6/8").await;
            [first_range, second_range]
        });

        let stream = Arc::new(
            NonLiveStream::new(NonLiveStreamOptions {
                client: None,
                link: format!("http://{address}/audio"),
                content_length: 8,
                dl_chunk_size: 3,
                start: 0,
                end: 3,
                #[cfg(feature = "ffmpeg")]
                ffmpeg_args: None,
            })
            .expect("construct non-live stream"),
        );

        let first_stream = Arc::clone(&stream);
        let first = tokio::spawn(async move { first_stream.chunk().await });
        let second_stream = Arc::clone(&stream);
        let second = tokio::spawn(async move { second_stream.chunk().await });

        first
            .await
            .expect("first chunk call must not panic")
            .expect("first chunk result")
            .expect("first chunk must contain bytes");
        second
            .await
            .expect("second chunk call must not panic")
            .expect("second chunk result")
            .expect("second chunk must contain bytes");

        let ranges = server.await.expect("range server should join");
        assert_eq!(ranges[0], "bytes=0-3");
        assert_eq!(
            ranges[1], "bytes=4-6",
            "two concurrent callers must not download the same byte range"
        );
    }
}

#[cfg(test)]
mod exact_boundary_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read local range request");
            assert!(read > 0, "client closed before range headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    fn range_header(request: &str) -> Option<String> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("range")
                .then(|| value.trim().to_owned())
        })
    }

    async fn respond(
        socket: &mut TcpStream,
        status: &str,
        body: &[u8],
        content_range: Option<&str>,
    ) {
        let content_range = content_range
            .map(|value| format!("Content-Range: {value}\r\n"))
            .unwrap_or_default();
        let headers = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{content_range}Connection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write local range response headers");
        socket
            .write_all(body)
            .await
            .expect("write local range response body");
    }

    #[tokio::test]
    async fn exact_chunk_boundary_stops_without_request_past_eof() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind exact-boundary range server");
        let address = listener.local_addr().expect("read exact-boundary address");

        let server = tokio::spawn(async move {
            let mut ranges = Vec::new();
            for index in 0..8 {
                let accepted =
                    tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
                let Ok(Ok((mut socket, _))) = accepted else {
                    break;
                };
                let request = read_request(&mut socket).await;
                let range = range_header(&request).expect("NonLive request must contain Range");
                ranges.push(range.clone());
                match (index, range.as_str()) {
                    (0, "bytes=0-2") => {
                        respond(
                            &mut socket,
                            "206 Partial Content",
                            b"abc",
                            Some("bytes 0-2/6"),
                        )
                        .await
                    }
                    (1, "bytes=3-5") => {
                        respond(
                            &mut socket,
                            "206 Partial Content",
                            b"def",
                            Some("bytes 3-5/6"),
                        )
                        .await
                    }
                    _ => respond(&mut socket, "416 Range Not Satisfiable", b"", None).await,
                }
            }
            ranges
        });

        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link: format!("http://{address}/media.bin"),
            content_length: 6,
            dl_chunk_size: 3,
            start: 0,
            end: 2,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct exact-boundary stream");

        assert_eq!(
            stream
                .chunk()
                .await
                .expect("first chunk must succeed")
                .as_deref(),
            Some(b"abc".as_slice())
        );
        assert_eq!(
            stream
                .chunk()
                .await
                .expect("second chunk must succeed")
                .as_deref(),
            Some(b"def".as_slice())
        );
        let third = stream.chunk().await;
        let ranges = server
            .await
            .expect("exact-boundary range server should join");

        assert_eq!(
            ranges,
            vec!["bytes=0-2", "bytes=3-5"],
            "no request may start at or beyond content_length"
        );
        assert!(
            matches!(third, Ok(None)),
            "stream aligned exactly to content length must terminate cleanly; got {third:?}"
        );
    }
}

#[cfg(test)]
mod one_byte_chunk_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read one-byte range request");
            assert!(
                read > 0,
                "client closed before one-byte request headers completed"
            );
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("one-byte HTTP request must be UTF-8")
    }

    fn range_header(request: &str) -> Option<String> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("range")
                .then(|| value.trim().to_owned())
        })
    }

    #[tokio::test]
    async fn one_byte_chunk_size_downloads_all_bytes_then_eof() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind one-byte range server");
        let address = listener.local_addr().expect("read one-byte server address");

        let server = tokio::spawn(async move {
            let mut ranges = Vec::new();
            for index in 0..4 {
                let accepted =
                    tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
                let Ok(Ok((mut socket, _))) = accepted else {
                    break;
                };
                let request = read_request(&mut socket).await;
                let range = range_header(&request).expect("one-byte request must contain Range");
                ranges.push(range);
                let body = match index {
                    0 => b"a".as_slice(),
                    1 => b"b".as_slice(),
                    2 => b"c".as_slice(),
                    _ => b"x".as_slice(),
                };
                let response = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {index}-{index}/3\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("write one-byte response headers");
                socket
                    .write_all(body)
                    .await
                    .expect("write one-byte response body");
            }
            ranges
        });

        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link: format!("http://{address}/media.bin"),
            content_length: 3,
            dl_chunk_size: 1,
            start: 0,
            end: 0,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct one-byte stream");

        let first = stream
            .chunk()
            .await
            .expect("first one-byte chunk call must succeed");
        let second = stream
            .chunk()
            .await
            .expect("second one-byte chunk call must succeed");
        let third = stream
            .chunk()
            .await
            .expect("third one-byte chunk call must succeed");
        let eof = stream.chunk().await.expect("EOF call must succeed");
        let ranges = server.await.expect("one-byte range server should join");

        assert_eq!(
            ranges,
            vec!["bytes=0-0", "bytes=1-1", "bytes=2-2"],
            "chunk size 1 must advance one byte at a time without an extra request"
        );
        assert_eq!(first.as_deref(), Some(b"a".as_slice()));
        assert_eq!(second.as_deref(), Some(b"b".as_slice()));
        assert_eq!(third.as_deref(), Some(b"c".as_slice()));
        assert!(eof.is_none(), "stream must terminate after the third byte");
    }
}

#[cfg(test)]
mod ignored_range_response_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read range-ignored request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    fn range_header(request: &str) -> String {
        request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("range")
                    .then(|| value.trim().to_owned())
            })
            .expect("request must contain Range")
    }

    async fn write_response(
        socket: &mut TcpStream,
        status: &str,
        body: &[u8],
        content_range: Option<&str>,
    ) {
        let content_range = content_range
            .map(|value| format!("Content-Range: {value}\r\n"))
            .unwrap_or_default();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{content_range}Connection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write response headers");
        socket.write_all(body).await.expect("write response body");
    }

    #[tokio::test]
    async fn range_ignored_200_full_body_is_emitted_once() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind range-ignored server");
        let address = listener.local_addr().expect("read range-ignored address");

        let server = tokio::spawn(async move {
            let mut ranges = Vec::new();
            for _ in 0..2 {
                let accepted =
                    tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
                let Ok(Ok((mut socket, _))) = accepted else {
                    break;
                };
                ranges.push(range_header(&read_request(&mut socket).await));
                write_response(&mut socket, "200 OK", b"abcdef", None).await;
            }
            ranges
        });

        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link: format!("http://{address}/media.bin"),
            content_length: 6,
            dl_chunk_size: 3,
            start: 0,
            end: 2,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct range-ignored stream");

        let first = stream.chunk().await.expect("first chunk must succeed");
        let second = stream
            .chunk()
            .await
            .expect("second chunk must terminate cleanly");
        let ranges = server.await.expect("range-ignored server should join");

        assert_eq!(first.as_deref(), Some(b"abcdef".as_slice()));
        assert!(
            second.is_none(),
            "full 200 representation must not be emitted twice"
        );
        assert_eq!(
            ranges,
            vec!["bytes=0-2"],
            "no second request is needed after a full 200 response"
        );
    }

    #[tokio::test]
    async fn range_ignored_after_partial_response_emits_only_missing_suffix() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mixed range server");
        let address = listener.local_addr().expect("read mixed range address");

        let server = tokio::spawn(async move {
            let mut ranges = Vec::new();
            for index in 0..3 {
                let accepted =
                    tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
                let Ok(Ok((mut socket, _))) = accepted else {
                    break;
                };
                ranges.push(range_header(&read_request(&mut socket).await));
                if index == 0 {
                    write_response(
                        &mut socket,
                        "206 Partial Content",
                        b"abc",
                        Some("bytes 0-2/6"),
                    )
                    .await;
                } else {
                    write_response(&mut socket, "200 OK", b"abcdef", None).await;
                }
            }
            ranges
        });

        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link: format!("http://{address}/media.bin"),
            content_length: 6,
            dl_chunk_size: 3,
            start: 0,
            end: 2,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct mixed range stream");

        let first = stream.chunk().await.expect("partial chunk must succeed");
        let second = stream
            .chunk()
            .await
            .expect("ignored-range fallback must succeed");
        let eof = stream.chunk().await.expect("stream must terminate cleanly");
        let ranges = server.await.expect("mixed range server should join");

        assert_eq!(first.as_deref(), Some(b"abc".as_slice()));
        assert_eq!(
            second.as_deref(),
            Some(b"def".as_slice()),
            "already-emitted prefix must be removed from a later full 200 response"
        );
        assert!(eof.is_none());
        assert_eq!(ranges, vec!["bytes=0-2", "bytes=3-5"]);
    }
}

#[cfg(test)]
mod short_partial_response_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use bytes::BytesMut;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read short-partial request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    fn range_header(request: &str) -> String {
        request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("range")
                    .then(|| value.trim().to_owned())
            })
            .expect("request must contain Range")
    }

    async fn write_partial(socket: &mut TcpStream, content_range: &str, body: &[u8]) {
        let response = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: {content_range}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write short-partial response headers");
        socket
            .write_all(body)
            .await
            .expect("write short-partial response body");
    }

    #[tokio::test]
    async fn short_partial_response_resumes_from_served_end() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind short-partial server");
        let address = listener.local_addr().expect("read short-partial address");

        let server = tokio::spawn(async move {
            let mut ranges = Vec::new();
            for index in 0..3 {
                let accepted =
                    tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
                let Ok(Ok((mut socket, _))) = accepted else {
                    break;
                };
                let range = range_header(&read_request(&mut socket).await);
                ranges.push(range.clone());
                match (index, range.as_str()) {
                    (0, "bytes=0-4") => {
                        write_partial(&mut socket, "bytes 0-1/6", b"ab").await;
                    }
                    (_, "bytes=2-5") => {
                        write_partial(&mut socket, "bytes 2-5/6", b"cdef").await;
                    }
                    (_, "bytes=5-5") => {
                        write_partial(&mut socket, "bytes 5-5/6", b"f").await;
                    }
                    _ => {
                        write_partial(&mut socket, "bytes 0-0/6", b"?").await;
                    }
                }
            }
            ranges
        });

        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link: format!("http://{address}/media.bin"),
            content_length: 6,
            dl_chunk_size: 5,
            start: 0,
            end: 4,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct short-partial stream");

        let mut output = BytesMut::new();
        for _ in 0..3 {
            match stream
                .chunk()
                .await
                .expect("short-partial chunk call must succeed")
            {
                Some(chunk) => output.extend_from_slice(&chunk),
                None => break,
            }
        }
        let ranges = server.await.expect("short-partial server should join");

        assert_eq!(
            ranges,
            vec!["bytes=0-4", "bytes=2-5"],
            "next request must resume after the last byte actually served, not after the originally requested end"
        );
        assert_eq!(
            output.as_ref(),
            b"abcdef",
            "short 206 responses must not cause bytes in the requested range to be skipped"
        );
    }
}

#[cfg(test)]
mod zero_chunk_size_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    fn options(link: String) -> NonLiveStreamOptions {
        NonLiveStreamOptions {
            client: None,
            link,
            content_length: 3,
            dl_chunk_size: 0,
            start: 0,
            end: 0,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        }
    }

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read zero-chunk request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    fn range_header(request: &str) -> String {
        request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("range")
                    .then(|| value.trim().to_owned())
            })
            .expect("request must contain Range")
    }

    fn range_is_non_descending(range: &str) -> bool {
        let Some(bounds) = range.strip_prefix("bytes=") else {
            return false;
        };
        let Some((start, end)) = bounds.split_once('-') else {
            return false;
        };
        let (Ok(start), Ok(end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
            return false;
        };
        start <= end
    }

    #[test]
    fn zero_chunk_size_is_rejected() {
        let result = NonLiveStream::new(options("http://127.0.0.1:9/media.bin".to_string()));
        assert!(
            result.is_err(),
            "a zero maximum chunk size must be rejected instead of constructing a stream with a non-advancing range end"
        );
    }

    #[tokio::test]
    async fn zero_chunk_size_never_emits_descending_range() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind zero-chunk server");
        let address = listener.local_addr().expect("read zero-chunk address");

        let server = tokio::spawn(async move {
            let mut ranges = Vec::new();
            for _ in 0..2 {
                let accepted =
                    tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
                let Ok(Ok((mut socket, _))) = accepted else {
                    break;
                };
                let range = range_header(&read_request(&mut socket).await);
                ranges.push(range);
                socket
                    .write_all(
                        b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nConnection: close\r\n\r\na",
                    )
                    .await
                    .expect("write zero-chunk response");
            }
            ranges
        });

        let stream = match NonLiveStream::new(options(format!("http://{address}/media.bin"))) {
            Ok(stream) => stream,
            Err(_) => return,
        };

        let _ = stream.chunk().await;
        let _ = stream.chunk().await;
        let ranges = server.await.expect("zero-chunk server should join");

        assert!(
            ranges.iter().all(|range| range_is_non_descending(range)),
            "zero chunk size must never produce a descending HTTP byte range; observed {ranges:?}"
        );
    }
}

#[cfg(test)]
mod content_range_total_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read content-range request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    async fn serve_once(content_range_total: u64) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind content-range server");
        let address = listener.local_addr().expect("read content-range address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept range request");
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("Range: bytes=0-2")),
                "client must request the expected range; got {request:?}"
            );
            let response = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 0-2/{content_range_total}\r\nConnection: close\r\n\r\nabc"
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write partial response");
        });
        (format!("http://{address}/media.bin"), server)
    }

    #[tokio::test]
    async fn partial_content_total_mismatch_is_rejected() {
        let (link, server) = serve_once(4).await;
        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link,
            content_length: 3,
            dl_chunk_size: 3,
            start: 0,
            end: 2,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct mismatch stream");

        let result = stream.chunk().await;
        server.await.expect("mismatch server should join");

        assert!(
            result.is_err(),
            "a 206 response for a different complete representation length must be rejected; got {result:?}"
        );
    }

    #[tokio::test]
    async fn matching_partial_content_total_remains_accepted() {
        let (link, server) = serve_once(3).await;
        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link,
            content_length: 3,
            dl_chunk_size: 3,
            start: 0,
            end: 2,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct matching stream");

        let result = stream.chunk().await;
        server.await.expect("matching server should join");

        assert_eq!(
            result
                .expect("matching representation length must succeed")
                .as_deref(),
            Some(b"abc".as_slice())
        );
    }
}

#[cfg(test)]
mod ignored_range_length_mismatch_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read ignored-range mismatch request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    #[tokio::test]
    async fn ignored_range_full_body_length_mismatch_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ignored-range mismatch server");
        let address = listener
            .local_addr()
            .expect("read ignored-range mismatch address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept range request");
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("Range: bytes=0-2")),
                "client must request the expected range; got {request:?}"
            );
            let body = b"abcd";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write ignored-range mismatch headers");
            socket
                .write_all(body)
                .await
                .expect("write ignored-range mismatch body");
        });

        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link: format!("http://{address}/media.bin"),
            content_length: 3,
            dl_chunk_size: 3,
            start: 0,
            end: 2,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct ignored-range mismatch stream");

        let result = stream.chunk().await;
        server
            .await
            .expect("ignored-range mismatch server should join");

        assert!(
            result.is_err(),
            "a full 200 representation whose length conflicts with known metadata must be rejected; got {result:?}"
        );
    }
}

#[cfg(test)]
mod missing_content_range_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read missing Content-Range request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    #[tokio::test]
    async fn partial_content_without_content_range_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind missing Content-Range server");
        let address = listener
            .local_addr()
            .expect("read missing Content-Range address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept range request");
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("Range: bytes=3-5")),
                "client must request the expected range; got {request:?}"
            );
            let body = b"abc";
            let response = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write missing Content-Range headers");
            socket
                .write_all(body)
                .await
                .expect("write missing Content-Range body");
        });

        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link: format!("http://{address}/media.bin"),
            content_length: 6,
            dl_chunk_size: 3,
            start: 3,
            end: 5,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct missing Content-Range stream");

        let result = stream.chunk().await;
        server
            .await
            .expect("missing Content-Range server should join");

        assert!(
            result.is_err(),
            "single-part HTTP 206 without Content-Range must be rejected because the returned byte offsets cannot be identified; got {result:?}"
        );
    }
}

#[cfg(test)]
mod unexpected_success_status_tests {
    use super::{NonLiveStream, NonLiveStreamOptions, Stream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read unexpected-status request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    #[tokio::test]
    async fn unexpected_success_status_does_not_advance_range() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind unexpected-status server");
        let address = listener
            .local_addr()
            .expect("read unexpected-status address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept range request");
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("Range: bytes=0-2")),
                "client must request the expected range; got {request:?}"
            );
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .expect("write 204 response");
        });

        let stream = NonLiveStream::new(NonLiveStreamOptions {
            client: None,
            link: format!("http://{address}/media.bin"),
            content_length: 6,
            dl_chunk_size: 3,
            start: 0,
            end: 2,
            #[cfg(feature = "ffmpeg")]
            ffmpeg_args: None,
        })
        .expect("construct unexpected-status stream");

        let result = stream.chunk().await;
        server.await.expect("unexpected-status server should join");

        assert!(
            result.is_err(),
            "a ranged GET may only be consumed as a full 200 or partial 206 representation; 204 must not be treated as an empty successful chunk or advance the byte cursor; got {result:?}"
        );
    }
}
