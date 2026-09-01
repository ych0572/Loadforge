use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::task::JoinHandle;

use crate::http::{Connection, ExpectSpec, H2Pool, Request, RequestResult};
use crate::metrics::Metrics;

/// A single weighted endpoint (one request per VU iteration).
#[derive(Clone)]
pub struct Endpoint {
    pub method: String,
    pub path: String,
    pub weight: u32,
    pub body: Option<bytes::Bytes>,
    pub headers: Vec<(String, String)>,
    pub expect: Option<ExpectSpec>,
}

/// One step in a business flow. A VU iteration runs all steps in order.
#[derive(Clone)]
pub struct FlowStep {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<bytes::Bytes>,
    pub extracts: Vec<(String, Vec<Segment>)>,
    pub expect: Option<ExpectSpec>,
}

#[derive(Clone, Debug)]
pub enum Segment {
    Key(String),
    Index(usize),
}

pub fn parse_extract_path(path: &str) -> Vec<Segment> {
    let path = path.trim();
    let path = path.strip_prefix("$.").unwrap_or(path);
    let path = path.strip_prefix('$').unwrap_or(path);
    path.split('.')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Ok(i) = s.parse::<usize>() {
                Segment::Index(i)
            } else {
                Segment::Key(s.to_string())
            }
        })
        .collect()
}

// ----------------------------------------------------------------------
// Prepared representations
// ----------------------------------------------------------------------

struct PreparedEndpoint {
    method: http::Method,
    path: String,
    body: Option<bytes::Bytes>,
    headers: Vec<(String, String)>,
    expect: Option<ExpectSpec>,
}

struct PreparedStep {
    method: http::Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Option<bytes::Bytes>,
    extracts: Vec<(String, Vec<Segment>)>,
    expect: Option<ExpectSpec>,
}

impl PreparedStep {
    fn render(&self, vars: &HashMap<String, String>) -> Request {
        let path = substitute(&self.path, vars);
        let headers = self
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), substitute(v, vars)))
            .collect();
        let body = self
            .body
            .as_ref()
            .map(|b| bytes::Bytes::from(substitute(std::str::from_utf8(b).unwrap_or(""), vars)));
        Request {
            method: self.method.clone(),
            path,
            headers,
            body,
            capture_body: !self.extracts.is_empty(),
            expect: self.expect.clone(),
        }
    }
}

struct Selector {
    endpoints: Vec<PreparedEndpoint>,
    cumulative: Vec<u32>,
    total: u32,
}

impl Selector {
    #[inline]
    fn pick(&self) -> &PreparedEndpoint {
        let mut rng = rand::thread_rng();
        let roll = rng.gen_range(1..=self.total);
        for (i, &cum) in self.cumulative.iter().enumerate() {
            if roll <= cum {
                return &self.endpoints[i];
            }
        }
        &self.endpoints[0]
    }
}

fn parse_method(m: &str) -> http::Method {
    http::Method::from_bytes(m.as_bytes()).unwrap_or(http::Method::GET)
}

// ----------------------------------------------------------------------
// Arrival-rate limiter + iteration counter
// ----------------------------------------------------------------------

pub(crate) struct RateLimiter {
    start: Instant,
    next_ns: AtomicU64,
    interval_ns: u64,
}

impl RateLimiter {
    pub(crate) fn new(rps: f64) -> Self {
        let interval_ns = (1_000_000_000.0 / rps.max(1.0)) as u64;
        Self {
            start: Instant::now(),
            next_ns: AtomicU64::new(0),
            interval_ns,
        }
    }

    pub(crate) async fn acquire(&self) {
        loop {
            let now = self.start.elapsed().as_nanos() as u64;
            let next = self.next_ns.load(Ordering::Acquire);
            let slot = if now >= next { now } else { next };
            let new_next = slot.wrapping_add(self.interval_ns);

            if self
                .next_ns
                .compare_exchange_weak(next, new_next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if slot > now {
                    tokio::time::sleep(Duration::from_nanos(slot - now)).await;
                }
                return;
            }
            tokio::time::sleep(Duration::from_micros(100)).await;
        }
    }
}

pub(crate) fn make_limiter(rps: Option<f64>) -> Option<Arc<RateLimiter>> {
    match rps {
        Some(r) if r > 0.0 => Some(Arc::new(RateLimiter::new(r))),
        _ => None,
    }
}

pub(crate) fn start_delay(index: u32, vu_count: u32, ramp_up: f64) -> Duration {
    if ramp_up <= 0.0 || vu_count == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(ramp_up * index as f64 / vu_count as f64)
}

/// Piecewise-linear load curve defined by a list of stages
/// `[{duration_secs, target_vu}, ...]`, starting from 0 VUs.
pub struct Stages {
    points: Vec<(f64, f64)>,
    pub total_duration: f64,
    pub max_target: u32,
}

impl Stages {
    pub fn new(stages: &[(f64, u32)]) -> Self {
        let mut points = Vec::with_capacity(stages.len() + 1);
        points.push((0.0, 0.0));
        let mut t = 0.0;
        let mut max_target = 0u32;
        for &(dur, target) in stages {
            t += dur;
            points.push((t, target as f64));
            max_target = max_target.max(target);
        }
        Self { points, total_duration: t, max_target }
    }

    pub fn target_at(&self, t: f64) -> f64 {
        let pts = &self.points;
        if t <= pts[0].0 {
            return pts[0].1;
        }
        let last = pts[pts.len() - 1];
        if t >= last.0 {
            return last.1;
        }
        for w in pts.windows(2) {
            let (t0, v0) = w[0];
            let (t1, v1) = w[1];
            if t >= t0 && t <= t1 {
                return v0 + (v1 - v0) * (t - t0) / (t1 - t0);
            }
        }
        last.1
    }
}

#[inline]
fn is_active(stages: &Stages, start: Instant, index: u32) -> bool {
    (index as f64) < stages.target_at(start.elapsed().as_secs_f64())
}

fn acquire_iteration(remaining: &AtomicU64) -> bool {
    loop {
        let cur = remaining.load(Ordering::SeqCst);
        if cur == 0 {
            return false;
        }
        if remaining
            .compare_exchange_weak(cur, cur - 1, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
}

// ----------------------------------------------------------------------
// Per-request helpers
// ----------------------------------------------------------------------

fn record_result(metrics: &Arc<Metrics>, result: &RequestResult) {
    metrics.record(result.latency_us, result.status, result.bytes, result.is_success, result.error);
    metrics.record_checks(result.check_passed, &result.check_failures);
}

async fn endpoint_request_conn(
    conn: &mut Connection,
    selector: &Selector,
    limiter: &Option<Arc<RateLimiter>>,
    metrics: &Arc<Metrics>,
) {
    if let Some(rl) = limiter {
        rl.acquire().await;
    }
    let ep = selector.pick();
    let req = Request {
        method: ep.method.clone(),
        path: ep.path.clone(),
        headers: ep.headers.clone(),
        body: ep.body.clone(),
        capture_body: false,
        expect: ep.expect.clone(),
    };
    let result = conn.request(&req).await;
    record_result(metrics, &result);
}

async fn endpoint_request_pool(
    pool: &H2Pool,
    selector: &Selector,
    limiter: &Option<Arc<RateLimiter>>,
    metrics: &Arc<Metrics>,
) {
    if let Some(rl) = limiter {
        rl.acquire().await;
    }
    let ep = selector.pick();
    let req = Request {
        method: ep.method.clone(),
        path: ep.path.clone(),
        headers: ep.headers.clone(),
        body: ep.body.clone(),
        capture_body: false,
        expect: ep.expect.clone(),
    };
    let result = pool.request(&req).await;
    record_result(metrics, &result);
}

async fn flow_request_conn(
    conn: &mut Connection,
    steps: &[PreparedStep],
    vars: &mut HashMap<String, String>,
    limiter: &Option<Arc<RateLimiter>>,
    metrics: &Arc<Metrics>,
) {
    for step in steps {
        if let Some(rl) = limiter {
            rl.acquire().await;
        }
        let req = step.render(vars);
        let result = conn.request(&req).await;
        record_result(metrics, &result);
        if !step.extracts.is_empty() {
            if let Some(body) = &result.body {
                extract_vars(body, &step.extracts, vars);
            }
        }
    }
}

async fn flow_request_pool(
    pool: &H2Pool,
    steps: &[PreparedStep],
    vars: &mut HashMap<String, String>,
    limiter: &Option<Arc<RateLimiter>>,
    metrics: &Arc<Metrics>,
) {
    for step in steps {
        if let Some(rl) = limiter {
            rl.acquire().await;
        }
        let req = step.render(vars);
        let result = pool.request(&req).await;
        record_result(metrics, &result);
        if !step.extracts.is_empty() {
            if let Some(body) = &result.body {
                extract_vars(body, &step.extracts, vars);
            }
        }
    }
}

// ----------------------------------------------------------------------
// Spawners
// ----------------------------------------------------------------------

pub fn spawn_endpoint_vus(
    vu_count: u32,
    base_url: String,
    endpoints: Vec<Endpoint>,
    metrics: Arc<Metrics>,
    duration_secs: u64,
    iterations: Option<u64>,
    stages: Option<Arc<Stages>>,
    insecure: bool,
    http2: bool,
    ramp_up: f64,
    rps: Option<f64>,
) -> Vec<JoinHandle<()>> {
    let mut prepared = Vec::with_capacity(endpoints.len());
    let mut cumulative = Vec::with_capacity(endpoints.len());
    let mut total = 0u32;

    for ep in &endpoints {
        total += ep.weight;
        cumulative.push(total);
        prepared.push(PreparedEndpoint {
            method: parse_method(&ep.method),
            path: ep.path.clone(),
            body: ep.body.clone(),
            headers: ep.headers.clone(),
            expect: ep.expect.clone(),
        });
    }

    let selector = Arc::new(Selector { endpoints: prepared, cumulative, total });
    let limiter = make_limiter(rps);
    let remaining = iterations.map(|i| Arc::new(AtomicU64::new(i)));
    let deadline = match &stages {
        Some(s) => Instant::now() + Duration::from_secs_f64(s.total_duration),
        None => Instant::now() + Duration::from_secs(duration_secs),
    };
    let h2_pool = if http2 {
        Some(Arc::new(H2Pool::new(&base_url, insecure)))
    } else {
        None
    };
    let mut handles = Vec::with_capacity(vu_count as usize);

    for index in 0..vu_count {
        let selector = Arc::clone(&selector);
        let metrics = Arc::clone(&metrics);
        let base_url = base_url.clone();
        let limiter = limiter.clone();
        let remaining = remaining.clone();
        let h2_pool = h2_pool.clone();
        let stages = stages.clone();
        let delay = if iterations.is_some() || stages.is_some() {
            Duration::ZERO
        } else {
            start_delay(index, vu_count, ramp_up)
        };

        handles.push(tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }

            if let Some(pool) = &h2_pool {
                if let Some(stages) = &stages {
                    let t0 = Instant::now();
                    while Instant::now() < deadline {
                        if !is_active(stages, t0, index) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            continue;
                        }
                        endpoint_request_pool(pool, &selector, &limiter, &metrics).await;
                    }
                } else if let Some(remaining) = &remaining {
                    while acquire_iteration(remaining) {
                        endpoint_request_pool(pool, &selector, &limiter, &metrics).await;
                    }
                } else {
                    while Instant::now() < deadline {
                        endpoint_request_pool(pool, &selector, &limiter, &metrics).await;
                    }
                }
            } else {
                let mut conn = Connection::new(&base_url, insecure).await;
                if let Some(stages) = &stages {
                    let t0 = Instant::now();
                    while Instant::now() < deadline {
                        if !is_active(stages, t0, index) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            continue;
                        }
                        endpoint_request_conn(&mut conn, &selector, &limiter, &metrics).await;
                    }
                } else if let Some(remaining) = &remaining {
                    while acquire_iteration(remaining) {
                        endpoint_request_conn(&mut conn, &selector, &limiter, &metrics).await;
                    }
                } else {
                    while Instant::now() < deadline {
                        endpoint_request_conn(&mut conn, &selector, &limiter, &metrics).await;
                    }
                }
            }
        }));
    }

    handles
}

pub fn spawn_flow_vus(
    vu_count: u32,
    base_url: String,
    steps: Vec<FlowStep>,
    metrics: Arc<Metrics>,
    duration_secs: u64,
    iterations: Option<u64>,
    stages: Option<Arc<Stages>>,
    insecure: bool,
    http2: bool,
    ramp_up: f64,
    rps: Option<f64>,
) -> Vec<JoinHandle<()>> {
    let steps: Arc<Vec<PreparedStep>> = Arc::new(
        steps
            .iter()
            .map(|s| PreparedStep {
                method: parse_method(&s.method),
                path: s.path.clone(),
                headers: s.headers.clone(),
                body: s.body.clone(),
                extracts: s.extracts.clone(),
                expect: s.expect.clone(),
            })
            .collect(),
    );

    let limiter = make_limiter(rps);
    let remaining = iterations.map(|i| Arc::new(AtomicU64::new(i)));
    let deadline = match &stages {
        Some(s) => Instant::now() + Duration::from_secs_f64(s.total_duration),
        None => Instant::now() + Duration::from_secs(duration_secs),
    };
    let h2_pool = if http2 {
        Some(Arc::new(H2Pool::new(&base_url, insecure)))
    } else {
        None
    };
    let mut handles = Vec::with_capacity(vu_count as usize);

    for index in 0..vu_count {
        let steps = Arc::clone(&steps);
        let metrics = Arc::clone(&metrics);
        let base_url = base_url.clone();
        let limiter = limiter.clone();
        let remaining = remaining.clone();
        let h2_pool = h2_pool.clone();
        let stages = stages.clone();
        let delay = if iterations.is_some() || stages.is_some() {
            Duration::ZERO
        } else {
            start_delay(index, vu_count, ramp_up)
        };

        handles.push(tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let mut vars: HashMap<String, String> = HashMap::new();

            if let Some(pool) = &h2_pool {
                if let Some(stages) = &stages {
                    let t0 = Instant::now();
                    while Instant::now() < deadline {
                        if !is_active(stages, t0, index) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            continue;
                        }
                        flow_request_pool(pool, &steps, &mut vars, &limiter, &metrics).await;
                    }
                } else if let Some(remaining) = &remaining {
                    while acquire_iteration(remaining) {
                        flow_request_pool(pool, &steps, &mut vars, &limiter, &metrics).await;
                    }
                } else {
                    while Instant::now() < deadline {
                        flow_request_pool(pool, &steps, &mut vars, &limiter, &metrics).await;
                    }
                }
            } else {
                let mut conn = Connection::new(&base_url, insecure).await;
                if let Some(stages) = &stages {
                    let t0 = Instant::now();
                    while Instant::now() < deadline {
                        if !is_active(stages, t0, index) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            continue;
                        }
                        flow_request_conn(&mut conn, &steps, &mut vars, &limiter, &metrics).await;
                    }
                } else if let Some(remaining) = &remaining {
                    while acquire_iteration(remaining) {
                        flow_request_conn(&mut conn, &steps, &mut vars, &limiter, &metrics).await;
                    }
                } else {
                    while Instant::now() < deadline {
                        flow_request_conn(&mut conn, &steps, &mut vars, &limiter, &metrics).await;
                    }
                }
            }
        }));
    }

    handles
}

// ----------------------------------------------------------------------
// Variable substitution + extraction
// ----------------------------------------------------------------------

fn substitute(template: &str, vars: &HashMap<String, String>) -> String {
    if !template.contains('$') {
        return template.to_string();
    }

    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(idx) = rest.find("${") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                out.push_str(vars.get(name).map(String::as_str).unwrap_or(""));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str("${");
                rest = &after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn extract_vars(
    body: &bytes::Bytes,
    extracts: &[(String, Vec<Segment>)],
    vars: &mut HashMap<String, String>,
) {
    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return,
    };

    for (name, segments) in extracts {
        if let Some(value) = walk(&parsed, segments) {
            vars.insert(name.clone(), value);
        }
    }
}

fn walk(value: &serde_json::Value, segments: &[Segment]) -> Option<String> {
    let mut cur = value;
    for seg in segments {
        cur = match seg {
            Segment::Key(k) => cur.get(k)?,
            Segment::Index(i) => cur.get(*i)?,
        };
    }

    match cur {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}