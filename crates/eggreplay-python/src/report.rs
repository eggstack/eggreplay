use eggreplay_core::{DiffFinding, RegressionReport};
use pyo3::prelude::*;

use crate::errors::RegressionError;

/// One redaction-safe typed difference from Rust's report schema.
#[pyclass(name = "DiffFinding", skip_from_py_object)]
#[derive(Clone)]
pub struct FindingView {
    finding: DiffFinding,
}

#[pymethods]
impl FindingView {
    #[getter]
    fn kind(&self) -> String {
        serde_json::to_value(&self.finding.kind)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    #[getter]
    fn field(&self) -> &str {
        &self.finding.field
    }

    #[getter]
    fn baseline(&self) -> &str {
        &self.finding.baseline
    }

    #[getter]
    fn candidate(&self) -> &str {
        &self.finding.candidate
    }
}

/// Rust-authored candidate regression report with schema-identical projections.
#[pyclass(name = "RegressionReport")]
pub struct ReportView {
    report: RegressionReport,
}

impl ReportView {
    pub(crate) fn from_report(report: RegressionReport) -> Self {
        Self { report }
    }
}

#[pymethods]
impl ReportView {
    #[staticmethod]
    fn from_json(json: &str) -> PyResult<Self> {
        let report = serde_json::from_str(json)
            .map_err(|_| RegressionError::new_err("invalid regression report"))?;
        Ok(Self { report })
    }

    #[getter]
    fn success(&self) -> bool {
        self.report.is_success()
    }

    #[getter]
    fn finding_count(&self) -> usize {
        self.report.findings.len()
    }

    #[getter]
    fn baseline_flow_ids(&self) -> Vec<String> {
        self.report.baseline_flow_ids.clone()
    }

    #[getter]
    fn findings(&self) -> Vec<FindingView> {
        self.report
            .findings
            .iter()
            .cloned()
            .map(|finding| FindingView { finding })
            .collect()
    }

    fn to_json(&self) -> PyResult<String> {
        serde_json::to_string(&self.report)
            .map_err(|_| RegressionError::new_err("regression report serialization failed"))
    }

    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let json = self.to_json()?;
        py.import("json")?
            .call_method1("loads", (json,))
            .map(Bound::unbind)
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<FindingView>()?;
    module.add_class::<ReportView>()?;
    module.add("RegressionError", module.py().get_type::<RegressionError>())?;
    Ok(())
}
