use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::ServerName;
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::TlsConnector;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Number of HTTP/2 connections in the shared pool. HTTP/2 multiplexes many
/// streams over few connections, so VUs share a small pool (this is inherent
/// to h2 and the key to its efficiency at high concurrency).
const H2_POOL_SIZE: usize = 8;

/// Per-request assertion spec (declarative, evaluated by Rust on the hot path).
#[derive(Clone, Debug)]
pub struct ExpectSpec {
    pub status: Option<u16>,
    pub body_contains: Option<String>,
    pub json: Option<serde_json::Value>,
}

impl ExpectSpec {
    pub fn is_empty(&self) -> bool {
        self.status.is_none() && self.body_contains.is_none() && self.json.is_none()
    }
}

/// A single failed assertion, kept for troubleshooting.
#[derive(Clone, Debug)]
pub struct CheckFailure {
    pub method: String,
    pub path: String,
    pub check: String,
    pub expected: String,
    pub actual: String,
}

/// Category of a connection-level failure (status 0 in the result).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConnError {
    Timeout,
    Refused,
    Reset,
    Tls,
    Dns,
    Protocol,
    Other,
}

impl ConnError {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnError::Timeout => "timeout",
            ConnError::Refused => "refused",
            ConnError::Reset => "reset",
            ConnError::Tls => "tls",
            ConnError::Dns => "dns",
            ConnError::Protocol => "protocol",
            ConnError::Other => "other",
        }
    }
}

fn classify_ref(e: &(dyn std::error::Error + 'static)) -> ConnError {
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        return match io.kind() {
            std::io::ErrorKind::TimedOut => ConnError::Timeout,
            std::io::ErrorKind::ConnectionRefused => ConnError::Refused,
            std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::UnexpectedEof => ConnError::Reset,
            std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::NotFound => ConnError::Dns,
            _ => ConnError::Other,
        };
    }
    if e.downcast_ref::<rustls::Error>().is_some() {
        return ConnError::Tls;
    }
    match e.source() {
        Some(s) => classify_ref(s),
        None => ConnError::Other,
    }
}

pub(crate) fn classify_conn_error(e: &(dyn std::error::Error + 'static)) -> ConnError {
    classify_ref(e)
}

pub struct RequestResult {
    pub status: u16,
    pub latency_us: u64,
    pub bytes: u64,
    pub is_success: bool,
    pub body: Option<Bytes>,
    pub check_passed: usize,
    pub check_failures: Vec<CheckFailure>,
    pub error: Option<ConnError>,
}

pub struct Request {
    pub method: http::Method,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Bytes>,
    pub capture_body: bool,
    pub expect: Option<ExpectSpec>,
}

fn fail(start: Instant, error: ConnError) -> RequestResult {
    RequestResult {
        status: 0,
        latency_us: start.elapsed().as_micros() as u64,
        bytes: 0,
        is_success: false,
        body: None,
        check_passed: 0,
        check_failures: Vec::new(),
        error: Some(error),
    }
}

/// Build a concrete request from a template + the connection authority.
fn build_request(addr: &str, req: &Request) -> Option<http::Request<Full<Bytes>>> {
    let mut builder = http::Request::builder()
        .method(req.method.as_str())
        .uri(req.path.as_str())
        .header("Host", addr);

    for (k, v) in &req.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }

    let body = req.body.clone().unwrap_or_default();
    builder.body(Full::new(body)).ok()
}

/// Turn a streaming response into a RequestResult (collect body + evaluate checks).
async fn finish_response(
    resp: http::Response<Incoming>,
    start: Instant,
    req: &Request,
) -> RequestResult {
    let status = resp.status().as_u16();
    let is_success = resp.status().is_success();
    let collected = resp.into_body().collect().await.ok();
    let raw_body: Option<Bytes> = collected.map(|c| c.to_bytes());
    let bytes = raw_body.as_ref().map(|b| b.len() as u64).unwrap_or(0);

    let (check_passed, check_failures) = match &req.expect {
        Some(exp) if !exp.is_empty() => {
            evaluate_expect(exp, req.method.as_str(), &req.path, status, raw_body.as_ref())
        }
        _ => (0, Vec::new()),
    };

    let body = if req.capture_body { raw_body } else { None };

    RequestResult {
        status,
        latency_us: start.elapsed().as_micros() as u64,
        bytes,
        is_success,
        body,
        check_passed,
        check_failures,
        error: None,
    }
}

enum Sender {
    H1(http1::SendRequest<Full<Bytes>>),
    H2(http2::SendRequest<Full<Bytes>>),
}

/// Per-VU HTTP/1.1 connection: owns its own TCP/TLS stream, no sharing, no locks.
pub struct Connection {
    addr: String,
    tls: Option<Arc<rustls::ClientConfig>>,
    server_name: Option<ServerName<'static>>,
    sender: Option<Sender>,
}

impl Connection {
    pub async fn new(base_url: &str, insecure: bool) -> Self {
        let parsed = parse_base(base_url, insecure, false);
        let sender = Self::connect(&parsed.addr, parsed.tls.as_ref(), parsed.server_name.as_ref(), false).await.ok();

        Self {
            addr: parsed.addr,
            tls: parsed.tls,
            server_name: parsed.server_name,
            sender,
        }
    }

    async fn connect(
        addr: &str,
        tls: Option<&Arc<rustls::ClientConfig>>,
        server_name: Option<&ServerName<'static>>,
        http2: bool,
    ) -> Result<Sender, ConnError> {
        match tokio::time::timeout(CONNECT_TIMEOUT, async {
            let stream = TcpStream::connect(addr).await.map_err(|e| classify_conn_error(&e))?;
            let _ = stream.set_nodelay(true);

            let sender = match (tls, server_name) {
                (Some(config), Some(name)) => {
                    let connector = TlsConnector::from(config.clone());
                    let tls_stream = connector
                        .connect(name.clone(), stream)
                        .await
                        .map_err(|_| ConnError::Tls)?;
                    if http2 {
                        let (sender, conn) = http2::Builder::new(TokioExecutor::new())
                            .handshake(TokioIo::new(tls_stream))
                            .await
                            .map_err(|e| classify_conn_error(&e))?;
                        tokio::task::spawn(async move {
                            let _ = conn.await;
                        });
                        Sender::H2(sender)
                    } else {
                        let (sender, conn) = http1::handshake(TokioIo::new(tls_stream))
                            .await
                            .map_err(|e| classify_conn_error(&e))?;
                        tokio::task::spawn(async move {
                            let _ = conn.await;
                        });
                        Sender::H1(sender)
                    }
                }
                _ => {
                    if http2 {
                        let (sender, conn) = http2::Builder::new(TokioExecutor::new())
                            .handshake(TokioIo::new(stream))
                            .await
                            .map_err(|e| classify_conn_error(&e))?;
                        tokio::task::spawn(async move {
                            let _ = conn.await;
                        });
                        Sender::H2(sender)
                    } else {
                        let (sender, conn) = http1::handshake(TokioIo::new(stream))
                            .await
                            .map_err(|e| classify_conn_error(&e))?;
                        tokio::task::spawn(async move {
                            let _ = conn.await;
                        });
                        Sender::H1(sender)
                    }
                }
            };

            Ok::<_, ConnError>(sender)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(ConnError::Timeout),
        }
    }

    async fn ensure_connected(&mut self) -> Result<(), ConnError> {
        if self.sender.is_some() {
            return Ok(());
        }
        match Self::connect(&self.addr, self.tls.as_ref(), self.server_name.as_ref(), false).await {
            Ok(s) => {
                self.sender = Some(s);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub async fn send(&mut self, req: &Request) -> Result<http::Response<Incoming>, ConnError> {
        self.ensure_connected().await?;

        let request = build_request(&self.addr, req).ok_or(ConnError::Other)?;

        let result = match self.sender.as_mut().unwrap() {
            Sender::H1(s) => s.send_request(request).await,
            Sender::H2(s) => s.send_request(request).await,
        };

        match result {
            Ok(resp) => Ok(resp),
            Err(e) => {
                self.sender = None;
                Err(classify_conn_error(&e))
            }
        }
    }

    #[inline]
    pub async fn request(&mut self, req: &Request) -> RequestResult {
        let start = Instant::now();
        let resp = match self.send(req).await {
            Ok(r) => r,
            Err(e) => return fail(start, e),
        };
        finish_response(resp, start, req).await
    }
}

/// Shared HTTP/2 connection pool. VUs round-robin over a few connections and
/// multiplex concurrent streams on each, so high VU counts don't mean one TCP
/// connection per VU.
pub struct H2Pool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    addr: String,
    tls: Option<Arc<rustls::ClientConfig>>,
    server_name: Option<ServerName<'static>>,
    slots: Vec<Mutex<Option<http2::SendRequest<Full<Bytes>>>>>,
    next: AtomicU64,
}

impl H2Pool {
    pub fn new(base_url: &str, insecure: bool) -> Self {
        let parsed = parse_base(base_url, insecure, true);
        let slots = (0..H2_POOL_SIZE).map(|_| Mutex::new(None)).collect();
        Self {
            inner: Arc::new(PoolInner {
                addr: parsed.addr,
                tls: parsed.tls,
                server_name: parsed.server_name,
                slots,
                next: AtomicU64::new(0),
            }),
        }
    }

    async fn connect(&self) -> Result<http2::SendRequest<Full<Bytes>>, ConnError> {
        let inner = &self.inner;
        match tokio::time::timeout(CONNECT_TIMEOUT, async {
            let stream = TcpStream::connect(&inner.addr).await.map_err(|e| classify_conn_error(&e))?;
            let _ = stream.set_nodelay(true);

            let sender = match (inner.tls.as_ref(), inner.server_name.as_ref()) {
                (Some(config), Some(name)) => {
                    let connector = TlsConnector::from(config.clone());
                    let tls_stream = connector
                        .connect(name.clone(), stream)
                        .await
                        .map_err(|_| ConnError::Tls)?;
                    let (sender, conn) = http2::Builder::new(TokioExecutor::new())
                        .handshake(TokioIo::new(tls_stream))
                        .await
                        .map_err(|e| classify_conn_error(&e))?;
                    tokio::task::spawn(async move {
                        let _ = conn.await;
                    });
                    sender
                }
                _ => {
                    let (sender, conn) = http2::Builder::new(TokioExecutor::new())
                        .handshake(TokioIo::new(stream))
                        .await
                        .map_err(|e| classify_conn_error(&e))?;
                    tokio::task::spawn(async move {
                        let _ = conn.await;
                    });
                    sender
                }
            };

            Ok::<_, ConnError>(sender)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(ConnError::Timeout),
        }
    }

    pub async fn request(&self, req: &Request) -> RequestResult {
        let start = Instant::now();
        let idx = (self.inner.next.fetch_add(1, Ordering::Relaxed) % self.inner.slots.len() as u64) as usize;

        let (sender, connect_err) = {
            let mut guard = self.inner.slots[idx].lock().await;
            if guard.is_none() {
                match self.connect().await {
                    Ok(s) => {
                        *guard = Some(s);
                        (guard.as_ref().cloned(), None)
                    }
                    Err(e) => (None, Some(e)),
                }
            } else {
                (guard.as_ref().cloned(), None)
            }
        };

        if let Some(e) = connect_err {
            return fail(start, e);
        }
        let Some(mut sender) = sender else {
            return fail(start, ConnError::Other);
        };

        let request = match build_request(&self.inner.addr, req) {
            Some(r) => r,
            None => return fail(start, ConnError::Other),
        };

        match sender.send_request(request).await {
            Ok(resp) => finish_response(resp, start, req).await,
            Err(e) => {
                if let Ok(mut guard) = self.inner.slots[idx].try_lock() {
                    *guard = None;
                }
                fail(start, classify_conn_error(&e))
            }
        }
    }
}

struct ParsedAddr {
    addr: String,
    tls: Option<Arc<rustls::ClientConfig>>,
    server_name: Option<ServerName<'static>>,
}

fn parse_base(base_url: &str, insecure: bool, http2: bool) -> ParsedAddr {
    let (is_tls, rest) = if let Some(r) = base_url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = base_url.strip_prefix("http://") {
        (false, r)
    } else {
        (false, base_url)
    };

    let authority = rest.split('/').next().unwrap_or(rest).trim_end_matches(':');
    let (host, port) = split_host_port(authority, is_tls);
    let addr = format!("{}:{}", host, port);

    let (tls, server_name) = if is_tls {
        let mut config = build_tls_config(insecure);
        config.alpn_protocols = vec![if http2 { b"h2".to_vec() } else { b"http/1.1".to_vec() }];
        let name = ServerName::try_from(host.to_string()).ok();
        (Some(Arc::new(config)), name)
    } else {
        (None, None)
    };

    ParsedAddr { addr, tls, server_name }
}

/// Evaluate an assertion spec against a response. Returns (passed, failures).
pub(crate) fn evaluate_expect(
    expect: &ExpectSpec,
    method: &str,
    path: &str,
    status: u16,
    body: Option<&Bytes>,
) -> (usize, Vec<CheckFailure>) {
    let mut passed = 0usize;
    let mut failures = Vec::new();

    let mk = |check: &str, expected: String, actual: String| CheckFailure {
        method: method.to_string(),
        path: path.to_string(),
        check: check.to_string(),
        expected,
        actual,
    };

    if let Some(expected_status) = expect.status {
        if status == expected_status {
            passed += 1;
        } else {
            failures.push(mk("status", expected_status.to_string(), status.to_string()));
        }
    }

    if let Some(needle) = &expect.body_contains {
        let text = body
            .map(|b| String::from_utf8_lossy(b).to_string())
            .unwrap_or_default();
        if text.contains(needle.as_str()) {
            passed += 1;
        } else {
            failures.push(mk("body_contains", needle.clone(), truncate(&text)));
        }
    }

    if let Some(exp_obj) = &expect.json {
        let parsed: Option<serde_json::Value> = body.and_then(|b| serde_json::from_slice(b).ok());
        if let Some(exp_map) = exp_obj.as_object() {
            for (key, ev) in exp_map {
                let actual = parsed.as_ref().and_then(|v| v.get(key));
                match actual {
                    Some(av) if av == ev => passed += 1,
                    Some(av) => failures.push(mk(&format!("json.{}", key), ev.to_string(), av.to_string())),
                    None => failures.push(mk(
                        &format!("json.{}", key),
                        ev.to_string(),
                        if parsed.is_none() {
                            "<body not json>".to_string()
                        } else {
                            "<missing>".to_string()
                        },
                    )),
                }
            }
        }
    }

    (passed, failures)
}

fn truncate(s: &str) -> String {
    if s.chars().count() > 200 {
        let head: String = s.chars().take(200).collect();
        format!("{}...", head)
    } else {
        s.to_string()
    }
}

pub(crate) fn split_host_port(hostport: &str, is_tls: bool) -> (String, u16) {
    let default_port = if is_tls { 443 } else { 80 };

    if let Some(rest) = hostport.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let host = rest[..end].to_string();
            let port = rest[end + 1..]
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(default_port);
            return (host, port);
        }
    }

    if let Some(idx) = hostport.rfind(':') {
        if let Ok(port) = hostport[idx + 1..].parse::<u16>() {
            return (hostport[..idx].to_string(), port);
        }
    }

    (hostport.to_string(), default_port)
}

pub(crate) fn build_tls_config(insecure: bool) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = if insecure {
        rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("no safe default TLS protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("no safe default TLS protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    config
}

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}