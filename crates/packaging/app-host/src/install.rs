//! `.qpkg` ingest.
//!
//! Accepts the bytes of a .qpkg tar archive (as produced by
//! `qos-pkg pack`), pulls every blob into the daemon's on-disk
//! `FsBlobStore`, unpacks the payload eagerly to
//! `<store>/payloads/<pkg-id-hex>/`, then writes a `RegistryEntry`
//! with the requested pinning and the manifest text.
//!
//! The .qpkg wire format is a tar with:
//!
//! ```text
//! qpkg.toml                # tiny header naming the PackageId + format
//! signed-root              # the 173-byte signed-root blob
//! manifest.toml            # the manifest (UTF-8 TOML)
//! collection.qcl           # the encoded payload Collection (binary)
//! blobs/<hex>              # each payload blob, named by its BLAKE3 hex
//! ```
//!
//! signed-root / manifest / collection are duplicated under
//! `blobs/<hex>` too by the CLI packager, but we ingest generically —
//! any entry whose bytes hash matches its content-address is welcome.

use std::io::Read;

use pkg::blobs::BlobStore;
use pkg::id::PackageId;
use serde::Deserialize;
use thiserror::Error;

use crate::registry::{Pinning, RegistryEntry};
use crate::store::{Store, StoreError};

/// One-line header at the top of every .qpkg tarball.
#[derive(Debug, Deserialize)]
struct QpkgHeader {
    /// `qpkg:<hex>` form of the `PackageId`.
    package_id: String,
    #[allow(dead_code)]
    format: u32,
}

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("parse .qpkg: {0}")]
    Parse(String),
    #[error("verify signature: {0}")]
    Verify(String),
    #[error("unpack payload: {0}")]
    Unpack(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// Ingest a .qpkg-shaped byte blob into `store`.
///
/// - Verifies the signature (best-effort: self-signed is accepted; the
///   trust-set is currently empty pending publisher-trust integration).
/// - Extracts the payload eagerly to `<store>/payloads/<pkg-id-hex>/`.
/// - Inserts a `RegistryEntry` with the requested pinning.
///
/// Returns the resolved [`PackageId`].
pub fn install_qpkg(
    store: &Store,
    bytes: &[u8],
    pinning: Pinning,
) -> Result<PackageId, InstallError> {
    // Ingest every tar entry into a scratch MemoryBlobStore first,
    // so we can compute+verify the PackageId before touching the
    // on-disk store. Then flush the memory store into the fs store.
    let mut mem = pkg::MemoryBlobStore::new();
    let mut header: Option<QpkgHeader> = None;

    let mut ar = tar::Archive::new(bytes);
    for entry in ar
        .entries()
        .map_err(|e| InstallError::Parse(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| InstallError::Parse(e.to_string()))?;
        let entry_path = entry.path().map(|p| p.to_path_buf()).unwrap_or_default();
        let name = entry_path.to_string_lossy().to_string();
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| InstallError::Parse(e.to_string()))?;
        match name.as_str() {
            "qpkg.toml" => {
                let s = String::from_utf8(buf)
                    .map_err(|e| InstallError::Parse(format!("qpkg.toml not UTF-8: {e}")))?;
                header = Some(
                    toml::from_str(&s)
                        .map_err(|e| InstallError::Parse(format!("qpkg.toml: {e}")))?,
                );
            }
            n if n.starts_with("blobs/") => {
                mem.put(&buf)
                    .map_err(|e| InstallError::Parse(format!("blob-store: {e}")))?;
            }
            "signed-root" | "manifest.toml" | "collection.qcl" => {
                mem.put(&buf)
                    .map_err(|e| InstallError::Parse(format!("blob-store: {e}")))?;
            }
            _ => { /* unknown tar entry — ignore */ }
        }
    }
    let header = header.ok_or_else(|| InstallError::Parse(".qpkg is missing qpkg.toml".into()))?;
    let id: PackageId = header
        .package_id
        .parse()
        .map_err(|e| InstallError::Parse(format!("qpkg.toml package_id: {e:?}")))?;

    // Verify — self-signed acceptable (empty trust set → UntrustedPublisher).
    let manifest = match pkg::verify(id, &mem, &[]) {
        pkg::Verification::Trusted { manifest } => manifest,
        pkg::Verification::UntrustedPublisher { manifest, .. } => manifest,
        other => {
            return Err(InstallError::Verify(format!("{other:?}")));
        }
    };

    // Flush the scratch memory store into the on-disk store.
    store.with_blobs(|fs_store| -> Result<(), InstallError> {
        for (h, bytes) in mem.iter() {
            // FsBlobStore is idempotent — re-inserting the same
            // content-addressed bytes is a no-op.
            if !fs_store.has(h) {
                fs_store
                    .put(bytes)
                    .map_err(|e| InstallError::Parse(format!("fs blob-store: {e}")))?;
            }
        }
        Ok(())
    })?;

    // Eagerly extract the payload.
    let payload_root = store.payloads_root().join(id.to_hex());
    if payload_root.exists() {
        std::fs::remove_dir_all(&payload_root)?;
    }
    std::fs::create_dir_all(&payload_root)?;
    // Unpack directly out of the scratch memory store — the fs store
    // has the same content, but pkg::unpack takes `&impl BlobStore`
    // and the memory store is Send + Sync-friendly.
    pkg::unpack(id, &mem, &payload_root).map_err(|e| InstallError::Unpack(e.to_string()))?;

    let manifest_text = manifest
        .to_toml_string()
        .map_err(|e| InstallError::Unpack(e.to_string()))?;
    let entry = RegistryEntry::new_now(id, pinning, payload_root, manifest_text);
    store.put_entry(entry)?;
    Ok(id)
}
