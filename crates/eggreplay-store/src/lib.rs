//! Crash-safe, streaming `.eggr` directory fixtures.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use eggreplay_core::{BlobRef, Flow, FlowError, SCHEMA_VERSION, SessionMetadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Default validation bounds for untrusted fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreLimits {
    /// Maximum JSONL line size.
    pub max_line_bytes: u64,
    /// Maximum body blob size.
    pub max_blob_bytes: u64,
    /// Maximum number of flows.
    pub max_flows: usize,
    /// Maximum aggregate body bytes referenced by a session.
    pub max_total_bytes: u64,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: 4 * 1024 * 1024,
            max_blob_bytes: 64 * 1024 * 1024,
            max_flows: 100_000,
            max_total_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Store failures with typed validation categories.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Filesystem operation failed.
    #[error("filesystem: {0}")]
    Io(#[from] io::Error),
    /// JSON was malformed or exceeded a limit.
    #[error("invalid fixture JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Fixture metadata or content is invalid.
    #[error("invalid fixture: {0}")]
    Invalid(String),
    /// A future schema was encountered.
    #[error("unsupported fixture schema {0}")]
    UnsupportedSchema(u16),
    /// A body hash or length did not verify.
    #[error("blob integrity failure for {0}")]
    Integrity(String),
}

impl From<FlowError> for StoreError {
    fn from(error: FlowError) -> Self {
        Self::Invalid(error.to_string())
    }
}

/// Final manifest written only after all flows and blobs are complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Session metadata.
    #[serde(flatten)]
    pub metadata: SessionMetadata,
    /// Marker proving atomic finalization completed.
    pub complete: bool,
    /// Number of JSONL flow records.
    pub flow_count: usize,
    /// Number of unique blobs.
    pub blob_count: usize,
}

/// A streaming body sink owned by a session writer.
pub struct BodyWriter {
    file: File,
    path: PathBuf,
    hasher: Sha256,
    length: u64,
    max_bytes: u64,
}

impl Write for BodyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let next = self
            .length
            .checked_add(buf.len() as u64)
            .ok_or_else(|| io::Error::other("body length overflow"))?;
        if next > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "body exceeds configured limit",
            ));
        }
        self.file.write_all(buf)?;
        self.hasher.update(buf);
        self.length = next;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl BodyWriter {
    /// Finish the stream, hash it, and atomically publish the content-addressed blob.
    pub fn finish(mut self) -> Result<eggreplay_core::BodyRef, StoreError> {
        self.file.flush()?;
        self.file.sync_all()?;
        if self.length == 0 {
            let _ = fs::remove_file(&self.path);
            return Ok(eggreplay_core::BodyRef::Empty);
        }
        let digest = format!("{:x}", self.hasher.finalize());
        let destination = self
            .path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| StoreError::Invalid("blob staging path has no store root".into()))?
            .join("blobs")
            .join(&digest);
        if destination.exists() {
            fs::remove_file(&self.path)?;
        } else {
            fs::rename(&self.path, &destination)?;
        }
        Ok(eggreplay_core::BodyRef::Blob(
            BlobRef::new(digest, self.length).map_err(StoreError::from)?,
        ))
    }
}

/// Writer for a not-yet-published `.eggr` session.
pub struct SessionWriter {
    destination: PathBuf,
    staging: PathBuf,
    flows: File,
    metadata: SessionMetadata,
    limits: StoreLimits,
    flow_count: usize,
}

impl SessionWriter {
    /// Create an incomplete sibling directory for a new session.
    pub fn create(
        destination: impl AsRef<Path>,
        metadata: SessionMetadata,
        limits: StoreLimits,
    ) -> Result<Self, StoreError> {
        let destination = destination.as_ref().to_path_buf();
        if destination.exists() {
            return Err(StoreError::Invalid(
                "fixture destination already exists".into(),
            ));
        }
        if metadata.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema(metadata.schema_version));
        }
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let name = destination
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| StoreError::Invalid("fixture path has no filename".into()))?;
        let staging = parent.join(format!(
            ".{name}.incomplete-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir(&staging)?;
        fs::create_dir(&staging.join("blobs"))?;
        let flows = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(staging.join("flows.jsonl"))?;
        Ok(Self {
            destination,
            staging,
            flows,
            metadata,
            limits,
            flow_count: 0,
        })
    }

    /// Start a bounded, streaming blob write.
    pub fn begin_blob(&self) -> Result<BodyWriter, StoreError> {
        let path = self
            .staging
            .join("blobs")
            .join(format!(".staging-{}", unique_suffix()));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        Ok(BodyWriter {
            file,
            path,
            hasher: Sha256::new(),
            length: 0,
            max_bytes: self.limits.max_blob_bytes,
        })
    }

    /// Append one validated flow record to the temporary JSONL stream.
    pub fn append_flow(&mut self, flow: &Flow) -> Result<(), StoreError> {
        if self.flow_count >= self.limits.max_flows {
            return Err(StoreError::Invalid(
                "flow count exceeds configured limit".into(),
            ));
        }
        flow.validate()?;
        validate_body_refs(flow, self.limits.max_blob_bytes)?;
        let encoded = serde_json::to_vec(flow)?;
        if encoded.len() as u64 > self.limits.max_line_bytes {
            return Err(StoreError::Invalid(
                "flow JSONL line exceeds configured limit".into(),
            ));
        }
        self.flows.write_all(&encoded)?;
        self.flows.write_all(b"\n")?;
        self.flow_count += 1;
        Ok(())
    }

    /// Atomically publish the complete session directory.
    pub fn finish(mut self) -> Result<Session, StoreError> {
        self.flows.flush()?;
        self.flows.sync_all()?;
        let manifest = Manifest {
            metadata: self.metadata.clone(),
            complete: true,
            flow_count: self.flow_count,
            blob_count: count_blobs(&self.staging.join("blobs"))?,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        let mut manifest_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.staging.join("manifest.json"))?;
        manifest_file.write_all(&manifest_bytes)?;
        manifest_file.write_all(b"\n")?;
        manifest_file.sync_all()?;
        fs::rename(&self.staging, &self.destination)?;
        Session::open(&self.destination, self.limits)
    }
}

/// An opened and validated `.eggr` session.
#[derive(Debug, Clone)]
pub struct Session {
    root: PathBuf,
    manifest: Manifest,
    limits: StoreLimits,
}

impl Session {
    /// Open a complete session and validate its manifest, flow records, and blobs.
    pub fn open(root: impl AsRef<Path>, limits: StoreLimits) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        let manifest: Manifest =
            serde_json::from_reader(BufReader::new(File::open(root.join("manifest.json"))?))?;
        if manifest.metadata.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema(
                manifest.metadata.schema_version,
            ));
        }
        if !manifest.complete {
            return Err(StoreError::Invalid("fixture manifest is incomplete".into()));
        }
        let session = Self {
            root,
            manifest,
            limits,
        };
        let mut count = 0usize;
        let mut total = 0u64;
        for item in session.iter_flows()? {
            let flow = item?;
            count += 1;
            total = total
                .checked_add(flow_body_bytes(&flow))
                .ok_or_else(|| StoreError::Invalid("total body length overflow".into()))?;
            if total > limits.max_total_bytes {
                return Err(StoreError::Invalid(
                    "total body length exceeds configured limit".into(),
                ));
            }
            validate_body_files(&session.root, &flow, limits)?;
        }
        if count != session.manifest.flow_count {
            return Err(StoreError::Invalid("manifest flow count mismatch".into()));
        }
        if count_blobs(&session.root.join("blobs"))? != session.manifest.blob_count {
            return Err(StoreError::Invalid("manifest blob count mismatch".into()));
        }
        Ok(session)
    }

    /// Return validated manifest metadata.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Stream flow records one JSONL line at a time.
    pub fn iter_flows(&self) -> Result<FlowIter, StoreError> {
        Ok(FlowIter {
            reader: BufReader::new(File::open(self.root.join("flows.jsonl"))?),
            limits: self.limits,
            seen: 0,
        })
    }

    /// Read and verify one body blob without loading unrelated fixture bodies.
    pub fn read_blob(&self, blob: &BlobRef) -> Result<Vec<u8>, StoreError> {
        validate_digest(&blob.sha256)?;
        if blob.length > self.limits.max_blob_bytes {
            return Err(StoreError::Invalid("blob exceeds configured limit".into()));
        }
        verify_blob_file(&self.root, blob, self.limits)?;
        let path = self.root.join("blobs").join(&blob.sha256);
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut bytes = Vec::with_capacity(blob.length.min(1024 * 1024) as usize);
        let mut buf = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
            if bytes.len() as u64 + read as u64 > blob.length {
                return Err(StoreError::Integrity(blob.sha256.clone()));
            }
            bytes.extend_from_slice(&buf[..read]);
        }
        if bytes.len() as u64 != blob.length || format!("{:x}", hasher.finalize()) != blob.sha256 {
            return Err(StoreError::Integrity(blob.sha256.clone()));
        }
        Ok(bytes)
    }
}

/// Bounded flow iterator.
pub struct FlowIter {
    reader: BufReader<File>,
    limits: StoreLimits,
    seen: usize,
}

impl Iterator for FlowIter {
    type Item = Result<Flow, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.seen >= self.limits.max_flows {
            return Some(Err(StoreError::Invalid(
                "flow count exceeds configured limit".into(),
            )));
        }
        let mut line = Vec::new();
        match self.reader.read_until(b'\n', &mut line) {
            Ok(0) => None,
            Ok(_) => {
                self.seen += 1;
                if line.len() as u64 > self.limits.max_line_bytes {
                    return Some(Err(StoreError::Invalid(
                        "flow JSONL line exceeds configured limit".into(),
                    )));
                }
                Some(match serde_json::from_slice::<Flow>(&line) {
                    Ok(flow) => flow.validate().map(|()| flow).map_err(StoreError::from),
                    Err(error) => Err(StoreError::Json(error)),
                })
            }
            Err(error) => Some(Err(StoreError::Io(error))),
        }
    }
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn validate_digest(digest: &str) -> Result<(), StoreError> {
    if digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || digest != digest.to_ascii_lowercase()
    {
        return Err(StoreError::Invalid("invalid blob digest".into()));
    }
    Ok(())
}

fn validate_body_refs(flow: &Flow, max_blob_bytes: u64) -> Result<(), StoreError> {
    let refs = [
        Some(&flow.request.body),
        match &flow.outcome {
            eggreplay_core::FlowOutcome::Response(response) => Some(&response.body),
            eggreplay_core::FlowOutcome::Error(_) => None,
        },
    ];
    for body in refs.into_iter().flatten() {
        if let eggreplay_core::BodyRef::Blob(blob) = body {
            validate_digest(&blob.sha256)?;
            if blob.length > max_blob_bytes {
                return Err(StoreError::Invalid(
                    "body reference exceeds configured limit".into(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_body_files(root: &Path, flow: &Flow, limits: StoreLimits) -> Result<(), StoreError> {
    let refs = [
        Some(&flow.request.body),
        match &flow.outcome {
            eggreplay_core::FlowOutcome::Response(response) => Some(&response.body),
            eggreplay_core::FlowOutcome::Error(_) => None,
        },
    ];
    for body in refs.into_iter().flatten() {
        if let eggreplay_core::BodyRef::Blob(blob) = body {
            verify_blob_file(root, blob, limits)?;
        }
    }
    Ok(())
}

fn verify_blob_file(root: &Path, blob: &BlobRef, limits: StoreLimits) -> Result<(), StoreError> {
    validate_digest(&blob.sha256)?;
    if blob.length > limits.max_blob_bytes {
        return Err(StoreError::Invalid("blob exceeds configured limit".into()));
    }
    let mut file = File::open(root.join("blobs").join(&blob.sha256))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| StoreError::Integrity(blob.sha256.clone()))?;
        if total > blob.length {
            return Err(StoreError::Integrity(blob.sha256.clone()));
        }
        hasher.update(&buf[..read]);
    }
    if total != blob.length || format!("{:x}", hasher.finalize()) != blob.sha256 {
        return Err(StoreError::Integrity(blob.sha256.clone()));
    }
    Ok(())
}

fn flow_body_bytes(flow: &Flow) -> u64 {
    flow.request.body.len().unwrap_or(0)
        + match &flow.outcome {
            eggreplay_core::FlowOutcome::Response(response) => response.body.len().unwrap_or(0),
            eggreplay_core::FlowOutcome::Error(_) => 0,
        }
}

fn count_blobs(root: &Path) -> Result<usize, StoreError> {
    Ok(fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|ty| ty.is_file()).unwrap_or(false))
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggreplay_core::{BodyRef, FlowOutcome, HttpRequest, HttpResponse, Provenance};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample() -> Flow {
        Flow {
            schema_version: SCHEMA_VERSION,
            id: "one".into(),
            started_at_ms: 1,
            completed_at_ms: Some(2),
            request: HttpRequest {
                method: "GET".into(),
                scheme: "http".into(),
                authority: "example.test".into(),
                path: "/".into(),
                query: vec![],
                headers: vec![],
                body: BodyRef::Absent,
                trailers: vec![],
            },
            outcome: FlowOutcome::Response(HttpResponse {
                status: 200,
                headers: vec![],
                body: BodyRef::Empty,
                trailers: vec![],
            }),
            physical_route: None,
            provenance: Provenance {
                mode: "test".into(),
                observer: "test".into(),
            },
            annotations: vec![],
            redactions: vec![],
        }
    }

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "eggreplay-store-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn round_trip_and_empty_body_are_distinct() {
        let destination = path("roundtrip");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let mut body = writer.begin_blob().unwrap();
        body.write_all(b"hello").unwrap();
        let reference = body.finish().unwrap();
        assert!(matches!(reference, BodyRef::Blob(_)));
        let mut flow = sample();
        flow.request.body = reference;
        writer.append_flow(&flow).unwrap();
        let session = writer.finish().unwrap();
        assert_eq!(session.manifest().flow_count, 1);
        let loaded: Vec<_> = session
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(loaded, vec![flow]);
        let blob = match &loaded[0].request.body {
            BodyRef::Blob(blob) => blob,
            _ => unreachable!(),
        };
        assert_eq!(session.read_blob(blob).unwrap(), b"hello");
        fs::remove_dir_all(destination).unwrap();
    }
}
