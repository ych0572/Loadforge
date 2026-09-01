use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use dashmap::DashMap;

use crate::http::{CheckFailure, ConnError};

/// Percentiles reported in the result, in ascending order.
pub const PERCENTILES: [u64; 5] = [50, 75, 90, 95, 99];

/// Keep at most this many assertion-failure samples in the result.
const MAX_CHECK_FAILURES: usize = 100;

/// Default per-second bucket count when the duration is unknown (iterations mode).
const DEFAULT_TIME_SERIES_CAP: usize = 3600;

/// One second of sampled metrics.
struct SecondBucket {
    requests: AtomicU64,
    failed: AtomicU64,
    bytes: AtomicU64,
    sum_latency_us: AtomicU64,
    latency_count: AtomicU64,
}

impl SecondBucket {
    fn new() -> Self {
        Self {
            requests: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            sum_latency_us: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
        }
    }
}

/// Lock-free metrics collector.
///
/// AtomicU64 for counters, DashMap for status codes / latency buckets, and a
/// pre-allocated Vec of per-second atomic buckets for time-series sampling.
/// No locks in the hot path. Assertion failures are rare, so their bounded
/// sample is guarded by a plain mutex (not touched on the success path).
pub struct Metrics {
    pub total: AtomicU64,
    pub success: AtomicU64,
    pub failed: AtomicU64,
    pub bytes: AtomicU64,
    pub min_latency_us: AtomicU64,
    pub max_latency_us: AtomicU64,
    pub sum_latency_us: AtomicU64,
    pub checks_passed: AtomicU64,
    pub checks_failed: AtomicU64,
    pub status_codes: DashMap<u16, u64>,
    pub latency_buckets: DashMap<u64, u64>,
    pub check_failures: Mutex<Vec<CheckFailure>>,
    pub errors: DashMap<String, u64>,
    seconds: Vec<SecondBucket>,
    pub start: Instant,
}

impl Metrics {
    pub fn new(duration_secs: u64) -> Self {
        let cap = if duration_secs > 0 {
            duration_secs as usize + 2
        } else {
            DEFAULT_TIME_SERIES_CAP
        }
        .max(1);
        Self {
            total: AtomicU64::new(0),
            success: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            min_latency_us: AtomicU64::new(u64::MAX),
            max_latency_us: AtomicU64::new(0),
            sum_latency_us: AtomicU64::new(0),
            checks_passed: AtomicU64::new(0),
            checks_failed: AtomicU64::new(0),
            status_codes: DashMap::new(),
            latency_buckets: DashMap::new(),
            check_failures: Mutex::new(Vec::new()),
            errors: DashMap::new(),
            seconds: (0..cap).map(|_| SecondBucket::new()).collect(),
            start: Instant::now(),
        }
    }

    #[inline]
    fn second_index(&self) -> usize {
        let s = self.start.elapsed().as_secs() as usize;
        s.min(self.seconds.len() - 1)
    }

    /// Record one request. Called on every response, must be fast.
    pub fn record(
        &self,
        latency_us: u64,
        status: u16,
        bytes: u64,
        is_success: bool,
        error: Option<ConnError>,
    ) {
        self.total.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.min_latency_us.fetch_min(latency_us, Ordering::Relaxed);
        self.max_latency_us.fetch_max(latency_us, Ordering::Relaxed);
        self.sum_latency_us.fetch_add(latency_us, Ordering::Relaxed);

        if is_success {
            self.success.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }

        let bucket_ms = latency_us / 1000;
        *self.latency_buckets.entry(bucket_ms).or_insert(0) += 1;
        *self.status_codes.entry(status).or_insert(0) += 1;

        if let Some(e) = error {
            *self.errors.entry(e.as_str().to_string()).or_insert(0) += 1;
        }

        let b = &self.seconds[self.second_index()];
        b.requests.fetch_add(1, Ordering::Relaxed);
        if !is_success {
            b.failed.fetch_add(1, Ordering::Relaxed);
        }
        b.bytes.fetch_add(bytes, Ordering::Relaxed);
        b.sum_latency_us.fetch_add(latency_us, Ordering::Relaxed);
        b.latency_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record assertion results for one request.
    pub fn record_checks(&self, passed: usize, failures: &[CheckFailure]) {
        if passed == 0 && failures.is_empty() {
            return;
        }
        self.checks_passed.fetch_add(passed as u64, Ordering::Relaxed);
        self.checks_failed.fetch_add(failures.len() as u64, Ordering::Relaxed);

        if !failures.is_empty() {
            let mut guard = self.check_failures.lock().unwrap();
            for f in failures {
                if guard.len() >= MAX_CHECK_FAILURES {
                    break;
                }
                guard.push(f.clone());
            }
        }
    }

    /// Record one received SSE event.
    pub fn record_event(&self, bytes: u64) {
        self.total.fetch_add(1, Ordering::Relaxed);
        self.success.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);

        let b = &self.seconds[self.second_index()];
        b.requests.fetch_add(1, Ordering::Relaxed);
        b.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Record a connection-level error for stream workloads (SSE / WS).
    pub fn record_conn_error(&self, error: ConnError) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        *self.errors.entry(error.as_str().to_string()).or_insert(0) += 1;

        let b = &self.seconds[self.second_index()];
        b.failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Build result snapshot. Called once after test completes.
    pub fn snapshot(&self) -> MetricsResult {
        let elapsed = self.start.elapsed().as_secs_f64();
        let total = self.total.load(Ordering::Relaxed);
        let success = self.success.load(Ordering::Relaxed);
        let failed = self.failed.load(Ordering::Relaxed);
        let bytes = self.bytes.load(Ordering::Relaxed);

        let rps = if elapsed > 0.0 { total as f64 / elapsed } else { 0.0 };

        let min_ms = if total > 0 {
            self.min_latency_us.load(Ordering::Relaxed) as f64 / 1000.0
        } else {
            0.0
        };
        let max_ms = if total > 0 {
            self.max_latency_us.load(Ordering::Relaxed) as f64 / 1000.0
        } else {
            0.0
        };
        let avg_ms = if total > 0 {
            self.sum_latency_us.load(Ordering::Relaxed) as f64 / total as f64 / 1000.0
        } else {
            0.0
        };

        let status_codes: BTreeMap<u16, u64> = self
            .status_codes
            .iter()
            .map(|entry| (*entry.key(), *entry.value()))
            .collect();

        let latency_dist: BTreeMap<u64, u64> = self
            .latency_buckets
            .iter()
            .map(|entry| (*entry.key(), *entry.value()))
            .collect();

        let values = compute_percentiles(&latency_dist, total, &PERCENTILES);
        let mut percentiles = BTreeMap::new();
        for (&p, &v) in PERCENTILES.iter().zip(values.iter()) {
            percentiles.insert(p, v);
        }

        let p50_ms = percentiles[&50];
        let p95_ms = percentiles[&95];
        let p99_ms = percentiles[&99];

        let checks_passed = self.checks_passed.load(Ordering::Relaxed);
        let checks_failed = self.checks_failed.load(Ordering::Relaxed);
        let check_failures = self.check_failures.lock().unwrap().clone();

        let errors: BTreeMap<String, u64> = self
            .errors
            .iter()
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect();

        let time_series = self.build_time_series();

        MetricsResult {
            total,
            success,
            failed,
            elapsed_secs: elapsed,
            rps,
            bytes,
            min_ms,
            max_ms,
            avg_ms,
            status_codes,
            percentiles,
            p50_ms,
            p95_ms,
            p99_ms,
            checks_passed,
            checks_failed,
            check_failures,
            errors,
            time_series,
        }
    }

    fn build_time_series(&self) -> Vec<TimePoint> {
        let secs = self.start.elapsed().as_secs() as usize + 1;
        let n = secs.min(self.seconds.len());
        let mut out = Vec::with_capacity(n);

        for i in 0..n {
            let b = &self.seconds[i];
            let requests = b.requests.load(Ordering::Relaxed);
            let failed = b.failed.load(Ordering::Relaxed);
            let bytes = b.bytes.load(Ordering::Relaxed);
            let latency_count = b.latency_count.load(Ordering::Relaxed);
            let avg_ms = if latency_count > 0 {
                b.sum_latency_us.load(Ordering::Relaxed) as f64 / latency_count as f64 / 1000.0
            } else {
                0.0
            };

            out.push(TimePoint {
                t: i as u64,
                requests,
                success: requests.saturating_sub(failed),
                failed,
                bytes,
                avg_ms,
            });
        }

        out
    }
}

/// One point in the per-second time series.
#[derive(Debug, Clone)]
pub struct TimePoint {
    /// Seconds since test start (0-based).
    pub t: u64,
    pub requests: u64,
    pub success: u64,
    pub failed: u64,
    pub bytes: u64,
    pub avg_ms: f64,
}

/// Metrics result returned to Python.
#[derive(Debug)]
pub struct MetricsResult {
    pub total: u64,
    pub success: u64,
    pub failed: u64,
    pub elapsed_secs: f64,
    pub rps: f64,
    pub bytes: u64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub avg_ms: f64,
    pub status_codes: BTreeMap<u16, u64>,
    pub percentiles: BTreeMap<u64, f64>,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub checks_passed: u64,
    pub checks_failed: u64,
    pub check_failures: Vec<CheckFailure>,
    pub errors: BTreeMap<String, u64>,
    pub time_series: Vec<TimePoint>,
}

fn compute_percentiles(buckets: &BTreeMap<u64, u64>, total: u64, percents: &[u64]) -> Vec<f64> {
    if total == 0 {
        return vec![0.0; percents.len()];
    }

    let targets: Vec<u64> = percents.iter().map(|&p| (total * p) / 100).collect();
    let mut results: Vec<Option<f64>> = vec![None; percents.len()];
    let mut remaining = percents.len();
    let mut cumulative = 0u64;

    for (&bucket_ms, &count) in buckets {
        cumulative += count;
        for (i, &target) in targets.iter().enumerate() {
            if results[i].is_none() && cumulative >= target {
                results[i] = Some(bucket_ms as f64);
                remaining -= 1;
            }
        }
        if remaining == 0 {
            break;
        }
    }

    results.into_iter().map(|o| o.unwrap_or(0.0)).collect()
}