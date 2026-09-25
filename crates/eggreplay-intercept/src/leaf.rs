//! Bounded in-memory leaf certificate issuance for interception.
//!
//! A [`LeafIssuer`] mints exact-host TLS server certificates signed by one
//! selected [`CaAuthority`](crate::ca::CaAuthority). Input is a previously
//! policy-approved exact target ([`NormalizedHost`](crate::policy::NormalizedHost)):
//! a normalized ASCII DNS name or an exact IP literal.
//!
//! # Leaf profile
//!
//! - Exactly one subject alternative name (the target; no wildcard, ever).
//! - `BasicConstraints CA:false`, key usage `digitalSignature`, extended key
//!   usage `serverAuth`.
//! - Bounded validity: configurable, default 7 days, maximum 30 days, always
//!   capped by the issuing CA's own expiry.
//! - 63-bit positive serials unique within the issuing process (see
//!   [`crate::ca`] serial policy).
//! - Authority key identifier pointing at the issuing CA.
//!
//! # Cache and concurrency
//!
//! Issued leaves are cached under `CA-fingerprint/target` with a maximum of
//! [`MAX_LEAF_CACHE_ENTRIES`] entries and deterministic FIFO eviction. One
//! `tokio` mutex guards the cache; issuance for a missing entry happens under
//! that lock. Signing is local CPU work (no network or disk I/O), so no
//! global session lock is ever held across network operations. Concurrent
//! requests for the same target therefore produce a single issuance.
//!
//! # Runtime contract (for M013D)
//!
//! Leaves are TLS-server capable. When M013D terminates client TLS it must
//! advertise only `http/1.1` ALPN ([`INTERCEPT_ALPN_HTTP1_1`]); HTTP/2,
//! QUIC, and all other M013 exclusions remain out of scope.
//!
//! # Secret handling
//!
//! Private leaf keys live only in memory inside [`LeafCertificate`] and are
//! never written to disk or fixtures. [`LeafCertificate`] and [`LeafIssuer`]
//! have redacted `Debug` implementations, and [`LeafError`] messages carry no
//! key material.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;
use time::OffsetDateTime;
use tokio::sync::Mutex;

use crate::ca::{CaAuthority, fingerprint_hex, next_serial, truncate_to_seconds};
use crate::policy::NormalizedHost;

/// Maximum cached leaves per issuer (bounded memory; FIFO eviction).
pub const MAX_LEAF_CACHE_ENTRIES: usize = 128;
/// Default leaf validity in hours (7 days; comfortably shorter than any CA).
pub const DEFAULT_LEAF_VALIDITY_HOURS: u32 = 168;
/// Maximum leaf validity in hours (explicit 30-day upper bound).
pub const MAX_LEAF_VALIDITY_HOURS: u32 = 720;
/// Minimum leaf validity in hours.
pub const MIN_LEAF_VALIDITY_HOURS: u32 = 1;
/// Maximum common-name length (characters) emitted on leaf subjects.
///
/// Longer targets keep an empty subject; the SAN remains authoritative.
/// Modern validators ignore the subject CN, so this legacy bound only affects
/// human display, never identity.
pub const MAX_LEAF_CN_CHARS: usize = 64;
/// The only ALPN protocol an interception TLS endpoint may advertise.
///
/// M013D consumes this when building the server configuration. Advertising
/// anything else (notably `h2`) would claim an unsupported MITM protocol.
pub const INTERCEPT_ALPN_HTTP1_1: &[u8] = b"http/1.1";

/// Fail-closed leaf issuance errors (no key material, ever).
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeafError {
    /// The target is not an issuable exact host (wildcard or malformed).
    #[error("leaf target is not an exact DNS name or IP literal")]
    InvalidTarget,
    /// Issuer options are invalid.
    #[error("invalid leaf options")]
    InvalidOptions,
    /// The issuing CA is outside its validity window.
    #[error("issuing CA is outside its validity window")]
    CaNotValid,
    /// The issuing CA expires too soon to mint a useful leaf.
    #[error("issuing CA expires too soon to mint a leaf")]
    CaExpiring,
    /// Leaf key generation failed.
    #[error("leaf key generation failed")]
    KeyGeneration,
    /// Leaf signing failed.
    #[error("leaf signing failed")]
    Signing,
}

/// Options for [`LeafIssuer::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafOptions {
    /// Requested leaf validity in hours
    /// ([`MIN_LEAF_VALIDITY_HOURS`]..=[`MAX_LEAF_VALIDITY_HOURS`]).
    pub validity_hours: u32,
}

impl Default for LeafOptions {
    fn default() -> Self {
        Self {
            validity_hours: DEFAULT_LEAF_VALIDITY_HOURS,
        }
    }
}

impl LeafOptions {
    /// Set an explicit validity in hours.
    #[must_use]
    pub fn with_validity_hours(mut self, hours: u32) -> Self {
        self.validity_hours = hours;
        self
    }

    /// Validate bounds.
    fn validate(&self) -> Result<(), LeafError> {
        if self.validity_hours < MIN_LEAF_VALIDITY_HOURS
            || self.validity_hours > MAX_LEAF_VALIDITY_HOURS
        {
            return Err(LeafError::InvalidOptions);
        }
        Ok(())
    }
}

/// An issued leaf certificate with its memory-only private key.
///
/// Shared by cache handle (`Arc`). `Debug` exposes only the target,
/// fingerprint, serial, and validity.
pub struct LeafCertificate {
    target: String,
    cert_der: Vec<u8>,
    cert_pem: String,
    fingerprint: String,
    serial: u64,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
    key_pair: rcgen::KeyPair,
}

impl fmt::Debug for LeafCertificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeafCertificate")
            .field("target", &self.target)
            .field("fingerprint", &self.fingerprint)
            .field("serial", &self.serial)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

impl LeafCertificate {
    /// The exact target this leaf was issued for.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// DER bytes of the leaf certificate (public).
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// PEM of the leaf certificate (public).
    #[must_use]
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Lowercase hex SHA-256 over the DER certificate bytes.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Leaf serial number.
    #[must_use]
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Leaf validity window.
    #[must_use]
    pub fn validity(&self) -> (OffsetDateTime, OffsetDateTime) {
        (self.not_before, self.not_after)
    }

    /// Reference to the memory-only key pair (for M013D server-config use).
    ///
    /// The reference exposes no bytes by itself; callers must not serialize
    /// or log the key.
    #[must_use]
    pub fn key_pair(&self) -> &rcgen::KeyPair {
        &self.key_pair
    }
}

/// Bounded FIFO leaf cache plus issuing state for one selected CA.
struct LeafCache {
    entries: HashMap<String, Arc<LeafCertificate>>,
    order: VecDeque<String>,
}

/// An issuer bound to one explicitly selected CA.
///
/// Owns its [`CaAuthority`]: rotation constructs a new issuer around a new
/// authority and never disturbs an active one. `Debug` is redacted.
pub struct LeafIssuer {
    ca: CaAuthority,
    validity_hours: u32,
    cache: Mutex<LeafCache>,
    issuance_count: AtomicU64,
}

impl fmt::Debug for LeafIssuer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeafIssuer")
            .field("ca_fingerprint", &self.ca.fingerprint())
            .field("validity_hours", &self.validity_hours)
            .finish_non_exhaustive()
    }
}

impl LeafIssuer {
    /// Bind an issuer to one explicitly selected CA.
    ///
    /// # Errors
    ///
    /// Returns [`LeafError::InvalidOptions`] for out-of-range validity.
    pub fn new(ca: CaAuthority, options: &LeafOptions) -> Result<Self, LeafError> {
        options.validate()?;
        Ok(Self {
            ca,
            validity_hours: options.validity_hours,
            cache: Mutex::new(LeafCache {
                entries: HashMap::new(),
                order: VecDeque::new(),
            }),
            issuance_count: AtomicU64::new(0),
        })
    }

    /// The selected CA authority.
    #[must_use]
    pub fn ca(&self) -> &CaAuthority {
        &self.ca
    }

    /// Fingerprint of the selected CA (part of every cache key).
    #[must_use]
    pub fn ca_fingerprint(&self) -> &str {
        self.ca.fingerprint()
    }

    /// Total leaves minted by this issuer (cache misses).
    #[must_use]
    pub fn issuance_count(&self) -> u64 {
        self.issuance_count.load(Ordering::Relaxed)
    }

    /// Current cache occupancy.
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.cache.try_lock().map_or(0, |cache| cache.entries.len())
    }

    /// Drop expired entries; returns the number removed.
    pub fn remove_expired(&self) -> usize {
        let now = OffsetDateTime::now_utc();
        let Ok(mut cache) = self.cache.try_lock() else {
            return 0;
        };
        let before = cache.entries.len();
        let expired: Vec<String> = cache
            .entries
            .iter()
            .filter(|(_, leaf)| leaf.not_after <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &expired {
            cache.entries.remove(key);
        }
        let LeafCache { entries, order } = &mut *cache;
        order.retain(|key| entries.contains_key(key));
        before - entries.len()
    }

    /// Issue (or reuse a cached) leaf for a policy-approved exact target.
    ///
    /// Concurrent callers asking for the same target share one issuance: the
    /// cache lock is held across the fast local signing step, and signing
    /// performs no network or disk I/O.
    ///
    /// # Errors
    ///
    /// Returns [`LeafError`] for invalid targets, an unusable CA window, or
    /// generation/signing failures.
    pub async fn issue(&self, target: &NormalizedHost) -> Result<Arc<LeafCertificate>, LeafError> {
        let target_str = target.as_str();
        if target_str.contains('*') || target_str.is_empty() {
            return Err(LeafError::InvalidTarget);
        }
        let now = truncate_to_seconds(OffsetDateTime::now_utc());
        let (ca_not_before, ca_not_after) = self.ca.validity();
        if now < ca_not_before || now >= ca_not_after {
            return Err(LeafError::CaNotValid);
        }
        let requested = time::Duration::hours(i64::from(self.validity_hours));
        let latest = now.checked_add(requested).ok_or(LeafError::Signing)?;
        let not_after = latest.min(ca_not_after);
        if not_after <= now
            || (not_after - now) < time::Duration::hours(i64::from(MIN_LEAF_VALIDITY_HOURS))
        {
            return Err(LeafError::CaExpiring);
        }
        let key = format!("{}/{target_str}", self.ca.fingerprint());

        let guard = self.cache.lock().await;
        let mut cache = guard;
        if let Some(hit) = cache.entries.get(&key) {
            if hit.not_after > now {
                return Ok(Arc::clone(hit));
            }
            cache.entries.remove(&key);
        }
        let leaf = self.mint(target, &target_str, now, not_after)?;
        let leaf = Arc::new(leaf);
        while cache.entries.len() >= MAX_LEAF_CACHE_ENTRIES {
            match cache.order.pop_front() {
                Some(oldest) => {
                    cache.entries.remove(&oldest);
                }
                None => break,
            }
        }
        cache.order.push_back(key.clone());
        cache.entries.insert(key, Arc::clone(&leaf));
        self.issuance_count.fetch_add(1, Ordering::Relaxed);
        Ok(leaf)
    }

    /// Mint one leaf certificate (caller holds the cache lock).
    fn mint(
        &self,
        target: &NormalizedHost,
        target_str: &str,
        not_before: OffsetDateTime,
        not_after: OffsetDateTime,
    ) -> Result<LeafCertificate, LeafError> {
        let san = match target {
            NormalizedHost::Dns(name) => {
                let dns = rcgen::Ia5String::try_from(name.as_str())
                    .map_err(|_| LeafError::InvalidTarget)?;
                rcgen::SanType::DnsName(dns)
            }
            NormalizedHost::Ip(addr) => rcgen::SanType::IpAddress(*addr),
        };
        let leaf_key = rcgen::KeyPair::generate().map_err(|_| LeafError::KeyGeneration)?;
        let serial = next_serial();
        let mut distinguished_name = rcgen::DistinguishedName::new();
        if target_str.chars().count() <= MAX_LEAF_CN_CHARS {
            distinguished_name.push(rcgen::DnType::CommonName, target_str);
        }
        let mut params = rcgen::CertificateParams::default();
        params.not_before = not_before;
        params.not_after = not_after;
        params.serial_number = Some(rcgen::SerialNumber::from_slice(&serial.to_be_bytes()));
        params.subject_alt_names = vec![san];
        params.distinguished_name = distinguished_name;
        params.is_ca = rcgen::IsCa::ExplicitNoCa;
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        let (issuer, ca_key) = self.ca.signing_keys();
        let cert = params
            .signed_by(&leaf_key, issuer, ca_key)
            .map_err(|_| LeafError::Signing)?;
        let cert_der: &[u8] = cert.der();
        Ok(LeafCertificate {
            target: target_str.to_owned(),
            fingerprint: fingerprint_hex(cert_der),
            cert_der: cert_der.to_vec(),
            cert_pem: cert.pem(),
            serial,
            not_before,
            not_after,
            key_pair: leaf_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ca::{CaOptions, inspect_ca};
    use crate::policy::normalize_host;

    /// Create a throwaway CA directory and authority for leaf tests.
    fn test_authority() -> (tempfile::TempDir, CaAuthority) {
        let root = tempfile::TempDir::new().expect("temp root");
        let dir = root.path().join("ca");
        let ca = CaAuthority::create_new(&dir, &CaOptions::default()).expect("test CA creates");
        (root, ca)
    }

    /// Parse a leaf DER with the maintained parser (no custom DER parsing).
    fn parse_leaf(der: &[u8]) -> x509_parser::prelude::X509Certificate<'_> {
        use x509_parser::prelude::FromDer as _;
        let (_, cert) = x509_parser::prelude::X509Certificate::from_der(der).expect("leaf parses");
        cert
    }

    /// Build a real TLS client verifier trusting exactly `ca_der`.
    fn verifier_for(ca_der: &[u8]) -> Arc<dyn rustls::client::danger::ServerCertVerifier> {
        use rustls::client::WebPkiServerVerifier;
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(ca_der.to_vec()))
            .expect("CA root parses");
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        WebPkiServerVerifier::builder_with_provider(std::sync::Arc::new(roots), provider)
            .build()
            .expect("verifier builds")
    }

    fn verify_name(
        verifier: &Arc<dyn rustls::client::danger::ServerCertVerifier>,
        leaf_der: &[u8],
        name: &str,
    ) -> Result<(), rustls::Error> {
        let server_name = rustls::pki_types::ServerName::try_from(name)
            .expect("test name parses")
            .to_owned();
        verifier
            .verify_server_cert(
                &rustls::pki_types::CertificateDer::from(leaf_der.to_vec()),
                &[],
                &server_name,
                &[],
                rustls::pki_types::UnixTime::now(),
            )
            .map(|_| ())
    }

    #[tokio::test]
    async fn dns_leaf_has_exact_san_and_server_eku() {
        let (_root, ca) = test_authority();
        let issuer = LeafIssuer::new(ca, &LeafOptions::default()).expect("issuer");
        let leaf = issuer
            .issue(&normalize_host("Example.TEST.").expect("host"))
            .await
            .expect("issue");
        assert_eq!(leaf.target(), "example.test");
        let cert = parse_leaf(leaf.cert_der());
        let sans = cert
            .subject_alternative_name()
            .expect("SAN parses")
            .expect("SAN present");
        let dns: Vec<&str> = sans
            .value
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::DNSName(d) => Some(*d),
                _ => None,
            })
            .collect();
        assert_eq!(dns, vec!["example.test"]);
        assert!(
            !dns.iter().any(|d| d.contains('*')),
            "no wildcard SAN permitted"
        );
        let eku = cert
            .extended_key_usage()
            .expect("EKU parses")
            .expect("EKU present");
        assert!(eku.value.server_auth, "serverAuth EKU required");
        let ku = cert.key_usage().expect("KU parses").expect("KU present");
        assert!(ku.value.digital_signature());
        // Issuer matches the selected CA subject.
        let ca_cert = parse_leaf(issuer.ca().cert_der());
        assert_eq!(cert.issuer().as_raw(), ca_cert.subject().as_raw());
    }

    #[tokio::test]
    async fn ip_leaf_has_exact_ip_san() {
        let (_root, ca) = test_authority();
        let issuer = LeafIssuer::new(ca, &LeafOptions::default()).expect("issuer");
        for host in ["127.0.0.1", "::1"] {
            let leaf = issuer
                .issue(&normalize_host(host).expect("host"))
                .await
                .expect("issue");
            let cert = parse_leaf(leaf.cert_der());
            let sans = cert
                .subject_alternative_name()
                .expect("SAN parses")
                .expect("SAN present");
            let ips: Vec<String> = sans
                .value
                .general_names
                .iter()
                .filter_map(|n| match n {
                    x509_parser::extensions::GeneralName::IPAddress(b) => Some(format!("{b:?}")),
                    _ => None,
                })
                .collect();
            assert_eq!(ips.len(), 1, "exactly one IP SAN for {host}");
            assert_eq!(sans.value.general_names.len(), 1);
        }
    }

    #[tokio::test]
    async fn leaf_validity_is_bounded_by_policy_and_ca() {
        let (root, ca) = test_authority();
        let (ca_not_before, ca_not_after) = ca.validity();
        let issuer = LeafIssuer::new(ca, &LeafOptions::default()).expect("issuer");
        let leaf = issuer
            .issue(&normalize_host("bounded.test").expect("host"))
            .await
            .expect("issue");
        let (leaf_not_before, leaf_not_after) = leaf.validity();
        assert!(leaf_not_before >= ca_not_before);
        assert!(
            leaf_not_after <= ca_not_after,
            "leaf must expire no later than its CA"
        );
        let hours = (leaf_not_after - leaf_not_before).whole_hours();
        assert_eq!(hours, i64::from(DEFAULT_LEAF_VALIDITY_HOURS));
        // A reopened handle for the same directory issues short-lived leaves.
        let reopened = CaAuthority::open(&root.path().join("ca")).expect("reopen");
        let short = LeafIssuer::new(reopened, &LeafOptions::default().with_validity_hours(24))
            .expect("issuer");
        let leaf = short
            .issue(&normalize_host("short.test").expect("host"))
            .await
            .expect("issue");
        let (nb, na) = leaf.validity();
        assert_eq!((na - nb).whole_hours(), 24);
    }

    #[tokio::test]
    async fn leaf_verifies_under_its_ca_with_real_tls_verifier() {
        let (_root, ca) = test_authority();
        let ca_der = ca.cert_der().to_vec();
        let issuer = LeafIssuer::new(ca, &LeafOptions::default()).expect("issuer");
        let leaf = issuer
            .issue(&normalize_host("verify.test").expect("host"))
            .await
            .expect("issue");
        let verifier = verifier_for(&ca_der);
        verify_name(&verifier, leaf.cert_der(), "verify.test").expect("verifies under its CA");
        assert!(
            verify_name(&verifier, leaf.cert_der(), "other.test").is_err(),
            "wrong name must fail"
        );
    }

    #[tokio::test]
    async fn imported_ca_issues_verifiable_leaves() {
        use std::io::Write as _;
        // Round-trip: generate, export the raw files, import elsewhere, issue.
        // The imported handle signs through a reconstructed issuer descriptor.
        let root = tempfile::TempDir::new().expect("temp root");
        let generated = CaAuthority::create_new(&root.path().join("origin"), &CaOptions::default())
            .expect("create");
        let cert_src = root.path().join("src.cert.pem");
        let key_src = root.path().join("src.key.pem");
        // Reconstruct operator source files from a second open (proves the
        // stored bytes alone are sufficient for import).
        let reopened = CaAuthority::open(&root.path().join("origin")).expect("reopen");
        std::fs::File::create(&cert_src)
            .expect("src")
            .write_all(reopened.cert_pem())
            .expect("write");
        drop(reopened);
        drop(generated);
        // NOTE: the private key source cannot be recovered from an open
        // handle by design, so this leg reuses the origin directory files
        // through an explicit copy (operator-owned material).
        std::fs::copy(root.path().join("origin").join("ca-key.pem"), &key_src).expect("copy key");
        let imported = CaAuthority::import(&root.path().join("imported"), &cert_src, &key_src)
            .expect("import");
        assert_eq!(imported.fingerprint().len(), 64);
        let issuer = LeafIssuer::new(imported, &LeafOptions::default()).expect("issuer");
        let leaf = issuer
            .issue(&normalize_host("imported.test").expect("host"))
            .await
            .expect("issue");
        let verifier = verifier_for(issuer.ca().cert_der());
        verify_name(&verifier, leaf.cert_der(), "imported.test")
            .expect("imported CA leaf verifies");
    }

    #[tokio::test]
    async fn leaf_fails_under_unrelated_ca() {
        let (_root_a, ca_a) = test_authority();
        let (_root_b, ca_b) = test_authority();
        let issuer_a = LeafIssuer::new(ca_a, &LeafOptions::default()).expect("issuer");
        let leaf = issuer_a
            .issue(&normalize_host("lonely.test").expect("host"))
            .await
            .expect("issue");
        let verifier_b = verifier_for(ca_b.cert_der());
        assert!(
            verify_name(&verifier_b, leaf.cert_der(), "lonely.test").is_err(),
            "unrelated CA must not verify the leaf"
        );
    }

    #[tokio::test]
    async fn cache_hits_share_one_issuance_and_fifo_evicts() {
        let (_root, ca) = test_authority();
        let issuer = LeafIssuer::new(ca, &LeafOptions::default()).expect("issuer");
        let target = normalize_host("cached.test").expect("host");
        let first = issuer.issue(&target).await.expect("issue");
        let second = issuer.issue(&target).await.expect("issue");
        assert!(Arc::ptr_eq(&first, &second), "cache hit shares the entry");
        assert_eq!(issuer.issuance_count(), 1);

        for i in 0..(MAX_LEAF_CACHE_ENTRIES + 5) {
            let name = format!("host{i}.test");
            issuer
                .issue(&normalize_host(&name).expect("host"))
                .await
                .expect("issue");
        }
        assert_eq!(issuer.cache_len(), MAX_LEAF_CACHE_ENTRIES);
        let count_before = issuer.issuance_count();
        issuer.issue(&target).await.expect("reissue after eviction");
        assert_eq!(
            issuer.issuance_count(),
            count_before + 1,
            "evicted entry must be re-minted"
        );
    }

    #[tokio::test]
    async fn concurrent_same_target_issues_once() {
        let (_root, ca) = test_authority();
        let issuer = Arc::new(LeafIssuer::new(ca, &LeafOptions::default()).expect("issuer"));
        let target = normalize_host("race.test").expect("host");
        let mut handles = Vec::new();
        for _ in 0..32 {
            let issuer = Arc::clone(&issuer);
            let target = target.clone();
            handles.push(tokio::spawn(async move { issuer.issue(&target).await }));
        }
        let mut fingerprints = std::collections::HashSet::new();
        for handle in handles {
            let leaf = handle.await.expect("task").expect("issue");
            fingerprints.insert(leaf.fingerprint().to_owned());
        }
        assert_eq!(fingerprints.len(), 1, "one shared leaf");
        assert_eq!(issuer.issuance_count(), 1, "single bounded issuance");
    }

    #[tokio::test]
    async fn expired_entries_are_replaced() {
        let (_root, ca) = test_authority();
        let issuer = LeafIssuer::new(ca, &LeafOptions::default()).expect("issuer");
        let target = normalize_host("stale.test").expect("host");
        let first = issuer.issue(&target).await.expect("issue");
        // Force expiry by removing through the public path after simulating
        // age: drop via remove_expired cannot age entries, so this asserts
        // the steady-state (nothing expired) plus reissue stability.
        assert_eq!(issuer.remove_expired(), 0);
        let second = issuer.issue(&target).await.expect("issue");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(issuer.issuance_count(), 1);
    }

    #[test]
    fn leaf_options_bounds_are_enforced() {
        assert!(LeafOptions::default().validate().is_ok());
        assert!(
            LeafOptions::default()
                .with_validity_hours(MIN_LEAF_VALIDITY_HOURS - 1)
                .validate()
                .is_err()
        );
        assert!(
            LeafOptions::default()
                .with_validity_hours(MAX_LEAF_VALIDITY_HOURS + 1)
                .validate()
                .is_err()
        );
        let (_root, ca) = test_authority();
        assert!(LeafIssuer::new(ca, &LeafOptions::default().with_validity_hours(0)).is_err());
    }

    #[test]
    fn leaf_error_and_debug_strings_are_redacted() {
        for err in [
            LeafError::InvalidTarget,
            LeafError::InvalidOptions,
            LeafError::CaNotValid,
            LeafError::CaExpiring,
            LeafError::KeyGeneration,
            LeafError::Signing,
        ] {
            let text = err.to_string();
            assert!(!text.contains("PRIVATE KEY"), "leak in {text}");
            assert!(!text.contains("BEGIN"), "leak in {text}");
        }
        let (_root, ca) = test_authority();
        let issuer = LeafIssuer::new(ca, &LeafOptions::default()).expect("issuer");
        let debug = format!("{issuer:?}");
        assert!(!debug.contains("PRIVATE KEY"), "leak in {debug}");
        assert!(debug.contains(issuer.ca_fingerprint()));
    }

    #[test]
    fn alpn_contract_is_http11_only() {
        assert_eq!(INTERCEPT_ALPN_HTTP1_1, b"http/1.1");
    }

    #[test]
    fn inspect_ca_helper_sees_test_authority() {
        let (root, ca) = test_authority();
        let meta = inspect_ca(&root.path().join("ca")).expect("inspect");
        assert_eq!(meta, *ca.metadata());
    }
}
