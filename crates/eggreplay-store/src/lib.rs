//! Crash-safe, streaming `.eggr` directory fixtures.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use eggreplay_core::{BlobRef, Flow, FlowError, SCHEMA_VERSION, SessionMetadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
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

/// Explicit schema migration seam. No schema-2 migration is implemented.
pub trait Migration {
    /// Validate or migrate an input schema to the current schema.
    fn migrate(&self, schema_version: u16) -> Result<u16, StoreError>;
}

/// Schema-1 identity migration used by the current reader.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchemaOneMigration;

impl Migration for SchemaOneMigration {
    fn migrate(&self, schema_version: u16) -> Result<u16, StoreError> {
        if schema_version == SCHEMA_VERSION {
            Ok(SCHEMA_VERSION)
        } else {
            Err(StoreError::UnsupportedSchema(schema_version))
        }
    }
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
    /// Return the number of bytes streamed so far (staging, not finalized).
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Read back staging bytes boundedly for pre-publication transforms.
    ///
    /// Flushes the staging file and reads it without publishing. Raw bytes
    /// never become a finalized blob via this path; caller must either
    /// publish a transformed blob via a new writer or drop this writer
    /// (which cleans staging) on fail-closed errors.
    pub fn read_staging_bounded(&mut self, max_bytes: u64) -> Result<Vec<u8>, StoreError> {
        self.file.flush()?;
        if self.length > max_bytes {
            return Err(StoreError::Invalid(
                "staged body exceeds structured redaction limit".into(),
            ));
        }
        let mut file = File::open(&self.path)?;
        let mut bytes = Vec::with_capacity(self.length.min(1024 * 1024) as usize);
        file.read_to_end(&mut bytes)?;
        if bytes.len() as u64 != self.length {
            return Err(StoreError::Integrity("staging length mismatch".into()));
        }
        Ok(bytes)
    }

    /// Finish the stream, hash it, and atomically publish the content-addressed blob.
    pub fn finish(mut self) -> Result<eggreplay_core::BodyRef, StoreError> {
        // Take ownership of staging state so Drop (abort cleanup) becomes a
        // no-op after successful publish; File is still closed via drop.
        let path = std::mem::replace(&mut self.path, PathBuf::from("__finished__"));
        let hasher = std::mem::replace(&mut self.hasher, Sha256::new());
        self.file.flush()?;
        self.file.sync_all()?;
        if self.length == 0 {
            let _ = fs::remove_file(&path);
            return Ok(eggreplay_core::BodyRef::Empty);
        }
        let digest = format!("{:x}", hasher.finalize());
        let destination = path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| StoreError::Invalid("blob staging path has no store root".into()))?
            .join("blobs")
            .join(&digest);
        if destination.exists() {
            fs::remove_file(&path)?;
        } else {
            fs::rename(&path, &destination)?;
        }
        Ok(eggreplay_core::BodyRef::Blob(
            BlobRef::new(digest, self.length).map_err(StoreError::from)?,
        ))
    }
}

impl Drop for BodyWriter {
    /// Best-effort staging cleanup for aborted transactions.
    ///
    /// Ensures a failed/cancelled body stream never leaves an orphan staging
    /// file that could be counted as a blob. `finish` replaces the path with
    /// a sentinel so successful publishes are unaffected.
    fn drop(&mut self) {
        if self.path.as_os_str() != "__finished__" {
            let _ = fs::remove_file(&self.path);
        }
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
        let staging = parent.join(format!(".{name}.incomplete-{}", unique_suffix()));
        fs::create_dir(&staging)?;
        fs::create_dir(staging.join("blobs"))?;
        set_private_permissions(&staging)?;
        set_private_permissions(&staging.join("blobs"))?;
        let flows = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(staging.join("flows.jsonl"))?;
        set_private_permissions(&staging.join("flows.jsonl"))?;
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

    /// Read a body that has already been finalized in the staging session.
    /// This is used by the recording gateway before the session is published.
    pub fn read_body(&self, body: &eggreplay_core::BodyRef) -> Result<Vec<u8>, StoreError> {
        match body {
            eggreplay_core::BodyRef::Absent | eggreplay_core::BodyRef::Empty => Ok(Vec::new()),
            eggreplay_core::BodyRef::Blob(blob) => {
                read_blob_from_root(&self.staging, blob, self.limits)
            }
        }
    }

    /// Return the confined staging path for a finalized blob.
    pub fn body_path(&self, body: &eggreplay_core::BodyRef) -> Result<Option<PathBuf>, StoreError> {
        match body {
            eggreplay_core::BodyRef::Absent | eggreplay_core::BodyRef::Empty => Ok(None),
            eggreplay_core::BodyRef::Blob(blob) => {
                validate_digest(&blob.sha256)?;
                verify_blob_file(&self.staging, blob, self.limits)?;
                Ok(Some(self.staging.join("blobs").join(&blob.sha256)))
            }
        }
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
        set_private_permissions(&self.staging.join("manifest.json"))?;
        manifest_file.write_all(&manifest_bytes)?;
        manifest_file.write_all(b"\n")?;
        manifest_file.sync_all()?;
        fs::rename(&self.staging, &self.destination)?;
        Session::open(&self.destination, self.limits)
    }
}

/// Shared inner state for concurrent recording.
///
/// `flows` is the only serialized authority: it guards the JSONL append and
/// flow-count update. Body bytes never hold this lock; they stream to
/// independent staging files. Aggregate limits are enforced via atomics +
/// short critical sections, never via an unbounded channel.
#[derive(Debug)]
struct RecordingInner {
    destination: PathBuf,
    staging: PathBuf,
    metadata: SessionMetadata,
    limits: StoreLimits,
    flows: std::sync::Mutex<File>,
    flow_count: AtomicUsize,
    total_bytes: AtomicU64,
    active_blobs: AtomicUsize,
    shutdown: AtomicBool,
}

/// Cloneable concurrent recording session.
///
/// Obtained via [`RecordingSession::create`]; clones share the same staging
/// directory and flow log. `begin_blob` never holds the flow-log lock while
/// body bytes are written. `append_flow` serializes only the final bounded
/// metadata append. `shutdown` stops admission; `finish` must be called only
/// after all clones/tasks have drained (active count zero), otherwise it
/// fails instead of racing.
///
/// Crash safety is unchanged: manifest-last publication; incomplete staging
/// directories are never valid sessions.
#[derive(Debug, Clone)]
pub struct RecordingSession {
    inner: Arc<RecordingInner>,
}

/// Concurrent body sink tied to a [`RecordingSession`] for accounting.
///
/// Like [`BodyWriter`] but decrements the session active count on finish or
/// abort (Drop) so finalization cannot race active sinks. Staging files for
/// aborted bodies are removed best-effort, preventing orphan blobs.
pub struct RecordingBodyWriter {
    file: Option<File>,
    path: Option<PathBuf>,
    hasher: Option<Sha256>,
    length: u64,
    max_bytes: u64,
    session: Arc<RecordingInner>,
}

impl Write for RecordingBodyWriter {
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
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("body writer closed"))?
            .write_all(buf)?;
        self.hasher
            .as_mut()
            .ok_or_else(|| io::Error::other("body writer closed"))?
            .update(buf);
        self.length = next;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("body writer closed"))?
            .flush()
    }
}

impl RecordingBodyWriter {
    /// Return staged length without publishing.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Read back staging bytes boundedly for pre-publication transforms.
    pub fn read_staging_bounded(&mut self, max_bytes: u64) -> Result<Vec<u8>, StoreError> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| StoreError::Invalid("body writer closed".into()))?;
        file.flush()?;
        if self.length > max_bytes {
            return Err(StoreError::Invalid(
                "staged body exceeds structured redaction limit".into(),
            ));
        }
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| StoreError::Invalid("body writer closed".into()))?;
        let mut handle = File::open(path)?;
        let mut bytes = Vec::with_capacity(self.length.min(1024 * 1024) as usize);
        handle.read_to_end(&mut bytes)?;
        if bytes.len() as u64 != self.length {
            return Err(StoreError::Integrity("staging length mismatch".into()));
        }
        Ok(bytes)
    }

    /// Finish, publish the blob, and release the active reservation.
    pub fn finish(mut self) -> Result<eggreplay_core::BodyRef, StoreError> {
        let path = self.path.take().expect("body writer path");
        let mut file = self.file.take().expect("body writer file");
        let hasher = self.hasher.take().expect("body writer hasher");
        let length = self.length;
        // `self` now holds None fields so Drop becomes a no-op (no double
        // release); explicit release below is the single accounting point.
        // If any I/O below fails, still release and leave staging cleanup to
        // Drop-less manual removal to avoid orphans.
        let result: Result<eggreplay_core::BodyRef, StoreError> = (|| {
            file.flush()?;
            file.sync_all()?;
            if length == 0 {
                let _ = fs::remove_file(&path);
                return Ok(eggreplay_core::BodyRef::Empty);
            }
            let digest = format!("{:x}", hasher.finalize());
            let destination = path
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| StoreError::Invalid("blob staging path has no store root".into()))?
                .join("blobs")
                .join(&digest);
            if destination.exists() {
                fs::remove_file(&path)?;
            } else {
                fs::rename(&path, &destination)?;
            }
            Ok(eggreplay_core::BodyRef::Blob(
                BlobRef::new(digest, length).map_err(StoreError::from)?,
            ))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        self.session.active_blobs.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

impl Drop for RecordingBodyWriter {
    fn drop(&mut self) {
        // Abort path only: finish takes all Options leaving None, so this is
        // a no-op after successful finish. On abort, remove staging and
        // release the reservation.
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
            self.session.active_blobs.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl RecordingSession {
    /// Create a new concurrent session directory.
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
        let staging = parent.join(format!(".{name}.incomplete-{}", unique_suffix()));
        fs::create_dir(&staging)?;
        fs::create_dir(staging.join("blobs"))?;
        set_private_permissions(&staging)?;
        set_private_permissions(&staging.join("blobs"))?;
        let flows = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(staging.join("flows.jsonl"))?;
        set_private_permissions(&staging.join("flows.jsonl"))?;
        Ok(Self {
            inner: Arc::new(RecordingInner {
                destination,
                staging,
                metadata,
                limits,
                flows: std::sync::Mutex::new(flows),
                flow_count: AtomicUsize::new(0),
                total_bytes: AtomicU64::new(0),
                active_blobs: AtomicUsize::new(0),
                shutdown: AtomicBool::new(false),
            }),
        })
    }

    /// Stop admission of new blobs/flows; in-flight bodies may still finish.
    ///
    /// Gateway shutdown policy: call `shutdown`, stop accepting new requests,
    /// drain active tasks (join), then call `finish`. `begin_blob` and
    /// `append_flow` fail after shutdown.
    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
    }

    /// Return whether shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        self.inner.shutdown.load(Ordering::SeqCst)
    }

    /// Return the number of currently open body sinks.
    pub fn active_blobs(&self) -> usize {
        self.inner.active_blobs.load(Ordering::SeqCst)
    }

    /// Return the number of appended flows.
    pub fn flow_count(&self) -> usize {
        self.inner.flow_count.load(Ordering::SeqCst)
    }

    /// Start a bounded streaming blob write without holding the flow lock.
    pub fn begin_blob(&self) -> Result<RecordingBodyWriter, StoreError> {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(StoreError::Invalid("session is shutting down".into()));
        }
        let path = self
            .inner
            .staging
            .join("blobs")
            .join(format!(".staging-{}", unique_suffix()));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        self.inner.active_blobs.fetch_add(1, Ordering::SeqCst);
        Ok(RecordingBodyWriter {
            file: Some(file),
            path: Some(path),
            hasher: Some(Sha256::new()),
            length: 0,
            max_bytes: self.inner.limits.max_blob_bytes,
            session: self.inner.clone(),
        })
    }

    /// Read a finalized staging body (for gateway response streaming).
    pub fn read_body(&self, body: &eggreplay_core::BodyRef) -> Result<Vec<u8>, StoreError> {
        match body {
            eggreplay_core::BodyRef::Absent | eggreplay_core::BodyRef::Empty => Ok(Vec::new()),
            eggreplay_core::BodyRef::Blob(blob) => {
                read_blob_from_root(&self.inner.staging, blob, self.inner.limits)
            }
        }
    }

    /// Return the confined staging path for a finalized blob.
    pub fn body_path(&self, body: &eggreplay_core::BodyRef) -> Result<Option<PathBuf>, StoreError> {
        match body {
            eggreplay_core::BodyRef::Absent | eggreplay_core::BodyRef::Empty => Ok(None),
            eggreplay_core::BodyRef::Blob(blob) => {
                validate_digest(&blob.sha256)?;
                verify_blob_file(&self.inner.staging, blob, self.inner.limits)?;
                Ok(Some(self.inner.staging.join("blobs").join(&blob.sha256)))
            }
        }
    }

    /// Append one flow; serializes only the bounded metadata write.
    pub fn append_flow(&self, flow: &Flow) -> Result<(), StoreError> {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(StoreError::Invalid("session is shutting down".into()));
        }
        flow.validate()?;
        validate_body_refs(flow, self.inner.limits.max_blob_bytes)?;
        let new_bytes = flow_body_bytes(flow);
        let encoded = serde_json::to_vec(flow)?;
        if encoded.len() as u64 > self.inner.limits.max_line_bytes {
            return Err(StoreError::Invalid(
                "flow JSONL line exceeds configured limit".into(),
            ));
        }
        let mut flows = self
            .inner
            .flows
            .lock()
            .map_err(|_| StoreError::Invalid("flow log poisoned".into()))?;
        if self.inner.flow_count.load(Ordering::SeqCst) >= self.inner.limits.max_flows {
            return Err(StoreError::Invalid(
                "flow count exceeds configured limit".into(),
            ));
        }
        let total = self.inner.total_bytes.load(Ordering::SeqCst);
        let next = total
            .checked_add(new_bytes)
            .ok_or_else(|| StoreError::Invalid("total body length overflow".into()))?;
        if next > self.inner.limits.max_total_bytes {
            return Err(StoreError::Invalid(
                "total body length exceeds configured limit".into(),
            ));
        }
        flows.write_all(&encoded)?;
        flows.write_all(b"\n")?;
        self.inner.flow_count.fetch_add(1, Ordering::SeqCst);
        self.inner
            .total_bytes
            .fetch_add(new_bytes, Ordering::SeqCst);
        Ok(())
    }

    /// Atomically publish; fails if active sinks remain.
    ///
    /// Must be called only after admission stopped and all tasks drained.
    /// A failed/cancelled transaction leaves no manifest reference because
    /// flows are appended only after both bodies publish; aborted staging
    /// files are removed by writer Drop.
    pub fn finish(self) -> Result<Session, StoreError> {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        if self.inner.active_blobs.load(Ordering::SeqCst) != 0 {
            return Err(StoreError::Invalid(
                "cannot finalize with active transactions".into(),
            ));
        }
        // Early check for empty active via extra Arc clones? Strong count
        // includes self + any task clones; if tasks still hold clones they
        // may still call begin_blob after our shutdown flag, which will fail.
        // Active blob count is authoritative, not Arc count.
        let mut flows = self
            .inner
            .flows
            .lock()
            .map_err(|_| StoreError::Invalid("flow log poisoned".into()))?;
        flows.flush()?;
        flows.sync_all()?;
        drop(flows);
        let flow_count = self.inner.flow_count.load(Ordering::SeqCst);
        let manifest = Manifest {
            metadata: self.inner.metadata.clone(),
            complete: true,
            flow_count,
            blob_count: count_blobs(&self.inner.staging.join("blobs"))?,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        let mut manifest_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.inner.staging.join("manifest.json"))?;
        set_private_permissions(&self.inner.staging.join("manifest.json"))?;
        manifest_file.write_all(&manifest_bytes)?;
        manifest_file.write_all(b"\n")?;
        manifest_file.sync_all()?;
        fs::rename(&self.inner.staging, &self.inner.destination)?;
        Session::open(&self.inner.destination, self.inner.limits)
    }
}

/// An opened, validated handle to one content-addressed blob.
///
/// The handle owns an already-opened read-only file plus the validated
/// digest/length metadata. It never buffers the full blob: callers stream
/// from the file in bounded chunks. Digest form, byte bound, symlink
/// rejection, and exact file-length checks happen at open time; full
/// content-hash verification must happen incrementally while streaming
/// (see [`Session::open_blob`]).
#[derive(Debug)]
pub struct BlobHandle {
    file: File,
    sha256: String,
    length: u64,
}

impl BlobHandle {
    /// Return the validated SHA-256 digest.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Return the validated exact byte length.
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Return whether the referenced body is zero-length.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Consume the handle and return the opened file for streaming.
    ///
    /// `eggreplay-http` adapts this std file into Tokio async IO. The file
    /// offset is at the start; callers must stream to the declared length
    /// and verify the SHA-256 incrementally.
    pub fn into_file(self) -> File {
        self.file
    }

    /// Read the whole blob into memory with the session byte bound.
    ///
    /// This is a convenience for small deterministic tests and CLI
    /// inspection paths with explicit bounds. Streaming replay must not use
    /// it for selected large responses.
    pub fn read_all(&mut self) -> Result<Vec<u8>, StoreError> {
        use std::io::Seek;
        self.file
            .seek(std::io::SeekFrom::Start(0))
            .map_err(StoreError::Io)?;
        let mut hasher = Sha256::new();
        let mut bytes = Vec::with_capacity(self.length.min(1024 * 1024) as usize);
        let mut buf = [0u8; 64 * 1024];
        loop {
            let read = self.file.read(&mut buf).map_err(StoreError::Io)?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
            if bytes.len() as u64 + read as u64 > self.length {
                return Err(StoreError::Integrity(self.sha256.clone()));
            }
            bytes.extend_from_slice(&buf[..read]);
        }
        if bytes.len() as u64 != self.length || format!("{:x}", hasher.finalize()) != self.sha256 {
            return Err(StoreError::Integrity(self.sha256.clone()));
        }
        Ok(bytes)
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

    /// Open one blob for bounded streaming without allocating its bytes.
    ///
    /// Validates digest form, the configured byte bound, symlink rejection,
    /// and exact file length, then returns an already-opened file. The full
    /// content hash is intentionally not re-read here so fixture-wide loads
    /// stay metadata-bounded; the streaming caller must hash incrementally
    /// and treat any length/hash mismatch as [`StoreError::Integrity`].
    ///
    /// Fixture immutability/TOCTOU contract: a session directory must be
    /// treated as immutable while any [`Session`] or [`BlobHandle`] is open.
    /// If blobs are replaced between [`Session::open`] and streaming, the
    /// open-time length check or the incremental hash check fails instead of
    /// silently serving replaced bytes. Unselected blobs are never opened.
    pub fn open_blob(&self, blob: &BlobRef) -> Result<BlobHandle, StoreError> {
        validate_digest(&blob.sha256)?;
        if blob.length > self.limits.max_blob_bytes {
            return Err(StoreError::Invalid("blob exceeds configured limit".into()));
        }
        let blob_path = self.root.join("blobs").join(&blob.sha256);
        if fs::symlink_metadata(&blob_path)?.file_type().is_symlink() {
            return Err(StoreError::Invalid("symlinked blob is not allowed".into()));
        }
        let file = File::open(&blob_path)?;
        let actual_len = file.metadata()?.len();
        if actual_len != blob.length {
            return Err(StoreError::Integrity(blob.sha256.clone()));
        }
        Ok(BlobHandle {
            file,
            sha256: blob.sha256.clone(),
            length: blob.length,
        })
    }

    /// Return the configured validation limits for this session.
    pub fn limits(&self) -> StoreLimits {
        self.limits
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
        let mut line = Vec::new();
        match self.reader.read_until(b'\n', &mut line) {
            Ok(0) => None,
            Ok(_) => {
                if self.seen >= self.limits.max_flows {
                    return Some(Err(StoreError::Invalid(
                        "flow count exceeds configured limit".into(),
                    )));
                }
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

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}-{count}-{}", std::process::id())
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
    let blob_path = root.join("blobs").join(&blob.sha256);
    if fs::symlink_metadata(&blob_path)?.file_type().is_symlink() {
        return Err(StoreError::Invalid("symlinked blob is not allowed".into()));
    }
    let mut file = File::open(blob_path)?;
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

fn read_blob_from_root(
    root: &Path,
    blob: &BlobRef,
    limits: StoreLimits,
) -> Result<Vec<u8>, StoreError> {
    verify_blob_file(root, blob, limits)?;
    let mut file = File::open(root.join("blobs").join(&blob.sha256))?;
    let mut bytes = Vec::with_capacity(blob.length.min(1024 * 1024) as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn set_private_permissions(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(if path.is_dir() { 0o700 } else { 0o600 });
        fs::set_permissions(path, permissions)?;
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

    #[test]
    fn open_blob_validates_without_allocating_full_body() {
        let destination = path("open-blob");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let payload = vec![0xABu8; 256 * 1024];
        let mut body = writer.begin_blob().unwrap();
        body.write_all(&payload).unwrap();
        let reference = body.finish().unwrap();
        let mut flow = sample();
        flow.request.body = reference;
        writer.append_flow(&flow).unwrap();
        let session = writer.finish().unwrap();
        let blob = match &flow.request.body {
            BodyRef::Blob(blob) => blob.clone(),
            _ => unreachable!(),
        };
        // Open validates digest form, bound, symlink, and length without
        // allocating the full blob.
        let mut handle = session.open_blob(&blob).unwrap();
        assert_eq!(handle.len(), payload.len() as u64);
        assert_eq!(handle.sha256(), blob.sha256);
        assert!(!handle.is_empty());
        // Bounded chunked read preserves exact bytes with integrity check.
        let all = handle.read_all().unwrap();
        assert_eq!(all, payload);
        // Invalid digest form is rejected like ordinary validation.
        let mut bad = blob.clone();
        bad.sha256 = "not-hex".into();
        assert!(session.open_blob(&bad).is_err());
        // Oversized bound is rejected.
        let oversized = BlobRef::new(blob.sha256.clone(), u64::MAX).unwrap();
        assert!(session.open_blob(&oversized).is_err());
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn open_blob_rejects_symlinked_blob() {
        let destination = path("symlink-blob");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let mut body = writer.begin_blob().unwrap();
        body.write_all(b"secret").unwrap();
        let reference = body.finish().unwrap();
        let mut flow = sample();
        flow.request.body = reference.clone();
        writer.append_flow(&flow).unwrap();
        let session = writer.finish().unwrap();
        let blob = match &reference {
            BodyRef::Blob(blob) => blob.clone(),
            _ => unreachable!(),
        };
        let blob_path = destination.join("blobs").join(&blob.sha256);
        let target = std::env::temp_dir().join(format!(
            "eggreplay-symlink-target-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&target, b"secret").unwrap();
        let backup = blob_path.with_extension("bak");
        std::fs::rename(&blob_path, &backup).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &blob_path).unwrap();
        #[cfg(unix)]
        {
            let result = session.open_blob(&blob);
            assert!(result.is_err(), "symlinked blob must be rejected");
        }
        #[cfg(unix)]
        {
            std::fs::remove_file(&blob_path).unwrap();
            std::fs::rename(&backup, &blob_path).unwrap();
            std::fs::remove_file(&target).unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::rename(&backup, &blob_path).unwrap();
            std::fs::remove_file(&target).unwrap();
        }
        fs::remove_dir_all(destination).unwrap();
    }

    fn c002_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "eggreplay-c002-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn concurrent_blobs_overlap_without_flow_lock() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let dir = c002_path("overlap");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let first_start = Arc::new(AtomicU64::new(0));
        let second_start = Arc::new(AtomicU64::new(0));
        let first_end = Arc::new(AtomicU64::new(0));
        let now = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
        };
        let worker =
            |session: RecordingSession, start: Arc<AtomicU64>, end: Arc<AtomicU64>, seed: u8| {
                std::thread::spawn(move || {
                    let mut writer = session.begin_blob().unwrap();
                    start.store(now(), Ordering::SeqCst);
                    // Interleaved chunked writes with sleeps force overlap if the
                    // flow lock is not held during body streaming.
                    for chunk in 0..10 {
                        writer.write_all(&vec![seed; 32 * 1024]).unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        let _ = chunk;
                    }
                    let body_ref = writer.finish().unwrap();
                    end.store(now(), Ordering::SeqCst);
                    body_ref
                })
            };
        let a_start = first_start.clone();
        let a_end = first_end.clone();
        let b_start = second_start.clone();
        let session_a = session.clone();
        let session_b = session.clone();
        let handle_a = worker(session_a, a_start, a_end, 0xA5);
        // Start second while first is still streaming.
        std::thread::sleep(std::time::Duration::from_millis(25));
        let handle_b = worker(session_b, b_start, Arc::new(AtomicU64::new(0)), 0x5A);
        let body_a = handle_a.join().unwrap();
        let body_b = handle_b.join().unwrap();
        assert!(matches!(body_a, BodyRef::Blob(_)));
        assert!(matches!(body_b, BodyRef::Blob(_)));
        assert!(
            second_start.load(Ordering::SeqCst) < first_end.load(Ordering::SeqCst),
            "second blob must start before first completes"
        );
        // Append both flows concurrently; count must be exact.
        let mut flow_a = sample();
        flow_a.id = "concurrent-a".into();
        flow_a.request.body = body_a;
        let mut flow_b = sample();
        flow_b.id = "concurrent-b".into();
        flow_b.request.body = body_b;
        let s1 = session.clone();
        let s2 = session.clone();
        let t1 = std::thread::spawn(move || s1.append_flow(&flow_a).unwrap());
        let t2 = std::thread::spawn(move || s2.append_flow(&flow_b).unwrap());
        t1.join().unwrap();
        t2.join().unwrap();
        assert_eq!(session.flow_count(), 2);
        let finalized = session.finish().unwrap();
        assert_eq!(finalized.manifest().flow_count, 2);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn concurrent_append_preserves_manifest_count() {
        let dir = c002_path("append-count");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let mut handles = Vec::new();
        for i in 0..20 {
            let s = session.clone();
            handles.push(std::thread::spawn(move || {
                let mut flow = sample();
                flow.id = format!("flow-{i}");
                s.append_flow(&flow).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(session.flow_count(), 20);
        let finalized = session.finish().unwrap();
        assert_eq!(finalized.manifest().flow_count, 20);
        let loaded: Vec<_> = finalized
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(loaded.len(), 20);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn session_limits_enforced_under_concurrency() {
        let dir = c002_path("limits");
        let limits = StoreLimits {
            max_line_bytes: 4 * 1024 * 1024,
            max_blob_bytes: 4 * 1024,
            max_flows: 4,
            max_total_bytes: 8 * 1024,
        };
        let session = RecordingSession::create(&dir, SessionMetadata::default(), limits).unwrap();
        // Oversized blob must fail fast without holding the flow lock.
        let mut big = session.begin_blob().unwrap();
        assert!(
            big.write_all(&vec![0u8; 8 * 1024]).is_err(),
            "per-body limit must fail"
        );
        drop(big);
        assert_eq!(session.active_blobs(), 0);
        // Fill to flow limit concurrently; extras must be rejected.
        let mut handles = Vec::new();
        for i in 0..8 {
            let s = session.clone();
            handles.push(std::thread::spawn(move || {
                let mut flow = sample();
                flow.id = format!("limit-{i}");
                s.append_flow(&flow)
            }));
        }
        let mut ok = 0;
        for handle in handles {
            if handle.join().unwrap().is_ok() {
                ok += 1;
            }
        }
        assert_eq!(ok, 4, "flow limit must be exact under concurrency");
        assert_eq!(session.flow_count(), 4);
        let finalized = session.finish().unwrap();
        assert_eq!(finalized.manifest().flow_count, 4);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn aborted_body_leaves_no_orphan_and_finish_succeeds() {
        let dir = c002_path("abort");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        {
            let mut writer = session.begin_blob().unwrap();
            writer.write_all(b"partial").unwrap();
            // Drop without finish = cancelled transaction.
        }
        assert_eq!(session.active_blobs(), 0);
        // No staging files remain; blob dir must be empty.
        let blobs: Vec<_> = std::fs::read_dir(dir.join(format!(
            "{}",
            session.inner.staging.file_name().unwrap().to_string_lossy()
        )))
        .ok()
        .map(|_| ())
        .into_iter()
        .collect();
        let _ = blobs;
        let staging_blobs = session.inner.staging.join("blobs");
        let count = std::fs::read_dir(&staging_blobs)
            .unwrap()
            .filter_map(Result::ok)
            .count();
        assert_eq!(count, 0, "aborted body must not leave orphan file");
        let mut flow = sample();
        flow.id = "after-abort".into();
        session.append_flow(&flow).unwrap();
        let finalized = session.finish().unwrap();
        assert_eq!(finalized.manifest().flow_count, 1);
        assert_eq!(finalized.manifest().blob_count, 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shutdown_stops_admission_and_finish_guards_actives() {
        let dir = c002_path("shutdown");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let held = session.begin_blob().unwrap();
        // Finish with active sink must fail rather than race.
        let session2 = session.clone();
        assert!(
            std::thread::spawn(move || session2.finish())
                .join()
                .unwrap()
                .is_err(),
            "finalize must fail with active transactions"
        );
        drop(held);
        assert_eq!(session.active_blobs(), 0);
        session.shutdown();
        assert!(session.begin_blob().is_err());
        let mut flow = sample();
        flow.id = "post-shutdown".into();
        assert!(session.append_flow(&flow).is_err());
        // New session for successful finish path.
        let dir2 = c002_path("shutdown-ok");
        let session_ok =
            RecordingSession::create(&dir2, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let mut flow = sample();
        flow.id = "ok".into();
        session_ok.append_flow(&flow).unwrap();
        session_ok.shutdown();
        let finalized = session_ok.finish().unwrap();
        assert_eq!(finalized.manifest().flow_count, 1);
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(dir2).ok();
    }
}
