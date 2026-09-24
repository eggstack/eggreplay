//! `CPython` adapter for `EggReplay`'s Rust authorities.

use pyo3::prelude::*;

mod config;
mod errors;
mod fixture;
mod lifecycle;
mod report;

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
    config::register(module)?;
    lifecycle::register(module)?;
    for class in [
        module.add_class::<fixture::PyFixture>(),
        module.add_class::<fixture::FlowView>(),
        module.add_class::<fixture::RequestView>(),
        module.add_class::<fixture::ResponseView>(),
        module.add_class::<fixture::FlowErrorView>(),
        module.add_class::<fixture::PyFlowIterator>(),
        module.add_class::<fixture::BodyReader>(),
    ] {
        class?;
    }
    module.add(
        "EggReplayError",
        module.py().get_type::<errors::EggReplayError>(),
    )?;
    module.add(
        "FixtureError",
        module.py().get_type::<errors::FixtureError>(),
    )?;
    module.add("MatchError", module.py().get_type::<errors::MatchError>())?;
    module.add(
        "NetworkError",
        module.py().get_type::<errors::NetworkError>(),
    )?;
    module.add(
        "ConfigurationError",
        module.py().get_type::<errors::ConfigurationError>(),
    )?;
    report::register(module)?;
    Ok(())
}
