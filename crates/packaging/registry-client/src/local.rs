//! A registry directory on this machine.
//!
//! The layout is the one `registry-server` keeps its state in and the one
//! `tilo-app-package publish --registry <dir>` writes, so a published
//! directory can be read offline or served unchanged:
//!
//! ```text
//! <root>/
//!   published-snapshot.json        the owner-signed full snapshot
//!   thin-packages/<root>.tapp      each package's thin form, by package root
//!   payload-blobs/<hash>           its payload blobs, by BLAKE3
//! ```
//!
//! Nothing here is trusted. A reader verifies the snapshot against the
//! pinned owner key and every package against its signed root and BLAKE3;
//! the `app` client does that with the same code it uses for an HTTPS
//! registry.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::{Signature, SigningKey, Verifier};
use registry_types::{
    MAX_SNAPSHOT_ENCODED_BYTES, MAX_SNAPSHOT_VALIDITY_SECONDS, PublisherAuthorization,
    RegistryEntry, SignedRegistrySnapshot, sign_snapshot, snapshot_hash,
};
use tilo_app_package::chunks::ObjectDir;
use tilo_app_package::{DiscardBlobs, PackageId};

use crate::entry::{EntryMetadata, Platform, ThinPackage, build_entry_with_objects};

/// File name of the owner-signed full snapshot.
pub const SNAPSHOT_FILE: &str = "published-snapshot.json";
/// Directory of thin package forms, named by package root.
pub const THIN_PACKAGES_DIR: &str = "thin-packages";
/// Directory of payload blobs, named by BLAKE3.
pub const PAYLOAD_BLOBS_DIR: &str = "payload-blobs";

/// A registry stored as a directory on this machine.
#[derive(Clone, Debug)]
pub struct LocalRegistry {
    root: PathBuf,
}

impl LocalRegistry {
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn snapshot_path(&self) -> PathBuf {
        self.root.join(SNAPSHOT_FILE)
    }

    pub fn thin_packages_dir(&self) -> PathBuf {
        self.root.join(THIN_PACKAGES_DIR)
    }

    pub fn payload_blobs_dir(&self) -> PathBuf {
        self.root.join(PAYLOAD_BLOBS_DIR)
    }

    fn thin_package_path(&self, root_hex: &str) -> Option<PathBuf> {
        is_hex_digest(root_hex).then(|| self.thin_packages_dir().join(format!("{root_hex}.tapp")))
    }

    /// The directory's signed snapshot, or `None` when it holds none yet.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is larger than a client accepts, cannot
    /// be read, or is not a snapshot.
    pub fn read_snapshot(&self) -> Result<Option<SignedRegistrySnapshot>> {
        let path = self.snapshot_path();
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("stat the registry snapshot"),
        };
        if metadata.len() > MAX_SNAPSHOT_ENCODED_BYTES as u64 {
            bail!("the registry snapshot exceeds the client's size bound");
        }
        let bytes = std::fs::read(&path).context("read the registry snapshot")?;
        let snapshot = serde_json::from_slice(&bytes).context("decode the registry snapshot")?;
        Ok(Some(snapshot))
    }

    /// The thin form of the package whose signed root is `root_hex`, checked
    /// to be exactly that package's metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the stored package cannot be read or does not
    /// match its name.
    pub fn read_thin_package(&self, root_hex: &str) -> Result<Option<Vec<u8>>> {
        let Some(path) = self.thin_package_path(root_hex) else {
            return Ok(None);
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("read the stored thin package"),
        };
        let decoded = tilo_app_package::tapp::read(bytes.as_slice(), &mut DiscardBlobs)
            .context("decode the stored thin package")?;
        if !decoded.meta.id.to_hex().eq_ignore_ascii_case(root_hex) {
            bail!("the stored thin package does not hash to its name");
        }
        Ok(Some(bytes))
    }

    /// Store a full package's thin form and every payload blob beside it.
    ///
    /// # Errors
    ///
    /// Returns an error when the bytes are not a package or a file cannot be
    /// written.
    fn store_package(&self, tapp_bytes: &[u8]) -> Result<PackageId> {
        let mut payload = std::collections::BTreeMap::new();
        let decoded = tilo_app_package::tapp::read(tapp_bytes, &mut payload)
            .context("decode the package to publish")?;
        std::fs::create_dir_all(self.payload_blobs_dir())?;
        for (hash, bytes) in &payload {
            let target = self.payload_blobs_dir().join(hash.to_hex());
            tilo_files::native::atomic::write(&target, bytes)
                .with_context(|| format!("write payload blob {}", target.display()))?;
        }
        let thin = tilo_app_package::tapp::to_bytes(&decoded.meta, None)
            .context("write the package's thin form")?;
        std::fs::create_dir_all(self.thin_packages_dir())?;
        let Some(target) = self.thin_package_path(&decoded.meta.id.to_hex()) else {
            bail!("the package has no valid root");
        };
        tilo_files::native::atomic::write(&target, &thin)
            .with_context(|| format!("write thin package {}", target.display()))?;
        Ok(decoded.meta.id)
    }
}

fn is_hex_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Publish one full `.tapp` into the registry directory at `root`, signed by
/// `publisher`, and advance the directory's owner-signed snapshot.
///
/// The package is kept as its thin form plus payload blobs, so a client
/// fetches only the blobs it lacks. `base_url` is the address the directory
/// will be served at if it is ever put behind `registry-server` (its
/// `/thin/<root>.tapp` route); a reader that opens the directory directly
/// verifies the same entry and reads the package by its signed root instead.
/// Publishing the same version twice with the same bytes is a no-op; the same
/// version with different bytes is refused.
///
/// # Errors
///
/// Returns an error when the package does not verify, its payload cannot be
/// stored, the existing snapshot cannot be read, or the new snapshot cannot
/// be written.
pub fn publish_to_dir(
    root: &Path,
    base_url: &str,
    tapp_bytes: &[u8],
    publisher: &SigningKey,
    owner: &SigningKey,
    platform: Platform,
    arch: Option<&str>,
    metadata: Option<&EntryMetadata>,
) -> Result<RegistryEntry> {
    let registry = LocalRegistry::open(root);
    std::fs::create_dir_all(root)?;
    let previous_snapshot = registry.read_snapshot()?;
    if let Some(previous) = &previous_snapshot {
        let message = registry_types::snapshot_signing_message(previous)?;
        let signature: [u8; 64] = hex::decode(&previous.owner_signature_hex)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| {
                anyhow!("the existing registry snapshot has no valid owner signature")
            })?;
        owner
            .verifying_key()
            .verify(&message, &Signature::from_bytes(&signature))
            .map_err(|_| anyhow!("the existing registry snapshot was signed by another owner"))?;
    }
    let package_id = tilo_app_package::tapp::read(tapp_bytes, &mut DiscardBlobs)
        .context("decode the package to publish")?
        .meta
        .manifest
        .package
        .id;
    let previous_entry = previous_snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .entries
            .iter()
            .find(|entry| entry.package_id == package_id)
    });

    let package_id = registry.store_package(tapp_bytes)?;
    let thin = registry
        .read_thin_package(&package_id.to_hex())?
        .ok_or_else(|| anyhow!("the package's thin form was not stored"))?;
    let objects = ObjectDir::new(registry.payload_blobs_dir());
    let entry = build_entry_with_objects(
        ThinPackage {
            tapp: &thin,
            objects: &objects,
        },
        &format!(
            "{}/thin/{}.tapp",
            base_url.trim_end_matches('/'),
            package_id.to_hex()
        ),
        publisher,
        platform,
        arch,
        previous_entry,
        metadata,
        &registry_types::retention::RetentionPolicy::default(),
    )?;

    write_snapshot(&registry, previous_snapshot.as_ref(), &entry, owner)?;
    Ok(entry)
}

/// Put `entry` into a new snapshot after `previous` and sign it with `owner`.
fn write_snapshot(
    registry: &LocalRegistry,
    previous: Option<&SignedRegistrySnapshot>,
    entry: &RegistryEntry,
    owner: &SigningKey,
) -> Result<()> {
    let mut entries = previous
        .map(|snapshot| snapshot.entries.clone())
        .unwrap_or_default();
    entries.retain(|existing| existing.package_id != entry.package_id);
    entries.push(entry.clone());
    entries.sort_by(|left, right| left.package_id.cmp(&right.package_id));
    if entries.len() > registry_types::MAX_SNAPSHOT_ENTRIES {
        bail!(
            "the registry directory would hold {} entries; a snapshot allows {}",
            entries.len(),
            registry_types::MAX_SNAPSHOT_ENTRIES
        );
    }

    let mut publishers: BTreeSet<String> = previous
        .iter()
        .flat_map(|snapshot| snapshot.publishers.iter())
        .map(|publisher| publisher.pubkey_hex.to_ascii_lowercase())
        .collect();
    publishers.insert(entry.publisher_pubkey_hex.to_ascii_lowercase());
    if publishers.len() > registry_types::MAX_SNAPSHOT_PUBLISHERS {
        bail!(
            "the registry directory would authorize {} publishers; a snapshot allows {}",
            publishers.len(),
            registry_types::MAX_SNAPSHOT_PUBLISHERS
        );
    }
    let publishers: Vec<PublisherAuthorization> = publishers
        .into_iter()
        .map(|pubkey_hex| PublisherAuthorization {
            pubkey_hex,
            can_add_new: true,
        })
        .collect();

    let now = tilo_clock::unix_now();
    let previous_snapshot_hash = previous.map(snapshot_hash).transpose()?.unwrap_or_default();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: previous.map_or(1, |snapshot| snapshot.sequence.saturating_add(1)),
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(MAX_SNAPSHOT_VALIDITY_SECONDS),
        previous_snapshot_hash_hex: previous_snapshot_hash,
        moderation_head_hex: String::new(),
        publishers,
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    sign_snapshot(&mut snapshot, owner)?;
    let bytes = serde_json::to_vec(&snapshot).context("encode the registry snapshot")?;
    tilo_files::native::atomic::write_creating_parent(&registry.snapshot_path(), &bytes)
        .context("write the registry snapshot")
}
