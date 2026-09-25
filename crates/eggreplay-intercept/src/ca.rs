//! Dedicated interception certificate-authority (CA) lifecycle.
//!
//! This module owns creation, import, inspection, export, rotation support,
//! and file-permission policy for the operator-owned interception CA. It is
//! deliberately separate from `.eggr` fixture storage: CA directories live at
//! caller-selected paths and contain exactly three files:
//!
//! ```text
//! ca-dir/
//!   metadata.json
//!   ca-cert.pem
//!   ca-key.pem
//! ```
//!
//! # Design choices
//!
//! - Key algorithm: ECDSA P-256 (SHA-256) via `ring`, generated and signed
//!   through `rcgen`. P-256 is supported by `rcgen`, `rustls`, and `ring` on
//!   every qualified platform, and P-256 CA certificates verify cleanly in
//!   mainstream client trust stores. RSA is excluded (`ring` cannot generate
//!   RSA keys, and larger keys buy nothing here); Ed25519 is excluded to keep
//!   the supported set to one maximally compatible algorithm.
//! - CA validity: 365-day default, 31-day minimum, 5-year (1825-day) maximum.
//!   Generated CAs carry `BasicConstraints CA:true` with a path-length
//!   constraint of 0 (they sign leaves directly, never intermediates), key
//!   usage `digitalSignature/keyCertSign/cRLSign`, no subject alternative
//!   names, and a 63-bit positive serial.
//! - Import accepts self-signed root CAs only (subject `==` issuer plus a
//!   cryptographic self-signature check). Intermediates are rejected: an
//!   interception trust anchor installed in clients must be a root.
//! - Writes are staged in a temporary sibling directory and atomically
//!   published (`create_dir` claims the target so an existing CA directory is
//!   never overwritten; file renames stay on one filesystem).
//! - [`CaAuthority::open`] revalidates structure, fingerprint binding, and
//!   (on Unix) permissions, but not expiry: inspecting or exporting an
//!   expired CA during rotation is legitimate. Leaf issuance refuses expired
//!   CAs instead (see [`crate::leaf`]).
//! - Rotation is creation of a distinct identity in a new directory. Handles
//!   are immutable snapshots: creating a new CA never mutates or invalidates
//!   an active [`CaAuthority`]. Selecting a CA for a listener is an explicit
//!   handle choice (M013D consumes the handle; nothing is implicit).
//! - No operation installs trust anywhere or exports private keys.
//!
//! # Permissions
//!
//! Unix: directory `0700`, private key `0600`, certificate/metadata `0644`.
//! [`CaAuthority::open`] rejects insecure directory/key permissions; the
//! explicit [`repair_ca_permissions`] action restores them. Imported source
//! files are only read, never modified.
//!
//! Windows: Rust's standard library exposes no per-user file ACLs, so this
//! module creates files with default sharing and documents the limitation
//! rather than claiming Unix equivalence: on Windows, CA directory secrecy
//! depends on the operator's profile directory ACLs. Permission enforcement
//! and repair are Unix-only; the Windows test only pins the documented
//! behavior (files are created, handles open, export works).
//!
//! # Secret handling
//!
//! [`CaError`] messages never contain key bytes, key paths, or PEM contents.
//! [`CaAuthority`] has a redacted `Debug` implementation (fingerprint and
//! public validity facts only). [`CaMetadata`] holds public facts exclusively
//! and is safe to log or persist.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration as StdDuration;

use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;
use x509_parser::oid_registry::{OID_KEY_TYPE_EC_PUBLIC_KEY, OID_SIG_ECDSA_WITH_SHA256};
use x509_parser::prelude::{FromDer, X509Certificate};

/// Version of the on-disk CA directory format written by this module.
pub const CA_FORMAT_VERSION: u32 = 1;
/// Public certificate filename inside a CA directory.
pub const CA_CERT_FILENAME: &str = "ca-cert.pem";
/// Private key filename inside a CA directory.
pub const CA_KEY_FILENAME: &str = "ca-key.pem";
/// Public metadata filename inside a CA directory.
pub const CA_METADATA_FILENAME: &str = "metadata.json";
/// Maximum accepted PEM file size (certificate, key, or metadata).
pub const MAX_PEM_FILE_BYTES: usize = 64 * 1024;
/// Maximum accepted metadata file size.
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
/// Maximum common-name length (characters) for generated CAs.
pub const MAX_CA_SUBJECT_CN_CHARS: usize = 128;
/// Default common name for generated CAs.
pub const DEFAULT_CA_COMMON_NAME: &str = "EggReplay Interception CA";
/// Default CA validity in days (conservative one year).
pub const DEFAULT_CA_VALIDITY_DAYS: u32 = 365;
/// Minimum CA validity in days (must comfortably exceed leaf maximums).
pub const MIN_CA_VALIDITY_DAYS: u32 = 31;
/// Maximum CA validity in days (explicit 5-year upper bound).
pub const MAX_CA_VALIDITY_DAYS: u32 = 1825;
/// Clock tolerance applied to CA expiry checks (covers reasonable skew).
pub const CA_CLOCK_TOLERANCE: StdDuration = StdDuration::from_secs(5 * 60);
/// Same tolerance as a `time` duration for validity arithmetic.
const CA_CLOCK_TOLERANCE_TIME: time::Duration = time::Duration::seconds(5 * 60);
/// Key algorithm identifier recorded in metadata and enforced on import.
pub const CA_KEY_ALGORITHM_ID: &str = "ECDSA-P256-SHA256";
/// Expected Unix mode for the CA directory.
pub const CA_DIR_MODE: u32 = 0o700;
/// Expected Unix mode for the CA private key.
pub const CA_KEY_MODE: u32 = 0o600;
/// Unix mode applied to the public certificate and metadata.
pub const CA_PUBLIC_MODE: u32 = 0o644;
/// Upper bound for human-readable name displays stored in metadata.
const DISPLAY_LEN: usize = 256;
/// Upper bound for operator-input details echoed in errors.
const DETAIL_LEN: usize = 200;

/// Fail-closed CA lifecycle errors.
///
/// Messages carry only static descriptions and public facts (modes, dates,
/// counts). They never contain key bytes, key paths, PEM contents, or source
/// file locations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaError {
    /// The target CA directory already exists; refusing to overwrite.
    #[error(
        "CA directory already exists; refusing to overwrite (choose a new directory for rotation)"
    )]
    AlreadyExists,
    /// The CA directory is missing or does not contain all required files.
    #[error("CA directory is missing or incomplete")]
    Incomplete,
    /// A filesystem operation failed (paths are deliberately omitted).
    #[error("CA storage I/O failed during {0}")]
    Io(&'static str),
    /// A PEM file exceeded the size bound.
    #[error("{0} exceeds the size bound")]
    TooLarge(&'static str),
    /// Metadata failed to parse or validate (bounded public detail).
    #[error("invalid CA metadata: {0}")]
    InvalidMetadata(String),
    /// Metadata format version is not supported.
    #[error("unsupported CA format version: {0}")]
    UnsupportedFormatVersion(u32),
    /// The stored fingerprint does not match the stored certificate.
    #[error("CA fingerprint does not match the stored certificate")]
    FingerprintMismatch,
    /// The certificate file is not a parseable single PEM certificate.
    #[error("CA certificate is not a single parseable PEM certificate")]
    CertUnparseable,
    /// The private key file is not a parseable PKCS#8 PEM private key.
    #[error("CA private key is not a parseable PKCS#8 PEM private key")]
    KeyUnparseable,
    /// More than one private key was supplied.
    #[error("exactly one CA private key is required")]
    MultipleKeys,
    /// No private key was found.
    #[error("no CA private key found")]
    NoKey,
    /// The certificate and private key do not correspond.
    #[error("CA certificate and private key do not match")]
    Mismatch,
    /// The certificate file holds more than one certificate.
    #[error("CA certificate file must hold exactly one certificate")]
    UnexpectedChain,
    /// The certificate is not a self-signed root CA.
    #[error("imported CA must be a self-signed root certificate")]
    NotSelfSigned,
    /// The certificate signature does not verify.
    #[error("CA certificate signature does not verify")]
    BadSignature,
    /// The certificate lacks `BasicConstraints CA:true`.
    #[error("certificate is not a CA (BasicConstraints CA:true is required)")]
    NotCa,
    /// The certificate lacks the `keyCertSign` key usage.
    #[error("certificate cannot sign leaves (keyCertSign key usage is required)")]
    MissingKeyCertSign,
    /// The key or signature algorithm is not ECDSA P-256/SHA-256.
    #[error("unsupported CA algorithm (ECDSA P-256 with SHA-256 is required)")]
    UnsupportedAlgorithm,
    /// The CA is expired (public date included for operability).
    #[error("CA certificate is expired (not-after {0})")]
    Expired(String),
    /// The CA is not yet valid (public date included for operability).
    #[error("CA certificate is not yet valid (not-before {0})")]
    NotYetValid(String),
    /// Directory or key permissions are insecure.
    #[error("{target} permissions are too permissive (mode {mode:o}); refusing to open")]
    InsecurePermissions {
        /// Which entry failed validation (`directory` or `private key`).
        target: &'static str,
        /// Observed Unix mode bits.
        mode: u32,
    },
    /// Permission repair is not supported on this platform.
    #[error("permission repair is only supported on Unix")]
    PermissionRepairUnsupported,
    /// Key or certificate generation failed.
    #[error("CA generation failed")]
    CertGeneration,
    /// Certificate signing failed.
    #[error("CA signing failed")]
    Signing,
    /// Operator-supplied options are invalid (bounded public detail).
    #[error("invalid CA options: {0}")]
    InvalidOptions(String),
    /// The export destination already exists; refusing to overwrite.
    #[error("export destination already exists; refusing to overwrite")]
    ExportDestExists,
}

/// How a CA directory came into existence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaOrigin {
    /// Generated locally by [`CaAuthority::create_new`].
    Created,
    /// Copied in from operator-supplied files by [`CaAuthority::import`].
    Imported,
}

/// Public, sanitized CA metadata stored as `metadata.json`.
///
/// Contains no key material and is safe to log, display, or persist. The
/// fingerprint binds the metadata to the stored certificate on every open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaMetadata {
    /// On-disk format version ([`CA_FORMAT_VERSION`]).
    pub format_version: u32,
    /// Lowercase hex SHA-256 over the DER certificate bytes.
    pub fingerprint_sha256: String,
    /// Human-readable certificate subject (bounded, from the certificate).
    pub subject_display: String,
    /// Human-readable certificate issuer (bounded, from the certificate).
    pub issuer_display: String,
    /// Certificate `notBefore` as RFC 3339.
    pub not_before_rfc3339: String,
    /// Certificate `notAfter` as RFC 3339.
    pub not_after_rfc3339: String,
    /// Directory creation/import time as RFC 3339.
    pub created_at_rfc3339: String,
    /// How this CA came into existence.
    pub origin: CaOrigin,
    /// Key algorithm identifier ([`CA_KEY_ALGORITHM_ID`]).
    pub key_algorithm: String,
    /// Public certificate filename ([`CA_CERT_FILENAME`]).
    pub cert_filename: String,
}

/// Options for [`CaAuthority::create_new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaOptions {
    /// Subject/issuer common name (bounded to [`MAX_CA_SUBJECT_CN_CHARS`]).
    pub common_name: String,
    /// Requested validity in days ([`MIN_CA_VALIDITY_DAYS` mass
    /// ..= [`MAX_CA_VALIDITY_DAYS`]).
    pub validity_days: u32,
}

impl Default for CaOptions {
    fn default() -> Self {
        Self {
            common_name: DEFAULT_CA_COMMON_NAME.to_owned(),
            validity_days: DEFAULT_CA_VALIDITY_DAYS,
        }
    }
}

impl CaOptions {
    /// Set an explicit common name.
    #[must_use]
    pub fn with_common_name(mut self, name: &str) -> Self {
        name.clone_into(&mut self.common_name);
        self
    }

    /// Set an explicit validity in days.
    #[must_use]
    pub fn with_validity_days(mut self, days: u32) -> Self {
        self.validity_days = days;
        self
    }
}

/// An opened interception CA: private key plus signing capability.
///
/// Handles are immutable snapshots bound to one directory's fingerprint.
/// Creating (rotating to) a new CA never affects an existing handle.
/// `Debug` is redacted to public facts only.
pub struct CaAuthority {
    metadata: CaMetadata,
    cert_der: Vec<u8>,
    cert_pem: Vec<u8>,
    key_pair: rcgen::KeyPair,
    /// Issuer descriptor for leaf signing. For freshly generated CAs this is
    /// the real certificate object; for reopened/imported CAs it is a
    /// reconstructed descriptor (same subject, key-id method, usages, and
    /// key) whose self-signature is never published. Either way,
    /// `signed_by` emits identical leaves: issuer name, authority key
    /// identifier, and signature depend only on the recovered fields.
    issuer: rcgen::Certificate,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
}

impl fmt::Debug for CaAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaAuthority")
            .field("fingerprint", &self.metadata.fingerprint_sha256)
            .field("origin", &self.metadata.origin)
            .field("subject", &self.metadata.subject_display)
            .field("not_before", &self.metadata.not_before_rfc3339)
            .field("not_after", &self.metadata.not_after_rfc3339)
            .finish_non_exhaustive()
    }
}

impl CaAuthority {
    /// Create a new CA in `dir`, failing if `dir` already exists.
    ///
    /// # Errors
    ///
    /// Returns [`CaError`] for invalid options, an existing directory, or
    /// storage/generation failures.
    pub fn create_new(dir: &Path, options: &CaOptions) -> Result<Self, CaError> {
        validate_options(options)?;
        if fs::symlink_metadata(dir).is_ok() {
            return Err(CaError::AlreadyExists);
        }
        let key_pair = rcgen::KeyPair::generate().map_err(|_| CaError::CertGeneration)?;
        if key_pair.algorithm() != &rcgen::PKCS_ECDSA_P256_SHA256 {
            return Err(CaError::UnsupportedAlgorithm);
        }
        let now = truncate_to_seconds(OffsetDateTime::now_utc());
        let not_after = now
            .checked_add(time::Duration::days(i64::from(options.validity_days)))
            .ok_or_else(|| CaError::InvalidOptions("validity overflow".to_owned()))?;
        let mut distinguished_name = rcgen::DistinguishedName::new();
        distinguished_name.push(rcgen::DnType::CommonName, options.common_name.as_str());
        let mut params = rcgen::CertificateParams::default();
        params.not_before = now;
        params.not_after = not_after;
        params.serial_number = Some(rcgen::SerialNumber::from_slice(
            &next_serial().to_be_bytes(),
        ));
        params.distinguished_name = distinguished_name;
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let issuer = params
            .self_signed(&key_pair)
            .map_err(|_| CaError::CertGeneration)?;
        let cert_pem = issuer.pem().into_bytes();
        let key_pem = key_pair.serialize_pem().into_bytes();
        let validated = parse_and_validate_ca(issuer.der(), true)?;
        let metadata = build_metadata(&validated, CaOrigin::Created);
        publish_ca_dir(dir, &cert_pem, &key_pem, &metadata)?;
        Self::open(dir)
    }

    /// Import an existing self-signed root CA, copying it into `dir`.
    ///
    /// Source files are only read, never modified. The runtime never depends
    /// on the source paths after a successful import.
    ///
    /// # Errors
    ///
    /// Returns [`CaError`] for oversized/unparseable/mismatched/non-CA/
    /// expired/unsupported material, or when `dir` already exists.
    pub fn import(dir: &Path, cert_src: &Path, key_src: &Path) -> Result<Self, CaError> {
        if fs::symlink_metadata(dir).is_ok() {
            return Err(CaError::AlreadyExists);
        }
        let cert_pem = read_bounded(cert_src, MAX_PEM_FILE_BYTES, "certificate")?;
        let key_pem = read_bounded(key_src, MAX_PEM_FILE_BYTES, "private key")?;
        let (cert_der, validated) = validate_import_candidate(&cert_pem, &key_pem)?;
        let metadata = build_metadata(&validated, CaOrigin::Imported);
        publish_ca_dir(dir, &cert_pem, &key_pem, &metadata)?;
        let opened = Self::open(dir)?;
        debug_assert_eq!(opened.cert_der, cert_der);
        Ok(opened)
    }

    /// Reopen an existing CA directory, revalidating structure, fingerprint
    /// binding, key pairing, and (on Unix) permissions.
    ///
    /// Expiry is intentionally not enforced here so expired CAs remain
    /// inspectable/exportable during rotation; issuance enforces validity.
    ///
    /// # Errors
    ///
    /// Returns [`CaError`] for incomplete/tampered directories, insecure
    /// permissions, or unparseable material.
    pub fn open(dir: &Path) -> Result<Self, CaError> {
        if !dir.is_dir() {
            return Err(CaError::Incomplete);
        }
        let metadata = inspect_ca(dir)?;
        check_permissions(dir)?;
        let cert_pem = read_bounded(
            &dir.join(CA_CERT_FILENAME),
            MAX_PEM_FILE_BYTES,
            "certificate",
        )?;
        let key_pem = read_bounded(
            &dir.join(CA_KEY_FILENAME),
            MAX_PEM_FILE_BYTES,
            "private key",
        )?;
        let (certs, _) = parse_identity(&cert_pem, &key_pem)?;
        if certs.len() != 1 {
            return Err(CaError::UnexpectedChain);
        }
        let cert_der: &[u8] = &certs[0];
        if fingerprint_hex(cert_der) != metadata.fingerprint_sha256 {
            return Err(CaError::FingerprintMismatch);
        }
        let key_pair = rcgen_key_pair(&key_pem)?;
        let issuer_params =
            rcgen::CertificateParams::from_ca_cert_der(&certs[0]).map_err(|_| CaError::Signing)?;
        // Reconstructed issuer descriptor (see field docs); its self-signature
        // is never published, only its name/key-id/usages/key are consumed.
        let issuer = issuer_params
            .self_signed(&key_pair)
            .map_err(|_| CaError::Signing)?;
        let (not_before, not_after) = validity_window(cert_der)?;
        Ok(Self {
            metadata,
            cert_der: cert_der.to_vec(),
            cert_pem,
            key_pair,
            issuer,
            not_before,
            not_after,
        })
    }

    /// Public metadata for this CA (safe to log or display).
    #[must_use]
    pub fn metadata(&self) -> &CaMetadata {
        &self.metadata
    }

    /// Lowercase hex SHA-256 fingerprint of the CA certificate.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.metadata.fingerprint_sha256
    }

    /// DER bytes of the CA certificate (public).
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// PEM bytes of the CA certificate (public; for operator trust setup).
    #[must_use]
    pub fn cert_pem(&self) -> &[u8] {
        &self.cert_pem
    }

    /// Certificate validity window.
    #[must_use]
    pub fn validity(&self) -> (OffsetDateTime, OffsetDateTime) {
        (self.not_before, self.not_after)
    }

    /// Copy only the public CA certificate to `dest`, refusing to overwrite.
    ///
    /// Never exports private key material.
    ///
    /// # Errors
    ///
    /// Returns [`CaError`] when the destination exists or storage fails.
    pub fn export_cert(&self, dest: &Path) -> Result<(), CaError> {
        write_new_file(dest, &self.cert_pem, CA_PUBLIC_MODE, "export")
    }

    /// Signing issuer descriptor plus CA key for leaf issuance.
    pub(crate) fn signing_keys(&self) -> (&rcgen::Certificate, &rcgen::KeyPair) {
        (&self.issuer, &self.key_pair)
    }
}

/// Read public metadata from a CA directory without touching the key.
///
/// Validates the format version and the fingerprint binding against the
/// stored certificate. Used by inspection and public-cert export paths.
///
/// # Errors
///
/// Returns [`CaError`] for missing/unparseable metadata or fingerprint
/// mismatches.
pub fn inspect_ca(dir: &Path) -> Result<CaMetadata, CaError> {
    if !dir.is_dir() {
        return Err(CaError::Incomplete);
    }
    let raw = read_bounded(
        &dir.join(CA_METADATA_FILENAME),
        MAX_METADATA_BYTES,
        "metadata",
    )?;
    let text = std::str::from_utf8(&raw)
        .map_err(|_| CaError::InvalidMetadata("metadata is not valid UTF-8".to_owned()))?;
    let metadata: CaMetadata = serde_json::from_str(text)
        .map_err(|err| CaError::InvalidMetadata(bound_detail(&err.to_string())))?;
    if metadata.format_version != CA_FORMAT_VERSION {
        return Err(CaError::UnsupportedFormatVersion(metadata.format_version));
    }
    if metadata.cert_filename != CA_CERT_FILENAME {
        return Err(CaError::InvalidMetadata(
            "unexpected certificate filename".to_owned(),
        ));
    }
    if metadata.key_algorithm != CA_KEY_ALGORITHM_ID {
        return Err(CaError::InvalidMetadata(
            "unexpected key algorithm".to_owned(),
        ));
    }
    let cert_pem = read_bounded(
        &dir.join(CA_CERT_FILENAME),
        MAX_PEM_FILE_BYTES,
        "certificate",
    )?;
    let cert_text = std::str::from_utf8(&cert_pem).map_err(|_| CaError::CertUnparseable)?;
    let (cert_der, _) = single_cert_der(cert_text)?;
    if fingerprint_hex(&cert_der) != metadata.fingerprint_sha256 {
        return Err(CaError::FingerprintMismatch);
    }
    Ok(metadata)
}

/// Copy only the public CA certificate from `dir` to `dest`.
///
/// Reads no key material and refuses to overwrite an existing destination.
///
/// # Errors
///
/// Returns [`CaError`] when the directory is invalid, the destination
/// exists, or storage fails.
pub fn export_ca_cert(dir: &Path, dest: &Path) -> Result<(), CaError> {
    inspect_ca(dir)?;
    let cert_pem = read_bounded(
        &dir.join(CA_CERT_FILENAME),
        MAX_PEM_FILE_BYTES,
        "certificate",
    )?;
    write_new_file(dest, &cert_pem, CA_PUBLIC_MODE, "export")
}

/// Restore restrictive permissions on a CA directory (Unix only).
///
/// Sets the directory to `0700`, the key to `0600`, and the public files to
/// `0644`. Never touches files outside the CA directory.
///
/// # Errors
///
/// Returns [`CaError::PermissionRepairUnsupported`] on non-Unix platforms.
pub fn repair_ca_permissions(dir: &Path) -> Result<(), CaError> {
    #[cfg(not(unix))]
    {
        let _ = dir;
        return Err(CaError::PermissionRepairUnsupported);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if !dir.is_dir() {
            return Err(CaError::Incomplete);
        }
        fs::set_permissions(dir, fs::Permissions::from_mode(CA_DIR_MODE))
            .map_err(|_| CaError::Io("repair directory"))?;
        for (name, mode) in [
            (CA_KEY_FILENAME, CA_KEY_MODE),
            (CA_CERT_FILENAME, CA_PUBLIC_MODE),
            (CA_METADATA_FILENAME, CA_PUBLIC_MODE),
        ] {
            fs::set_permissions(dir.join(name), fs::Permissions::from_mode(mode))
                .map_err(|_| CaError::Io("repair file"))?;
        }
        Ok(())
    }
}

/// Facts recovered from a validated CA certificate.
#[derive(Debug, PartialEq, Eq)]
struct ValidatedCa {
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
    subject_display: String,
    issuer_display: String,
}

/// Validate operator-supplied creation options.
fn validate_options(options: &CaOptions) -> Result<(), CaError> {
    if options.common_name.trim().is_empty() {
        return Err(CaError::InvalidOptions(
            "common name must not be empty".to_owned(),
        ));
    }
    if options.common_name.chars().count() > MAX_CA_SUBJECT_CN_CHARS {
        return Err(CaError::InvalidOptions(format!(
            "common name exceeds {MAX_CA_SUBJECT_CN_CHARS} characters"
        )));
    }
    if options.validity_days < MIN_CA_VALIDITY_DAYS || options.validity_days > MAX_CA_VALIDITY_DAYS
    {
        return Err(CaError::InvalidOptions(format!(
            "validity must be {MIN_CA_VALIDITY_DAYS}..={MAX_CA_VALIDITY_DAYS} days"
        )));
    }
    Ok(())
}

/// Validate an import candidate: pairing plus CA-property policy.
///
/// Returns the single DER certificate and its validated facts. Enforces the
/// maintained-parser rule: all X.509 property checks go through `x509-parser`
/// (read-only) and `eggnet-tls` (pairing); no custom DER parsing.
fn validate_import_candidate(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<(Vec<u8>, ValidatedCa), CaError> {
    let (certs, _) = parse_identity(cert_pem, key_pem)?;
    if certs.len() != 1 {
        return Err(CaError::UnexpectedChain);
    }
    // Supported-algorithm enforcement on the key side: the stored key must be
    // usable by the rcgen/ring signer (PKCS#8 ECDSA P-256).
    rcgen_key_pair(key_pem)?;
    let validated = parse_and_validate_ca(&certs[0], true)?;
    Ok((certs[0].to_vec(), validated))
}

/// Run `eggnet-tls` pairing validation, translating errors to redacted kinds.
fn parse_identity(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<
    (
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    ),
    CaError,
> {
    if cert_pem.len() > MAX_PEM_FILE_BYTES {
        return Err(CaError::TooLarge("certificate"));
    }
    if key_pem.len() > MAX_PEM_FILE_BYTES {
        return Err(CaError::TooLarge("private key"));
    }
    eggnet_tls::parse_identity_pem(cert_pem, key_pem).map_err(|err| {
        use eggnet_tls::TlsError as E;
        match err {
            E::MultiplePrivateKeysFound => CaError::MultipleKeys,
            E::NoPrivateKeyFound => CaError::NoKey,
            E::NoCertificatesFound
            | E::CertReadError(_)
            | E::KeyReadError(_)
            | E::CertFileNotFound(_)
            | E::KeyFileNotFound(_) => CaError::CertUnparseable,
            // `InvalidKey` covers both unloadable keys and pairing failures;
            // both fail closed as a mismatch without echoing details.
            _ => CaError::Mismatch,
        }
    })
}

/// Parse a PKCS#8 PEM key with rcgen and enforce ECDSA P-256.
fn rcgen_key_pair(key_pem: &[u8]) -> Result<rcgen::KeyPair, CaError> {
    let text = std::str::from_utf8(key_pem).map_err(|_| CaError::KeyUnparseable)?;
    let key_pair = rcgen::KeyPair::from_pem(text).map_err(|_| CaError::KeyUnparseable)?;
    if key_pair.algorithm() != &rcgen::PKCS_ECDSA_P256_SHA256 {
        return Err(CaError::UnsupportedAlgorithm);
    }
    Ok(key_pair)
}

/// Parse and validate CA properties from DER (policy checks only).
///
/// When `enforce_validity` is set, expiry is checked against the current time
/// with [`CA_CLOCK_TOLERANCE`]; `open` passes `false` so expired CAs stay
/// inspectable, while creation and import pass `true`.
fn parse_and_validate_ca(der: &[u8], enforce_validity: bool) -> Result<ValidatedCa, CaError> {
    let (_, cert) = X509Certificate::from_der(der).map_err(|_| CaError::CertUnparseable)?;
    if cert.signature_algorithm.algorithm != OID_SIG_ECDSA_WITH_SHA256 {
        return Err(CaError::UnsupportedAlgorithm);
    }
    if cert.public_key().algorithm.algorithm != OID_KEY_TYPE_EC_PUBLIC_KEY {
        return Err(CaError::UnsupportedAlgorithm);
    }
    if cert.subject().as_raw() != cert.issuer().as_raw() {
        return Err(CaError::NotSelfSigned);
    }
    let not_before = cert.validity().not_before.to_datetime();
    let not_after = cert.validity().not_after.to_datetime();
    if not_after <= not_before {
        return Err(CaError::CertUnparseable);
    }
    let is_ca = cert
        .basic_constraints()
        .map_err(|_| CaError::CertUnparseable)?
        .is_some_and(|bc| bc.value.ca);
    if !is_ca {
        return Err(CaError::NotCa);
    }
    let can_sign = cert
        .key_usage()
        .map_err(|_| CaError::CertUnparseable)?
        .is_some_and(|ku| ku.value.key_cert_sign());
    if !can_sign {
        return Err(CaError::MissingKeyCertSign);
    }
    if enforce_validity {
        check_validity(not_before, not_after, OffsetDateTime::now_utc())?;
    }
    // Cryptographic self-signature proof (ring supports ECDSA P-256/SHA-256).
    cert.verify_signature(None)
        .map_err(|_| CaError::BadSignature)?;
    Ok(ValidatedCa {
        not_before,
        not_after,
        subject_display: bound_display(&cert.subject().to_string()),
        issuer_display: bound_display(&cert.issuer().to_string()),
    })
}

/// Parse only the validity window from DER (no policy enforcement).
fn validity_window(der: &[u8]) -> Result<(OffsetDateTime, OffsetDateTime), CaError> {
    let (_, cert) = X509Certificate::from_der(der).map_err(|_| CaError::CertUnparseable)?;
    Ok((
        cert.validity().not_before.to_datetime(),
        cert.validity().not_after.to_datetime(),
    ))
}

/// Enforce the expiry policy with clock tolerance.
fn check_validity(
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
    now: OffsetDateTime,
) -> Result<(), CaError> {
    if now < not_before - CA_CLOCK_TOLERANCE_TIME {
        return Err(CaError::NotYetValid(rfc3339(not_before)));
    }
    if now > not_after + CA_CLOCK_TOLERANCE_TIME {
        return Err(CaError::Expired(rfc3339(not_after)));
    }
    Ok(())
}

/// Build public metadata for a validated certificate.
fn build_metadata(validated: &ValidatedCa, origin: CaOrigin) -> CaMetadata {
    CaMetadata {
        format_version: CA_FORMAT_VERSION,
        fingerprint_sha256: String::new(), // filled by publish path below
        subject_display: validated.subject_display.clone(),
        issuer_display: validated.issuer_display.clone(),
        not_before_rfc3339: rfc3339(validated.not_before),
        not_after_rfc3339: rfc3339(validated.not_after),
        created_at_rfc3339: rfc3339(truncate_to_seconds(OffsetDateTime::now_utc())),
        origin,
        key_algorithm: CA_KEY_ALGORITHM_ID.to_owned(),
        cert_filename: CA_CERT_FILENAME.to_owned(),
    }
}

/// Stage files in a sibling temp directory, then atomically publish.
///
/// Claims `dir` with `create_dir` (failing when it exists, so rotation
/// targets are never silently overwritten), renames staged files in, applies
/// Unix modes, and revalidates permissions. Staging residue is removed on
/// both success and failure paths.
fn publish_ca_dir(
    dir: &Path,
    cert_pem: &[u8],
    key_pem: &[u8],
    metadata: &CaMetadata,
) -> Result<(), CaError> {
    let parent = dir.parent().ok_or(CaError::Io("stage directory"))?;
    // Fingerprint binds metadata to the exact bytes being published.
    let fingerprint = fingerprint_hex(&single_cert_der_bytes(cert_pem)?);
    let mut metadata = metadata.clone();
    metadata.fingerprint_sha256 = fingerprint;
    let metadata_json =
        serde_json::to_string_pretty(&metadata).map_err(|_| CaError::Io("metadata"))?;
    let staging = tempfile::TempDir::new_in(parent).map_err(|_| CaError::Io("stage directory"))?;
    write_staged_file(&staging, CA_CERT_FILENAME, cert_pem, CA_PUBLIC_MODE)?;
    write_staged_file(
        &staging,
        CA_METADATA_FILENAME,
        metadata_json.as_bytes(),
        CA_PUBLIC_MODE,
    )?;
    write_staged_file(&staging, CA_KEY_FILENAME, key_pem, CA_KEY_MODE)?;
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(CaError::AlreadyExists);
        }
        Err(_) => return Err(CaError::Io("publish directory")),
    }
    for name in [CA_CERT_FILENAME, CA_METADATA_FILENAME, CA_KEY_FILENAME] {
        fs::rename(staging.path().join(name), dir.join(name))
            .map_err(|_| CaError::Io("publish file"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(dir, fs::Permissions::from_mode(CA_DIR_MODE))
            .map_err(|_| CaError::Io("publish directory"))?;
    }
    check_permissions(dir)?;
    Ok(())
}

/// Write one staged file with restrictive creation modes on Unix.
fn write_staged_file(
    staging: &tempfile::TempDir,
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<(), CaError> {
    use std::io::Write as _;
    let path = staging.path().join(name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .map_err(|_| CaError::Io("stage file"))?;
        file.write_all(bytes)
            .map_err(|_| CaError::Io("stage file"))?;
        file.sync_all().map_err(|_| CaError::Io("stage file"))?;
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| CaError::Io("stage file"))?;
        file.write_all(bytes)
            .map_err(|_| CaError::Io("stage file"))?;
    }
    Ok(())
}

/// Write a new file, refusing to overwrite an existing destination.
fn write_new_file(
    dest: &Path,
    bytes: &[u8],
    #[allow(unused_variables)] mode: u32,
    context: &'static str,
) -> Result<(), CaError> {
    use std::io::Write as _;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(dest)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(CaError::ExportDestExists);
            }
            Err(_) => return Err(CaError::Io(context)),
        };
        file.write_all(bytes).map_err(|_| CaError::Io(context))?;
        file.sync_all().map_err(|_| CaError::Io(context))?;
    }
    #[cfg(not(unix))]
    {
        let mut file = match OpenOptions::new().write(true).create_new(true).open(dest) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(CaError::ExportDestExists);
            }
            Err(_) => return Err(CaError::Io(context)),
        };
        file.write_all(bytes).map_err(|_| CaError::Io(context))?;
    }
    Ok(())
}

/// Enforce directory/key permissions (Unix only; documented no-op elsewhere).
fn check_permissions(dir: &Path) -> Result<(), CaError> {
    #[cfg(not(unix))]
    {
        let _ = dir;
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let dir_mode = fs::metadata(dir)
            .map_err(|_| CaError::Io("permissions"))
            .map(|m| m.permissions().mode() & 0o777)?;
        if dir_mode != CA_DIR_MODE {
            return Err(CaError::InsecurePermissions {
                target: "directory",
                mode: dir_mode,
            });
        }
        let key_mode = fs::metadata(dir.join(CA_KEY_FILENAME))
            .map_err(|_| CaError::Io("permissions"))
            .map(|m| m.permissions().mode() & 0o777)?;
        if key_mode != CA_KEY_MODE {
            return Err(CaError::InsecurePermissions {
                target: "private key",
                mode: key_mode,
            });
        }
        Ok(())
    }
}

/// Read a file with an upfront size bound (plus a streaming cap for safety).
fn read_bounded(path: &Path, limit: usize, what: &'static str) -> Result<Vec<u8>, CaError> {
    use std::io::Read as _;
    let len = fs::metadata(path)
        .map_err(|_| CaError::Io(what))
        .map(|m| m.len())?;
    if len > limit as u64 {
        return Err(CaError::TooLarge(what));
    }
    let file = fs::File::open(path).map_err(|_| CaError::Io(what))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CaError::Io(what))?;
    if bytes.len() > limit {
        return Err(CaError::TooLarge(what));
    }
    Ok(bytes)
}

/// Extract exactly one DER certificate from PEM text.
fn single_cert_der(text: &str) -> Result<(Vec<u8>, usize), CaError> {
    use rustls::pki_types::pem::PemObject as _;
    use std::io::Cursor;
    let mut count = 0;
    let mut first = Vec::new();
    for item in rustls::pki_types::CertificateDer::pem_reader_iter(Cursor::new(text.as_bytes())) {
        let cert = item.map_err(|_| CaError::CertUnparseable)?;
        count += 1;
        if count == 1 {
            first = cert.to_vec();
        }
    }
    if count != 1 {
        return Err(if count == 0 {
            CaError::CertUnparseable
        } else {
            CaError::UnexpectedChain
        });
    }
    Ok((first, count))
}

/// Extract the single DER certificate from PEM bytes being published.
fn single_cert_der_bytes(pem: &[u8]) -> Result<Vec<u8>, CaError> {
    let text = std::str::from_utf8(pem).map_err(|_| CaError::CertUnparseable)?;
    single_cert_der(text).map(|(der, _)| der)
}

/// Lowercase hex SHA-256 over DER bytes.
pub(crate) fn fingerprint_hex(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Format an [`OffsetDateTime`] as RFC 3339 (UTC, second precision).
fn rfc3339(moment: OffsetDateTime) -> String {
    chrono::DateTime::from_timestamp(moment.unix_timestamp(), moment.nanosecond()).map_or_else(
        || "1970-01-01T00:00:00Z".to_owned(),
        |dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true),
    )
}

/// Truncate sub-second precision (certificates serialize whole seconds).
pub(crate) fn truncate_to_seconds(moment: OffsetDateTime) -> OffsetDateTime {
    moment.replace_nanosecond(0).unwrap_or(moment)
}

/// Truncate a human display string to a bounded length.
fn bound_display(input: &str) -> String {
    if input.len() <= DISPLAY_LEN {
        return input.to_owned();
    }
    let mut out: String = input.chars().take(DISPLAY_LEN).collect();
    out.push_str("...");
    out
}

/// Truncate operator-supplied detail echoed in errors.
fn bound_detail(input: &str) -> String {
    if input.len() <= DETAIL_LEN {
        return input.to_owned();
    }
    let mut out: String = input.chars().take(DETAIL_LEN).collect();
    out.push_str("...");
    out
}

/// Process-unique 63-bit positive serial numbers.
///
/// A process-wide counter stepped by an odd constant from a seed mixed with
/// wall-clock time and the process id. Values are unique within the process
/// (the realistic scope: one issuer process mints its leaves); across
/// processes collisions are negligible but not impossible, which is
/// documented rather than promised.
pub(crate) fn next_serial() -> u64 {
    static SEED: OnceLock<u64> = OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seed = *SEED.get_or_init(|| {
        const FALLBACK: u64 = 0x9E37_79B9_7F4A_7C15;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(FALLBACK, |d| {
                u64::try_from(d.as_nanos()).unwrap_or(FALLBACK)
            });
        nanos
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(u64::from(std::process::id()).wrapping_mul(0xBF58_476D_1CE4_E5B9))
            | 0x0100_0000_0000_0001
    });
    let step = COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    (seed.wrapping_add(step)) & 0x7FFF_FFFF_FFFF_FFFF
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Generate a throwaway self-signed CA with rcgen for negative tests.
    fn make_rcgen_ca(
        common_name: &str,
        not_before: OffsetDateTime,
        not_after: OffsetDateTime,
        is_ca: rcgen::IsCa,
        key_usages: Vec<rcgen::KeyUsagePurpose>,
    ) -> (rcgen::KeyPair, rcgen::Certificate) {
        let key = rcgen::KeyPair::generate().expect("test key");
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, common_name);
        let mut params = rcgen::CertificateParams::default();
        params.not_before = not_before;
        params.not_after = not_after;
        params.serial_number = Some(rcgen::SerialNumber::from_slice(
            &next_serial().to_be_bytes(),
        ));
        params.distinguished_name = dn;
        params.is_ca = is_ca;
        params.key_usages = key_usages;
        let cert = params.self_signed(&key).expect("test cert");
        (key, cert)
    }

    fn default_ca_params() -> (OffsetDateTime, OffsetDateTime) {
        let now = truncate_to_seconds(OffsetDateTime::now_utc());
        (
            now - time::Duration::days(1),
            now + time::Duration::days(365),
        )
    }

    #[test]
    fn serials_are_positive_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1024 {
            let serial = next_serial();
            assert_eq!(
                serial & 0x8000_0000_0000_0000,
                0,
                "serial must stay positive"
            );
            assert!(serial > 0);
            assert!(
                seen.insert(serial),
                "serial must be unique within the process"
            );
        }
    }

    #[test]
    fn fingerprint_is_stable_hex() {
        let fp = fingerprint_hex(b"abc");
        assert_eq!(fp.len(), 64);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(fp, fingerprint_hex(b"abc"));
        assert_ne!(fp, fingerprint_hex(b"abd"));
    }

    #[test]
    fn generated_ca_validates_with_policy() {
        let (nb, na) = default_ca_params();
        let (_, cert) = make_rcgen_ca(
            "test",
            nb,
            na,
            rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0)),
            vec![
                rcgen::KeyUsagePurpose::DigitalSignature,
                rcgen::KeyUsagePurpose::KeyCertSign,
                rcgen::KeyUsagePurpose::CrlSign,
            ],
        );
        let validated = parse_and_validate_ca(cert.der(), true).expect("valid CA");
        assert_eq!(validated.not_before, nb);
        assert_eq!(validated.not_after, na);
        assert!(validated.subject_display.contains("test"));
    }

    #[test]
    fn expired_ca_fails_policy_but_parses_window() {
        let now = truncate_to_seconds(OffsetDateTime::now_utc());
        let (_, cert) = make_rcgen_ca(
            "expired",
            now - time::Duration::days(30),
            now - time::Duration::days(1),
            rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained),
            vec![rcgen::KeyUsagePurpose::KeyCertSign],
        );
        assert!(matches!(
            parse_and_validate_ca(cert.der(), true),
            Err(CaError::Expired(_))
        ));
        let (nb, na) = validity_window(cert.der()).expect("window parses");
        assert!(na < now);
        assert!(nb < na);
    }

    #[test]
    fn not_yet_valid_ca_fails_policy() {
        let now = truncate_to_seconds(OffsetDateTime::now_utc());
        let (_, cert) = make_rcgen_ca(
            "future",
            now + time::Duration::days(1),
            now + time::Duration::days(30),
            rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained),
            vec![rcgen::KeyUsagePurpose::KeyCertSign],
        );
        assert!(matches!(
            parse_and_validate_ca(cert.der(), true),
            Err(CaError::NotYetValid(_))
        ));
    }

    #[test]
    fn non_ca_certificate_is_rejected() {
        let (nb, na) = default_ca_params();
        let (_, cert) = make_rcgen_ca(
            "leaf",
            nb,
            na,
            rcgen::IsCa::NoCa,
            vec![rcgen::KeyUsagePurpose::DigitalSignature],
        );
        assert_eq!(parse_and_validate_ca(cert.der(), true), Err(CaError::NotCa));
    }

    #[test]
    fn ca_without_key_cert_sign_is_rejected() {
        let (nb, na) = default_ca_params();
        let (_, cert) = make_rcgen_ca(
            "no-sign",
            nb,
            na,
            rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained),
            vec![rcgen::KeyUsagePurpose::DigitalSignature],
        );
        assert_eq!(
            parse_and_validate_ca(cert.der(), true),
            Err(CaError::MissingKeyCertSign)
        );
    }

    #[test]
    fn options_bounds_are_enforced() {
        assert!(validate_options(&CaOptions::default()).is_ok());
        assert!(
            validate_options(&CaOptions::default().with_validity_days(MIN_CA_VALIDITY_DAYS - 1))
                .is_err()
        );
        assert!(
            validate_options(&CaOptions::default().with_validity_days(MAX_CA_VALIDITY_DAYS + 1))
                .is_err()
        );
        assert!(validate_options(&CaOptions::default().with_common_name("")).is_err());
        let long: String = "x".repeat(MAX_CA_SUBJECT_CN_CHARS + 1);
        assert!(validate_options(&CaOptions::default().with_common_name(&long)).is_err());
    }

    /// Write an rcgen certificate/key pair to source files for import tests.
    fn write_import_sources(
        dir: &Path,
        name: &str,
        key: &rcgen::KeyPair,
        cert: &rcgen::Certificate,
    ) -> (PathBuf, PathBuf) {
        use std::io::Write as _;
        let cert_src = dir.join(format!("{name}.cert.pem"));
        let key_src = dir.join(format!("{name}.key.pem"));
        let mut file = fs::File::create(&cert_src).expect("cert source");
        file.write_all(cert.pem().as_bytes()).expect("write cert");
        let mut file = fs::File::create(&key_src).expect("key source");
        file.write_all(key.serialize_pem().as_bytes())
            .expect("write key");
        (cert_src, key_src)
    }

    #[test]
    fn import_rejects_expired_and_future_cas_without_publishing() {
        let root = tempfile::TempDir::new().expect("temp root");
        let now = truncate_to_seconds(OffsetDateTime::now_utc());
        for (name, nb, na, expected) in [
            (
                "expired",
                now - time::Duration::days(30),
                now - time::Duration::days(1),
                "expired",
            ),
            (
                "future",
                now + time::Duration::days(1),
                now + time::Duration::days(30),
                "future",
            ),
        ] {
            let (key, cert) = make_rcgen_ca(
                name,
                nb,
                na,
                rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained),
                vec![rcgen::KeyUsagePurpose::KeyCertSign],
            );
            let (cert_src, key_src) = write_import_sources(root.path(), name, &key, &cert);
            let dir = root.path().join(format!("ca-{name}"));
            let result = CaAuthority::import(&dir, &cert_src, &key_src);
            if expected == "expired" {
                assert!(matches!(result, Err(CaError::Expired(_))), "{result:?}");
            } else {
                assert!(matches!(result, Err(CaError::NotYetValid(_))), "{result:?}");
            }
            assert!(!dir.exists(), "failed import must not publish");
        }
    }

    #[test]
    fn import_rejects_mismatched_and_non_ca_material() {
        let root = tempfile::TempDir::new().expect("temp root");
        let (nb, na) = default_ca_params();
        let ca_usages = vec![
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let (key_a, cert_a) = make_rcgen_ca(
            "a",
            nb,
            na,
            rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained),
            ca_usages.clone(),
        );
        let (key_b, cert_b) = make_rcgen_ca(
            "b",
            nb,
            na,
            rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained),
            ca_usages,
        );
        let (cert_a_src, _) = write_import_sources(root.path(), "a", &key_a, &cert_a);
        let (_, key_b_src) = write_import_sources(root.path(), "b", &key_b, &cert_b);
        let dir = root.path().join("ca-mismatch");
        assert_eq!(
            CaAuthority::import(&dir, &cert_a_src, &key_b_src).map(|_| ()),
            Err(CaError::Mismatch)
        );
        assert!(!dir.exists());

        // Non-CA end-entity certificate is rejected even with a matching key.
        let (leaf_key, leaf_cert) = make_rcgen_ca(
            "leaf",
            nb,
            na,
            rcgen::IsCa::NoCa,
            vec![rcgen::KeyUsagePurpose::DigitalSignature],
        );
        let (leaf_cert_src, leaf_key_src) =
            write_import_sources(root.path(), "leaf", &leaf_key, &leaf_cert);
        let dir = root.path().join("ca-leaf");
        assert_eq!(
            CaAuthority::import(&dir, &leaf_cert_src, &leaf_key_src).map(|_| ()),
            Err(CaError::NotCa)
        );
        assert!(!dir.exists());

        // Garbage bytes are rejected without publishing.
        let garbage = root.path().join("garbage.pem");
        fs::write(&garbage, b"not a pem file at all").expect("write garbage");
        let dir = root.path().join("ca-garbage");
        assert!(
            CaAuthority::import(&dir, &garbage, &leaf_key_src).is_err(),
            "garbage cert must fail"
        );
        assert!(!dir.exists());
    }

    #[test]
    fn error_strings_carry_no_key_material() {
        // Every variant's Display must be constructible and free of PEM markers.
        let errors = vec![
            CaError::AlreadyExists,
            CaError::Incomplete,
            CaError::Io("test"),
            CaError::TooLarge("private key"),
            CaError::InvalidMetadata("x".to_owned()),
            CaError::UnsupportedFormatVersion(99),
            CaError::FingerprintMismatch,
            CaError::CertUnparseable,
            CaError::KeyUnparseable,
            CaError::MultipleKeys,
            CaError::NoKey,
            CaError::Mismatch,
            CaError::UnexpectedChain,
            CaError::NotSelfSigned,
            CaError::BadSignature,
            CaError::NotCa,
            CaError::MissingKeyCertSign,
            CaError::UnsupportedAlgorithm,
            CaError::Expired("today".to_owned()),
            CaError::NotYetValid("today".to_owned()),
            CaError::InsecurePermissions {
                target: "private key",
                mode: 0o644,
            },
            CaError::PermissionRepairUnsupported,
            CaError::CertGeneration,
            CaError::Signing,
            CaError::InvalidOptions("x".to_owned()),
            CaError::ExportDestExists,
        ];
        for err in errors {
            let text = err.to_string();
            assert!(!text.contains("PRIVATE KEY"), "leak in {text}");
            assert!(!text.contains("BEGIN"), "leak in {text}");
            assert!(!text.contains(".pem"), "path leak in {text}");
            assert!(!text.contains("-----"), "PEM block leak in {text}");
        }
    }
}
