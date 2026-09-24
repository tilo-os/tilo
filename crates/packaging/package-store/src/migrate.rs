//! Moving packages installed before payloads lived in the content store.
//!
//! Two older layouts existed, each with its own registry:
//!
//! - `tilo-install` unpacked every package to `payloads/<app id>/` with its
//!   signed metadata under `integrity/<app id>/`, in this same store root
//!   and with the row shape [`crate::InstalledPackage`] keeps.
//! - the app host kept a private blob directory (`store/<hash>`) *and* an
//!   unpacked copy of every package (`payloads/<package id>/`), with its own
//!   `registry.toml`.
//!
//! Both migrations verify each package's signed chain and every payload
//! file, move the bytes into the content store, materialise the tree in the
//! content-addressed layout and only then rewrite the registry; the old
//! files go last. Each is idempotent: a crash anywhere leaves either the old
//! row (migrated again next time) or the new one.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tilo_app_package::{PackageMeta, Verification};

use crate::content::{self, Content};
use crate::integrity;
use crate::layout::parse_id_hex;
use crate::store::{Admission, PackageStore};
use crate::{InstalledPackage, StoreError};
use tilo_clock::unix_now;

/// What a migration did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Migration {
    /// App ids now in the content-addressed layout.
    pub migrated: Vec<String>,
    /// Packages left as they were, with the reason.
    pub kept: Vec<(String, String)>,
}

impl Migration {
    pub fn is_empty(&self) -> bool {
        self.migrated.is_empty() && self.kept.is_empty()
    }
}

/// Ingest every payload file of `meta` from `file_of(rel, hash)`, refusing a
/// file whose bytes do not match the signed collection.
fn ingest_payload(
    content: &Content<'_>,
    meta: &PackageMeta,
    file_of: impl Fn(&str, &tilo_app_package::BlobHash) -> PathBuf,
) -> Result<(), String> {
    let mut done = std::collections::BTreeSet::new();
    for (rel, hash) in &meta.collection.entries {
        if !done.insert(*hash) {
            continue;
        }
        let path = std::path::absolute(file_of(rel, hash)).map_err(|error| error.to_string())?;
        let stored = content
            .ingest_file(&path)
            .map_err(|error| format!("{rel}: {error}"))?;
        if stored != *hash {
            return Err(format!("{rel} no longer matches its signed hash"));
        }
    }
    Ok(())
}

fn signature_is_valid(meta: &PackageMeta) -> bool {
    matches!(
        tilo_app_package::verify(meta, &[]),
        Verification::Trusted { .. }
            | Verification::AttestedBy { .. }
            | Verification::UntrustedPublisher { .. }
            | Verification::Revoked { .. }
    )
}

/// The signed metadata of package `id_hex` from an app-host blob
/// directory, with its hash chain and signature checked.
fn app_host_meta(blobs: &Path, id_hex: &str) -> Result<PackageMeta, String> {
    let id = parse_id_hex(id_hex).ok_or_else(|| "malformed package id".to_string())?;
    let read = |hash: &[u8; 32]| {
        std::fs::read(blobs.join(hex::encode(hash))).map_err(|error| error.to_string())
    };
    let root = read(id.as_bytes())?;
    let decoded = tilo_app_package::SignedRoot::decode(&root).map_err(|error| error.to_string())?;
    let manifest = read(decoded.manifest_hash.as_bytes())?;
    let collection = read(decoded.payload_root_hash.as_bytes())?;
    let meta = PackageMeta::from_parts(id, &root, manifest, collection)
        .map_err(|error| error.to_string())?;
    if !signature_is_valid(&meta) {
        return Err("signature does not verify".into());
    }
    Ok(meta)
}

#[derive(Deserialize)]
struct AppHostRegistry {
    #[serde(default)]
    entry: Vec<AppHostRow>,
}

#[derive(Deserialize)]
struct AppHostRow {
    id: String,
    #[serde(default)]
    pinning: String,
    #[serde(default)]
    installed_at_unix: u64,
    #[serde(default)]
    last_used_unix: u64,
}

impl PackageStore {
    /// Move rows whose tree is still under `payloads/<app id>` into the
    /// content store. Rows whose record is missing or whose files no longer
    /// match stay as they are (they keep launching) and are reported.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry or the content store fails.
    pub fn migrate_legacy_layout(&self) -> Result<Migration, StoreError> {
        let _lock = self.lock()?;
        let layout = self.layout().clone();
        let legacy_root = layout.root().join("payloads");
        let rows: Vec<InstalledPackage> = self
            .registry()?
            .into_entries()
            .into_iter()
            .filter(|row| row.payload_root.starts_with(&legacy_root))
            .collect();
        if rows.is_empty() {
            return Ok(Migration::default());
        }
        let mut report = Migration::default();
        let prepared = content::session(&layout.content(), &layout.tmp(), |content| {
            let mut prepared = Vec::new();
            for row in &rows {
                let record_dir = integrity::legacy_record_dir(layout.root(), &row.id);
                let meta = integrity::read_record(&record_dir, &row.id, &row.package_id_hex)
                    .map_err(|error| error.to_string())
                    .and_then(|record| {
                        PackageMeta::from_root(
                            record.root,
                            record.manifest_bytes,
                            record.collection.encode(),
                        )
                        .map_err(|error| error.to_string())
                    });
                let meta = match meta {
                    Ok(meta) => meta,
                    Err(reason) => {
                        report.kept.push((row.id.clone(), reason));
                        continue;
                    }
                };
                if let Err(reason) =
                    ingest_payload(content, &meta, |rel, _| row.payload_root.join(rel))
                {
                    report.kept.push((row.id.clone(), reason));
                    continue;
                }
                self.materialise(content, &meta)?;
                prepared.push((row.clone(), meta));
            }
            Ok(prepared)
        })?;

        let mut registry = self.registry()?;
        for (old, meta) in &prepared {
            if let Some(row) = registry.find_mut(&old.id) {
                row.payload_root = layout.payload(meta.id);
                row.manifest_path = layout.manifest(meta.id);
                if row.last_used == 0 {
                    row.last_used = row.installed_at;
                }
            }
        }
        registry.save(&layout.registry())?;
        for (old, _) in prepared {
            self.retire_tree(&old)?;
            report.migrated.push(old.id);
        }
        self.collect_garbage_locked()?;
        Ok(report)
    }

    /// Import the packages of a retired app-host store at `old_root`, then
    /// delete its blob directory, unpacked copies and registry. An app id
    /// already installed here wins over the imported copy.
    ///
    /// # Errors
    ///
    /// Returns an error when the old registry cannot be parsed, or the
    /// registry or content store fails.
    pub fn import_app_host_store(&self, old_root: &Path) -> Result<Migration, StoreError> {
        let old_registry = old_root.join("registry.toml");
        let raw = match std::fs::read_to_string(&old_registry) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Migration::default());
            }
            Err(error) => return Err(error.into()),
        };
        let rows = toml::from_str::<AppHostRegistry>(&raw)
            .map_err(|error| StoreError::Tapp(format!("{}: {error}", old_registry.display())))?
            .entry;
        let _lock = self.lock()?;
        let layout = self.layout().clone();
        let blobs = old_root.join("store");
        let installed = self.registry()?;
        let mut report = Migration::default();
        let prepared = content::session(&layout.content(), &layout.tmp(), |content| {
            let mut prepared = Vec::new();
            for row in &rows {
                let meta = match app_host_meta(&blobs, &row.id) {
                    Ok(meta) => meta,
                    Err(reason) => {
                        report.kept.push((row.id.clone(), reason));
                        continue;
                    }
                };
                if installed.find(&meta.manifest.package.id).is_some() {
                    report.kept.push((
                        meta.manifest.package.id.clone(),
                        "already installed in the app store".into(),
                    ));
                    continue;
                }
                if let Err(reason) =
                    ingest_payload(content, &meta, |_, hash| blobs.join(hash.to_hex()))
                {
                    report.kept.push((meta.manifest.package.id.clone(), reason));
                    continue;
                }
                self.materialise(content, &meta)?;
                prepared.push((meta, row));
            }
            Ok(prepared)
        })?;

        let mut registry = self.registry()?;
        for (meta, old) in &prepared {
            let installed_at = if old.installed_at_unix == 0 {
                unix_now()
            } else {
                old.installed_at_unix
            };
            let mut row = self.row_for(
                meta,
                Admission {
                    pinned: old.pinning == "pinned",
                    attested_by: String::new(),
                    source: format!("app host store {}", old_root.display()),
                },
                installed_at,
            );
            row.last_used = old.last_used_unix.max(installed_at);
            report.migrated.push(row.id.clone());
            registry.upsert(row);
        }
        registry.save(&layout.registry())?;
        for dir in ["store", "payloads", "manifests"] {
            match std::fs::remove_dir_all(old_root.join(dir)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
        }
        std::fs::remove_file(&old_registry)?;
        self.collect_garbage_locked()?;
        Ok(report)
    }
}
