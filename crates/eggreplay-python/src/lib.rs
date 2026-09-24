//! `CPython` adapter for `EggReplay`'s Rust authorities.

use pyo3::prelude::*;

/// Return the `EggReplay` crate version as a small ABI/import smoke value.
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Await a Rust future through the process-wide Tokio bridge.
#[pyfunction]
fn async_value(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async { Ok::<_, PyErr>(42_u32) })
}

/// Wait asynchronously for `seconds`; cancellation drops the Rust future.
#[pyfunction]
fn async_sleep(py: Python<'_>, seconds: f64) -> PyResult<Bound<'_, PyAny>> {
    let duration = std::time::Duration::from_secs_f64(seconds.max(0.0));
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        tokio::time::sleep(duration).await;
        Ok::<_, PyErr>(())
    })
}

/// Private native `EggReplay` extension.
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(version, module)?)?;
    module.add_function(wrap_pyfunction!(async_value, module)?)?;
    module.add_function(wrap_pyfunction!(async_sleep, module)?)?;
    Ok(())
}
