//! M013C CA lifecycle and leaf issuance integration proofs.
//!
//! Hermetic and local-only: every test roots itself in a fresh temporary
//! directory. No `.eggr` store, network, or trust installation is involved.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use eggreplay_intercept::{
    CaAuthority, CaError, CaOptions, LeafIssuer, LeafOptions, export_ca_cert, inspect_ca,
    normalize_host, repair_ca_permissions,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn create_ca(root: &Path, name: &str) -> (PathBuf, CaAuthority) {
    let dir = root.join(name);
    let ca = CaAuthority::create_new(&dir, &CaOptions::default()).expect("create CA");
    (dir, ca)
}

fn dir_entries(dir: &Path) -> HashSet<String> {
    fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// A distinctive slice of the CA private key base64 payload (test-only).
fn key_sentinel(ca_dir: &Path) -> String {
    let key_pem = fs::read_to_string(ca_dir.join("ca-key.pem")).expect("read key");
    let body: String = key_pem
        .lines()
        .filter(|line| !line.contains("-----"))
        .collect();
    assert!(body.len() > 80, "key payload must be substantial");
    body[20..60].to_owned()
}

fn collect_error_strings() -> Vec<String> {
    let missing = Path::new("/definitely/not/here");
    let mut out = Vec::new();
    for result in [
        CaAuthority::open(missing).map(drop),
        CaAuthority::create_new(missing, &CaOptions::default()).map(drop),
        export_ca_cert(missing, &missing.join("out.pem")).map(drop),
        inspect_ca(missing).map(drop),
    ] {
        if let Err(err) = result {
            out.push(err.to_string());
        }
    }
    out.push(
        CaError::InsecurePermissions {
            target: "private key",
            mode: 0o644,
        }
        .to_string(),
    );
    out
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn initialize_and_reopen_preserves_identity() {
    let root = tempfile::TempDir::new().expect("temp root");
    let (dir, ca) = create_ca(root.path(), "ca");
    assert_eq!(
        dir_entries(&dir),
        HashSet::from([
            "metadata.json".to_owned(),
            "ca-cert.pem".to_owned(),
            "ca-key.pem".to_owned(),
        ])
    );
    let meta = ca.metadata();
    assert_eq!(meta.format_version, 1);
    assert_eq!(meta.fingerprint_sha256.len(), 64);
    assert!(
        meta.fingerprint_sha256
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    );
    assert!(meta.subject_display.contains("EggReplay Interception CA"));
    assert_eq!(meta.origin, eggreplay_intercept::CaOrigin::Created);
    assert_eq!(meta.key_algorithm, "ECDSA-P256-SHA256");
    assert_eq!(meta.cert_filename, "ca-cert.pem");
    // Metadata must never carry key material markers.
    let meta_text = fs::read_to_string(dir.join("metadata.json")).expect("metadata");
    assert!(!meta_text.contains("PRIVATE KEY"));
    assert!(!meta_text.contains("-----BEGIN"));

    let reopened = CaAuthority::open(&dir).expect("reopen");
    assert_eq!(reopened.metadata(), ca.metadata());
    assert_eq!(reopened.fingerprint(), ca.fingerprint());
    assert_eq!(reopened.cert_der(), ca.cert_der());
}

#[test]
fn create_new_never_overwrites() {
    let root = tempfile::TempDir::new().expect("temp root");
    let (dir, ca) = create_ca(root.path(), "ca");
    let fingerprint = ca.fingerprint().to_owned();
    let meta_text = fs::read_to_string(dir.join("metadata.json")).expect("metadata");
    assert_eq!(
        CaAuthority::create_new(&dir, &CaOptions::default()).map(drop),
        Err(CaError::AlreadyExists)
    );
    // Originals are untouched.
    let reopened = CaAuthority::open(&dir).expect("reopen");
    assert_eq!(reopened.fingerprint(), fingerprint);
    assert_eq!(
        fs::read_to_string(dir.join("metadata.json")).expect("metadata"),
        meta_text
    );
}

#[test]
fn import_copies_and_detaches_from_sources() {
    let root = tempfile::TempDir::new().expect("temp root");
    // Operator-owned source files (copied out of a created CA for the test).
    let (_src_dir, src_ca) = create_ca(root.path(), "src-ca");
    let cert_src = root.path().join("op.cert.pem");
    let key_src = root.path().join("op.key.pem");
    fs::copy(root.path().join("src-ca").join("ca-cert.pem"), &cert_src).expect("copy");
    fs::copy(root.path().join("src-ca").join("ca-key.pem"), &key_src).expect("copy");

    let dir = root.path().join("imported");
    let imported = CaAuthority::import(&dir, &cert_src, &key_src).expect("import");
    assert_eq!(
        imported.metadata().origin,
        eggreplay_intercept::CaOrigin::Imported
    );
    assert_eq!(imported.fingerprint(), src_ca.fingerprint());

    // Sources can vanish; the runtime no longer depends on them.
    fs::remove_file(&cert_src).expect("remove");
    fs::remove_file(&key_src).expect("remove");
    let reopened = CaAuthority::open(&dir).expect("reopen after source removal");
    assert_eq!(reopened.fingerprint(), src_ca.fingerprint());
}

#[test]
fn failed_import_leaves_no_residue() {
    let root = tempfile::TempDir::new().expect("temp root");
    let (_a_dir, ca_a) = create_ca(root.path(), "a");
    let (_b_dir, _ca_b) = create_ca(root.path(), "b");
    // Mismatched key: cert from A, key from B.
    let dir = root.path().join("bad-import");
    let result = CaAuthority::import(
        &dir,
        &root.path().join("a").join("ca-cert.pem"),
        &root.path().join("b").join("ca-key.pem"),
    );
    assert_eq!(result.map(drop), Err(CaError::Mismatch));
    assert!(!dir.exists(), "failed import must not publish");
    // No staging residue next to the target.
    for entry in dir_entries(root.path()) {
        assert!(!entry.starts_with(".tmp"), "staging residue: {entry}");
    }
    let _ = ca_a;
}

#[test]
fn tampered_cert_breaks_fingerprint_binding() {
    let root = tempfile::TempDir::new().expect("temp root");
    let (dir, _ca) = create_ca(root.path(), "ca");
    let cert_path = dir.join("ca-cert.pem");
    let text = fs::read_to_string(&cert_path).expect("read cert");
    // Deterministic tamper: flip one base64 body char to a different valid
    // alphabet char so the PEM armor stays parseable but the DER changes.
    // Either rejection proves the fingerprint binding holds: Mismatch when
    // the DER still parses but no longer matches metadata, Unparseable when
    // the flip lands on framing-sensitive bits.
    let header_end = text.find("-----END").expect("footer");
    let body_end = text[..header_end]
        .rfind(char::is_alphanumeric)
        .expect("body");
    let mut bytes = text.into_bytes();
    let original = bytes[body_end];
    let replacement = if original == b'A' { b'B' } else { b'A' };
    bytes[body_end] = replacement;
    fs::write(&cert_path, &bytes).expect("tamper");
    assert!(
        matches!(
            CaAuthority::open(&dir).map(drop),
            Err(CaError::FingerprintMismatch | CaError::CertUnparseable)
        ),
        "tampered cert must be rejected"
    );
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn unix_permissions_are_enforced_and_repaired() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::TempDir::new().expect("temp root");
    let (dir, _ca) = create_ca(root.path(), "ca");
    let mode = |name: &str| {
        fs::metadata(dir.join(name))
            .expect("stat")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(
        fs::metadata(&dir).expect("stat").permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(mode("ca-key.pem"), 0o600);
    assert_eq!(mode("ca-cert.pem"), 0o644);
    assert_eq!(mode("metadata.json"), 0o644);

    // Loosened key permissions fail closed with an explicit repair path.
    fs::set_permissions(dir.join("ca-key.pem"), fs::Permissions::from_mode(0o644)).expect("loosen");
    assert!(matches!(
        CaAuthority::open(&dir).map(drop),
        Err(CaError::InsecurePermissions { .. })
    ));
    repair_ca_permissions(&dir).expect("repair");
    assert_eq!(mode("ca-key.pem"), 0o600);
    CaAuthority::open(&dir).expect("opens after repair");
}

#[cfg(unix)]
#[test]
fn import_never_modifies_source_permissions() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::TempDir::new().expect("temp root");
    let (_src_dir, _src_ca) = create_ca(root.path(), "src-ca");
    let cert_src = root.path().join("op.cert.pem");
    let key_src = root.path().join("op.key.pem");
    fs::copy(root.path().join("src-ca").join("ca-cert.pem"), &cert_src).expect("copy");
    fs::copy(root.path().join("src-ca").join("ca-key.pem"), &key_src).expect("copy");
    fs::set_permissions(&key_src, fs::Permissions::from_mode(0o644)).expect("loosen src");
    CaAuthority::import(&root.path().join("imported"), &cert_src, &key_src).expect("import");
    // The operator's original keeps its (insecure) mode; only the copy is locked down.
    assert_eq!(
        fs::metadata(&key_src).expect("stat").permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(
        fs::metadata(root.path().join("imported").join("ca-key.pem"))
            .expect("stat")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(windows)]
#[test]
fn windows_permission_behavior_is_documented() {
    // Windows has no Unix mode bits: files are created with default sharing
    // and permission enforcement/repair are unavailable. This test pins the
    // documented behavior instead of claiming Unix equivalence. Operators
    // must rely on profile-directory ACLs for CA secrecy on Windows.
    let root = tempfile::TempDir::new().expect("temp root");
    let (dir, ca) = create_ca(root.path(), "ca");
    CaAuthority::open(&dir).expect("opens without Unix enforcement");
    assert_eq!(
        repair_ca_permissions(&dir).map(drop),
        Err(CaError::PermissionRepairUnsupported)
    );
    let dest = root.path().join("export.pem");
    ca.export_cert(&dest).expect("export works");
    assert!(dest.is_file());
}

// ---------------------------------------------------------------------------
// Export / rotation
// ---------------------------------------------------------------------------

#[test]
fn export_contains_only_the_public_certificate() {
    let root = tempfile::TempDir::new().expect("temp root");
    let (dir, ca) = create_ca(root.path(), "ca");
    let dest = root.path().join("trust.pem");
    ca.export_cert(&dest).expect("export");
    let text = fs::read_to_string(&dest).expect("read export");
    assert!(text.contains("-----BEGIN CERTIFICATE-----"));
    assert!(!text.contains("PRIVATE KEY"));
    assert_eq!(text.matches("-----BEGIN").count(), 1);
    // Refuses to overwrite.
    assert_eq!(
        ca.export_cert(&dest).map(drop),
        Err(CaError::ExportDestExists)
    );
    // The free function serves the same bytes without opening the key.
    let dest2 = root.path().join("trust2.pem");
    export_ca_cert(&dir, &dest2).expect("free export");
    assert_eq!(
        fs::read(&dest2).expect("read"),
        fs::read(&dest).expect("read")
    );
}

#[test]
fn rotation_creates_a_distinct_identity_and_keeps_the_old_handle() {
    let root = tempfile::TempDir::new().expect("temp root");
    let (_old_dir, old_ca) = create_ca(root.path(), "old");
    let old_issuer = LeafIssuer::new(old_ca, &LeafOptions::default()).expect("issuer");
    let (_new_dir, new_ca) = create_ca(root.path(), "new");
    assert_ne!(old_issuer.ca_fingerprint(), new_ca.fingerprint());
    // The active handle is unaffected by rotation.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let leaf = old_issuer
            .issue(&normalize_host("stable.test").expect("host"))
            .await
            .expect("old issuer still mints");
        assert_eq!(leaf.target(), "stable.test");
    });
}

#[test]
fn cache_key_includes_ca_identity() {
    let root = tempfile::TempDir::new().expect("temp root");
    let (_a_dir, ca_a) = create_ca(root.path(), "a");
    let (_b_dir, ca_b) = create_ca(root.path(), "b");
    let issuer_a = LeafIssuer::new(ca_a, &LeafOptions::default()).expect("issuer");
    let issuer_b = LeafIssuer::new(ca_b, &LeafOptions::default()).expect("issuer");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let target = normalize_host("shared.test").expect("host");
        let leaf_a = issuer_a.issue(&target).await.expect("issue a");
        let leaf_b = issuer_b.issue(&target).await.expect("issue b");
        assert_ne!(
            leaf_a.fingerprint(),
            leaf_b.fingerprint(),
            "same target under different CAs must not share cache entries"
        );
        assert_eq!(issuer_a.issuance_count(), 1);
        assert_eq!(issuer_b.issuance_count(), 1);
    });
}

// ---------------------------------------------------------------------------
// Leakage
// ---------------------------------------------------------------------------

#[test]
fn no_key_leakage_across_artifacts() {
    let root = tempfile::TempDir::new().expect("temp root");
    let (dir, ca) = create_ca(root.path(), "ca");
    let sentinel = key_sentinel(&dir);

    let mut scanned = Vec::new();
    scanned.push(fs::read_to_string(dir.join("metadata.json")).expect("metadata"));
    let dest = root.path().join("trust.pem");
    ca.export_cert(&dest).expect("export");
    scanned.push(fs::read_to_string(&dest).expect("export"));
    scanned.push(format!("{ca:?}"));
    scanned.extend(collect_error_strings());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let issuer = LeafIssuer::new(
            CaAuthority::open(&dir).expect("reopen"),
            &LeafOptions::default(),
        )
        .expect("issuer");
        let leaf = issuer
            .issue(&normalize_host("quiet.test").expect("host"))
            .await
            .expect("issue");
        scanned.push(format!("{issuer:?}"));
        scanned.push(format!("{leaf:?}"));
        scanned.push(leaf.cert_pem().to_owned());
    });

    // Every other file under the temp root (staging, sources, exports).
    let mut other_files = Vec::new();
    let mut stack = vec![root.path().to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).expect("walk") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                other_files.push(path);
            }
        }
    }
    for path in &other_files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == "ca-key.pem" || name.ends_with(".key.pem") {
            continue; // expected key holders
        }
        let text = fs::read_to_string(path).unwrap_or_default();
        scanned.push(text);
    }

    for text in &scanned {
        assert!(
            !text.contains(&sentinel),
            "private-key sentinel leaked into scanned artifact"
        );
        assert!(
            !text.contains("PRIVATE KEY"),
            "private-key block leaked into scanned artifact"
        );
    }
    // Nothing ever lands in a fixture store.
    assert!(!root.path().join(".eggr").exists());
}
