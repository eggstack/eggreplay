use std::{io::Read, path::Path, sync::Arc};

use eggreplay_core::{
    BodyRef, Flow, FlowError as SemanticError, FlowOutcome, HttpRequest, HttpResponse,
};
use eggreplay_store::{BlobHandle, FlowIter, Session, StoreError, StoreLimits};
use pyo3::{exceptions::PyValueError, prelude::*, types::PyBytes};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::errors::{FixtureError, fixture_error};

const MAX_BODY_CHUNK: usize = 8 * 1024 * 1024;

fn json_value<'py>(py: Python<'py>, value: &impl Serialize) -> PyResult<Bound<'py, PyAny>> {
    let json = serde_json::to_string(value).map_err(fixture_error)?;
    py.import("json")?.call_method1("loads", (json,))
}

/// An owned immutable flow snapshot; duplicate header and query values remain ordered sequences.
#[pyclass(name = "Flow", skip_from_py_object)]
#[derive(Clone)]
pub struct FlowView {
    flow: Flow,
}

/// Typed owned request projection with ordered duplicate fields.
#[pyclass(name = "Request")]
pub struct RequestView {
    request: HttpRequest,
}

#[pymethods]
impl RequestView {
    #[getter]
    fn method(&self) -> &str {
        &self.request.method
    }
    #[getter]
    fn scheme(&self) -> &str {
        &self.request.scheme
    }
    #[getter]
    fn authority(&self) -> &str {
        &self.request.authority
    }
    #[getter]
    fn path(&self) -> &str {
        &self.request.path
    }
    #[getter]
    fn query(&self) -> Vec<(String, String)> {
        self.request
            .query
            .iter()
            .map(|pair| (pair.key.clone(), pair.value.clone()))
            .collect()
    }
    #[getter]
    fn headers(&self) -> Vec<(String, String)> {
        self.request
            .headers
            .iter()
            .map(|item| (item.name.clone(), item.value.clone()))
            .collect()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value(py, &self.request).map(Bound::unbind)
    }
}

/// Typed owned successful response projection.
#[pyclass(name = "Response")]
pub struct ResponseView {
    response: HttpResponse,
}

#[pymethods]
impl ResponseView {
    #[getter]
    fn status(&self) -> u16 {
        self.response.status
    }
    #[getter]
    fn headers(&self) -> Vec<(String, String)> {
        self.response
            .headers
            .iter()
            .map(|item| (item.name.clone(), item.value.clone()))
            .collect()
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value(py, &self.response).map(Bound::unbind)
    }
}

/// Typed Rust semantic failure attached to a recorded flow.
#[pyclass(name = "FlowErrorInfo")]
pub struct FlowErrorView {
    error: SemanticError,
}

#[pymethods]
impl FlowErrorView {
    #[getter]
    fn category(&self) -> String {
        serde_json::to_value(self.error.category)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
    #[getter]
    fn phase(&self) -> String {
        serde_json::to_value(self.error.phase)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
    #[getter]
    fn message(&self) -> &str {
        &self.error.message
    }
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value(py, &self.error).map(Bound::unbind)
    }
}

#[pymethods]
impl FlowView {
    #[getter]
    fn id(&self) -> &str {
        &self.flow.id
    }

    #[getter]
    fn request(&self) -> RequestView {
        RequestView {
            request: self.flow.request.clone(),
        }
    }

    #[getter]
    fn response(&self) -> Option<ResponseView> {
        match &self.flow.outcome {
            FlowOutcome::Response(response) => Some(ResponseView {
                response: response.clone(),
            }),
            FlowOutcome::Error(_) => None,
        }
    }

    #[getter]
    fn error(&self) -> Option<FlowErrorView> {
        match &self.flow.outcome {
            FlowOutcome::Response(_) => None,
            FlowOutcome::Error(error) => Some(FlowErrorView {
                error: error.clone(),
            }),
        }
    }

    #[getter]
    fn outcome(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value(py, &self.flow.outcome).map(Bound::unbind)
    }

    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value(py, &self.flow).map(Bound::unbind)
    }

    fn __repr__(&self) -> String {
        format!("Flow(schema_version={})", self.flow.schema_version)
    }
}

/// Incrementally validated bounded reader for one content-addressed body.
#[pyclass(name = "BodyReader")]
pub struct BodyReader {
    _session: Arc<Session>,
    file: Option<std::fs::File>,
    length: u64,
    digest: String,
    read: u64,
    hasher: Sha256,
    closed: bool,
    verified: bool,
}

impl BodyReader {
    fn from_handle(session: Arc<Session>, handle: BlobHandle) -> Self {
        let length = handle.len();
        let digest = handle.sha256().to_owned();
        Self {
            _session: session,
            file: Some(handle.into_file()),
            length,
            digest,
            read: 0,
            hasher: Sha256::new(),
            closed: false,
            verified: false,
        }
    }

    fn read_inner(&mut self, size: usize) -> Result<Vec<u8>, StoreError> {
        if self.closed {
            return Err(StoreError::Invalid("body reader is closed".into()));
        }
        if size > MAX_BODY_CHUNK {
            return Err(StoreError::Invalid(
                "body chunk exceeds configured limit".into(),
            ));
        }
        if size == 0 || self.verified {
            return Ok(Vec::new());
        }
        let available = usize::try_from(self.length - self.read).unwrap_or(usize::MAX);
        let mut chunk = vec![0; size.min(available)];
        let count = self
            .file
            .as_mut()
            .ok_or_else(|| StoreError::Invalid("body reader is closed".into()))?
            .read(&mut chunk)?;
        chunk.truncate(count);
        self.read += count as u64;
        self.hasher.update(&chunk);
        if count == 0 || self.read == self.length {
            if self.read != self.length
                || format!("{:x}", self.hasher.clone().finalize()) != self.digest
            {
                return Err(StoreError::Integrity(self.digest.clone()));
            }
            self.verified = true;
        }
        Ok(chunk)
    }
}

#[pymethods]
impl BodyReader {
    #[getter]
    fn length(&self) -> u64 {
        self.length
    }

    #[getter]
    fn sha256(&self) -> &str {
        &self.digest
    }

    fn read<'py>(&mut self, py: Python<'py>, size: usize) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = py.detach(|| self.read_inner(size)).map_err(fixture_error)?;
        Ok(PyBytes::new(py, &bytes))
    }

    fn read_all<'py>(
        &mut self,
        py: Python<'py>,
        max_bytes: Option<u64>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        if self.read != 0 {
            return Err(PyValueError::new_err("read_all requires an unread body"));
        }
        let cap = max_bytes.unwrap_or(1024 * 1024);
        if self.length > cap {
            return Err(PyValueError::new_err("body exceeds read_all byte limit"));
        }
        let capacity = usize::try_from(self.length)
            .map_err(|_| PyValueError::new_err("body is too large for this interpreter"))?;
        let mut result = Vec::with_capacity(capacity);
        while (result.len() as u64) < self.length {
            let remaining =
                usize::try_from((self.length - result.len() as u64).min(MAX_BODY_CHUNK as u64))
                    .map_err(|_| PyValueError::new_err("body chunk exceeds interpreter limits"))?;
            let chunk = py
                .detach(|| self.read_inner(remaining))
                .map_err(fixture_error)?;
            if chunk.is_empty() {
                break;
            }
            result.extend_from_slice(&chunk);
        }
        if result.len() as u64 != self.length {
            return Err(FixtureError::new_err("body integrity validation failed"));
        }
        Ok(PyBytes::new(py, &result))
    }

    fn close(&mut self) {
        let _ = self.file.take();
        self.closed = true;
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __iter__(slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyBytes>>> {
        if self.verified {
            return Ok(None);
        }
        let size = (self.length - self.read).min(64 * 1024) as usize;
        let chunk = py.detach(|| self.read_inner(size)).map_err(fixture_error)?;
        if chunk.is_empty() {
            Ok(None)
        } else {
            Ok(Some(PyBytes::new(py, &chunk).unbind()))
        }
    }

    fn __exit__(
        &mut self,
        _exception_type: Option<Bound<'_, PyAny>>,
        _exception: Option<Bound<'_, PyAny>>,
        _traceback: Option<Bound<'_, PyAny>>,
    ) {
        self.close();
    }
}

/// Lazy ordered flow iterator retaining the validated fixture session.
#[pyclass(name = "FlowIterator")]
pub struct PyFlowIterator {
    iter: FlowIter,
}

#[pymethods]
impl PyFlowIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<FlowView>> {
        py.detach(|| self.iter.next())
            .transpose()
            .map(|flow| flow.map(|flow| FlowView { flow }))
            .map_err(fixture_error)
    }
}

/// Read-only validated `.eggr` fixture.
#[pyclass(name = "Fixture")]
pub struct PyFixture {
    pub(crate) session: Arc<Session>,
}

#[pymethods]
impl PyFixture {
    #[new]
    fn new(py: Python<'_>, path: &str) -> PyResult<Self> {
        let path = Path::new(path).to_owned();
        let session = py
            .detach(|| Session::open(path, StoreLimits::default()))
            .map_err(fixture_error)?;
        Ok(Self {
            session: Arc::new(session),
        })
    }

    #[getter]
    fn manifest(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value(py, self.session.manifest()).map(Bound::unbind)
    }

    #[getter]
    fn extension_metadata(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value(py, &self.session.manifest().extensions).map(Bound::unbind)
    }

    fn websocket_summary(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let session = Arc::clone(&self.session);
        let Some(bytes) = py
            .detach(move || session.read_extension("websocket-messages"))
            .map_err(fixture_error)?
        else {
            return Ok(None);
        };
        let transcript: eggreplay_core::WebSocketTranscript =
            serde_json::from_slice(&bytes).map_err(fixture_error)?;
        let summary = serde_json::json!({
            "conversation_count": transcript.conversations.len(),
            "message_count": transcript.conversations.iter().map(|item| item.messages.len()).sum::<usize>(),
            "flow_ids": transcript.conversations.iter().map(|item| item.flow_id.clone()).collect::<Vec<_>>(),
        });
        json_value(py, &summary).map(Bound::unbind).map(Some)
    }

    fn scenario_summary(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let session = Arc::clone(&self.session);
        let Some(bytes) = py
            .detach(move || session.read_extension("rules"))
            .map_err(fixture_error)?
        else {
            return Ok(None);
        };
        let rules: eggreplay_core::ScenarioRules =
            serde_json::from_slice(&bytes).map_err(fixture_error)?;
        let summary = serde_json::json!({
            "scenario_count": rules.scenarios.len(),
            "scenario_ids": rules.scenarios.iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
        });
        json_value(py, &summary).map(Bound::unbind).map(Some)
    }

    fn stream_summary(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let session = Arc::clone(&self.session);
        let Some(bytes) = py
            .detach(move || session.read_extension("stream-events"))
            .map_err(fixture_error)?
        else {
            return Ok(None);
        };
        let events: eggreplay_core::StreamEvents =
            serde_json::from_slice(&bytes).map_err(fixture_error)?;
        let summary = serde_json::json!({
            "flow_count": events.flows.len(),
            "event_count": events.flows.iter().map(|flow| flow.request.len() + flow.response.len()).sum::<usize>(),
            "flow_ids": events.flows.iter().map(|flow| flow.flow_id.clone()).collect::<Vec<_>>(),
        });
        json_value(py, &summary).map(Bound::unbind).map(Some)
    }

    fn iter_flows(&self, py: Python<'_>) -> PyResult<PyFlowIterator> {
        let session = Arc::clone(&self.session);
        Ok(PyFlowIterator {
            iter: py
                .detach(move || session.iter_flows())
                .map_err(fixture_error)?,
        })
    }

    fn flow(&self, id: &str, py: Python<'_>) -> PyResult<Option<FlowView>> {
        let mut iter = self.session.iter_flows().map_err(fixture_error)?;
        py.detach(|| {
            iter.find_map(|flow| match flow {
                Ok(flow) if flow.id == id => Some(Ok(flow)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
        })
        .transpose()
        .map(|flow| flow.map(|flow| FlowView { flow }))
        .map_err(fixture_error)
    }

    fn open_body(&self, flow_id: &str, side: &str, py: Python<'_>) -> PyResult<BodyReader> {
        let flow = self
            .flow(flow_id, py)?
            .ok_or_else(|| FixtureError::new_err("flow was not found"))?;
        let body = match (side, &flow.flow.outcome) {
            ("request", _) => &flow.flow.request.body,
            ("response", FlowOutcome::Response(response)) => &response.body,
            ("response", FlowOutcome::Error(_)) => {
                return Err(FixtureError::new_err("flow has no response body"));
            }
            _ => {
                return Err(PyValueError::new_err(
                    "side must be 'request' or 'response'",
                ));
            }
        };
        let BodyRef::Blob(blob) = body else {
            return Err(FixtureError::new_err("flow body has no stored blob"));
        };
        let handle = py
            .detach(|| self.session.open_blob(blob))
            .map_err(fixture_error)?;
        Ok(BodyReader::from_handle(Arc::clone(&self.session), handle))
    }

    fn __repr__(&self) -> String {
        format!("Fixture(flow_count={})", self.session.manifest().flow_count)
    }
}
