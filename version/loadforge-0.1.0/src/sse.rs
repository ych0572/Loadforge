use std::sync::Arc;
use std::time::{Duration, Instant};

use http_body_util::BodyExt;
use tokio::task::JoinHandle;

use crate::http::{evaluate_expect, ConnError, Connection, ExpectSpec, Request};
use crate::metrics::Metrics;
use crate::scheduler::start_delay;

/// SSE workload definition.
#[derive(Clone)]
pub struct SseConfig {
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub expect: Option<ExpectSpec>,
}

/// Each VU opens an SSE stream and counts incoming events until the deadline.
/// `total` = events received, `failed` = connection errors, `bytes` = event bytes.
pub fn spawn_sse_vus(
    vu_count: u32,
    base_url: String,
    sse: SseConfig,
    metrics: Arc<Metrics>,
    duration_secs: u64,
    insecure: bool,
    ramp_up: f64,
) -> Vec<JoinHandle<()>> {
    let SseConfig { path, mut headers, expect } = sse;
    headers.push(("Accept".to_string(), "text/event-stream".to_string()));
    let headers = Arc::new(headers);
    let path = Arc::new(path);

    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let mut handles = Vec::with_capacity(vu_count as usize);

    for index in 0..vu_count {
        let headers = Arc::clone(&headers);
        let path = Arc::clone(&path);
        let metrics = Arc::clone(&metrics);
        let base_url = base_url.clone();
        let expect = expect.clone();
        let delay = start_delay(index, vu_count, ramp_up);

        handles.push(tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }

            let mut conn = Connection::new(&base_url, insecure).await;

            while Instant::now() < deadline {
                let req = Request {
                    method: http::Method::GET,
                    path: path.as_ref().clone(),
                    headers: headers.as_ref().clone(),
                    body: None,
                    capture_body: false,
                    expect: None,
                };

                let resp = match conn.send(&req).await {
                    Ok(r) => r,
                    Err(e) => {
                        metrics.record_conn_error(e);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                };

                if !resp.status().is_success() {
                    metrics.record_conn_error(ConnError::Protocol);
                    continue;
                }

                let mut body = resp.into_body();
                let mut buf: Vec<u8> = Vec::new();

                while Instant::now() < deadline {
                    match body.frame().await {
                        Some(Ok(frame)) => {
                            let data = match frame.into_data() {
                                Ok(d) => d,
                                Err(_) => continue,
                            };
                            buf.extend_from_slice(&data);
                            while let Some((consume, event_data)) = next_event(&buf) {
                                metrics.record_event(event_data.len() as u64);
                                if let Some(exp) = &expect {
                                    let body = bytes::Bytes::from(event_data.clone());
                                    let (passed, failures) =
                                        evaluate_expect(exp, "GET", path.as_str(), 200, Some(&body));
                                    metrics.record_checks(passed, &failures);
                                }
                                buf.drain(..consume);
                            }
                        }
                        Some(Err(_)) | None => break, // stream ended / errored: reconnect
                    }
                }
            }
        }));
    }

    handles
}

/// Find the next SSE event boundary and return its `data` payload.
/// Returns (bytes_to_consume, data).
fn next_event(buf: &[u8]) -> Option<(usize, String)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i + 2, parse_sse_data(&buf[..i])));
        }
        if i + 3 < buf.len()
            && buf[i] == b'\r'
            && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            return Some((i + 4, parse_sse_data(&buf[..i])));
        }
        i += 1;
    }
    None
}

/// Extract the `data:` field(s) of one SSE event, joined by `\n` per the spec.
fn parse_sse_data(block: &[u8]) -> String {
    let text = String::from_utf8_lossy(block);
    let mut data = String::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(v) = line.strip_prefix("data:") {
            let v = v.strip_prefix(' ').unwrap_or(v);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(v);
        }
    }
    data
}