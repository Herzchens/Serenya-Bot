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

        let end = self.end_index().await;

        // Nothing else remain set controllers to the beginning state and send None to finish
        if end == 0 {
            let mut end = self.end.write().await;
            let mut start = self.start.write().await;
            *end = self.end_static;
            *start = self.start_static;

            // Send None to close
            return Ok(None);
        }

        if end >= self.content_length {
            let mut end = self.end.write().await;
            *end = 0;
        }

        let mut headers = DEFAULT_HEADERS.clone();

        let end = self.end_index().await;
        let range_end = if end == 0 {
            "".to_string()
        } else {
            end.to_string()
        };

        headers.insert(
            reqwest::header::RANGE,
            format!("bytes={}-{}", self.start_index().await, range_end)
                .parse()
                .unwrap(),
        );

        let ua = crate::utils::get_user_agent_for_url(&self.link);
        headers.insert(reqwest::header::USER_AGENT, ua.parse().unwrap());

        let mut response = self
            .client
            .get(&self.link)
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

        if end != 0 {
            let mut start = self.start.write().await;
            *start = end + 1;
            let mut end = self.end.write().await;
            *end += self.dl_chunk_size;
        }

        Ok(Some(buf.into()))
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

    async fn respond(mut socket: TcpStream) {
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest")
            .await
            .expect("write range response");
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
                    respond(first_socket).await;
                    (second_range, second_socket)
                }
                Ok(Err(error)) => panic!("accept second range request failed: {error}"),
                Err(_) => {
                    respond(first_socket).await;
                    let (mut second_socket, _) = listener
                        .accept()
                        .await
                        .expect("accept serialized second range request");
                    let second_request = read_request(&mut second_socket).await;
                    let second_range = range_header(&second_request);
                    (second_range, second_socket)
                }
            };
            respond(second_socket).await;
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
