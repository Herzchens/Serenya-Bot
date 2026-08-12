use crate::constants::{DEFAULT_HEADERS, DEFAULT_MAX_RETRIES};
use crate::stream::{
    encryption::Encryption, media_format::MediaFormat, remote_data::RemoteData, segment::Segment,
    streams::Stream,
};
use crate::structs::{CustomRetryableStrategy, VideoError};
use crate::utils::{get_html, make_absolute_url};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use m3u8_rs::parse_media_playlist;
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
    chunk_lock: Mutex<()>,
}

fn elapsed_since(last_refresh: u128, current_time: u128) -> u128 {
    current_time.saturating_sub(last_refresh)
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
                data: RemoteData::new(seg_url, segment.byte_range.clone()),
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

        let mut response = self
            .client
            .get(first_segment.0.url().as_str())
            .headers(headers)
            .send()
            .await
            .map_err(VideoError::ReqwestMiddleware)?
            .error_for_status()
            .map_err(VideoError::Reqwest)?;

        let mut buf: BytesMut = BytesMut::new();

        while let Some(chunk) = response.chunk().await.map_err(VideoError::Reqwest)? {
            buf.extend(chunk);
        }

        // Decrypt data bytes
        buf = BytesMut::from_iter(first_segment.1.decrypt(&self.client, &buf).await?);

        // Delete downloaded segment from segments array
        let mut segment_vector = self.segments.write().await;
        segment_vector.remove(0);

        Ok(Some(buf.into()))
    }
}

#[cfg(test)]
mod live_edge_tests {
    use super::{
        elapsed_since, Encryption, LiveStream, LiveStreamOptions, MediaFormat, RemoteData, Segment,
        Stream,
    };
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn clock_rollback_does_not_underflow_live_refresh_elapsed_time() {
        assert_eq!(elapsed_since(20_001, 20_000), 0);
        assert_eq!(elapsed_since(20_000, 20_001), 1);
    }

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
        server.await.expect("local playlist server should join");

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
mod vendored_byte_range_request_tests {
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
            .expect("bind local vendored byte-range server");
        let address = listener
            .local_addr()
            .expect("read local vendored byte-range server address");

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("accept vendored segment request");
            let mut request = [0_u8; 4096];
            let read = socket
                .read(&mut request)
                .await
                .expect("read vendored segment request");
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\npart")
                .await
                .expect("write vendored segment response");
            request
        });

        let stream = LiveStream::new(LiveStreamOptions {
            client: None,
            stream_url: format!("http://{address}/playlist.m3u8"),
        })
        .expect("construct vendored live stream");
        let segment_url = url::Url::parse(&format!("http://{address}/media.bin"))
            .expect("parse local vendored media URL");
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
            .expect("vendored chunk request should succeed")
            .expect("queued vendored segment should produce bytes");
        let request = server
            .await
            .expect("vendored byte-range server should join");
        (body, request)
    }

    #[tokio::test]
    async fn vendored_live_chunk_without_byte_range_omits_range_header() {
        let (body, request) = run_chunk(None).await;
        assert_eq!(&body[..], b"part");
        assert!(
            !request.to_ascii_lowercase().contains("\r\nrange:"),
            "ordinary vendored HLS segments must not invent a byte-range request"
        );
    }

    #[tokio::test]
    async fn vendored_live_chunk_sends_range_header_for_byte_ranged_segment() {
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
            "vendored EXT-X-BYTERANGE media must be fetched with its declared HTTP Range"
        );
    }
}
