use reqwest::header::CONTENT_RANGE;
use std::time::{Duration, Instant};

#[derive(thiserror::Error, Debug)]
pub enum ProbeError {
    #[error("HTTP 403 Forbidden: Access denied by YouTube")]
    Http403,

    #[error("HTTP Status Error: status code {0}")]
    HttpStatus(reqwest::StatusCode),

    #[error("Range request was not honored: expected HTTP 206 Partial Content, got {0}")]
    RangeNotHonored(reqwest::StatusCode),

    #[error(
        "Invalid Content-Range for requested bytes {requested_start}-{requested_end}: {content_range}"
    )]
    InvalidContentRange {
        requested_start: u64,
        requested_end: u64,
        content_range: String,
    },

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Empty response body from stream probe")]
    EmptyBody,

    #[error("Truncated range response body: expected {expected} bytes, received {received}")]
    TruncatedBody { expected: usize, received: usize },

    #[error("Throttled: speed {0:.2} KB/s is below threshold {1:.2} KB/s")]
    Throttled(f64, f64),
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub total_bytes: usize,
    pub elapsed: Duration,
    pub speed_kbps: f64,
}

/// Parse and validate the byte range claimed by an HTTP 206 response.
///
/// The returned range must correspond exactly to the range requested by the
/// probe. The representation size is used to choose later sample offsets.
fn parse_content_range(
    response: &reqwest::Response,
    requested_start: u64,
    requested_end: u64,
) -> Result<(u64, u64, u64), ProbeError> {
    let raw = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .trim()
        .to_owned();

    let parsed = raw
        .strip_prefix("bytes ")
        .and_then(|value| value.split_once('/'))
        .and_then(|(range, total)| {
            let (start, end) = range.split_once('-')?;

            Some((
                start.trim().parse::<u64>().ok()?,
                end.trim().parse::<u64>().ok()?,
                total.trim().parse::<u64>().ok()?,
            ))
        });

    let Some((returned_start, returned_end, total)) = parsed else {
        return Err(ProbeError::InvalidContentRange {
            requested_start,
            requested_end,
            content_range: raw,
        });
    };

    if total == 0
        || returned_start >= total
        || returned_end < returned_start
        || returned_end >= total
    {
        return Err(ProbeError::InvalidContentRange {
            requested_start,
            requested_end,
            content_range: raw,
        });
    }

    let expected_end = requested_end.min(total.saturating_sub(1));

    if returned_start != requested_start || returned_end != expected_end {
        return Err(ProbeError::InvalidContentRange {
            requested_start,
            requested_end,
            content_range: raw,
        });
    }

    Ok((returned_start, returned_end, total))
}

fn build_range_request(
    http_client: &reqwest::Client,
    stream_url: &str,
    user_agent: &str,
    client_kind: &str,
    start: u64,
    end: u64,
) -> reqwest::RequestBuilder {
    let mut request = http_client
        .get(stream_url)
        .header("User-Agent", user_agent)
        .header("Range", format!("bytes={start}-{end}"))
        .timeout(Duration::from_secs(4));

    if client_kind == "WEB" || client_kind == "WEB_SAFARI" || client_kind.is_empty() {
        request = request
            .header("Referer", "https://www.youtube.com/")
            .header("Origin", "https://www.youtube.com");
    }

    request
}

async fn probe_range(
    http_client: &reqwest::Client,
    stream_url: &str,
    user_agent: &str,
    client_kind: &str,
    start: u64,
    end: u64,
) -> Result<(usize, Option<u64>, f64), ProbeError> {
    let started = Instant::now();

    let mut response =
        build_range_request(http_client, stream_url, user_agent, client_kind, start, end)
            .send()
            .await?;

    if response.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(ProbeError::Http403);
    }

    if !response.status().is_success() {
        return Err(ProbeError::HttpStatus(response.status()));
    }

    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(ProbeError::RangeNotHonored(response.status()));
    }

    let (returned_start, returned_end, representation_size) =
        parse_content_range(&response, start, end)?;

    let expected_bytes = returned_end
        .saturating_sub(returned_start)
        .saturating_add(1)
        .min(usize::MAX as u64) as usize;

    let mut total_bytes = 0usize;

    while let Some(chunk) = response.chunk().await? {
        let remaining = expected_bytes.saturating_sub(total_bytes);
        total_bytes += chunk.len().min(remaining);

        if total_bytes >= expected_bytes {
            break;
        }
    }

    if total_bytes == 0 {
        return Err(ProbeError::EmptyBody);
    }

    if total_bytes != expected_bytes {
        return Err(ProbeError::TruncatedBody {
            expected: expected_bytes,
            received: total_bytes,
        });
    }

    let elapsed = started.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();

    let speed_kbps = if elapsed_secs > 0.0 {
        (total_bytes as f64 / 1024.0) / elapsed_secs
    } else {
        (total_bytes as f64) / 1024.0
    };

    Ok((total_bytes, Some(representation_size), speed_kbps))
}

/// Probe a stream across its representation instead of trusting only its first bytes.
///
/// The total byte budget remains approximately `bytes_to_probe`. For ranged responses
/// with a known representation size, the budget is distributed across the start,
/// middle, and end of the resource.
///
/// Every sampled range must independently satisfy the configured minimum throughput.
/// This prevents concurrent probes or a fast early range from masking throttling at
/// another offset.
pub async fn probe_stream_health(
    http_client: &reqwest::Client,
    stream_url: &str,
    user_agent: &str,
    client_kind: &str,
    bytes_to_probe: usize,
    min_speed_kbps: f64,
) -> Result<ProbeResult, ProbeError> {
    if bytes_to_probe == 0 {
        return Err(ProbeError::EmptyBody);
    }

    let started = Instant::now();

    let first_budget = bytes_to_probe.div_ceil(3);
    let first_end = first_budget.saturating_sub(1) as u64;

    let (first_bytes, representation_size, first_speed_kbps) = probe_range(
        http_client,
        stream_url,
        user_agent,
        client_kind,
        0,
        first_end,
    )
    .await?;

    let mut total_bytes = first_bytes;
    let mut minimum_speed_kbps = first_speed_kbps;

    if bytes_to_probe > first_budget {
        let remaining_budget = bytes_to_probe - first_budget;

        match representation_size {
            Some(size) if size > 0 && size <= bytes_to_probe as u64 => {
                if size > first_budget as u64 {
                    let (bytes, _, speed_kbps) = probe_range(
                        http_client,
                        stream_url,
                        user_agent,
                        client_kind,
                        first_budget as u64,
                        size - 1,
                    )
                    .await?;

                    total_bytes += bytes;
                    minimum_speed_kbps = minimum_speed_kbps.min(speed_kbps);
                }
            }

            Some(size) if size > bytes_to_probe as u64 => {
                let middle_budget = remaining_budget.div_ceil(2);
                let tail_budget = remaining_budget - middle_budget;

                let middle_len = middle_budget as u64;
                let middle_start = (size / 2)
                    .saturating_sub(middle_len / 2)
                    .min(size.saturating_sub(middle_len));
                let middle_end = middle_start + middle_len - 1;

                if tail_budget > 0 {
                    let tail_len = tail_budget as u64;
                    let tail_start = size - tail_len;
                    let tail_end = size - 1;

                    let middle_probe = probe_range(
                        http_client,
                        stream_url,
                        user_agent,
                        client_kind,
                        middle_start,
                        middle_end,
                    );

                    let tail_probe = probe_range(
                        http_client,
                        stream_url,
                        user_agent,
                        client_kind,
                        tail_start,
                        tail_end,
                    );

                    let ((middle_bytes, _, middle_speed_kbps), (tail_bytes, _, tail_speed_kbps)) =
                        tokio::try_join!(middle_probe, tail_probe)?;

                    total_bytes += middle_bytes + tail_bytes;
                    minimum_speed_kbps = minimum_speed_kbps
                        .min(middle_speed_kbps)
                        .min(tail_speed_kbps);
                } else {
                    let (middle_bytes, _, middle_speed_kbps) = probe_range(
                        http_client,
                        stream_url,
                        user_agent,
                        client_kind,
                        middle_start,
                        middle_end,
                    )
                    .await?;

                    total_bytes += middle_bytes;
                    minimum_speed_kbps = minimum_speed_kbps.min(middle_speed_kbps);
                }
            }

            _ => {
                let (bytes, _, speed_kbps) = probe_range(
                    http_client,
                    stream_url,
                    user_agent,
                    client_kind,
                    first_budget as u64,
                    bytes_to_probe.saturating_sub(1) as u64,
                )
                .await?;

                total_bytes += bytes;
                minimum_speed_kbps = minimum_speed_kbps.min(speed_kbps);
            }
        }
    }

    let elapsed = started.elapsed();

    tracing::debug!(
        total_bytes,
        elapsed_ms = elapsed.as_millis(),
        speed_kbps = format!("{:.2} KB/s", minimum_speed_kbps),
        representation_size,
        "Probed stream access across representation successfully"
    );

    if minimum_speed_kbps < min_speed_kbps {
        return Err(ProbeError::Throttled(minimum_speed_kbps, min_speed_kbps));
    }

    Ok(ProbeResult {
        total_bytes,
        elapsed,
        speed_kbps: minimum_speed_kbps,
    })
}

#[cfg(test)]
mod multi_range_access_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn later_range_403_invalidates_stream_probe() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake stream server");

        let addr = listener
            .local_addr()
            .expect("read fake stream server address");

        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("accept fake stream connection");

                let mut request = Vec::new();
                let mut buf = [0_u8; 4096];

                loop {
                    let n = socket.read(&mut buf).await.expect("read fake HTTP request");

                    if n == 0 {
                        break;
                    }

                    request.extend_from_slice(&buf[..n]);

                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }

                let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();

                let range_line = request_text
                    .lines()
                    .find(|line| line.starts_with("range: bytes="))
                    .expect("probe request must contain Range header");

                let range = range_line
                    .strip_prefix("range: bytes=")
                    .expect("strip Range prefix");

                let (start, end) = range.split_once('-').expect("bounded byte range");

                let start = start.trim().parse::<u64>().expect("parse range start");

                let end = end.trim().parse::<u64>().expect("parse range end");

                if start == 0 {
                    let body_len = (end - start + 1) as usize;

                    let headers = format!(
                        "HTTP/1.1 206 Partial Content\r\n\
                         Content-Length: {}\r\n\
                         Content-Range: bytes {}-{}/1048576\r\n\
                         Connection: close\r\n\
                         \r\n",
                        body_len, start, end
                    );

                    socket
                        .write_all(headers.as_bytes())
                        .await
                        .expect("write fake 206 headers");

                    socket
                        .write_all(&vec![b'x'; body_len])
                        .await
                        .expect("write fake 206 body");
                } else {
                    socket
                        .write_all(
                            b"HTTP/1.1 403 Forbidden\r\n\
                              Content-Length: 0\r\n\
                              Connection: close\r\n\
                              \r\n",
                        )
                        .await
                        .expect("write fake 403 response");
                }

                let _ = socket.shutdown().await;
            }
        });

        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("build test HTTP client");

        let stream_url = format!("http://{addr}/stream");

        let result = probe_stream_health(
            &http_client,
            &stream_url,
            "bug13-test-agent",
            "VISIONOS",
            102_400,
            0.0,
        )
        .await;

        server.abort();

        assert!(
            matches!(result, Err(ProbeError::Http403)),
            "BUG #13: first-range-only probe accepted a stream whose later ranges return 403: {result:?}"
        );
    }
}

#[cfg(test)]
mod per_range_throttle_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn handle_connection(mut socket: TcpStream) {
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];

        loop {
            let n = socket
                .read(&mut buf)
                .await
                .expect("read fake throttled HTTP request");

            if n == 0 {
                return;
            }

            request.extend_from_slice(&buf[..n]);

            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();

        let range_line = request_text
            .lines()
            .find(|line| line.starts_with("range: bytes="))
            .expect("probe request must contain Range");

        let range = range_line
            .strip_prefix("range: bytes=")
            .expect("strip Range prefix");

        let (start, end) = range.split_once('-').expect("bounded byte range");

        let start = start.trim().parse::<u64>().expect("parse range start");

        let end = end.trim().parse::<u64>().expect("parse range end");

        let body_len = (end - start + 1) as usize;

        // ~33 KiB / 750 ms ~= 44 KiB/s per individual request:
        // deliberately below the production 50 KiB/s threshold.
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;

        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\n\
             Content-Length: {}\r\n\
             Content-Range: bytes {}-{}/1048576\r\n\
             Connection: close\r\n\
             \r\n",
            body_len, start, end
        );

        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write fake throttled headers");

        socket
            .write_all(&vec![b'x'; body_len])
            .await
            .expect("write fake throttled body");

        let _ = socket.shutdown().await;
    }

    #[tokio::test]
    async fn parallel_ranges_do_not_mask_per_stream_throttling() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake throttled server");

        let addr = listener
            .local_addr()
            .expect("fake throttled server address");

        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener
                    .accept()
                    .await
                    .expect("accept fake throttled connection");

                tokio::spawn(handle_connection(socket));
            }
        });

        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("build throttling test client");

        let stream_url = format!("http://{addr}/stream");

        let result = probe_stream_health(
            &http_client,
            &stream_url,
            "bug13-throttle-test",
            "VISIONOS",
            102_400,
            50.0,
        )
        .await;

        server.abort();

        assert!(
            matches!(result, Err(ProbeError::Throttled(_, _))),
            "parallel range probing masked a below-threshold stream: {result:?}"
        );
    }
}

#[cfg(test)]
mod range_status_validation_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn handle_connection(mut socket: TcpStream, later_range_seen: Arc<AtomicBool>) {
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];

        loop {
            let n = socket
                .read(&mut buf)
                .await
                .expect("read fake range-ignoring request");

            if n == 0 {
                return;
            }

            request.extend_from_slice(&buf[..n]);

            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();

        let range_line = request_text
            .lines()
            .find(|line| line.starts_with("range: bytes="))
            .expect("probe request must contain Range");

        let range = range_line
            .strip_prefix("range: bytes=")
            .expect("strip Range prefix");

        let (start, _) = range.split_once('-').expect("bounded byte range");

        let start = start.trim().parse::<u64>().expect("parse Range start");

        if start > 0 {
            later_range_seen.store(true, Ordering::SeqCst);
        }

        // Deliberately IGNORE Range and always return the same body
        // beginning at representation byte zero.
        let body_len = 102_400usize;

        let headers = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Length: {}\r\n\
             Content-Type: audio/webm\r\n\
             Connection: close\r\n\
             \r\n",
            body_len
        );

        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write fake 200 headers");

        socket
            .write_all(&vec![b'x'; body_len])
            .await
            .expect("write fake 200 body");

        let _ = socket.shutdown().await;
    }

    #[tokio::test]
    async fn ignored_range_must_not_validate_later_offsets() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake range-ignoring server");

        let addr = listener.local_addr().expect("read fake server address");

        let later_range_seen = Arc::new(AtomicBool::new(false));

        let server_flag = later_range_seen.clone();

        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.expect("accept fake connection");

                let flag = server_flag.clone();

                tokio::spawn(async move {
                    handle_connection(socket, flag).await;
                });
            }
        });

        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("build range-ignored test client");

        let stream_url = format!("http://{addr}/stream");

        let result = probe_stream_health(
            &http_client,
            &stream_url,
            "bug13-range-test",
            "VISIONOS",
            102_400,
            0.0,
        )
        .await;

        server.abort();

        assert!(
            !later_range_seen.load(Ordering::SeqCst),
            "probe should reject the first HTTP 200 response before trusting later offsets"
        );

        assert!(
            matches!(
                result,
                Err(ProbeError::RangeNotHonored(status))
                    if status == reqwest::StatusCode::OK
            ),
            "Range-ignoring server was not rejected precisely: {result:?}"
        );
    }
}

#[cfg(test)]
mod content_range_validation_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn handle_connection(mut socket: TcpStream, later_range_seen: Arc<AtomicBool>) {
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];

        loop {
            let n = socket
                .read(&mut buf)
                .await
                .expect("read fake wrong-range request");

            if n == 0 {
                return;
            }

            request.extend_from_slice(&buf[..n]);

            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();

        let range_line = request_text
            .lines()
            .find(|line| line.starts_with("range: bytes="))
            .expect("probe request must contain Range");

        let range = range_line
            .strip_prefix("range: bytes=")
            .expect("strip Range prefix");

        let (requested_start, requested_end) = range.split_once('-').expect("bounded Range");

        let requested_start = requested_start
            .trim()
            .parse::<u64>()
            .expect("parse requested start");

        let requested_end = requested_end
            .trim()
            .parse::<u64>()
            .expect("parse requested end");

        if requested_start > 0 {
            later_range_seen.store(true, Ordering::SeqCst);
        }

        let body_len = (requested_end - requested_start + 1) as usize;

        // Claim 206 but deliberately lie about the returned offset:
        // every response says it contains bytes beginning at zero.
        let fake_end = body_len.saturating_sub(1);

        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\n\
             Content-Length: {}\r\n\
             Content-Range: bytes 0-{}/1048576\r\n\
             Connection: close\r\n\
             \r\n",
            body_len, fake_end
        );

        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write fake wrong-range headers");

        socket
            .write_all(&vec![b'x'; body_len])
            .await
            .expect("write fake wrong-range body");

        let _ = socket.shutdown().await;
    }

    #[tokio::test]
    async fn mismatched_content_range_must_not_validate_requested_offset() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake wrong-range server");

        let addr = listener.local_addr().expect("read fake server address");

        let later_range_seen = Arc::new(AtomicBool::new(false));

        let server_flag = later_range_seen.clone();

        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.expect("accept fake connection");

                let flag = server_flag.clone();

                tokio::spawn(async move {
                    handle_connection(socket, flag).await;
                });
            }
        });

        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("build wrong-range test client");

        let stream_url = format!("http://{addr}/stream");

        let result = probe_stream_health(
            &http_client,
            &stream_url,
            "bug13-wrong-range-test",
            "VISIONOS",
            102_400,
            0.0,
        )
        .await;

        server.abort();

        assert!(
            later_range_seen.load(Ordering::SeqCst),
            "probe never requested a non-zero offset"
        );

        assert!(
            matches!(result, Err(ProbeError::InvalidContentRange { .. })),
            "mismatched Content-Range was not rejected precisely: {result:?}"
        );
    }
}

#[cfg(test)]
mod short_partial_body_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn handle_connection(mut socket: TcpStream) {
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];

        loop {
            let n = socket
                .read(&mut buf)
                .await
                .expect("read fake truncated-range request");

            if n == 0 {
                return;
            }

            request.extend_from_slice(&buf[..n]);

            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();

        let range_line = request_text
            .lines()
            .find(|line| line.starts_with("range: bytes="))
            .expect("probe request must contain Range");

        let range = range_line
            .strip_prefix("range: bytes=")
            .expect("strip Range prefix");

        let (start, end) = range.split_once('-').expect("bounded byte range");

        let start = start.trim().parse::<u64>().expect("parse requested start");

        let end = end.trim().parse::<u64>().expect("parse requested end");

        // The Content-Range claims the full requested range was served,
        // but the HTTP body deliberately contains only one byte.
        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\n\
             Content-Length: 1\r\n\
             Content-Range: bytes {}-{}/1048576\r\n\
             Connection: close\r\n\
             \r\n",
            start, end
        );

        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write fake truncated headers");

        socket
            .write_all(b"x")
            .await
            .expect("write one-byte truncated body");

        let _ = socket.shutdown().await;
    }

    #[tokio::test]
    async fn truncated_partial_body_must_not_validate_stream_access() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake truncated-range server");

        let addr = listener.local_addr().expect("read fake server address");

        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener
                    .accept()
                    .await
                    .expect("accept fake truncated connection");

                tokio::spawn(handle_connection(socket));
            }
        });

        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("build truncated-range test client");

        let stream_url = format!("http://{addr}/stream");

        let result = probe_stream_health(
            &http_client,
            &stream_url,
            "truncated-range-test",
            "VISIONOS",
            102_400,
            0.0,
        )
        .await;

        server.abort();

        assert!(
            matches!(
                result,
                Err(ProbeError::TruncatedBody {
                    expected,
                    received: 1
                }) if expected > 1
            ),
            "truncated 206 body was not rejected precisely: {result:?}"
        );
    }
}

#[cfg(test)]
mod valid_multi_range_control_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn handle_connection(mut socket: TcpStream, nonzero_ranges: Arc<AtomicUsize>) {
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];

        loop {
            let n = socket
                .read(&mut buf)
                .await
                .expect("read valid range request");

            if n == 0 {
                return;
            }

            request.extend_from_slice(&buf[..n]);

            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();

        let range_line = request_text
            .lines()
            .find(|line| line.starts_with("range: bytes="))
            .expect("probe request must contain Range");

        let range = range_line
            .strip_prefix("range: bytes=")
            .expect("strip Range prefix");

        let (start, end) = range.split_once('-').expect("bounded byte range");

        let start = start.trim().parse::<u64>().expect("parse range start");

        let end = end.trim().parse::<u64>().expect("parse range end");

        if start > 0 {
            nonzero_ranges.fetch_add(1, Ordering::SeqCst);
        }

        let body_len = (end - start + 1) as usize;

        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\n\
             Content-Length: {}\r\n\
             Content-Range: bytes {}-{}/1048576\r\n\
             Connection: close\r\n\
             \r\n",
            body_len, start, end
        );

        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write valid 206 headers");

        socket
            .write_all(&vec![b'x'; body_len])
            .await
            .expect("write valid range body");

        let _ = socket.shutdown().await;
    }

    #[tokio::test]
    async fn valid_start_middle_and_tail_ranges_remain_accepted() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind valid range server");

        let addr = listener.local_addr().expect("read valid server address");

        let nonzero_ranges = Arc::new(AtomicUsize::new(0));
        let server_counter = nonzero_ranges.clone();

        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener
                    .accept()
                    .await
                    .expect("accept valid range connection");

                let counter = server_counter.clone();

                tokio::spawn(async move {
                    handle_connection(socket, counter).await;
                });
            }
        });

        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("build valid range test client");

        let stream_url = format!("http://{addr}/stream");

        let result = probe_stream_health(
            &http_client,
            &stream_url,
            "valid-range-test",
            "VISIONOS",
            102_400,
            0.0,
        )
        .await;

        server.abort();

        let result = result.expect("fully valid ranged representation must remain playable");

        assert_eq!(
            result.total_bytes, 102_400,
            "probe must consume exactly its configured byte budget"
        );

        assert!(
            nonzero_ranges.load(Ordering::SeqCst) >= 2,
            "valid control did not exercise both later representation samples"
        );
    }
}
