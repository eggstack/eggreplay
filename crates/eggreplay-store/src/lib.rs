//! Crash-safe, streaming `.eggr` directory fixtures.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use eggreplay_core::{BlobRef, Flow, FlowError, SessionMetadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

const MAX_EXTENSIONS: usize = 64;
const MAX_EXTENSION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_EXTENSION_BYTES: u64 = 32 * 1024 * 1024;
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
        if (eggreplay_core::SESSION_SCHEMA_V1..=eggreplay_core::SESSION_SCHEMA_VERSION)
            .contains(&schema_version)
        {
            Ok(schema_version)
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
    /// Bounded session-level extension registry (empty for schema 1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<ExtensionDescriptor>,
}

/// A confined, versioned session extension file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionDescriptor {
    /// Stable extension name.
    pub name: String,
    /// Extension payload schema version.
    pub schema_version: u16,
    /// Relative path, currently restricted to one filename under the fixture root.
    pub path: String,
    /// Whether replay semantics require understanding this extension.
    pub required_for_replay: bool,
}

/// A streaming body sink owned by a session writer.
pub struct BodyWriter {
    // `Option` so `finish` can close the staging file before the
    // Windows-hostile rename/remove of an open file. Unix behavior is
    // unchanged.
    file: Option<File>,
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
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("body writer closed"))?
            .write_all(buf)?;
        self.hasher.update(buf);
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
        self.file
            .as_mut()
            .ok_or_else(|| StoreError::Invalid("body writer closed".into()))?
            .flush()?;
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
    ///
    /// The staging file is closed before any remove/rename because Windows
    /// denies those operations on open files. Unix behavior is unchanged.
    pub fn finish(mut self) -> Result<eggreplay_core::BodyRef, StoreError> {
        // Take ownership of staging state so Drop (abort cleanup) becomes a
        // no-op after successful publish; the file is closed explicitly
        // below before any rename/remove.
        let path = std::mem::replace(&mut self.path, PathBuf::from("__finished__"));
        let hasher = std::mem::replace(&mut self.hasher, Sha256::new());
        let file = self
            .file
            .take()
            .ok_or_else(|| StoreError::Invalid("body writer closed".into()))?;
        let mut file = file;
        file.flush()?;
        file.sync_all()?;
        drop(file);
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
    total_bytes: u64,
    extensions: Vec<ExtensionDescriptor>,
    stream_events: eggreplay_core::StreamEvents,
    stream_event_ids: std::collections::HashSet<String>,
    stream_event_records: usize,
    stream_event_object_bytes: usize,
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
        if !(eggreplay_core::SESSION_SCHEMA_V1..=eggreplay_core::SESSION_SCHEMA_VERSION)
            .contains(&metadata.schema_version)
        {
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
            total_bytes: 0,
            extensions: Vec::new(),
            stream_events: eggreplay_core::StreamEvents::default(),
            stream_event_ids: std::collections::HashSet::new(),
            stream_event_records: 0,
            stream_event_object_bytes: 0,
        })
    }

    /// Write a bounded extension payload into the staging session.
    pub fn write_extension(
        &mut self,
        name: &str,
        schema_version: u16,
        path: &str,
        required_for_replay: bool,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        write_extension_file(
            &self.staging,
            &mut self.extensions,
            name,
            schema_version,
            path,
            required_for_replay,
            bytes,
        )?;
        self.metadata.schema_version = eggreplay_core::SESSION_SCHEMA_VERSION;
        Ok(())
    }

    /// Append bounded, payload-free event metadata for one recorded flow.
    pub fn append_stream_events(
        &mut self,
        events: eggreplay_core::FlowStreamEvents,
    ) -> Result<(), StoreError> {
        let single = eggreplay_core::StreamEvents {
            schema_version: eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
            flows: vec![events.clone()],
        };
        single.validate().map_err(StoreError::Invalid)?;
        if self.stream_event_ids.contains(&events.flow_id) {
            return Err(StoreError::Invalid("duplicate stream event flow id".into()));
        }
        let records = events.request.len().saturating_add(events.response.len());
        let next_records = self.stream_event_records.saturating_add(records);
        if next_records > eggreplay_core::stream::MAX_STREAM_EVENTS_PER_SESSION {
            return Err(StoreError::Invalid(
                "aggregate stream event count exceeds configured limit".into(),
            ));
        }
        let object_bytes = serde_json::to_vec(&events)?.len();
        let next_object_bytes = self
            .stream_event_object_bytes
            .saturating_add(object_bytes)
            .saturating_add(usize::from(!self.stream_events.flows.is_empty()));
        let base_bytes = serde_json::to_vec(&eggreplay_core::StreamEvents::default())?.len() - 2;
        if base_bytes.saturating_add(next_object_bytes)
            > eggreplay_core::stream::MAX_STREAM_EVENTS_BYTES
        {
            return Err(StoreError::Invalid(
                "stream event metadata exceeds configured limit".into(),
            ));
        }
        self.stream_event_ids.insert(events.flow_id.clone());
        self.stream_event_records = next_records;
        self.stream_event_object_bytes = next_object_bytes;
        self.stream_events.flows.push(events);
        Ok(())
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
            file: Some(file),
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
        let next_total = self
            .total_bytes
            .checked_add(flow_body_bytes(flow))
            .ok_or_else(|| StoreError::Invalid("total body length overflow".into()))?;
        if next_total > self.limits.max_total_bytes {
            return Err(StoreError::Invalid(
                "total body length exceeds configured limit".into(),
            ));
        }
        self.flows.write_all(&encoded)?;
        self.flows.write_all(b"\n")?;
        self.flow_count += 1;
        self.total_bytes = next_total;
        Ok(())
    }

    /// Atomically publish the complete session directory.
    ///
    /// Windows cannot rename a directory that contains open files, so the
    /// flow-log and manifest handles are flushed, synced, and closed
    /// before the staging directory is renamed. Unix behavior is unchanged.
    pub fn finish(mut self) -> Result<Session, StoreError> {
        self.flows.flush()?;
        self.flows.sync_all()?;
        if !self.stream_events.flows.is_empty() {
            self.stream_events.validate().map_err(StoreError::Invalid)?;
            let bytes = serde_json::to_vec(&self.stream_events)?;
            write_extension_file(
                &self.staging,
                &mut self.extensions,
                "stream-events",
                eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
                "stream-events.json",
                true,
                &bytes,
            )?;
            self.metadata.schema_version = eggreplay_core::SESSION_SCHEMA_VERSION;
        }
        let manifest = Manifest {
            metadata: self.metadata.clone(),
            complete: true,
            flow_count: self.flow_count,
            blob_count: count_blobs(&self.staging.join("blobs"))?,
            extensions: self.extensions.clone(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        {
            let mut manifest_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(self.staging.join("manifest.json"))?;
            set_private_permissions(&self.staging.join("manifest.json"))?;
            manifest_file.write_all(&manifest_bytes)?;
            manifest_file.write_all(b"\n")?;
            manifest_file.sync_all()?;
        }
        let Self {
            destination,
            staging,
            flows,
            metadata: _,
            limits,
            flow_count: _,
            total_bytes: _,
            extensions: _,
            stream_events: _,
            stream_event_ids: _,
            stream_event_records: _,
            stream_event_object_bytes: _,
        } = self;
        drop(flows);
        fs::rename(&staging, &destination)?;
        Session::open(&destination, limits)
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
    // `Option` so `finish` can close the flow log before the
    // Windows-hostile staging rename. `None` is only observable during
    // finalization, after which the session is consumed.
    flows: std::sync::Mutex<Option<File>>,
    flow_count: AtomicUsize,
    total_bytes: AtomicU64,
    active_blobs: AtomicUsize,
    shutdown: AtomicBool,
    extensions: std::sync::Mutex<Vec<ExtensionDescriptor>>,
    stream_events: std::sync::Mutex<eggreplay_core::StreamEvents>,
    stream_event_ids: std::sync::Mutex<std::collections::HashSet<String>>,
    stream_event_records: AtomicUsize,
    stream_event_object_bytes: AtomicUsize,
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
    ///
    /// The staging file is closed before any remove/rename because Windows
    /// denies those operations on open files. Unix behavior is unchanged.
    pub fn finish(mut self) -> Result<eggreplay_core::BodyRef, StoreError> {
        let path = self.path.take().expect("body writer path");
        let mut file = self.file.take().expect("body writer file");
        let hasher = self.hasher.take().expect("body writer hasher");
        let length = self.length;
        // `self` now holds None fields so Drop becomes a no-op (no double
        // release); explicit release below is the single accounting point.
        if let Err(error) = file.flush().and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&path);
            self.session.active_blobs.fetch_sub(1, Ordering::SeqCst);
            return Err(error.into());
        }
        drop(file);
        let result: Result<eggreplay_core::BodyRef, StoreError> = (|| {
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
        // a no-op after successful finish. On abort, close the file before
        // removing staging (Windows denies removing open files) and release
        // the reservation.
        if let Some(path) = self.path.take() {
            drop(self.file.take());
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
        if !(eggreplay_core::SESSION_SCHEMA_V1..=eggreplay_core::SESSION_SCHEMA_VERSION)
            .contains(&metadata.schema_version)
        {
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
                flows: std::sync::Mutex::new(Some(flows)),
                flow_count: AtomicUsize::new(0),
                total_bytes: AtomicU64::new(0),
                active_blobs: AtomicUsize::new(0),
                shutdown: AtomicBool::new(false),
                extensions: std::sync::Mutex::new(Vec::new()),
                stream_events: std::sync::Mutex::new(eggreplay_core::StreamEvents::default()),
                stream_event_ids: std::sync::Mutex::new(std::collections::HashSet::new()),
                stream_event_records: AtomicUsize::new(0),
                stream_event_object_bytes: AtomicUsize::new(0),
            }),
        })
    }

    /// Write a bounded extension before finalization. Extension publication is
    /// serialized and must finish before `finish` writes the manifest.
    pub fn write_extension(
        &self,
        name: &str,
        schema_version: u16,
        path: &str,
        required_for_replay: bool,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(StoreError::Invalid("session is shutting down".into()));
        }
        let mut extensions = self
            .inner
            .extensions
            .lock()
            .map_err(|_| StoreError::Invalid("extension registry poisoned".into()))?;
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(StoreError::Invalid("session is shutting down".into()));
        }
        write_extension_file(
            &self.inner.staging,
            &mut extensions,
            name,
            schema_version,
            path,
            required_for_replay,
            bytes,
        )
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

    /// Append bounded, payload-free event metadata for one recorded flow.
    pub fn append_stream_events(
        &self,
        events: eggreplay_core::FlowStreamEvents,
    ) -> Result<(), StoreError> {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(StoreError::Invalid("session is shutting down".into()));
        }
        let mut stored = self
            .inner
            .stream_events
            .lock()
            .map_err(|_| StoreError::Invalid("stream event registry poisoned".into()))?;
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(StoreError::Invalid("session is shutting down".into()));
        }
        let single = eggreplay_core::StreamEvents {
            schema_version: eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
            flows: vec![events.clone()],
        };
        single.validate().map_err(StoreError::Invalid)?;
        let mut ids = self
            .inner
            .stream_event_ids
            .lock()
            .map_err(|_| StoreError::Invalid("stream event id registry poisoned".into()))?;
        if ids.contains(&events.flow_id) {
            return Err(StoreError::Invalid("duplicate stream event flow id".into()));
        }
        if stored.flows.len() >= eggreplay_core::stream::MAX_STREAM_EVENTS_PER_SESSION {
            return Err(StoreError::Invalid(
                "stream event flow count exceeds configured limit".into(),
            ));
        }
        let records = events.request.len().saturating_add(events.response.len());
        let next_records = self
            .inner
            .stream_event_records
            .load(Ordering::SeqCst)
            .saturating_add(records);
        if next_records > eggreplay_core::stream::MAX_STREAM_EVENTS_PER_SESSION {
            return Err(StoreError::Invalid(
                "aggregate stream event count exceeds configured limit".into(),
            ));
        }
        let object_bytes = serde_json::to_vec(&events)?.len();
        let next_object_bytes = self
            .inner
            .stream_event_object_bytes
            .load(Ordering::SeqCst)
            .saturating_add(object_bytes)
            .saturating_add(usize::from(!stored.flows.is_empty()));
        let base_bytes = serde_json::to_vec(&eggreplay_core::StreamEvents::default())?.len() - 2;
        if base_bytes.saturating_add(next_object_bytes)
            > eggreplay_core::stream::MAX_STREAM_EVENTS_BYTES
        {
            return Err(StoreError::Invalid(
                "stream event metadata exceeds configured limit".into(),
            ));
        }
        ids.insert(events.flow_id.clone());
        self.inner
            .stream_event_records
            .store(next_records, Ordering::SeqCst);
        self.inner
            .stream_event_object_bytes
            .store(next_object_bytes, Ordering::SeqCst);
        stored.flows.push(events);
        Ok(())
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
        let flows = flows
            .as_mut()
            .ok_or_else(|| StoreError::Invalid("session is finalizing".into()))?;
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
        // Close the flow log before the staging rename: Windows denies
        // renaming a directory with open files while Unix permits it.
        // The manifest handle is likewise scoped so it closes first.
        let flows_file = {
            let mut flows = self
                .inner
                .flows
                .lock()
                .map_err(|_| StoreError::Invalid("flow log poisoned".into()))?;
            let mut flows_file = flows
                .take()
                .ok_or_else(|| StoreError::Invalid("session is already finalizing".into()))?;
            flows_file.flush()?;
            flows_file.sync_all()?;
            flows_file
        };
        drop(flows_file);
        let flow_count = self.inner.flow_count.load(Ordering::SeqCst);
        {
            let stream_events = self
                .inner
                .stream_events
                .lock()
                .map_err(|_| StoreError::Invalid("stream event registry poisoned".into()))?;
            if !stream_events.flows.is_empty() {
                stream_events.validate().map_err(StoreError::Invalid)?;
                let bytes = serde_json::to_vec(&*stream_events)?;
                if bytes.len() > eggreplay_core::stream::MAX_STREAM_EVENTS_BYTES {
                    return Err(StoreError::Invalid(
                        "stream event metadata exceeds configured limit".into(),
                    ));
                }
                let mut extensions = self
                    .inner
                    .extensions
                    .lock()
                    .map_err(|_| StoreError::Invalid("extension registry poisoned".into()))?;
                write_extension_file(
                    &self.inner.staging,
                    &mut extensions,
                    "stream-events",
                    eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
                    "stream-events.json",
                    true,
                    &bytes,
                )?;
            }
        }
        let manifest = Manifest {
            metadata: {
                let mut metadata = self.inner.metadata.clone();
                if !self
                    .inner
                    .extensions
                    .lock()
                    .map_err(|_| StoreError::Invalid("extension registry poisoned".into()))?
                    .is_empty()
                {
                    metadata.schema_version = eggreplay_core::SESSION_SCHEMA_VERSION;
                }
                metadata
            },
            complete: true,
            flow_count,
            blob_count: count_blobs(&self.inner.staging.join("blobs"))?,
            extensions: self
                .inner
                .extensions
                .lock()
                .map_err(|_| StoreError::Invalid("extension registry poisoned".into()))?
                .clone(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        {
            let mut manifest_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(self.inner.staging.join("manifest.json"))?;
            set_private_permissions(&self.inner.staging.join("manifest.json"))?;
            manifest_file.write_all(&manifest_bytes)?;
            manifest_file.write_all(b"\n")?;
            manifest_file.sync_all()?;
        }
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
        if !(eggreplay_core::SESSION_SCHEMA_V1..=eggreplay_core::SESSION_SCHEMA_VERSION)
            .contains(&manifest.metadata.schema_version)
        {
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
        validate_extensions(&session.root, &session.manifest.extensions)?;
        let mut count = 0usize;
        let mut total = 0u64;
        let mut response_lengths = std::collections::HashMap::new();
        for item in session.iter_flows()? {
            let flow = item?;
            let response_length = match &flow.outcome {
                eggreplay_core::FlowOutcome::Response(response) => response.body.len().unwrap_or(0),
                eggreplay_core::FlowOutcome::Error(_) => 0,
            };
            if response_lengths
                .insert(flow.id.clone(), response_length)
                .is_some()
            {
                return Err(StoreError::Invalid("duplicate flow id".into()));
            }
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
        if let Some(extension) = session
            .manifest
            .extensions
            .iter()
            .find(|extension| extension.name == "stream-events")
        {
            let events: eggreplay_core::StreamEvents =
                serde_json::from_reader(File::open(session.root.join(&extension.path))?)?;
            for flow_events in events.flows {
                let expected_length =
                    response_lengths.get(&flow_events.flow_id).ok_or_else(|| {
                        StoreError::Invalid("stream events reference an unknown flow".into())
                    })?;
                let event_length = flow_events.response.iter().fold(0u64, |total, event| {
                    total.saturating_add(match &event.event {
                        eggreplay_core::StreamEventKind::Data { length, .. } => *length,
                        _ => 0,
                    })
                });
                if event_length != *expected_length {
                    return Err(StoreError::Invalid(
                        "stream event byte count disagrees with response body".into(),
                    ));
                }
            }
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

    /// Copy a validated session into a new transactional destination. Blob
    /// bytes stream through bounded writers, and every digest is rechecked.
    /// Passing schema 2 performs a schema-1 to current-session-schema upgrade;
    /// flow records remain schema 1.
    pub fn copy_to(
        &self,
        destination: impl AsRef<Path>,
        target_schema: u16,
    ) -> Result<Session, StoreError> {
        if !(eggreplay_core::SESSION_SCHEMA_V1..=eggreplay_core::SESSION_SCHEMA_VERSION)
            .contains(&target_schema)
        {
            return Err(StoreError::UnsupportedSchema(target_schema));
        }
        if target_schema == 1 && !self.manifest.extensions.is_empty() {
            return Err(StoreError::Invalid(
                "cannot downgrade a session with extensions".into(),
            ));
        }
        let mut metadata = self.manifest.metadata.clone();
        metadata.schema_version = target_schema;
        let mut writer = SessionWriter::create(destination, metadata, self.limits)?;

        // Copy the complete blob namespace, including blobs referenced by
        // extensions rather than flow bodies. Invalid/orphan content is not
        // silently carried into the new publication.
        for entry in fs::read_dir(self.root.join("blobs"))? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(StoreError::Invalid(
                    "blob directory may contain only regular files".into(),
                ));
            }
            if entry.file_name() == ".gitkeep" && entry.metadata()?.len() == 0 {
                continue;
            }
            let digest = entry
                .file_name()
                .into_string()
                .map_err(|_| StoreError::Invalid("blob name is not UTF-8".into()))?;
            validate_digest(&digest)?;
            let length = entry.metadata()?.len();
            let reference = BlobRef::new(digest.clone(), length)
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            let handle = self.open_blob(&reference)?;
            let mut source = handle.into_file();
            let mut sink = writer.begin_blob()?;
            io::copy(&mut source, &mut sink)?;
            match sink.finish()? {
                eggreplay_core::BodyRef::Blob(copied) if copied.sha256 == digest => {}
                _ => return Err(StoreError::Integrity(digest)),
            }
        }
        for flow in self.iter_flows()? {
            writer.append_flow(&flow?)?;
        }
        for extension in &self.manifest.extensions {
            let bytes = self
                .read_extension(&extension.name)?
                .ok_or_else(|| StoreError::Invalid("extension disappeared during copy".into()))?;
            writer.write_extension(
                &extension.name,
                extension.schema_version,
                &extension.path,
                extension.required_for_replay,
                &bytes,
            )?;
        }
        writer.finish()
    }

    /// Merge this fixture with a second completed recording into a new
    /// transaction. The first fixture owns session metadata and extension
    /// authority; conflicting extension names fail closed.
    pub fn merge_to(
        &self,
        additional: &Session,
        destination: impl AsRef<Path>,
        target_schema: u16,
    ) -> Result<Session, StoreError> {
        if !(eggreplay_core::SESSION_SCHEMA_V1..=eggreplay_core::SESSION_SCHEMA_VERSION)
            .contains(&target_schema)
        {
            return Err(StoreError::UnsupportedSchema(target_schema));
        }
        let limits = StoreLimits {
            max_line_bytes: self
                .limits
                .max_line_bytes
                .min(additional.limits.max_line_bytes),
            max_blob_bytes: self
                .limits
                .max_blob_bytes
                .min(additional.limits.max_blob_bytes),
            max_flows: self.limits.max_flows.min(additional.limits.max_flows),
            max_total_bytes: self
                .limits
                .max_total_bytes
                .min(additional.limits.max_total_bytes),
        };
        let mut metadata = self.manifest.metadata.clone();
        metadata.schema_version = target_schema;
        let mut writer = SessionWriter::create(destination, metadata, limits)?;
        copy_blob_namespace(&mut writer, self)?;
        copy_blob_namespace(&mut writer, additional)?;
        for flow in self.iter_flows()?.chain(additional.iter_flows()?) {
            writer.append_flow(&flow?)?;
        }
        let mut extensions = std::collections::BTreeMap::new();
        for source in [self, additional] {
            for extension in &source.manifest.extensions {
                let bytes = source.read_extension(&extension.name)?.ok_or_else(|| {
                    StoreError::Invalid("extension disappeared during merge".into())
                })?;
                if let Some((prior, prior_bytes)) = extensions.get(&extension.name) {
                    if prior != extension || prior_bytes != &bytes {
                        return Err(StoreError::Invalid(format!(
                            "conflicting extension {:?} during merge",
                            extension.name
                        )));
                    }
                } else {
                    extensions.insert(extension.name.clone(), (extension.clone(), bytes));
                }
            }
        }
        for (extension, bytes) in extensions.into_values() {
            writer.write_extension(
                &extension.name,
                extension.schema_version,
                &extension.path,
                extension.required_for_replay,
                &bytes,
            )?;
        }
        writer.finish()
    }

    /// Read and validate one extension payload, bounded by the extension caps.
    pub fn read_extension(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(extension) = self
            .manifest
            .extensions
            .iter()
            .find(|item| item.name == name)
        else {
            return Ok(None);
        };
        let path = self.root.join(&extension.path);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::Invalid(
                "extension must be a regular non-symlink file".into(),
            ));
        }
        if metadata.len() > MAX_EXTENSION_BYTES {
            return Err(StoreError::Invalid(
                "extension exceeds configured limit".into(),
            ));
        }
        let file = File::open(path)?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_EXTENSION_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_EXTENSION_BYTES {
            return Err(StoreError::Invalid(
                "extension exceeds configured limit".into(),
            ));
        }
        Ok(Some(bytes))
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

fn copy_blob_namespace(writer: &mut SessionWriter, source: &Session) -> Result<(), StoreError> {
    for entry in fs::read_dir(source.root.join("blobs"))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(StoreError::Invalid(
                "blob directory may contain only regular files".into(),
            ));
        }
        if entry.file_name() == ".gitkeep" && entry.metadata()?.len() == 0 {
            continue;
        }
        let digest = entry
            .file_name()
            .into_string()
            .map_err(|_| StoreError::Invalid("blob name is not UTF-8".into()))?;
        validate_digest(&digest)?;
        let length = entry.metadata()?.len();
        let reference = BlobRef::new(digest.clone(), length)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let handle = source.open_blob(&reference)?;
        let mut from = handle.into_file();
        let mut to = writer.begin_blob()?;
        io::copy(&mut from, &mut to)?;
        match to.finish()? {
            eggreplay_core::BodyRef::Blob(copied) if copied.sha256 == digest => {}
            _ => return Err(StoreError::Integrity(digest)),
        }
    }
    Ok(())
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

fn write_extension_file(
    root: &Path,
    extensions: &mut Vec<ExtensionDescriptor>,
    name: &str,
    schema_version: u16,
    path: &str,
    required_for_replay: bool,
    bytes: &[u8],
) -> Result<(), StoreError> {
    validate_extension_name(name)?;
    validate_extension_path(path)?;
    if schema_version == 0 {
        return Err(StoreError::Invalid(
            "extension schema version must be positive".into(),
        ));
    }
    if extensions.len() >= MAX_EXTENSIONS {
        return Err(StoreError::Invalid(
            "extension count exceeds configured limit".into(),
        ));
    }
    if bytes.len() as u64 > MAX_EXTENSION_BYTES {
        return Err(StoreError::Invalid(
            "extension exceeds configured limit".into(),
        ));
    }
    if extensions
        .iter()
        .any(|entry| entry.name == name || entry.path == path)
    {
        return Err(StoreError::Invalid(
            "duplicate extension name or path".into(),
        ));
    }
    let current_total: u64 = extensions
        .iter()
        .filter_map(|entry| fs::metadata(root.join(&entry.path)).ok().map(|m| m.len()))
        .sum();
    if current_total.saturating_add(bytes.len() as u64) > MAX_TOTAL_EXTENSION_BYTES {
        return Err(StoreError::Invalid(
            "aggregate extensions exceed configured limit".into(),
        ));
    }
    let destination = root.join(path);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&destination)?;
    set_private_permissions(&destination)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    extensions.push(ExtensionDescriptor {
        name: name.to_owned(),
        schema_version,
        path: path.to_owned(),
        required_for_replay,
    });
    Ok(())
}

fn validate_extension_name(name: &str) -> Result<(), StoreError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(StoreError::Invalid("invalid extension name".into()));
    }
    Ok(())
}

fn validate_extension_path(path: &str) -> Result<(), StoreError> {
    let candidate = Path::new(path);
    if path.is_empty()
        || path.len() > 128
        || candidate.components().count() != 1
        || candidate.file_name().and_then(|name| name.to_str()) != Some(path)
        || path.starts_with('.')
    {
        return Err(StoreError::Invalid(
            "extension path must be a confined filename".into(),
        ));
    }
    Ok(())
}

fn validate_extensions(root: &Path, extensions: &[ExtensionDescriptor]) -> Result<(), StoreError> {
    if extensions.len() > MAX_EXTENSIONS {
        return Err(StoreError::Invalid(
            "extension count exceeds configured limit".into(),
        ));
    }
    let mut names = std::collections::BTreeSet::new();
    let mut paths = std::collections::BTreeSet::new();
    let mut total = 0u64;
    for extension in extensions {
        validate_extension_name(&extension.name)?;
        validate_extension_path(&extension.path)?;
        if extension.schema_version == 0
            || !names.insert(&extension.name)
            || !paths.insert(&extension.path)
        {
            return Err(StoreError::Invalid(
                "invalid or duplicate extension descriptor".into(),
            ));
        }
        let path = root.join(&extension.path);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::Invalid(
                "extension must be a regular non-symlink file".into(),
            ));
        }
        if metadata.len() > MAX_EXTENSION_BYTES {
            return Err(StoreError::Invalid(
                "extension exceeds configured limit".into(),
            ));
        }
        total = total
            .checked_add(metadata.len())
            .ok_or_else(|| StoreError::Invalid("aggregate extension length overflow".into()))?;
        if total > MAX_TOTAL_EXTENSION_BYTES {
            return Err(StoreError::Invalid(
                "aggregate extensions exceed configured limit".into(),
            ));
        }
        if extension.required_for_replay
            && !matches!(
                extension.name.as_str(),
                "rules" | "stream-events" | "websocket-messages" | "interop-provenance"
            )
        {
            return Err(StoreError::Invalid(format!(
                "unknown required extension {}",
                extension.name
            )));
        }
        if extension.name == "rules" {
            if extension.schema_version != eggreplay_core::RULES_SCHEMA_VERSION {
                return Err(StoreError::Invalid(format!(
                    "unsupported rules extension schema {}",
                    extension.schema_version
                )));
            }
            let rules: eggreplay_core::ScenarioRules = serde_json::from_reader(File::open(&path)?)?;
            rules.validate().map_err(StoreError::Invalid)?;
        }
        if extension.name == "stream-events" {
            if extension.schema_version != eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION {
                return Err(StoreError::Invalid(format!(
                    "unsupported stream-events extension schema {}",
                    extension.schema_version
                )));
            }
            let events: eggreplay_core::StreamEvents = serde_json::from_reader(File::open(path)?)?;
            events.validate().map_err(StoreError::Invalid)?;
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

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(if path.is_dir() { 0o700 } else { 0o600 });
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), StoreError> {
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
        // Git cannot preserve an empty directory. Checked-in empty-session
        // fixtures carry this marker so their required blobs directory exists.
        .filter(|entry| entry.file_name() != ".gitkeep")
        .filter(|entry| entry.file_type().map(|ty| ty.is_file()).unwrap_or(false))
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggreplay_core::{
        BodyRef, FlowOutcome, HttpRequest, HttpResponse, Provenance, SCHEMA_VERSION,
    };
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
    fn schema_two_extension_registry_is_bounded_and_confined() {
        let destination = path("extensions");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        writer
            .write_extension(
                "rules",
                1,
                "rules.json",
                true,
                br#"{"schema_version":1,"scenarios":[]}"#,
            )
            .unwrap();
        assert!(
            writer
                .write_extension("other", 1, "../escape.json", false, b"{}")
                .is_err()
        );
        let session = writer.finish().unwrap();
        assert_eq!(session.manifest().metadata.schema_version, 2);
        assert_eq!(session.manifest().extensions.len(), 1);
        assert_eq!(
            session.read_extension("rules").unwrap().unwrap(),
            br#"{"schema_version":1,"scenarios":[]}"#
        );
        assert!(session.read_extension("missing").unwrap().is_none());
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn stream_event_extension_round_trips_without_body_payloads() {
        let destination = path("stream-events");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let mut flow = sample();
        flow.id = "stream-1".into();
        writer.append_flow(&flow).unwrap();
        writer
            .append_stream_events(eggreplay_core::FlowStreamEvents {
                flow_id: flow.id.clone(),
                start_offset_ns: 10,
                request: vec![
                    eggreplay_core::StreamEvent {
                        delta_ns: 1,
                        event: eggreplay_core::StreamEventKind::Data {
                            offset: 0,
                            length: 4,
                        },
                    },
                    eggreplay_core::StreamEvent {
                        delta_ns: 2,
                        event: eggreplay_core::StreamEventKind::End,
                    },
                ],
                response: vec![],
            })
            .unwrap();
        let session = writer.finish().unwrap();
        let bytes = session.read_extension("stream-events").unwrap().unwrap();
        assert!(!bytes.windows(4).any(|window| window == b"body"));
        let parsed: eggreplay_core::StreamEvents = serde_json::from_slice(&bytes).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.flows[0].flow_id, "stream-1");
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn writer_emits_stream_events_as_required() {
        // M010-C1 #1: writers that emit `stream-events` register it with
        // `required_for_replay=true`.
        let destination = path("stream-required");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let mut flow = sample();
        flow.id = "required-1".into();
        writer.append_flow(&flow).unwrap();
        writer
            .append_stream_events(eggreplay_core::FlowStreamEvents {
                flow_id: flow.id.clone(),
                start_offset_ns: 0,
                request: Vec::new(),
                response: vec![eggreplay_core::StreamEvent {
                    delta_ns: 0,
                    event: eggreplay_core::StreamEventKind::End,
                }],
            })
            .unwrap();
        let session = writer.finish().unwrap();
        let descriptor = session
            .manifest()
            .extensions
            .iter()
            .find(|extension| extension.name == "stream-events")
            .expect("stream-events extension must be present");
        assert!(
            descriptor.required_for_replay,
            "stream-events must be required_for_replay=true"
        );
        fs::remove_dir_all(destination).unwrap();

        // Concurrent session path must also mark required.
        let concurrent = path("stream-required-concurrent");
        let recording = RecordingSession::create(
            &concurrent,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        recording.append_flow(&flow).unwrap();
        recording
            .append_stream_events(eggreplay_core::FlowStreamEvents {
                flow_id: flow.id.clone(),
                start_offset_ns: 0,
                request: Vec::new(),
                response: vec![eggreplay_core::StreamEvent {
                    delta_ns: 0,
                    event: eggreplay_core::StreamEventKind::End,
                }],
            })
            .unwrap();
        recording.shutdown();
        let finished = recording.finish().unwrap();
        let descriptor = finished
            .manifest()
            .extensions
            .iter()
            .find(|extension| extension.name == "stream-events")
            .expect("concurrent stream-events must be present");
        assert!(descriptor.required_for_replay);
        fs::remove_dir_all(concurrent).unwrap();
    }

    #[test]
    fn malformed_and_unsupported_stream_events_fail_closed() {
        // M010-C1 #5: malformed, unsupported-version, duplicate-flow, and
        // body-length inconsistent stream metadata fail closed.
        // Unsupported descriptor schema version: finalization fails closed
        // via Session::open validation inside finish.
        {
            let destination = path("stream-unsupported-schema");
            let mut writer = SessionWriter::create(
                &destination,
                SessionMetadata::default(),
                StoreLimits::default(),
            )
            .unwrap();
            writer.append_flow(&sample()).unwrap();
            writer
                .write_extension("stream-events", 999, "stream-events.json", true, b"{}")
                .unwrap();
            assert!(writer.finish().is_err());
            // Staging remains; clean up parent-tracked destination attempt.
            // `finish` consumes writer, so on failure the staging dir remains
            // under a temp incomplete name; remove destination if created.
            fs::remove_dir_all(&destination).ok();
        }
        // Malformed JSON payload fails closed at finalization.
        {
            let destination = path("stream-malformed");
            let mut writer = SessionWriter::create(
                &destination,
                SessionMetadata::default(),
                StoreLimits::default(),
            )
            .unwrap();
            writer.append_flow(&sample()).unwrap();
            writer
                .write_extension(
                    "stream-events",
                    eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
                    "stream-events.json",
                    true,
                    b"{not json",
                )
                .unwrap();
            assert!(writer.finish().is_err());
            fs::remove_dir_all(&destination).ok();
        }
        // Duplicate flow ids fail at append time and at validation.
        {
            let destination = path("stream-duplicate");
            let mut writer = SessionWriter::create(
                &destination,
                SessionMetadata::default(),
                StoreLimits::default(),
            )
            .unwrap();
            let mut flow = sample();
            flow.id = "dup".into();
            writer.append_flow(&flow).unwrap();
            let events = eggreplay_core::FlowStreamEvents {
                flow_id: "dup".into(),
                start_offset_ns: 0,
                request: Vec::new(),
                response: vec![eggreplay_core::StreamEvent {
                    delta_ns: 0,
                    event: eggreplay_core::StreamEventKind::End,
                }],
            };
            writer.append_stream_events(events.clone()).unwrap();
            assert!(writer.append_stream_events(events).is_err());
            let session = writer.finish().unwrap();
            // Body-length mismatch: stream DATA length disagrees with response
            // body (Empty expects 0, DATA claims bytes).
            drop(session);
            fs::remove_dir_all(destination).unwrap();
        }
        // Body-length inconsistent: Session::open must reject when DATA bytes
        // disagree with the flow response body.
        {
            // `sample` has Empty response body (0 bytes) but we claim 4 DATA bytes.
            let bad = eggreplay_core::StreamEvents {
                schema_version: eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
                flows: vec![eggreplay_core::FlowStreamEvents {
                    flow_id: "mismatch".into(),
                    start_offset_ns: 0,
                    request: Vec::new(),
                    response: vec![
                        eggreplay_core::StreamEvent {
                            delta_ns: 0,
                            event: eggreplay_core::StreamEventKind::Data {
                                offset: 0,
                                length: 4,
                            },
                        },
                        eggreplay_core::StreamEvent {
                            delta_ns: 1,
                            event: eggreplay_core::StreamEventKind::End,
                        },
                    ],
                }],
            };
            let mismatch_dir = path("stream-body-mismatch-2");
            let mut flow = sample();
            flow.id = "mismatch".into();
            let mut writer2 = SessionWriter::create(
                &mismatch_dir,
                SessionMetadata::default(),
                StoreLimits::default(),
            )
            .unwrap();
            writer2.append_flow(&flow).unwrap();
            writer2
                .write_extension(
                    "stream-events",
                    eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
                    "stream-events.json",
                    true,
                    &serde_json::to_vec(&bad).unwrap(),
                )
                .unwrap();
            // Body-length mismatch fails closed at finalization.
            assert!(writer2.finish().is_err());
            fs::remove_dir_all(&mismatch_dir).ok();
        }
    }

    #[test]
    #[cfg(unix)]
    fn required_extension_rejects_symlinked_payload() {
        let destination = path("symlink-extension");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        writer
            .write_extension(
                "rules",
                1,
                "rules.json",
                true,
                br#"{"schema_version":1,"scenarios":[]}"#,
            )
            .unwrap();
        let session = writer.finish().unwrap();
        drop(session);
        let extension = destination.join("rules.json");
        let backup = destination.join("rules.backup");
        let target = path("symlink-extension-target");
        fs::write(&target, br#"{"schema_version":1,"scenarios":[]}"#).unwrap();
        fs::rename(&extension, &backup).unwrap();
        std::os::unix::fs::symlink(&target, &extension).unwrap();
        assert!(Session::open(&destination, StoreLimits::default()).is_err());
        fs::remove_file(extension).unwrap();
        fs::rename(backup, destination.join("rules.json")).unwrap();
        fs::remove_file(target).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn unknown_required_extension_is_rejected() {
        let destination = path("unknown-required-extension");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        writer
            .write_extension(
                "rules",
                1,
                "rules.json",
                true,
                br#"{"schema_version":1,"scenarios":[]}"#,
            )
            .unwrap();
        let session = writer.finish().unwrap();
        drop(session);
        let manifest_path = destination.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["extensions"][0]["name"] = serde_json::json!("future-required");
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let result = Session::open(&destination, StoreLimits::default());
        assert!(
            matches!(result, Err(StoreError::Invalid(message)) if message.contains("unknown required extension"))
        );
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn checked_in_schema_fixtures_open_and_current_extension_is_readable() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let schema_one = Session::open(fixtures.join("schema-1-empty"), StoreLimits::default())
            .expect("schema-1 golden fixture must remain readable");
        assert_eq!(schema_one.manifest().metadata.schema_version, 1);
        let schema_two = Session::open(fixtures.join("schema-2-rules"), StoreLimits::default())
            .expect("schema-2 golden fixture must validate");
        let rules = schema_two.read_extension("rules").unwrap().unwrap();
        assert_eq!(
            rules.strip_suffix(b"\n").unwrap_or(&rules),
            br#"{"schema_version":1,"scenarios":[]}"#
        );
    }

    #[test]
    fn schema_migration_is_transactional_and_current_copy_is_idempotent() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let source = Session::open(fixtures.join("schema-1-empty"), StoreLimits::default())
            .expect("schema-1 golden fixture must open");
        let migrated_path = path("migrated-schema-two");
        let migrated = source
            .copy_to(&migrated_path, eggreplay_core::SESSION_SCHEMA_VERSION)
            .unwrap();
        assert_eq!(migrated.manifest().metadata.schema_version, 2);
        assert_eq!(migrated.manifest().flow_count, 0);
        let copied_path = path("current-copy");
        let copied = migrated
            .copy_to(&copied_path, eggreplay_core::SESSION_SCHEMA_VERSION)
            .unwrap();
        assert_eq!(copied.manifest().metadata, migrated.manifest().metadata);
        assert_eq!(copied.manifest().extensions, migrated.manifest().extensions);
        fs::remove_dir_all(migrated_path).unwrap();
        fs::remove_dir_all(copied_path).unwrap();
    }

    #[test]
    fn merge_to_combines_flows_and_deduplicates_blobs_transactionally() {
        let base_path = path("merge-base");
        let add_path = path("merge-add");
        let merged_path = path("merge-result");
        let mut base_writer = SessionWriter::create(
            &base_path,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let mut base_blob = base_writer.begin_blob().unwrap();
        base_blob.write_all(b"shared").unwrap();
        let shared = base_blob.finish().unwrap();
        let mut base_flow = sample();
        base_flow.request.body = shared.clone();
        base_writer.append_flow(&base_flow).unwrap();
        let base = base_writer.finish().unwrap();

        let mut add_writer = SessionWriter::create(
            &add_path,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let mut duplicate_blob = add_writer.begin_blob().unwrap();
        duplicate_blob.write_all(b"shared").unwrap();
        let duplicate = duplicate_blob.finish().unwrap();
        let mut new_blob = add_writer.begin_blob().unwrap();
        new_blob.write_all(b"new").unwrap();
        let new_reference = new_blob.finish().unwrap();
        let mut repeated = sample();
        repeated.id = "additional".into();
        repeated.request.body = duplicate;
        add_writer.append_flow(&repeated).unwrap();
        let mut added = sample();
        added.id = "added".into();
        added.request.body = new_reference;
        add_writer.append_flow(&added).unwrap();
        let additional = add_writer.finish().unwrap();

        let merged = base
            .merge_to(
                &additional,
                &merged_path,
                eggreplay_core::SESSION_SCHEMA_VERSION,
            )
            .unwrap();
        assert_eq!(merged.manifest().flow_count, 3);
        assert_eq!(merged.manifest().blob_count, 2);
        assert_eq!(
            merged
                .read_blob(match &shared {
                    BodyRef::Blob(blob) => blob,
                    _ => unreachable!(),
                })
                .unwrap(),
            b"shared"
        );
        assert_eq!(
            merged
                .iter_flows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .len(),
            3
        );
        for dir in [base_path, add_path, merged_path] {
            fs::remove_dir_all(dir).unwrap();
        }
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
        // Close the blob handle before removing the session directory:
        // Windows denies removing files with open handles.
        drop(handle);
        fs::remove_dir_all(destination).unwrap();
    }

    // Symlink construction requires privileges that are not reliably
    // available on standard GitHub-hosted Windows runners, so this
    // rejection test is Unix-only. The production `symlink_metadata`
    // rejection check itself remains portable and runs on Windows.
    #[test]
    #[cfg(unix)]
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
        std::os::unix::fs::symlink(&target, &blob_path).unwrap();
        let result = session.open_blob(&blob);
        assert!(result.is_err(), "symlinked blob must be rejected");
        std::fs::remove_file(&blob_path).unwrap();
        std::fs::rename(&backup, &blob_path).unwrap();
        std::fs::remove_file(&target).unwrap();
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
