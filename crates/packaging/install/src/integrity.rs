//! Proving an installed package is still the package that was installed.
//!
//! Install-time verification only proves the bytes were signed by
//! someone trusted — not what they look like a week later, since the
//! payload is an ordinary directory an app, user, or half-finished
//! write can change. So the install records the three verified
//! artefacts it already held (signed root, manifest bytes, encoded
//! payload collection), and [`verify`] replays the chain against disk:
//!
//! ```text
//! blake3(signed-root)      == the package id            (identity)
//! blake3(manifest.toml)    == root.manifest_hash        (signature covers it)
//! blake3(collection.qcl)   == root.payload_root_hash    (signature covers it)
//! blake3(<each payload file>) == its collection entry   (contents)
//! ```
//!
//! Every link is checked, so a mismatch is reported at the link that
//! broke, not as one opaque "corrupt". The record is kept **outside**
//! the payload directory: the payload is bind-mounted into the app's
//! sandbox, and an app has no business reading the evidence used to check it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pkg::blobs::{BlobHash, Collection};
use pkg::signed::SignedRoot;

/// Filenames inside one package's integrity record.
const SIGNED_ROOT: &str = "signed-root";
const MANIFEST: &str = "manifest.toml";
const COLLECTION: &str = "collection.qcl";

/// What went wrong, at the link where it went wrong.
#[derive(Debug, thiserror::Error)]
pub enum IntegrityError {
    #[error(
        "no integrity record for {0} — it was installed before integrity records existed, or the record was deleted. Reinstall to create one."
    )]
    NoRecord(String),
    #[error("reading integrity record at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing integrity record at {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "the recorded signed root does not hash to the package id — the record itself was tampered with"
    )]
    RootIdMismatch,
    #[error("recorded signed root is malformed: {0}")]
    RootMalformed(String),
    #[error("recorded manifest does not match the hash the signature covers")]
    ManifestMismatch,
    #[error("recorded payload collection does not match the hash the signature covers")]
    CollectionMismatch,
    #[error("payload collection is malformed: {0}")]
    CollectionMalformed(String),
}

/// A file in the payload that no longer matches what was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFault {
    /// Named in the collection, absent on disk.
    Missing(String),
    /// Present, but its bytes hash to something else.
    Modified(String),
    /// Unreadable (permissions, an I/O error).
    Unreadable { path: String, detail: String },
}

impl std::fmt::Display for FileFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileFault::Missing(p) => write!(f, "missing:  {p}"),
            FileFault::Modified(p) => write!(f, "modified: {p}"),
            FileFault::Unreadable { path, detail } => {
                write!(f, "unreadable: {path} ({detail})")
            }
        }
    }
}

/// A completed re-verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub files_checked: usize,
    pub faults: Vec<FileFault>,
    /// Files present in the payload directory that the collection does
    /// not name. Reported, not failed: the launcher only executes what
    /// the manifest names, and a stray file is a strong signal without
    /// being proof of tampering.
    pub unexpected: Vec<String>,
}

impl Report {
    pub fn is_intact(&self) -> bool {
        self.faults.is_empty()
    }
}

/// Where one package's record lives.
pub fn record_dir(data_root: &Path, package_id: &str) -> PathBuf {
    data_root.join("integrity").join(sanitise(package_id))
}

/// A package id comes from a manifest, so it is untrusted input like
/// any other path component.
fn sanitise(raw: &str) -> String {
    let mapped: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.is_empty() || mapped.chars().all(|c| c == '.') {
        return format!(
            "pkg_{}",
            hex::encode(blake3::hash(raw.as_bytes()).as_bytes())
        );
    }
    mapped
}

/// Write the record for a package that has just been verified and unpacked.
///
/// The three blobs are the *verified* ones the installer already
/// held, not a re-derivation from disk — recording what was checked
/// is what makes a later check meaningful.
///
/// # Errors
///
/// Returns [`IntegrityError::Write`] if the record directory or any of
/// the three recorded blobs can't be written.
pub fn record(
    data_root: &Path,
    package_id: &str,
    signed_root: &[u8],
    manifest_bytes: &[u8],
    collection_bytes: &[u8],
) -> Result<PathBuf, IntegrityError> {
    let dir = record_dir(data_root, package_id);
    std::fs::create_dir_all(&dir).map_err(|source| IntegrityError::Write {
        path: dir.clone(),
        source,
    })?;
    for (name, bytes) in [
        (SIGNED_ROOT, signed_root),
        (MANIFEST, manifest_bytes),
        (COLLECTION, collection_bytes),
    ] {
        let path = dir.join(name);
        atomic_file::write(&path, bytes).map_err(|source| IntegrityError::Write {
            path: path.clone(),
            source,
        })?;
    }
    Ok(dir)
}

/// Remove a package's record. Best-effort: a missing record is the state we were heading for.
pub fn forget(data_root: &Path, package_id: &str) {
    let _ = std::fs::remove_dir_all(record_dir(data_root, package_id));
}

/// Whether a record exists for this package.
pub fn has_record(data_root: &Path, package_id: &str) -> bool {
    record_dir(data_root, package_id).join(SIGNED_ROOT).exists()
}

/// Replay the whole chain: recorded root → recorded manifest and collection → every payload file on disk.
///
/// # Errors
///
/// Returns [`IntegrityError::NoRecord`] if no record exists for
/// `package_id`; [`IntegrityError::Read`] if a recorded blob can't be
/// read; [`IntegrityError::RootIdMismatch`] if the recorded root
/// doesn't hash to `package_id_hex`; [`IntegrityError::RootMalformed`]/
/// [`CollectionMalformed`] if the recorded root or collection can't be
/// decoded; and [`IntegrityError::ManifestMismatch`]/[`CollectionMismatch`]
/// if a recorded blob's hash disagrees with what the signed root covers.
///
/// [`CollectionMalformed`]: IntegrityError::CollectionMalformed
/// [`CollectionMismatch`]: IntegrityError::CollectionMismatch
pub fn verify(
    data_root: &Path,
    package_id: &str,
    package_id_hex: &str,
    payload_root: &Path,
) -> Result<Report, IntegrityError> {
    let dir = record_dir(data_root, package_id);
    if !dir.join(SIGNED_ROOT).exists() {
        return Err(IntegrityError::NoRecord(package_id.to_string()));
    }
    let read = |name: &str| -> Result<Vec<u8>, IntegrityError> {
        let path = dir.join(name);
        std::fs::read(&path).map_err(|source| IntegrityError::Read { path, source })
    };
    let signed_bytes = read(SIGNED_ROOT)?;
    let manifest_bytes = read(MANIFEST)?;
    let collection_bytes = read(COLLECTION)?;

    // 1. The record names the package it claims to — without this,
    //    swapping it for another package's record would make any payload "verify".
    if hex::encode(BlobHash::of(&signed_bytes).0) != package_id_hex.to_ascii_lowercase() {
        return Err(IntegrityError::RootIdMismatch);
    }
    let root = SignedRoot::decode(&signed_bytes)
        .map_err(|e| IntegrityError::RootMalformed(e.to_string()))?;

    // 2 + 3. The two hashes the publisher's signature actually covers.
    if BlobHash::of(&manifest_bytes) != root.manifest_hash {
        return Err(IntegrityError::ManifestMismatch);
    }
    if BlobHash::of(&collection_bytes) != root.payload_root_hash {
        return Err(IntegrityError::CollectionMismatch);
    }
    let collection = Collection::decode(&collection_bytes)
        .map_err(|e| IntegrityError::CollectionMalformed(e.to_string()))?;

    // 4. The bytes on disk.
    let mut faults = Vec::new();
    let mut expected: BTreeMap<&String, &BlobHash> = BTreeMap::new();
    for (rel, hash) in &collection.entries {
        expected.insert(rel, hash);
        let path = payload_root.join(rel);
        match std::fs::read(&path) {
            Ok(bytes) => {
                if BlobHash::of(&bytes) != *hash {
                    faults.push(FileFault::Modified(rel.clone()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                faults.push(FileFault::Missing(rel.clone()));
            }
            Err(e) => faults.push(FileFault::Unreadable {
                path: rel.clone(),
                detail: e.to_string(),
            }),
        }
    }

    let mut unexpected = Vec::new();
    collect_unexpected(payload_root, payload_root, &expected, &mut unexpected);
    unexpected.sort();

    Ok(Report {
        files_checked: collection.entries.len(),
        faults,
        unexpected,
    })
}

/// Everything under the payload root the collection does not name.
/// `manifest.toml` is written by the installer rather than carried in
/// the payload, so it's expected and not reported.
fn collect_unexpected(
    root: &Path,
    dir: &Path,
    expected: &BTreeMap<&String, &BlobHash>,
    out: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_unexpected(root, &path, expected, out);
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel == "manifest.toml" {
            continue;
        }
        if !expected.keys().any(|k| **k == rel) {
            out.push(rel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a payload directory plus the record that describes it, exactly as the installer would.
    fn fixture(root: &Path) -> (PathBuf, String) {
        let payload = root.join("payloads/dev.qos.example");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), b"binary bytes").unwrap();
        std::fs::write(payload.join("data.txt"), b"hello").unwrap();

        let manifest_bytes = b"[package]\nid = \"dev.qos.example\"\n".to_vec();
        let collection = Collection::from_entries([
            ("bin/app".to_string(), BlobHash::of(b"binary bytes")),
            ("data.txt".to_string(), BlobHash::of(b"hello")),
        ]);
        let collection_bytes = collection.encode();
        let signed = SignedRoot::unsigned(
            BlobHash::of(&manifest_bytes),
            BlobHash::of(&collection_bytes),
            [7u8; 32],
        );
        let signed_bytes = signed.encode().to_vec();
        let id_hex = hex::encode(BlobHash::of(&signed_bytes).0);

        record(
            root,
            "dev.qos.example",
            &signed_bytes,
            &manifest_bytes,
            &collection_bytes,
        )
        .unwrap();
        (payload, id_hex)
    }

    #[test]
    fn an_untouched_install_verifies() {
        let tmp = tempfile::tempdir().unwrap();
        let (payload, id_hex) = fixture(tmp.path());
        let report = verify(tmp.path(), "dev.qos.example", &id_hex, &payload).unwrap();
        assert!(report.is_intact(), "{report:?}");
        assert_eq!(report.files_checked, 2);
        assert!(report.unexpected.is_empty());
    }

    #[test]
    fn a_modified_payload_file_is_named() {
        let tmp = tempfile::tempdir().unwrap();
        let (payload, id_hex) = fixture(tmp.path());
        std::fs::write(payload.join("bin/app"), b"tampered").unwrap();
        let report = verify(tmp.path(), "dev.qos.example", &id_hex, &payload).unwrap();
        assert!(!report.is_intact());
        assert_eq!(report.faults, vec![FileFault::Modified("bin/app".into())]);
    }

    #[test]
    fn a_deleted_payload_file_is_named() {
        let tmp = tempfile::tempdir().unwrap();
        let (payload, id_hex) = fixture(tmp.path());
        std::fs::remove_file(payload.join("data.txt")).unwrap();
        let report = verify(tmp.path(), "dev.qos.example", &id_hex, &payload).unwrap();
        assert_eq!(report.faults, vec![FileFault::Missing("data.txt".into())]);
    }

    #[test]
    fn an_added_file_is_reported_without_failing_the_check() {
        let tmp = tempfile::tempdir().unwrap();
        let (payload, id_hex) = fixture(tmp.path());
        std::fs::write(payload.join("extra.so"), b"?").unwrap();
        let report = verify(tmp.path(), "dev.qos.example", &id_hex, &payload).unwrap();
        assert!(report.is_intact(), "a stray file is not proof of tampering");
        assert_eq!(report.unexpected, vec!["extra.so".to_string()]);
    }

    /// Swapping the record for a well-formed one describing different
    /// bytes must not let a payload verify. This is what ties the
    /// record to the signature chain rather than to itself.
    #[test]
    fn a_record_that_does_not_hash_to_the_package_id_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (payload, _id_hex) = fixture(tmp.path());
        let wrong_hex = hex::encode([0u8; 32]);
        let err = verify(tmp.path(), "dev.qos.example", &wrong_hex, &payload)
            .expect_err("the record does not describe that package id");
        assert!(matches!(err, IntegrityError::RootIdMismatch));
    }

    #[test]
    fn tampering_with_the_recorded_collection_is_caught_before_any_file_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let (payload, id_hex) = fixture(tmp.path());
        let forged = Collection::from_entries([("bin/app".to_string(), BlobHash::of(b"tampered"))]);
        std::fs::write(
            record_dir(tmp.path(), "dev.qos.example").join(COLLECTION),
            forged.encode(),
        )
        .unwrap();
        let err = verify(tmp.path(), "dev.qos.example", &id_hex, &payload)
            .expect_err("the collection hash is covered by the signature");
        assert!(matches!(err, IntegrityError::CollectionMismatch));
    }

    #[test]
    fn a_package_with_no_record_says_so_rather_than_passing() {
        let tmp = tempfile::tempdir().unwrap();
        let err = verify(
            tmp.path(),
            "dev.qos.never-installed",
            &hex::encode([0u8; 32]),
            tmp.path(),
        )
        .expect_err("no record");
        assert!(matches!(err, IntegrityError::NoRecord(_)));
    }

    #[test]
    fn forget_removes_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = fixture(tmp.path());
        assert!(has_record(tmp.path(), "dev.qos.example"));
        forget(tmp.path(), "dev.qos.example");
        assert!(!has_record(tmp.path(), "dev.qos.example"));
    }
}
