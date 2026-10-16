//! Proving an installed package is still the package that was installed.
//!
//! Install-time verification only proves the bytes were signed by someone
//! trusted — not what they look like a week later, since a payload tree is
//! an ordinary directory an app, a user or a half-finished write can
//! change. An install therefore keeps the three verified metadata blobs it
//! checked (signed root, manifest, encoded payload collection) beside the
//! tree, and [`verify`] replays the chain against disk:
//!
//! ```text
//! blake3(signed-root)         == the package id         (identity)
//! signature over the root     by the root's publisher   (authenticity)
//! blake3(manifest.toml)       == root.manifest_hash     (signature covers it)
//! blake3(collection.qcl)      == root.payload_root_hash (signature covers it)
//! blake3(<each payload file>) == its collection entry   (contents)
//! ```
//!
//! Every link is checked, so a mismatch is reported at the link that broke.
//! The record lives outside the payload tree: the tree is bind-mounted into
//! the app's sandbox, and an app has no business reading the evidence used
//! to check it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tilo_app_package::{BlobHash, Collection, SignedRoot};

use crate::layout::{COLLECTION_FILE, MANIFEST_FILE, SIGNED_ROOT_FILE};
use crate::registry::InstalledPackage;

/// What went wrong, at the link where it went wrong.
#[derive(Debug, thiserror::Error)]
pub enum IntegrityError {
    #[error(
        "no integrity record for {0} — it was installed before integrity records existed, or the record was deleted. Reinstall to create one."
    )]
    NoRecord(String),
    #[error("reading package integrity data at {path}: {source}")]
    Read {
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
    #[error("recorded package root is not signed")]
    RootUnsigned,
    #[error("recorded package publisher key is invalid")]
    InvalidPublisherKey,
    #[error("recorded package signature is invalid")]
    RootSignatureMismatch,
    #[error("recorded manifest does not match the hash the signature covers")]
    ManifestMismatch,
    #[error("installed manifest differs from the signed manifest record")]
    InstalledManifestMismatch,
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
    /// Files present in the payload directory that the collection does not
    /// name. Reported, not failed: the launcher only executes what the
    /// manifest names, and a stray file is a strong signal without being
    /// proof of tampering.
    pub unexpected: Vec<String>,
}

impl Report {
    pub fn is_intact(&self) -> bool {
        self.faults.is_empty()
    }
}

/// The recorded metadata of one package, with its hash chain checked.
#[derive(Debug, Clone)]
pub struct Record {
    pub root: SignedRoot,
    pub manifest_bytes: Vec<u8>,
    pub collection: Collection,
}

/// Where `package`'s record lives: the metadata directory the manifest
/// sits in, or — for a package installed before payloads were content
/// addressed — `<store>/integrity/<app id>`.
pub fn record_dir(package: &InstalledPackage) -> PathBuf {
    if let Some(dir) = package.manifest_path.parent()
        && dir.join(SIGNED_ROOT_FILE).is_file()
    {
        return dir.to_path_buf();
    }
    let store = package
        .payload_root
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("/"));
    legacy_record_dir(store, &package.id)
}

/// The record directory the pre-content-store installer used.
pub fn legacy_record_dir(store_root: &Path, app_id: &str) -> PathBuf {
    store_root.join("integrity").join(sanitise(app_id))
}

/// An app id comes from a manifest, so it is untrusted input like any
/// other path component.
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
        return format!("pkg_{}", BlobHash::of(raw.as_bytes()).to_hex());
    }
    mapped
}

/// Read and check the record in `dir` against `package_id_hex`: identity,
/// signature, and both hashes the signature covers.
///
/// # Errors
///
/// Returns the first broken link as an [`IntegrityError`].
pub fn read_record(
    dir: &Path,
    app_id: &str,
    package_id_hex: &str,
) -> Result<Record, IntegrityError> {
    if !dir.join(SIGNED_ROOT_FILE).is_file() {
        return Err(IntegrityError::NoRecord(app_id.to_string()));
    }
    let read = |name: &str| -> Result<Vec<u8>, IntegrityError> {
        let path = dir.join(name);
        std::fs::read(&path).map_err(|source| IntegrityError::Read { path, source })
    };
    let signed_bytes = read(SIGNED_ROOT_FILE)?;
    let manifest_bytes = read(MANIFEST_FILE)?;
    let collection_bytes = read(COLLECTION_FILE)?;

    // The record names the package it claims to — without this, swapping it
    // for another package's record would make any payload "verify".
    if !BlobHash::of(&signed_bytes)
        .to_hex()
        .eq_ignore_ascii_case(package_id_hex)
    {
        return Err(IntegrityError::RootIdMismatch);
    }
    let root = SignedRoot::decode(&signed_bytes)
        .map_err(|e| IntegrityError::RootMalformed(e.to_string()))?;
    check_signature(&root)?;
    if BlobHash::of(&manifest_bytes) != root.manifest_hash {
        return Err(IntegrityError::ManifestMismatch);
    }
    if BlobHash::of(&collection_bytes) != root.payload_root_hash {
        return Err(IntegrityError::CollectionMismatch);
    }
    let collection = Collection::decode(&collection_bytes)
        .map_err(|e| IntegrityError::CollectionMalformed(e.to_string()))?;
    Ok(Record {
        root,
        manifest_bytes,
        collection,
    })
}

fn check_signature(root: &SignedRoot) -> Result<(), IntegrityError> {
    let signature = root.signature.ok_or(IntegrityError::RootUnsigned)?;
    let publisher = VerifyingKey::from_bytes(&root.publisher_identity)
        .map_err(|_| IntegrityError::InvalidPublisherKey)?;
    publisher
        .verify(&root.signing_bytes(), &Signature::from_bytes(&signature))
        .map_err(|_| IntegrityError::RootSignatureMismatch)
}

/// Replay the whole chain for `package`: its record, the manifest the
/// launcher reads, and every payload file under `payload_root` (normally
/// `package.payload_root`; a caller that copied the tree passes the copy).
///
/// # Errors
///
/// Returns an [`IntegrityError`] when the record is missing or broken, or
/// the installed manifest differs from the signed one, or the payload tree
/// cannot be enumerated. File-level damage is
/// reported in the [`Report`], not as an error.
pub fn verify(package: &InstalledPackage, payload_root: &Path) -> Result<Report, IntegrityError> {
    let record = read_record(&record_dir(package), &package.id, &package.package_id_hex)?;
    let installed_manifest =
        std::fs::read(&package.manifest_path).map_err(|source| IntegrityError::Read {
            path: package.manifest_path.clone(),
            source,
        })?;
    if installed_manifest != record.manifest_bytes {
        return Err(IntegrityError::InstalledManifestMismatch);
    }

    let mut faults = Vec::new();
    for (rel, hash) in &record.collection.entries {
        let path = payload_root.join(rel);
        match std::fs::File::open(&path).and_then(|mut file| BlobHash::of_reader(&mut file)) {
            Ok((actual, _)) if actual == *hash => {}
            Ok(_) => faults.push(FileFault::Modified(rel.clone())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                faults.push(FileFault::Missing(rel.clone()));
            }
            Err(e) => faults.push(FileFault::Unreadable {
                path: rel.clone(),
                detail: e.to_string(),
            }),
        }
    }

    // A manifest kept inside the tree (the pre-content-store layout) is the
    // installer's, not a stray file.
    let manifest_rel = package
        .manifest_path
        .strip_prefix(&package.payload_root)
        .ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"));
    let expected: BTreeSet<&str> = record
        .collection
        .entries
        .keys()
        .map(String::as_str)
        .chain(manifest_rel.as_deref())
        .collect();
    let mut unexpected = Vec::new();
    collect_unexpected(payload_root, payload_root, &expected, &mut unexpected)?;
    unexpected.sort();

    Ok(Report {
        files_checked: record.collection.entries.len(),
        faults,
        unexpected,
    })
}

/// Everything under the payload root the collection does not name.
fn collect_unexpected(
    root: &Path,
    dir: &Path,
    expected: &BTreeSet<&str>,
    out: &mut Vec<String>,
) -> Result<(), IntegrityError> {
    let read_error = |source| IntegrityError::Read {
        path: dir.to_path_buf(),
        source,
    };
    let entries = std::fs::read_dir(dir).map_err(read_error)?;
    for entry in entries {
        let entry = entry.map_err(read_error)?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| IntegrityError::Read {
            path: path.clone(),
            source,
        })?;
        // Inspect the payload tree itself, not directories a symlink points
        // outside it (or back into it).
        if file_type.is_dir() {
            collect_unexpected(root, &path, expected, out)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|error| IntegrityError::Read {
                path: path.clone(),
                source: std::io::Error::other(error),
            })?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !expected.contains(rel.as_str()) {
            out.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    /// A materialised tree plus the record beside it, as an install writes
    /// them.
    fn fixture(root: &Path) -> InstalledPackage {
        fixture_with_collection(
            root,
            Collection::from_entries([
                ("bin/app".to_string(), BlobHash::of(b"binary bytes")),
                ("data.txt".to_string(), BlobHash::of(b"hello")),
            ]),
        )
    }

    fn fixture_with_collection(root: &Path, collection: Collection) -> InstalledPackage {
        let manifest_bytes = b"[package]\nid = \"so.tilo.example\"\n".to_vec();
        let collection_bytes = collection.encode();
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut signed = SignedRoot::unsigned(
            BlobHash::of(&manifest_bytes),
            BlobHash::of(&collection_bytes),
            signing_key.verifying_key().to_bytes(),
        );
        // The has-signature bit is part of the signed preimage.
        signed.signature = Some([0; 64]);
        signed.signature = Some(signing_key.sign(&signed.signing_bytes()).to_bytes());
        let signed_bytes = signed.encode().to_vec();
        let id_hex = BlobHash::of(&signed_bytes).to_hex();

        let payload = root.join("pkgs").join(&id_hex);
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), b"binary bytes").unwrap();
        std::fs::write(payload.join("data.txt"), b"hello").unwrap();
        let meta = root.join("pkgs").join(format!("{id_hex}.pkg"));
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join(SIGNED_ROOT_FILE), &signed_bytes).unwrap();
        std::fs::write(meta.join(MANIFEST_FILE), &manifest_bytes).unwrap();
        std::fs::write(meta.join(COLLECTION_FILE), &collection_bytes).unwrap();
        InstalledPackage {
            id: "so.tilo.example".into(),
            version: "1.0.0".into(),
            package_id_hex: id_hex,
            payload_root: payload,
            manifest_path: meta.join(MANIFEST_FILE),
            publisher_key_hex: String::new(),
            pinned: false,
            installed_at: 0,
            last_used: 0,
            attested_by: String::new(),
            cli_commands: Vec::new(),
            runtime: String::new(),
            source: String::new(),
            system: false,
        }
    }

    #[test]
    fn verification_requires_an_observable_payload_tree_even_without_files() {
        let tmp = tempfile::tempdir().unwrap();
        let package = fixture_with_collection(tmp.path(), Collection::from_entries([]));
        std::fs::remove_dir_all(&package.payload_root).unwrap();
        let error = verify(&package, &package.payload_root).unwrap_err();
        assert!(matches!(error, IntegrityError::Read { path, source }
            if path == package.payload_root && source.kind() == std::io::ErrorKind::NotFound));

        std::fs::write(&package.payload_root, b"not a directory").unwrap();
        assert!(matches!(verify(&package, &package.payload_root),
            Err(IntegrityError::Read { path, .. }) if path == package.payload_root));
    }

    #[test]
    fn verification_names_every_changed_payload_file() {
        // phase: an_untouched_install_verifies
        {
            let tmp = tempfile::tempdir().unwrap();
            let package = fixture(tmp.path());
            let report = verify(&package, &package.payload_root).unwrap();
            assert!(report.is_intact(), "{report:?}");
            assert_eq!(report.files_checked, 2);
            assert!(report.unexpected.is_empty());
        }
        // phase: a_modified_payload_file_is_named
        {
            let tmp = tempfile::tempdir().unwrap();
            let package = fixture(tmp.path());
            std::fs::write(package.payload_root.join("bin/app"), b"tampered").unwrap();
            let report = verify(&package, &package.payload_root).unwrap();
            assert_eq!(report.faults, vec![FileFault::Modified("bin/app".into())]);
        }
        // phase: a_deleted_file_is_named_and_an_added_one_reported
        {
            let tmp = tempfile::tempdir().unwrap();
            let package = fixture(tmp.path());
            std::fs::remove_file(package.payload_root.join("data.txt")).unwrap();
            std::fs::write(package.payload_root.join("extra.so"), b"?").unwrap();
            let report = verify(&package, &package.payload_root).unwrap();
            assert_eq!(report.faults, vec![FileFault::Missing("data.txt".into())]);
            assert_eq!(report.unexpected, vec!["extra.so".to_string()]);
        }
    }

    #[test]
    fn the_install_record_itself_is_under_integrity() {
        // phase: a_record_that_does_not_hash_to_the_package_id_is_refused
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut package = fixture(tmp.path());
            package.package_id_hex = hex::encode([0u8; 32]);
            let err = verify(&package, &package.payload_root.clone())
                .expect_err("the record does not describe that package id");
            assert!(matches!(err, IntegrityError::RootIdMismatch));
        }
        // phase: tampering_with_the_recorded_collection_is_caught_before_any_file_is_read
        {
            let tmp = tempfile::tempdir().unwrap();
            let package = fixture(tmp.path());
            let forged =
                Collection::from_entries([("bin/app".to_string(), BlobHash::of(b"tampered"))]);
            std::fs::write(record_dir(&package).join(COLLECTION_FILE), forged.encode()).unwrap();
            let err =
                verify(&package, &package.payload_root).expect_err("covered by the signature");
            assert!(matches!(err, IntegrityError::CollectionMismatch));
        }
        // phase: a_legacy_install_keeps_its_record_under_integrity
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut package = fixture(tmp.path());
            let legacy = legacy_record_dir(tmp.path(), &package.id);
            std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            std::fs::rename(record_dir(&package), &legacy).unwrap();
            let payload = tmp.path().join("payloads/so.tilo.example");
            std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
            std::fs::rename(&package.payload_root, &payload).unwrap();
            std::fs::copy(legacy.join(MANIFEST_FILE), payload.join(MANIFEST_FILE)).unwrap();
            package.payload_root = payload.clone();
            package.manifest_path = payload.join(MANIFEST_FILE);
            let report = verify(&package, &payload).unwrap();
            assert!(
                report.is_intact() && report.unexpected.is_empty(),
                "{report:?}"
            );
        }
        // phase: a_package_with_no_record_says_so_rather_than_passing
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut package = fixture(tmp.path());
            package.manifest_path = tmp.path().join("elsewhere/manifest.toml");
            let err = verify(&package, &package.payload_root.clone()).expect_err("no record");
            assert!(matches!(err, IntegrityError::NoRecord(_)));
        }
    }
}
