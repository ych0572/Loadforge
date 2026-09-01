use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;

use crate::http::{build_tls_config, classify_conn_error, split_host_port, ConnError, CONNECT_TIMEOUT};
use crate::metrics::Metrics;
use crate::scheduler::start_delay;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const MAX_FRAME: u64 = 8 * 1024 * 1024;

trait Stream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> Stream for T {}

type WsStream = Box<dyn Stream + Unpin + Send>;

/// WebSocket send/receive mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsMode {
    /// send one message, await one reply, measure round-trip.
    Echo,
    /// fire-and-forget: send as fast as possible, no reply wait.
    Send,
    /// receive-only: count incoming messages (optionally send a subscribe message once).
    Recv,
}

/// WebSocket workload definition.
#[derive(Clone)]
pub struct WsConfig {
    pub url: String,
    pub message: Bytes,
    pub mode: WsMode,
    pub send_interval: f64,
}

/// Each VU opens a WebSocket and runs send -> receive-echo rounds until the
/// deadline. `total` = messages sent, `failed` = connection errors,
/// latency = round-trip time, status_codes = {101: echoes}.
pub fn spawn_ws_vus(
    vu_count: u32,
    ws: WsConfig,
    metrics: Arc<Metrics>,
    duration_secs: u64,
    insecure: bool,
    ramp_up: f64,
) -> Vec<JoinHandle<()>> {
    let ws = Arc::new(ws);
    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let mut handles = Vec::with_capacity(vu_count as usize);

    for index in 0..vu_count {
        let ws = Arc::clone(&ws);
        let metrics = Arc::clone(&metrics);
        let delay = start_delay(index, vu_count, ramp_up);

        handles.push(tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }

            let mut buf: Vec<u8> = Vec::new();
            let interval = Duration::from_secs_f64(ws.send_interval.max(0.0));

            while Instant::now() < deadline {
                let mut io = match connect_ws(&ws.url, insecure).await {
                    Ok(io) => io,
                    Err(e) => {
                        metrics.record_conn_error(e);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                };

                match ws.mode {
                    WsMode::Echo => {
                        while Instant::now() < deadline {
                            let start = Instant::now();
                            if send_frame(&mut io, 0x1, &ws.message).await.is_err() {
                                break;
                            }
                            match recv_message(&mut io, &mut buf).await {
                                Ok(Some(reply)) => {
                                    let rtt_us = start.elapsed().as_micros() as u64;
                                    let bytes = (ws.message.len() + reply.len()) as u64;
                                    metrics.record(rtt_us, 101, bytes, true, None);
                                }
                                Ok(None) | Err(_) => break,
                            }
                        }
                    }
                    WsMode::Send => {
                        while Instant::now() < deadline {
                            if !interval.is_zero() {
                                tokio::time::sleep(interval).await;
                            }
                            if send_frame(&mut io, 0x1, &ws.message).await.is_err() {
                                break;
                            }
                            metrics.record(0, 101, ws.message.len() as u64, true, None);
                        }
                    }
                    WsMode::Recv => {
                        if !ws.message.is_empty() {
                            if send_frame(&mut io, 0x1, &ws.message).await.is_err() {
                                continue;
                            }
                        }
                        while Instant::now() < deadline {
                            match recv_message(&mut io, &mut buf).await {
                                Ok(Some(msg)) => {
                                    metrics.record(0, 101, msg.len() as u64, true, None);
                                }
                                Ok(None) | Err(_) => break,
                            }
                        }
                    }
                }
            }
        }));
    }

    handles
}

async fn connect_ws(url: &str, insecure: bool) -> Result<WsStream, ConnError> {
    let (is_tls, rest) = if let Some(r) = url.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err(ConnError::Other);
    };

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let authority = authority.trim_end_matches(':');
    let (host, port) = split_host_port(authority, is_tls);
    let addr = format!("{}:{}", host, port);
    let hostport = authority.to_string();
    let key = base64_encode(&rand::random::<[u8; 16]>());

    let tcp = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return Err(classify_conn_error(&e)),
        Err(_) => return Err(ConnError::Timeout),
    };
    let _ = tcp.set_nodelay(true);

    let mut io: WsStream = if is_tls {
        let config = Arc::new(build_tls_config(insecure));
        let name = ServerName::try_from(host.clone()).map_err(|_| ConnError::Other)?;
        let connector = TlsConnector::from(config);
        let tls_stream = match tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(name, tcp)).await {
            Ok(Ok(t)) => t,
            Ok(Err(_)) => return Err(ConnError::Tls),
            Err(_) => return Err(ConnError::Timeout),
        };
        Box::new(tls_stream)
    } else {
        Box::new(tcp)
    };

    handshake(&mut io, &hostport, &path, &key).await.ok_or(ConnError::Protocol)?;
    Ok(io)
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut S,
    hostport: &str,
    path: &str,
    key: &str,
) -> Option<()> {
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    io.write_all(req.as_bytes()).await.ok()?;

    let mut resp: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = io.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        resp.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_seq(&resp, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&resp[..pos]);
            if !head.starts_with("HTTP/1.1 101") {
                return None;
            }

            let mut input = key.to_string();
            input.push_str(WS_GUID);
            let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, input.as_bytes());
            let expected = base64_encode(digest.as_ref());

            let accept_ok = head.lines().any(|line| {
                let line = line.trim();
                match line.split_once(':') {
                    Some((name, value)) => {
                        name.trim().eq_ignore_ascii_case("sec-websocket-accept")
                            && value.trim() == expected
                    }
                    None => false,
                }
            });

            return if accept_ok { Some(()) } else { None };
        }
    }
}

/// Send a masked client frame (FIN=1).
async fn send_frame<S: AsyncWrite + Unpin>(io: &mut S, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mask = rand::random::<[u8; 4]>();
    let mut header = Vec::with_capacity(14);
    header.push(0x80 | opcode);

    let len = payload.len();
    if len < 126 {
        header.push(0x80 | len as u8);
    } else if len <= 0xFFFF {
        header.push(0x80 | 126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(0x80 | 127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }
    header.extend_from_slice(&mask);

    let masked: Vec<u8> = payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
    io.write_all(&header).await?;
    io.write_all(&masked).await?;
    Ok(())
}

/// Read one complete data message, responding to pings and ignoring pongs.
async fn recv_message<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut S,
    buf: &mut Vec<u8>,
) -> Result<Option<Vec<u8>>, std::io::Error> {
    let mut message: Vec<u8> = Vec::new();

    loop {
        let mut h = [0u8; 2];
        io.read_exact(&mut h).await?;

        let fin = h[0] & 0x80 != 0;
        let opcode = h[0] & 0x0F;
        let masked = h[1] & 0x80 != 0;
        let mut len = (h[1] & 0x7F) as u64;

        if len == 126 {
            let mut e = [0u8; 2];
            io.read_exact(&mut e).await?;
            len = u16::from_be_bytes(e) as u64;
        } else if len == 127 {
            let mut e = [0u8; 8];
            io.read_exact(&mut e).await?;
            len = u64::from_be_bytes(e);
        }

        if len > MAX_FRAME {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
        }

        let mut mask = [0u8; 4];
        if masked {
            io.read_exact(&mut mask).await?;
        }

        buf.resize(len as usize, 0);
        io.read_exact(buf).await?;
        if masked {
            for (i, b) in buf.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }

        match opcode {
            0x1 | 0x2 => {
                message.extend_from_slice(&buf[..len as usize]);
                if fin {
                    return Ok(Some(message));
                }
            }
            0x8 => return Ok(None), // close
            0x9 => {
                let pong = buf[..len as usize].to_vec();
                send_frame(io, 0xA, &pong).await?;
            }
            0xA => {} // pong
            _ => {}
        }
    }
}

fn find_seq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn base64_encode(data: &[u8]) -> String {
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
    }
    out
}