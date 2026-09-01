use pyo3::prelude::*;
use pyo3::types::PyDict;

mod engine;
mod http;
mod metrics;
mod scheduler;
mod sse;
mod ws;

/// Run a load test from a plan dict.
///
/// This is the only function exposed to Python.
/// Python defines *what* to test, Rust decides *how* to run it.
///
/// Plan shapes (one workload per plan):
/// - `endpoints`: weighted single requests (HTTP/1.1 or HTTP/2).
/// - `flow`: a business flow of ordered steps.
/// - `sse`: a Server-Sent Events stream.
/// - `ws`: a WebSocket echo workload.
/// `base_url` may be `http://` or `https://`; set `"insecure": true` to skip TLS
/// verification; set `"http2": true` to use HTTP/2.
#[pyfunction]
fn run<'py>(py: Python<'py>, plan: Bound<'py, PyDict>) -> PyResult<Bound<'py, PyDict>> {
    let engine = engine::Engine::new(&plan)?;

    // Create a dedicated Tokio runtime for this test run.
    // This keeps the runtime lifecycle tied to the test, not the Python process.
    //
    // Use all available cores (detected at runtime) so the engine can
    // actually saturate the machine - system resources are the real ceiling.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    // Release the GIL during execution. Python does nothing while Rust runs.
    let result = py.allow_threads(|| runtime.block_on(engine.run()))?;

    result.to_dict(py)
}

/// Loadforge - High-performance load testing engine.
///
/// Python defines the plan, Rust runs it.
#[pymodule]
fn loadforge(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(run, m)?)?;
    Ok(())
}
