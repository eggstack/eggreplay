use eggreplay_core::{
    ComparisonPolicy as RustComparisonPolicy, RedactionConfig as RustRedactionConfig,
};
use pyo3::{prelude::*, types::PyDict};

use crate::errors::ConfigurationError;

#[pyfunction]
pub fn validate_matcher_profile(value: &str) -> PyResult<String> {
    serde_json::from_value::<eggreplay_core::MatcherProfile>(serde_json::Value::String(
        value.into(),
    ))
    .map_err(|_| ConfigurationError::new_err("unknown matcher profile"))?;
    Ok(value.to_owned())
}

#[pyfunction]
pub fn validate_consumption_mode(value: &str) -> PyResult<String> {
    serde_json::from_value::<eggreplay_core::ConsumptionMode>(serde_json::Value::String(
        value.into(),
    ))
    .map_err(|_| ConfigurationError::new_err("unknown consumption mode"))?;
    Ok(value.to_owned())
}

#[pyfunction]
pub fn validate_record_mode(
    mode: &str,
    fixture_exists: bool,
    upstream_configured: bool,
) -> PyResult<(String, bool)> {
    let mode = serde_json::from_value::<eggreplay_core::RecordMode>(serde_json::Value::String(
        mode.to_owned(),
    ))
    .map_err(|_| ConfigurationError::new_err("unknown record mode"))?;
    let policy = mode
        .resolve(fixture_exists, upstream_configured)
        .map_err(ConfigurationError::new_err)?;
    Ok((policy.mode.to_string(), policy.upstream_enabled))
}

#[pyfunction]
pub fn validate_stream_timing(value: &str) -> PyResult<String> {
    let mode =
        eggreplay_core::StreamTimingMode::parse(value).map_err(ConfigurationError::new_err)?;
    Ok(match mode {
        eggreplay_core::StreamTimingMode::Immediate => "immediate".to_owned(),
        eggreplay_core::StreamTimingMode::Recorded => "recorded".to_owned(),
        eggreplay_core::StreamTimingMode::Scaled(factor) => format!("scaled:{factor}"),
    })
}

#[pyfunction]
pub fn enum_values(kind: &str) -> PyResult<Vec<String>> {
    let values = match kind {
        "matcher_profile" => vec![
            serde_json::to_value(eggreplay_core::MatcherProfile::Strict),
            serde_json::to_value(eggreplay_core::MatcherProfile::Practical),
        ],
        "consumption_mode" => vec![
            serde_json::to_value(eggreplay_core::ConsumptionMode::Once),
            serde_json::to_value(eggreplay_core::ConsumptionMode::RepeatLast),
            serde_json::to_value(eggreplay_core::ConsumptionMode::Unlimited),
        ],
        "record_mode" => vec![
            serde_json::to_value(eggreplay_core::RecordMode::Sealed),
            serde_json::to_value(eggreplay_core::RecordMode::Once),
            serde_json::to_value(eggreplay_core::RecordMode::AppendNew),
            serde_json::to_value(eggreplay_core::RecordMode::ReRecord),
        ],
        "websocket_redaction" => vec![
            serde_json::to_value(eggreplay_core::WebSocketRedaction::WholeMessage),
            serde_json::to_value(eggreplay_core::WebSocketRedaction::CloseReason),
        ],
        _ => return Err(ConfigurationError::new_err("unknown enum family")),
    };
    values
        .into_iter()
        .map(|value| {
            let value =
                value.map_err(|_| ConfigurationError::new_err("Rust enum serialization failed"))?;
            value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.as_str())
                .map(str::to_owned)
                .ok_or_else(|| ConfigurationError::new_err("invalid Rust enum representation"))
        })
        .collect()
}

/// Rust-validated delay policy for ordered stream playback.
#[pyclass(name = "StreamTimingMode", frozen)]
pub struct StreamTimingOptions {
    inner: eggreplay_core::StreamTimingMode,
}

#[pymethods]
impl StreamTimingOptions {
    #[new]
    fn new(value: &str) -> PyResult<Self> {
        eggreplay_core::StreamTimingMode::parse(value)
            .map(|inner| Self { inner })
            .map_err(ConfigurationError::new_err)
    }

    #[getter]
    fn value(&self) -> String {
        match self.inner {
            eggreplay_core::StreamTimingMode::Immediate => "immediate".to_owned(),
            eggreplay_core::StreamTimingMode::Recorded => "recorded".to_owned(),
            eggreplay_core::StreamTimingMode::Scaled(factor) => format!("scaled:{factor}"),
        }
    }
}

/// Rust-backed redaction selectors.
#[pyclass(name = "RedactionConfig", frozen)]
pub struct RedactionOptions {
    inner: RustRedactionConfig,
}

#[pymethods]
impl RedactionOptions {
    #[new]
    #[pyo3(signature = (headers=None, query_keys=None, json_paths=None))]
    fn new(
        headers: Option<Vec<String>>,
        query_keys: Option<Vec<String>>,
        json_paths: Option<Vec<String>>,
    ) -> Self {
        let mut inner = RustRedactionConfig::default_secure();
        if let Some(headers) = headers {
            inner.headers = headers
                .into_iter()
                .map(|item| item.to_ascii_lowercase())
                .collect();
        }
        if let Some(query_keys) = query_keys {
            inner.query_keys = query_keys.into_iter().collect();
        }
        if let Some(json_paths) = json_paths {
            inner.json_paths = json_paths.into_iter().collect();
        }
        Self { inner }
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let result = PyDict::new(py);
        result.set_item("headers", self.inner.headers.iter().collect::<Vec<_>>())?;
        result.set_item(
            "query_keys",
            self.inner.query_keys.iter().collect::<Vec<_>>(),
        )?;
        result.set_item(
            "json_paths",
            self.inner.json_paths.iter().collect::<Vec<_>>(),
        )?;
        Ok(result)
    }
}

/// Rust-validated stream, SSE, cadence, and WebSocket comparison policy.
#[pyclass(name = "ComparisonPolicy", frozen)]
pub struct ComparisonOptions {
    inner: RustComparisonPolicy,
}

#[pymethods]
impl ComparisonOptions {
    #[new]
    #[pyo3(signature = (compare_stream_events=false, cadence_tolerance_ns=None, compare_sse=false, sse_ignored=None, websocket_cadence_tolerance_ns=None))]
    fn new(
        compare_stream_events: bool,
        cadence_tolerance_ns: Option<u64>,
        compare_sse: bool,
        sse_ignored: Option<Vec<String>>,
        websocket_cadence_tolerance_ns: Option<u64>,
    ) -> PyResult<Self> {
        let inner = RustComparisonPolicy {
            compare_stream_events,
            cadence_tolerance_ns,
            compare_sse,
            sse_ignored: sse_ignored.unwrap_or_default(),
            websocket_cadence_tolerance_ns,
        };
        inner.validate().map_err(ConfigurationError::new_err)?;
        Ok(Self { inner })
    }

    #[getter]
    fn compare_stream_events(&self) -> bool {
        self.inner.compare_stream_events
    }

    #[getter]
    fn compare_sse(&self) -> bool {
        self.inner.compare_sse
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let result = PyDict::new(py);
        result.set_item("compare_stream_events", self.inner.compare_stream_events)?;
        result.set_item("cadence_tolerance_ns", self.inner.cadence_tolerance_ns)?;
        result.set_item("compare_sse", self.inner.compare_sse)?;
        result.set_item("sse_ignored", self.inner.sse_ignored.clone())?;
        result.set_item(
            "websocket_cadence_tolerance_ns",
            self.inner.websocket_cadence_tolerance_ns,
        )?;
        Ok(result)
    }
}

/// Typed direct or Eggress route selection, with mutually exclusive target fields.
#[pyclass(name = "RouteSpecification", frozen)]
pub struct RouteSpecification {
    kind: String,
    target: Option<String>,
}

#[pymethods]
impl RouteSpecification {
    #[new]
    #[pyo3(signature = (kind, target=None))]
    fn new(kind: &str, target: Option<String>) -> PyResult<Self> {
        match (kind, target.as_deref()) {
            ("direct", None) => Ok(Self {
                kind: kind.into(),
                target,
            }),
            ("eggress", Some(target)) if !target.trim().is_empty() => {
                eggreplay_http::parse_route(target)
                    .map_err(ConfigurationError::new_err)?
                    .ok_or_else(|| {
                        ConfigurationError::new_err("route target must select Eggress")
                    })?;
                Ok(Self {
                    kind: kind.into(),
                    target: Some(target.into()),
                })
            }
            ("direct", Some(_)) => Err(ConfigurationError::new_err(
                "direct routes do not accept a target",
            )),
            ("eggress", None | Some(_)) => Err(ConfigurationError::new_err(
                "Eggress routes require a non-empty target",
            )),
            _ => Err(ConfigurationError::new_err(
                "route kind must be direct or eggress",
            )),
        }
    }

    #[getter]
    fn kind(&self) -> &str {
        &self.kind
    }

    #[getter]
    fn target(&self) -> Option<String> {
        self.target
            .as_deref()
            .map(eggreplay_http::redact_route_credentials)
    }

    fn to_dict(&self) -> (&str, Option<String>) {
        (&self.kind, self.target())
    }
}

/// Opt-in WebSocket capture and comparison controls.
#[pyclass(name = "WebSocketOptions", frozen)]
pub struct WebSocketOptions {
    recording_enabled: bool,
    cadence_tolerance_ns: Option<u64>,
    redaction: Option<eggreplay_core::WebSocketRedaction>,
}

#[pymethods]
impl WebSocketOptions {
    #[new]
    #[pyo3(signature = (recording_enabled=false, cadence_tolerance_ns=None, redaction_mode="none"))]
    fn new(
        recording_enabled: bool,
        cadence_tolerance_ns: Option<u64>,
        redaction_mode: &str,
    ) -> PyResult<Self> {
        let redaction = if redaction_mode == "none" {
            None
        } else {
            Some(
                serde_json::from_value(serde_json::json!({"kind": redaction_mode})).map_err(
                    |_| ConfigurationError::new_err("unsupported WebSocket redaction mode"),
                )?,
            )
        };
        if cadence_tolerance_ns == Some(0) {
            return Err(ConfigurationError::new_err(
                "WebSocket cadence tolerance must be positive",
            ));
        }
        Ok(Self {
            recording_enabled,
            cadence_tolerance_ns,
            redaction,
        })
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let result = PyDict::new(py);
        result.set_item("recording_enabled", self.recording_enabled)?;
        result.set_item("cadence_tolerance_ns", self.cadence_tolerance_ns)?;
        let redaction_mode = self.redaction.as_ref().map_or_else(
            || "none".to_owned(),
            |redaction| {
                serde_json::to_value(redaction)
                    .ok()
                    .and_then(|value| value.get("kind")?.as_str().map(str::to_owned))
                    .unwrap_or_default()
            },
        );
        result.set_item("redaction_mode", redaction_mode)?;
        Ok(result)
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(enum_values, module)?)?;
    module.add_function(wrap_pyfunction!(validate_matcher_profile, module)?)?;
    module.add_function(wrap_pyfunction!(validate_consumption_mode, module)?)?;
    module.add_function(wrap_pyfunction!(validate_record_mode, module)?)?;
    module.add_function(wrap_pyfunction!(validate_stream_timing, module)?)?;
    module.add_class::<RedactionOptions>()?;
    module.add_class::<StreamTimingOptions>()?;
    module.add_class::<ComparisonOptions>()?;
    module.add_class::<RouteSpecification>()?;
    module.add_class::<WebSocketOptions>()?;
    Ok(())
}
