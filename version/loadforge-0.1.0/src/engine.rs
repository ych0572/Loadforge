use std::collections::HashMap;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};

use crate::http::{CheckFailure, ExpectSpec};
use crate::metrics::{Metrics, TimePoint};
use crate::scheduler::{self, Endpoint, FlowStep, Segment, Stages};
use crate::sse::{self, SseConfig};
use crate::ws::{self, WsConfig};

/// Core execution engine.
pub struct Engine {
    base_url: String,
    insecure: bool,
    http2: bool,
    workload: Workload,
    vu_count: u32,
    duration_secs: u64,
    iterations: Option<u64>,
    stages: Option<Arc<Stages>>,
    ramp_up: f64,
    rps: Option<f64>,
}

enum Workload {
    Endpoints(Vec<Endpoint>),
    Flow(Vec<FlowStep>),
    Sse(SseConfig),
    Ws(WsConfig),
}

impl Engine {
    pub fn new(plan: &Bound<'_, PyDict>) -> PyResult<Self> {
        let stages_parsed: Option<Vec<(f64, u32)>> = parse_stages(plan)?;

        let iterations: Option<u64> = plan
            .get_item("iterations")?
            .and_then(|v| v.extract().ok())
            .filter(|&i| i > 0);

        if stages_parsed.is_some() && iterations.is_some() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "stages and iterations are mutually exclusive",
            ));
        }

        let (vu_count, duration_secs, stages) = match stages_parsed {
            Some(list) if !list.is_empty() => {
                let s = Stages::new(&list);
                (s.max_target.max(1), s.total_duration.ceil() as u64, Some(Arc::new(s)))
            }
            _ => {
                let vu: u32 = plan
                    .get_item("vu")?
                    .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("missing vu"))?
                    .extract()?;
                let dur: u64 = match plan.get_item("duration")? {
                    Some(v) if !v.is_none() => v.extract()?,
                    _ => 0,
                };
                if dur == 0 && iterations.is_none() {
                    return Err(pyo3::exceptions::PyValueError::new_err(
                        "missing duration, iterations, or stages",
                    ));
                }
                (vu, dur, None)
            }
        };

        let insecure: bool = match plan.get_item("insecure")? {
            Some(v) => v.extract().unwrap_or(false),
            None => false,
        };

        let http2: bool = plan
            .get_item("http2")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(false);

        let ramp_up: f64 = plan
            .get_item("ramp_up")?
            .and_then(|v| extract_number(&v))
            .unwrap_or(0.0);

        let rps: Option<f64> = plan.get_item("rps")?.and_then(|v| extract_number(&v));

        let sse = parse_sse(plan)?;
        let ws = parse_ws(plan)?;
        let is_ws = ws.is_some();

        let workload = if let Some(flow) = plan.get_item("flow")? {
            let steps: Vec<Bound<'_, PyDict>> = flow.extract()?;
            Workload::Flow(parse_flow(steps)?)
        } else if let Some(endpoints_py) = plan.get_item("endpoints")? {
            let endpoints_py: Vec<Bound<'_, PyDict>> = endpoints_py.extract()?;
            Workload::Endpoints(parse_endpoints(endpoints_py)?)
        } else if let Some(s) = sse {
            Workload::Sse(s)
        } else if let Some(w) = ws {
            Workload::Ws(w)
        } else {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "missing endpoints, flow, sse, or ws",
            ));
        };

        if duration_secs == 0 && matches!(workload, Workload::Sse(_) | Workload::Ws(_)) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "sse/ws workloads require duration",
            ));
        }
        if stages.is_some() && matches!(workload, Workload::Sse(_) | Workload::Ws(_)) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "stages only apply to endpoints/flow workloads",
            ));
        }

        let base_url: String = if is_ws {
            plan.get_item("base_url")?
                .and_then(|v| v.extract().ok())
                .unwrap_or_default()
        } else {
            plan.get_item("base_url")?
                .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("missing base_url"))?
                .extract()?
        };

        Ok(Self {
            base_url,
            insecure,
            http2,
            workload,
            vu_count,
            duration_secs,
            iterations,
            stages,
            ramp_up,
            rps,
        })
    }

    pub async fn run(&self) -> PyResult<RunResult> {
        let metrics = Arc::new(Metrics::new(self.duration_secs));

        let handles = match &self.workload {
            Workload::Endpoints(endpoints) => scheduler::spawn_endpoint_vus(
                self.vu_count,
                self.base_url.clone(),
                endpoints.clone(),
                Arc::clone(&metrics),
                self.duration_secs,
                self.iterations,
                self.stages.clone(),
                self.insecure,
                self.http2,
                self.ramp_up,
                self.rps,
            ),
            Workload::Flow(steps) => scheduler::spawn_flow_vus(
                self.vu_count,
                self.base_url.clone(),
                steps.clone(),
                Arc::clone(&metrics),
                self.duration_secs,
                self.iterations,
                self.stages.clone(),
                self.insecure,
                self.http2,
                self.ramp_up,
                self.rps,
            ),
            Workload::Sse(s) => sse::spawn_sse_vus(
                self.vu_count,
                self.base_url.clone(),
                s.clone(),
                Arc::clone(&metrics),
                self.duration_secs,
                self.insecure,
                self.ramp_up,
            ),
            Workload::Ws(w) => ws::spawn_ws_vus(
                self.vu_count,
                w.clone(),
                Arc::clone(&metrics),
                self.duration_secs,
                self.insecure,
                self.ramp_up,
            ),
        };

        for handle in handles {
            let _ = handle.await;
        }

        let snapshot = metrics.snapshot();

        Ok(RunResult {
            total: snapshot.total,
            success: snapshot.success,
            failed: snapshot.failed,
            elapsed_secs: snapshot.elapsed_secs,
            rps: snapshot.rps,
            bytes: snapshot.bytes,
            min_ms: snapshot.min_ms,
            max_ms: snapshot.max_ms,
            avg_ms: snapshot.avg_ms,
            p50_ms: snapshot.p50_ms,
            p95_ms: snapshot.p95_ms,
            p99_ms: snapshot.p99_ms,
            percentiles: snapshot.percentiles,
            status_codes: snapshot.status_codes,
            checks_passed: snapshot.checks_passed,
            checks_failed: snapshot.checks_failed,
            check_failures: snapshot.check_failures,
            errors: snapshot.errors,
            time_series: snapshot.time_series,
        })
    }
}

// ----------------------------------------------------------------------
// Plan parsing helpers
// ----------------------------------------------------------------------

fn extract_number(v: &Bound<'_, PyAny>) -> Option<f64> {
    v.extract::<f64>()
        .or_else(|_| v.extract::<i64>().map(|i| i as f64))
        .or_else(|_| v.extract::<u64>().map(|u| u as f64))
        .ok()
}

fn parse_endpoints(endpoints_py: Vec<Bound<'_, PyDict>>) -> PyResult<Vec<Endpoint>> {
    let mut endpoints = Vec::with_capacity(endpoints_py.len());
    for ep in endpoints_py {
        let method: String = get_string(&ep, "method")?.unwrap_or_else(|| "GET".to_string());
        let path: String = required_string(&ep, "path")?;
        let weight: u32 = ep
            .get_item("weight")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(1);
        let body = parse_body(&ep)?;
        let headers = parse_headers(&ep)?;
        let expect = parse_expect(&ep)?;

        endpoints.push(Endpoint {
            method: method.to_uppercase(),
            path,
            weight,
            body,
            headers,
            expect,
        });
    }
    Ok(endpoints)
}

fn parse_flow(steps: Vec<Bound<'_, PyDict>>) -> PyResult<Vec<FlowStep>> {
    let mut flow = Vec::with_capacity(steps.len());
    for step in steps {
        let method: String = get_string(&step, "method")?.unwrap_or_else(|| "GET".to_string());
        let path: String = required_string(&step, "path")?;
        let headers = parse_headers(&step)?;
        let body = parse_body(&step)?;
        let extracts = parse_extracts(&step)?;
        let expect = parse_expect(&step)?;

        flow.push(FlowStep {
            method: method.to_uppercase(),
            path,
            headers,
            body,
            extracts,
            expect,
        });
    }
    Ok(flow)
}

fn parse_expect(dict: &Bound<'_, PyDict>) -> PyResult<Option<ExpectSpec>> {
    match dict.get_item("expect")? {
        Some(v) if !v.is_none() => {
            let d: &Bound<'_, PyDict> = v
                .downcast()
                .map_err(|_| pyo3::exceptions::PyValueError::new_err("expect must be a dict"))?;
            let status: Option<u16> = d.get_item("status")?.and_then(|x| x.extract().ok());
            let body_contains: Option<String> = get_string(d, "body_contains")?;
            let json: Option<serde_json::Value> = d.get_item("json")?.map(|x| py_to_json(&x));
            Ok(Some(ExpectSpec {
                status,
                body_contains,
                json,
            }))
        }
        _ => Ok(None),
    }
}

/// Convert an arbitrary Python object into a serde_json::Value.
fn py_to_json(v: &Bound<'_, PyAny>) -> serde_json::Value {
    if v.is_none() {
        return serde_json::Value::Null;
    }
    if let Ok(d) = v.downcast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (k, val) in d {
            let key: String = k.extract().unwrap_or_default();
            map.insert(key, py_to_json(&val));
        }
        return serde_json::Value::Object(map);
    }
    if let Ok(l) = v.downcast::<PyList>() {
        return serde_json::Value::Array(l.iter().map(|x| py_to_json(&x)).collect());
    }
    if let Ok(b) = v.extract::<bool>() {
        return serde_json::Value::Bool(b);
    }
    if let Ok(i) = v.extract::<i64>() {
        return serde_json::Value::Number(i.into());
    }
    if let Ok(u) = v.extract::<u64>() {
        return serde_json::Value::Number(u.into());
    }
    if let Ok(f) = v.extract::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return serde_json::Value::Number(n);
        }
    }
    if let Ok(s) = v.extract::<String>() {
        return serde_json::Value::String(s);
    }
    serde_json::Value::Null
}

fn parse_stages(plan: &Bound<'_, PyDict>) -> PyResult<Option<Vec<(f64, u32)>>> {
    match plan.get_item("stages")? {
        Some(v) if !v.is_none() => {
            let list: &Bound<'_, PyList> = v
                .downcast()
                .map_err(|_| pyo3::exceptions::PyValueError::new_err("stages must be a list"))?;
            let mut out = Vec::with_capacity(list.len());
            for item in list.iter() {
                let d: &Bound<'_, PyDict> = item
                    .downcast()
                    .map_err(|_| pyo3::exceptions::PyValueError::new_err("each stage must be a dict"))?;
                let duration: f64 = d
                    .get_item("duration")?
                    .and_then(|x| extract_number(&x))
                    .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("stage missing duration"))?;
                let target: u32 = d
                    .get_item("target")?
                    .and_then(|x| x.extract().ok())
                    .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("stage missing target"))?;
                out.push((duration, target));
            }
            Ok(Some(out))
        }
        _ => Ok(None),
    }
}

fn parse_sse(plan: &Bound<'_, PyDict>) -> PyResult<Option<SseConfig>> {
    match plan.get_item("sse")? {
        Some(v) if !v.is_none() => {
            let d: &Bound<'_, PyDict> = v
                .downcast()
                .map_err(|_| pyo3::exceptions::PyValueError::new_err("sse must be a dict"))?;
            let path = required_string(d, "path")?;
            let headers = parse_headers(d)?;
            let expect = parse_expect(d)?;
            Ok(Some(SseConfig { path, headers, expect }))
        }
        _ => Ok(None),
    }
}

fn parse_ws(plan: &Bound<'_, PyDict>) -> PyResult<Option<WsConfig>> {
    match plan.get_item("ws")? {
        Some(v) if !v.is_none() => {
            let d: &Bound<'_, PyDict> = v
                .downcast()
                .map_err(|_| pyo3::exceptions::PyValueError::new_err("ws must be a dict"))?;
            let url = required_string(d, "url")?;
            let message = get_string(d, "message")?.unwrap_or_default();
            let mode = match get_string(d, "mode")?.as_deref() {
                Some("send") => ws::WsMode::Send,
                Some("recv") => ws::WsMode::Recv,
                _ => ws::WsMode::Echo,
            };
            let send_interval: f64 = d
                .get_item("send_interval")?
                .and_then(|v| extract_number(&v))
                .unwrap_or(0.0);
            Ok(Some(WsConfig {
                url,
                message: bytes::Bytes::from(message),
                mode,
                send_interval,
            }))
        }
        _ => Ok(None),
    }
}

fn required_string(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<String> {
    dict.get_item(key)?
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err(format!("missing {}", key)))?
        .extract()
}

fn get_string(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match dict.get_item(key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract()?)),
        _ => Ok(None),
    }
}

fn parse_body(dict: &Bound<'_, PyDict>) -> PyResult<Option<bytes::Bytes>> {
    match dict.get_item("body")? {
        Some(v) if !v.is_none() => {
            let s: String = v
                .extract()
                .map_err(|_| pyo3::exceptions::PyValueError::new_err("body must be a string"))?;
            Ok(Some(bytes::Bytes::from(s)))
        }
        _ => Ok(None),
    }
}

fn parse_headers(dict: &Bound<'_, PyDict>) -> PyResult<Vec<(String, String)>> {
    match dict.get_item("headers")? {
        Some(v) if !v.is_none() => {
            let map: HashMap<String, String> = v.extract().map_err(|_| {
                pyo3::exceptions::PyValueError::new_err("headers must be a dict of strings")
            })?;
            Ok(map.into_iter().collect())
        }
        _ => Ok(Vec::new()),
    }
}

fn parse_extracts(step: &Bound<'_, PyDict>) -> PyResult<Vec<(String, Vec<Segment>)>> {
    match step.get_item("extract")? {
        Some(v) if !v.is_none() => {
            let d: &Bound<'_, PyDict> = v
                .downcast()
                .map_err(|_| pyo3::exceptions::PyValueError::new_err("extract must be a dict"))?;
            let mut out = Vec::with_capacity(d.len());
            for (k, val) in d.iter() {
                let name: String = k.extract()?;
                let path: String = val.extract()?;
                out.push((name, scheduler::parse_extract_path(&path)));
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// Run result returned to Python as a dict.
pub struct RunResult {
    pub total: u64,
    pub success: u64,
    pub failed: u64,
    pub elapsed_secs: f64,
    pub rps: f64,
    pub bytes: u64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub percentiles: std::collections::BTreeMap<u64, f64>,
    pub status_codes: std::collections::BTreeMap<u16, u64>,
    pub checks_passed: u64,
    pub checks_failed: u64,
    pub check_failures: Vec<CheckFailure>,
    pub errors: std::collections::BTreeMap<String, u64>,
    pub time_series: Vec<TimePoint>,
}

impl RunResult {
    pub fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new_bound(py);

        dict.set_item("total", self.total)?;
        dict.set_item("success", self.success)?;
        dict.set_item("failed", self.failed)?;
        dict.set_item("elapsed_secs", self.elapsed_secs)?;
        dict.set_item("rps", self.rps)?;
        dict.set_item("bytes", self.bytes)?;
        dict.set_item("min_ms", self.min_ms)?;
        dict.set_item("max_ms", self.max_ms)?;
        dict.set_item("avg_ms", self.avg_ms)?;
        dict.set_item("p50_ms", self.p50_ms)?;
        dict.set_item("p95_ms", self.p95_ms)?;
        dict.set_item("p99_ms", self.p99_ms)?;

        let percentiles_dict = PyDict::new_bound(py);
        for (&p, &v) in &self.percentiles {
            percentiles_dict.set_item(p, v)?;
        }
        dict.set_item("percentiles", percentiles_dict)?;

        let status_dict = PyDict::new_bound(py);
        for (&code, &count) in &self.status_codes {
            status_dict.set_item(code, count)?;
        }
        dict.set_item("status_codes", status_dict)?;

        dict.set_item("checks_passed", self.checks_passed)?;
        dict.set_item("checks_failed", self.checks_failed)?;

        let failures = PyList::empty_bound(py);
        for f in &self.check_failures {
            let d = PyDict::new_bound(py);
            d.set_item("method", &f.method)?;
            d.set_item("path", &f.path)?;
            d.set_item("check", &f.check)?;
            d.set_item("expected", &f.expected)?;
            d.set_item("actual", &f.actual)?;
            failures.append(d)?;
        }
        dict.set_item("check_failures", failures)?;

        let ts = PyList::empty_bound(py);
        for p in &self.time_series {
            let d = PyDict::new_bound(py);
            d.set_item("t", p.t)?;
            d.set_item("requests", p.requests)?;
            d.set_item("success", p.success)?;
            d.set_item("failed", p.failed)?;
            d.set_item("bytes", p.bytes)?;
            d.set_item("avg_ms", p.avg_ms)?;
            ts.append(d)?;
        }
        dict.set_item("time_series", ts)?;

        let errors_dict = PyDict::new_bound(py);
        for (k, v) in &self.errors {
            errors_dict.set_item(k, v)?;
        }
        dict.set_item("errors", errors_dict)?;

        Ok(dict)
    }
}