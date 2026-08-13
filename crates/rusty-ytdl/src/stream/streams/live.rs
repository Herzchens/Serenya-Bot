use crate::constants::{DEFAULT_HEADERS, DEFAULT_MAX_RETRIES};
use crate::stream::{
    encryption::Encryption, media_format::MediaFormat, remote_data::RemoteData, segment::Segment,
    streams::Stream,
};
use crate::structs::{CustomRetryableStrategy, VideoError};
use crate::utils::{get_html, make_absolute_url};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use m3u8_rs::{parse_media_playlist, ByteRange};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};

pub struct LiveStreamOptions {
    pub client: Option<reqwest_middleware::ClientWithMiddleware>,
    pub stream_url: String,
}

pub struct LiveStream {
    client: reqwest_middleware::ClientWithMiddleware,
    stream_url: String,

    last_refresh: RwLock<u128>,
    segments: RwLock<Vec<(Segment, Encryption)>>,
    is_end: RwLock<bool>,
    last_seg: RwLock<Option<(u64, u64)>>,
    emitted_initialization: RwLock<Option<RemoteData>>,
    chunk_lock: Mutex<()>,
}

fn elapsed_since(last_refresh: u128, current_time: u128) -> u128 {
    current_time.saturating_sub(last_refresh)
}

fn normalize_media_byte_range(
    uri: &url::Url,
    byte_range: Option<&ByteRange>,
    previous: &mut Option<(url::Url, u64)>,
) -> Result<Option<ByteRange>, VideoError> {
    let Some(range) = byte_range else {
        *previous = None;
        return Ok(None);
    };

    if range.length == 0 {
        return Err(VideoError::M3U8ParseError(
            "HLS byte range length must be greater than zero".to_string(),
        ));
    }

    let start = match range.offset {
        Some(offset) => offset,
        None => {
            let (previous_uri, previous_end) = previous.as_ref().ok_or_else(|| {
                VideoError::M3U8ParseError(
                    "implicit HLS byte range has no previous sub-range".to_string(),
                )
            })?;
            if previous_uri != uri {
                return Err(VideoError::M3U8ParseError(
                    "implicit HLS byte range changed media resource".to_string(),
                ));
            }
            previous_end.checked_add(1).ok_or_else(|| {
                VideoError::M3U8ParseError("implicit HLS byte range offset overflow".to_string())
            })?
        }
    };

    let end = start
        .checked_add(range.length - 1)
        .ok_or_else(|| VideoError::M3U8ParseError("HLS byte range end overflow".to_string()))?;
    *previous = Some((uri.clone(), end));

    Ok(Some(ByteRange {
        length: range.length,
        offset: Some(start),
    }))
}

impl LiveStream {
    pub fn new(options: LiveStreamOptions) -> Result<Self, VideoError> {
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

        Ok(Self {
            client,
            stream_url: options.stream_url,
            last_refresh: RwLock::new(0),
            segments: RwLock::new(vec![]),
            is_end: RwLock::new(false),
            last_seg: RwLock::new(None),
            emitted_initialization: RwLock::new(None),
            chunk_lock: Mutex::new(()),
        })
    }

    async fn last_refresh(&self) -> u128 {
        *self.last_refresh.read().await
    }

    async fn segments(&self) -> Vec<(Segment, Encryption)> {
        (*self.segments.read().await).clone()
    }

    async fn is_end(&self) -> bool {
        *self.is_end.read().await
    }

    async fn last_seg(&self) -> Option<(u64, u64)> {
        *self.last_seg.read().await
    }

    async fn refresh_playlist(&self) -> Result<(), VideoError> {
        let body = get_html(&self.client, &self.stream_url, None).await?;

        let media_playlist = parse_media_playlist(body.as_bytes())
            .map_err(|e| VideoError::M3U8ParseError(e.to_string()))?
            .1;

        let mut cur_init = None;
        let mut previous_media_range = None;

        // Loop through media segments
        let mut discon_offset = 0;
        let mut encryption = Encryption::None;
        for (seq, segment) in (media_playlist.media_sequence..).zip(media_playlist.segments.iter())
        {
            // Calculate segment discontinuity
            if segment.discontinuity {
                discon_offset += 1;
            }
            let discon_seq = media_playlist.discontinuity_sequence + discon_offset;

            // Resolve the media resource before normalizing EXT-X-BYTERANGE state.
            // Equivalent relative spellings (for example `media.bin` and `./media.bin`)
            // must compare by the resource they resolve to, not by their raw playlist text.
            // This still happens before skipping an already-downloaded segment because a
            // sliding playlist can need that segment to establish the next implicit offset.
            let byte_range = if segment.byte_range.is_some() {
                let range_resource = make_absolute_url(&self.stream_url, &segment.uri)?;
                normalize_media_byte_range(
                    &range_resource,
                    segment.byte_range.as_ref(),
                    &mut previous_media_range,
                )?
            } else {
                previous_media_range = None;
                None
            };

            // Skip segment if already downloaded
            if let Some(s) = self.last_seg().await {
                if s >= (discon_seq, seq) {
                    continue;
                }
            }

            // Check encryption
            if let Some(key) = &segment.key {
                encryption = Encryption::new(key, &self.stream_url, seq).await?;
            }

            // Parse URL before committing sequence progress. A malformed segment must remain
            // retryable on the next playlist refresh instead of being marked as consumed.
            let seg_url = make_absolute_url(&self.stream_url, &segment.uri)?;

            // Make Initialization
            let init = if let Some(map) = &segment.map {
                let init = RemoteData::new(
                    make_absolute_url(&self.stream_url, &map.uri)?,
                    map.byte_range.clone(),
                );
                cur_init = Some(init.clone());
                Some(init)
            } else {
                cur_init.clone()
            };

            let segment = Segment {
                data: RemoteData::new(seg_url, byte_range),
                discon_seq,
                seq,
                format: MediaFormat::Unknown,
                initialization: init,
            };

            // if segments already in segment vector skip it
            if !self
                .segments()
                .await
                .iter()
                .any(|x| (x.0.discon_seq, x.0.seq) == (segment.discon_seq, segment.seq))
            {
                let mut segment_vector = self.segments.write().await;
                segment_vector.push((segment.clone(), encryption.clone()));
            }

            // Only advance after every fallible part of segment construction succeeded.
            let mut last_seg = self.last_seg.write().await;
            *last_seg = Some((discon_seq, seq));
        }

        // Set last refresh to check refresh playlist functionality
        let mut last_refresh = self.last_refresh.write().await;
        let start = SystemTime::now();
        *last_refresh = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();
        drop(last_refresh);

        // Set is_end bool to control chunk function
        // if stream ended
        if media_playlist.end_list {
            let mut is_end = self.is_end.write().await;
            *is_end = media_playlist.end_list;
        }

        Ok(())
    }
}

#[async_trait]
impl Stream for LiveStream {
    async fn chunk(&self) -> Result<Option<Bytes>, VideoError> {
        let _chunk_guard = self.chunk_lock.lock().await;
        let segments = self.segments().await;

        // if stream end and no segments left end it
        if self.is_end().await && segments.is_empty() {
            return Ok(None);
        }

        let live_seconds = 20000; // refresh millis

        let start = SystemTime::now();
        let current_time = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        let sleep_time = elapsed_since(self.last_refresh().await, current_time);

        // Sleep until to wait new segments uploaded to get new segments
        if sleep_time < live_seconds && segments.is_empty() && !self.is_end().await {
            tokio::time::sleep_until(
                tokio::time::Instant::now()
                    + Duration::from_millis((live_seconds - sleep_time) as u64),
            )
            .await;
        }

        let start = SystemTime::now();
        let current_time = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        // if last refresh bigger than live_seconds refresh playlist
        if elapsed_since(self.last_refresh().await, current_time) >= live_seconds
            && !self.is_end().await
        {
            self.refresh_playlist().await?;
        }

        // cannot get any segments return empty buffer array
        let segments = self.segments().await;
        if segments.is_empty() {
            return Ok(Some(Bytes::new()));
        }

        let first_segment = segments.first().unwrap();

        // EXT-X-MAP is the initialization section for the media segments that follow it.
        // Fetch a changed map before the media request, but do not commit it as emitted
        // until the whole chunk succeeds so a failed media download remains retryable.
        let pending_initialization = if let Some(initialization) = &first_segment.0.initialization {
            let already_emitted = self.emitted_initialization.read().await;
            let needs_initialization = already_emitted.as_ref() != Some(initialization);
            drop(already_emitted);

            if needs_initialization {
                let (bytes, _) = initialization.fetch(&self.client).await?;
                Some((initialization.clone(), bytes))
            } else {
                None
            }
        } else {
            None
        };

        let requested_byte_range = first_segment.0.data.byte_range_bounds();
        let mut headers = DEFAULT_HEADERS.clone();
        if let Some(range) = first_segment.0.data.byte_range_string() {
            let range = range
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|error| {
                    VideoError::DownloadError(format!("Invalid HLS byte range: {error}"))
                })?;
            headers.insert(reqwest::header::RANGE, range);
        }
        let ua = crate::utils::get_user_agent_for_url(first_segment.0.url().as_str());
        headers.insert(reqwest::header::USER_AGENT, ua.parse().unwrap());

        let response = self
            .client
            .get(first_segment.0.url().as_str())
            .headers(headers)
            .send()
            .await
            .map_err(VideoError::ReqwestMiddleware)?;
        let status = response.status();
        let mut response = response.error_for_status().map_err(VideoError::Reqwest)?;
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(VideoError::DownloadError(format!(
                "unexpected HTTP status {status} for HLS media segment"
            )));
        }

        let mut buf: BytesMut = BytesMut::new();

        while let Some(chunk) = response.chunk().await.map_err(VideoError::Reqwest)? {
            buf.extend(chunk);
        }

        // A server is allowed to ignore Range and return the complete representation with
        // HTTP 200. EXT-X-BYTERANGE still defines only the requested sub-range as this
        // media segment, so recover that sub-range from the complete body before decrypting.
        if status == reqwest::StatusCode::OK {
            if let Some((range_start, range_length)) = requested_byte_range {
                let range_start = usize::try_from(range_start).map_err(|_| {
                    VideoError::DownloadError(
                        "HLS byte-range start does not fit this platform".to_string(),
                    )
                })?;
                let range_length = usize::try_from(range_length).map_err(|_| {
                    VideoError::DownloadError(
                        "HLS byte-range length does not fit this platform".to_string(),
                    )
                })?;
                let range_end = range_start.checked_add(range_length).ok_or_else(|| {
                    VideoError::DownloadError("HLS byte-range end overflow".to_string())
                })?;
                if range_end > buf.len() {
                    return Err(VideoError::DownloadError(format!(
                        "server ignored HLS Range but returned only {} bytes for requested bytes={}-{}",
                        buf.len(),
                        range_start,
                        range_end.saturating_sub(1)
                    )));
                }
                buf = BytesMut::from(&buf[range_start..range_end]);
            }
        }

        // Decrypt data bytes
        buf = BytesMut::from_iter(first_segment.1.decrypt(&self.client, &buf).await?);

        let mut output = BytesMut::new();
        if let Some((initialization, bytes)) = pending_initialization {
            output.extend_from_slice(&bytes);
            *self.emitted_initialization.write().await = Some(initialization);
        }
        output.extend_from_slice(&buf);

        // Delete downloaded segment from segments array only after both initialization and
        // media processing succeeded.
        let mut segment_vector = self.segments.write().await;
        segment_vector.remove(0);

        Ok(Some(output.into()))
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::{
        Encryption, LiveStream, LiveStreamOptions, MediaFormat, RemoteData, Segment, Stream,
    };
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn respond(mut socket: tokio::net::TcpStream) {
        let mut request = [0_u8; 1024];
        let _ = socket
            .read(&mut request)
            .await
            .expect("read segment request");
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest")
            .await
            .expect("write segment response");
    }

    #[tokio::test]
    async fn concurrent_chunk_calls_do_not_consume_the_same_segment_twice() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local segment server");
        let address = listener.local_addr().expect("read local server address");

        let server = tokio::spawn(async move {
            let (first_socket, _) = listener
                .accept()
                .await
                .expect("accept first segment request");
            let second = tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
            match second {
                Ok(Ok((second_socket, _))) => {
                    let ((), ()) = tokio::join!(respond(first_socket), respond(second_socket));
                }
                Ok(Err(error)) => panic!("accept second segment request failed: {error}"),
                Err(_) => respond(first_socket).await,
            }
        });

        let stream = Arc::new(
            LiveStream::new(LiveStreamOptions {
                client: None,
                stream_url: format!("http://{address}/playlist.m3u8"),
            })
            .expect("construct live stream"),
        );
        let segment_url = url::Url::parse(&format!("http://{address}/segment.ts"))
            .expect("parse local segment URL");
        stream.segments.write().await.push((
            Segment {
                data: RemoteData::new(segment_url, None),
                discon_seq: 0,
                seq: 1,
                format: MediaFormat::Unknown,
                initialization: None,
            },
            Encryption::None,
        ));
        *stream.last_refresh.write().await = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis();
        *stream.is_end.write().await = true;

        let first_stream = Arc::clone(&stream);
        let first = tokio::spawn(async move { first_stream.chunk().await });
        let second_stream = Arc::clone(&stream);
        let second = tokio::spawn(async move { second_stream.chunk().await });

        let first = first
            .await
            .expect("first chunk call must not panic")
            .expect("first chunk result");
        let second = second
            .await
            .expect("second chunk call must not panic")
            .expect("second chunk result");
        server.await.expect("local segment server should join");

        let delivered = usize::from(first.is_some()) + usize::from(second.is_some());
        assert_eq!(
            delivered, 1,
            "one queued segment must be delivered exactly once"
        );
    }
}

#[cfg(test)]
mod clock_tests {
    use super::elapsed_since;

    #[test]
    fn clock_rollback_does_not_underflow_live_refresh_elapsed_time() {
        assert_eq!(elapsed_since(20_001, 20_000), 0);
        assert_eq!(elapsed_since(20_000, 20_001), 1);
    }
}

#[cfg(test)]
mod refresh_failure_tests {
    use super::{LiveStream, LiveStreamOptions};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn failed_segment_url_does_not_advance_last_sequence() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local playlist server");
        let address = listener.local_addr().expect("read local server address");
        let body = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:10,\nhttp://[::1\n";

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept playlist request");
            let mut request = [0_u8; 1024];
            let _ = socket
                .read(&mut request)
                .await
                .expect("read playlist request");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .await
                .expect("write playlist headers");
            socket
                .write_all(body.as_bytes())
                .await
                .expect("write playlist body");
        });

        let stream = LiveStream::new(LiveStreamOptions {
            client: None,
            stream_url: format!("http://{address}/playlist.m3u8"),
        })
        .expect("construct live stream");

        let result = stream.refresh_playlist().await;
        server.await.expect("playlist server should join");
        assert!(
            result.is_err(),
            "malformed segment URL must reject the refresh"
        );
        assert!(
            stream.last_seg().await.is_none(),
            "a segment that failed URL parsing was never queued and must remain retryable"
        );
    }
}

#[cfg(test)]
mod byte_range_request_tests {
    use super::{
        Encryption, LiveStream, LiveStreamOptions, MediaFormat, RemoteData, Segment, Stream,
    };
    use m3u8_rs::ByteRange;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn run_chunk(range: Option<ByteRange>) -> (bytes::Bytes, String) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local byte-range server");
        let address = listener
            .local_addr()
            .expect("read local byte-range server address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept segment request");
            let mut request = [0_u8; 4096];
            let read = socket
                .read(&mut request)
                .await
                .expect("read segment request");
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            let response: &[u8] = if request
                .to_ascii_lowercase()
                .contains("\r\nrange: bytes=10-13\r\n")
            {
                b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 10-13/18\r\nConnection: close\r\n\r\npart"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\npart"
            };
            socket
                .write_all(response)
                .await
                .expect("write segment response");
            request
        });

        let stream = LiveStream::new(LiveStreamOptions {
            client: None,
            stream_url: format!("http://{address}/playlist.m3u8"),
        })
        .expect("construct live stream");
        let segment_url =
            url::Url::parse(&format!("http://{address}/media.bin")).expect("parse local media URL");
        stream.segments.write().await.push((
            Segment {
                data: RemoteData::new(segment_url, range),
                discon_seq: 0,
                seq: 1,
                format: MediaFormat::Unknown,
                initialization: None,
            },
            Encryption::None,
        ));
        *stream.last_refresh.write().await = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis();
        *stream.is_end.write().await = true;

        let body = stream
            .chunk()
            .await
            .expect("chunk request should succeed")
            .expect("queued segment should produce bytes");
        let request = server.await.expect("local byte-range server should join");
        (body, request)
    }

    #[tokio::test]
    async fn live_chunk_without_byte_range_omits_range_header() {
        let (body, request) = run_chunk(None).await;
        assert_eq!(&body[..], b"part");
        assert!(
            !request.to_ascii_lowercase().contains("\r\nrange:"),
            "ordinary HLS segments must not invent a byte-range request"
        );
    }

    #[tokio::test]
    async fn live_chunk_sends_range_header_for_byte_ranged_segment() {
        let (body, request) = run_chunk(Some(ByteRange {
            length: 4,
            offset: Some(10),
        }))
        .await;
        assert_eq!(&body[..], b"part");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("\r\nrange: bytes=10-13\r\n"),
            "EXT-X-BYTERANGE media must be fetched with its declared HTTP Range"
        );
    }
}

#[cfg(test)]
mod implicit_byte_range_tests {
    use super::{normalize_media_byte_range, LiveStream, LiveStreamOptions, Stream};
    use m3u8_rs::ByteRange;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = [0_u8; 4096];
        let read = socket.read(&mut request).await.expect("read local request");
        String::from_utf8_lossy(&request[..read]).into_owned()
    }

    async fn respond(socket: &mut TcpStream, body: &[u8]) {
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write response headers");
        socket.write_all(body).await.expect("write response body");
    }

    async fn respond_partial(socket: &mut TcpStream, content_range: &str, body: &[u8]) {
        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: {content_range}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write partial response headers");
        socket
            .write_all(body)
            .await
            .expect("write partial response body");
    }

    async fn capture_two_ranges(second_byterange: &'static str) -> (String, String) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local HLS server");
        let address = listener.local_addr().expect("read local HLS address");
        let playlist = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:10,\n#EXT-X-BYTERANGE:4@10\nmedia.bin\n#EXTINF:10,\n#EXT-X-BYTERANGE:{second_byterange}\nmedia.bin\n#EXT-X-ENDLIST\n"
        );

        let server = tokio::spawn(async move {
            let (mut playlist_socket, _) =
                listener.accept().await.expect("accept playlist request");
            let _ = read_request(&mut playlist_socket).await;
            respond(&mut playlist_socket, playlist.as_bytes()).await;

            let (mut first_socket, _) =
                listener.accept().await.expect("accept first media request");
            let first_request = read_request(&mut first_socket).await;
            respond_partial(&mut first_socket, "bytes 10-13/18", b"one!").await;

            let (mut second_socket, _) = listener
                .accept()
                .await
                .expect("accept second media request");
            let second_request = read_request(&mut second_socket).await;
            respond_partial(&mut second_socket, "bytes 14-17/18", b"two!").await;

            (first_request, second_request)
        });

        let stream = LiveStream::new(LiveStreamOptions {
            client: None,
            stream_url: format!("http://{address}/playlist.m3u8"),
        })
        .expect("construct live stream");
        stream
            .refresh_playlist()
            .await
            .expect("local playlist must parse and refresh");

        assert_eq!(
            stream
                .chunk()
                .await
                .expect("first chunk request")
                .as_deref(),
            Some(b"one!".as_slice())
        );
        assert_eq!(
            stream
                .chunk()
                .await
                .expect("second chunk request")
                .as_deref(),
            Some(b"two!".as_slice())
        );

        server.await.expect("local HLS server should join")
    }

    #[tokio::test]
    async fn explicit_byte_range_offsets_remain_independent_control() {
        let (first, second) = capture_two_ranges("4@14").await;
        assert!(
            first
                .to_ascii_lowercase()
                .contains("\r\nrange: bytes=10-13\r\n"),
            "first explicit byte range must be preserved"
        );
        assert!(
            second
                .to_ascii_lowercase()
                .contains("\r\nrange: bytes=14-17\r\n"),
            "second explicit byte range must be preserved"
        );
    }

    #[tokio::test]
    async fn implicit_byte_range_continues_after_previous_subrange() {
        let (first, second) = capture_two_ranges("4").await;
        assert!(
            first
                .to_ascii_lowercase()
                .contains("\r\nrange: bytes=10-13\r\n"),
            "first explicit byte range must be preserved"
        );
        assert!(
            second
                .to_ascii_lowercase()
                .contains("\r\nrange: bytes=14-17\r\n"),
            "an omitted EXT-X-BYTERANGE offset must continue immediately after the previous sub-range"
        );
    }

    #[test]
    fn implicit_byte_range_accepts_equivalent_resolved_resource_uri() {
        let base = "http://example.invalid/path/playlist.m3u8";
        let first = crate::utils::make_absolute_url(base, "media.bin")
            .expect("first relative URI must resolve");
        let second = crate::utils::make_absolute_url(base, "./media.bin")
            .expect("equivalent relative URI must resolve");
        assert_eq!(
            first, second,
            "control: both spellings identify the same resolved media resource"
        );

        let mut previous = None;
        let explicit = ByteRange {
            length: 4,
            offset: Some(10),
        };
        normalize_media_byte_range(&first, Some(&explicit), &mut previous)
            .expect("explicit predecessor range must normalize");

        let implicit = ByteRange {
            length: 4,
            offset: None,
        };
        let normalized = normalize_media_byte_range(&second, Some(&implicit), &mut previous);
        assert!(
            normalized.is_ok(),
            "equivalent URI spellings resolve to the same media resource and must continue the byte range"
        );
        assert_eq!(
            normalized
                .expect("equivalent resource must normalize")
                .expect("implicit range should remain present")
                .offset,
            Some(14)
        );
    }

    #[test]
    fn implicit_byte_range_rejects_a_different_resource() {
        let mut previous = None;
        let first = url::Url::parse("http://example.invalid/first.bin")
            .expect("first control URL must parse");
        let second = url::Url::parse("http://example.invalid/second.bin")
            .expect("second control URL must parse");
        let explicit = ByteRange {
            length: 4,
            offset: Some(10),
        };
        normalize_media_byte_range(&first, Some(&explicit), &mut previous)
            .expect("explicit control range must normalize");

        let implicit = ByteRange {
            length: 4,
            offset: None,
        };
        assert!(
            normalize_media_byte_range(&second, Some(&implicit), &mut previous).is_err(),
            "implicit byte ranges may only continue on the same media resource"
        );
    }
}

#[cfg(test)]
mod initialization_map_tests {
    use super::{
        Encryption, LiveStream, LiveStreamOptions, MediaFormat, RemoteData, Segment, Stream,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read local HLS request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    fn request_path(request: &str) -> String {
        request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request line must contain a path")
            .to_owned()
    }

    async fn respond(socket: &mut TcpStream, body: &[u8]) {
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write local HLS response headers");
        socket
            .write_all(body)
            .await
            .expect("write local HLS response body");
    }

    async fn run_one_segment(with_map: bool) -> (bytes::Bytes, Vec<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local HLS initialization server");
        let address = listener
            .local_addr()
            .expect("read local HLS initialization address");
        let map_line = if with_map {
            "#EXT-X-MAP:URI=\"init.mp4\"\n"
        } else {
            ""
        };
        let playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:1\n{map_line}#EXTINF:10,\nmedia.m4s\n#EXT-X-ENDLIST\n"
        );

        let server = tokio::spawn(async move {
            let (mut playlist_socket, _) =
                listener.accept().await.expect("accept playlist request");
            let playlist_request = read_request(&mut playlist_socket).await;
            assert_eq!(request_path(&playlist_request), "/playlist.m3u8");
            respond(&mut playlist_socket, playlist.as_bytes()).await;

            let mut paths = Vec::new();
            for _ in 0..2 {
                let accepted =
                    tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
                let Ok(Ok((mut socket, _))) = accepted else {
                    break;
                };
                let request = read_request(&mut socket).await;
                let path = request_path(&request);
                let body: &[u8] = match path.as_str() {
                    "/init.mp4" => b"INIT",
                    "/media.m4s" => b"MEDIA",
                    other => panic!("unexpected local HLS path: {other}"),
                };
                paths.push(path);
                respond(&mut socket, body).await;
            }
            paths
        });

        let stream = LiveStream::new(LiveStreamOptions {
            client: None,
            stream_url: format!("http://{address}/playlist.m3u8"),
        })
        .expect("construct live stream");
        stream
            .refresh_playlist()
            .await
            .expect("local initialization playlist must parse");
        let bytes = stream
            .chunk()
            .await
            .expect("live chunk request must succeed")
            .expect("queued segment must produce bytes");
        let paths = server
            .await
            .expect("local HLS initialization server should join");
        (bytes, paths)
    }

    #[tokio::test]
    async fn live_chunk_without_ext_x_map_remains_plain_media_control() {
        let (bytes, paths) = run_one_segment(false).await;
        assert_eq!(&bytes[..], b"MEDIA");
        assert_eq!(paths, vec!["/media.m4s"]);
    }

    #[tokio::test]
    async fn live_chunk_emits_ext_x_map_before_media_segment() {
        let (bytes, paths) = run_one_segment(true).await;
        assert_eq!(
            paths,
            vec!["/init.mp4", "/media.m4s"],
            "EXT-X-MAP initialization must be fetched before its media segment"
        );
        assert_eq!(
            &bytes[..],
            b"INITMEDIA",
            "the initialization section must precede the media bytes returned to the stream consumer"
        );
    }

    #[tokio::test]
    async fn unchanged_ext_x_map_is_emitted_only_once_across_segments() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local repeated-map server");
        let address = listener.local_addr().expect("read repeated-map address");

        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for _ in 0..3 {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("accept repeated-map request");
                let request = read_request(&mut socket).await;
                let path = request_path(&request);
                let body: &[u8] = match path.as_str() {
                    "/init.mp4" => b"INIT",
                    "/one.m4s" => b"ONE",
                    "/two.m4s" => b"TWO",
                    other => panic!("unexpected repeated-map path: {other}"),
                };
                paths.push(path);
                respond(&mut socket, body).await;
            }
            paths
        });

        let stream = LiveStream::new(LiveStreamOptions {
            client: None,
            stream_url: format!("http://{address}/playlist.m3u8"),
        })
        .expect("construct repeated-map stream");
        let initialization = RemoteData::new(
            url::Url::parse(&format!("http://{address}/init.mp4"))
                .expect("parse repeated-map initialization URL"),
            None,
        );
        {
            let mut segments = stream.segments.write().await;
            for (seq, name) in [(1_u64, "one.m4s"), (2_u64, "two.m4s")] {
                segments.push((
                    Segment {
                        data: RemoteData::new(
                            url::Url::parse(&format!("http://{address}/{name}"))
                                .expect("parse repeated-map media URL"),
                            None,
                        ),
                        discon_seq: 0,
                        seq,
                        format: MediaFormat::Unknown,
                        initialization: Some(initialization.clone()),
                    },
                    Encryption::None,
                ));
            }
        }
        *stream.last_refresh.write().await = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis();
        *stream.is_end.write().await = true;

        let first = stream
            .chunk()
            .await
            .expect("first repeated-map chunk must succeed")
            .expect("first repeated-map segment must exist");
        let second = stream
            .chunk()
            .await
            .expect("second repeated-map chunk must succeed")
            .expect("second repeated-map segment must exist");
        let paths = server.await.expect("repeated-map server should join");

        assert_eq!(&first[..], b"INITONE");
        assert_eq!(&second[..], b"TWO");
        assert_eq!(paths, vec!["/init.mp4", "/one.m4s", "/two.m4s"]);
    }
}

#[cfg(test)]
mod unexpected_success_status_tests {
    use super::{
        Encryption, LiveStream, LiveStreamOptions, MediaFormat, RemoteData, Segment, Stream,
    };
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn unexpected_success_status_keeps_hls_segment_retryable() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind HLS status server");
        let address = listener.local_addr().expect("read HLS status address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept segment request");
            let mut request = [0_u8; 1024];
            let _ = socket
                .read(&mut request)
                .await
                .expect("read segment request");
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .expect("write 204 segment response");
        });

        let stream = LiveStream::new(LiveStreamOptions {
            client: None,
            stream_url: format!("http://{address}/playlist.m3u8"),
        })
        .expect("construct HLS stream");
        let segment_url =
            url::Url::parse(&format!("http://{address}/segment.ts")).expect("parse segment URL");
        stream.segments.write().await.push((
            Segment {
                data: RemoteData::new(segment_url, None),
                discon_seq: 0,
                seq: 1,
                format: MediaFormat::Unknown,
                initialization: None,
            },
            Encryption::None,
        ));
        *stream.last_refresh.write().await = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis();
        *stream.is_end.write().await = true;

        let result = stream.chunk().await;
        server.await.expect("HLS status server should join");
        let remaining = stream.segments.read().await.len();

        assert!(
            result.is_err() && remaining == 1,
            "204 must be rejected before consuming the HLS media segment; got result={result:?}, remaining={remaining}"
        );
    }
}

#[cfg(test)]
mod ignored_byte_range_response_tests {
    use super::{
        Encryption, LiveStream, LiveStreamOptions, MediaFormat, RemoteData, Segment, Stream,
    };
    use m3u8_rs::ByteRange;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("read HLS byte-range request");
            assert!(read > 0, "client closed before request headers completed");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request).expect("local HTTP request must be UTF-8")
    }

    #[tokio::test]
    async fn ignored_hls_byte_range_emits_only_requested_subrange() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ignored HLS byte-range server");
        let address = listener
            .local_addr()
            .expect("read ignored HLS byte-range address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept segment request");
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("Range: bytes=2-3")),
                "client must request the declared HLS sub-range; got {request:?}"
            );
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
                )
                .await
                .expect("write full ignored-range representation");
        });

        let stream = LiveStream::new(LiveStreamOptions {
            client: None,
            stream_url: format!("http://{address}/playlist.m3u8"),
        })
        .expect("construct HLS stream");
        let segment_url =
            url::Url::parse(&format!("http://{address}/segment.ts")).expect("parse segment URL");
        stream.segments.write().await.push((
            Segment {
                data: RemoteData::new(
                    segment_url,
                    Some(ByteRange {
                        length: 2,
                        offset: Some(2),
                    }),
                ),
                discon_seq: 0,
                seq: 1,
                format: MediaFormat::Unknown,
                initialization: None,
            },
            Encryption::None,
        ));
        *stream.last_refresh.write().await = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis();
        *stream.is_end.write().await = true;

        let result = stream.chunk().await;
        server
            .await
            .expect("ignored HLS byte-range server should join");

        assert_eq!(
            result
                .expect("ignored Range fallback should preserve the declared HLS segment")
                .as_deref(),
            Some(b"cd".as_slice()),
            "EXT-X-BYTERANGE declares only bytes 2-3 as this media segment; a server returning the whole resource must not make the client emit the whole resource"
        );
    }
}
