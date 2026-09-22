//! `.tapp` ingest.
//!
//! Accepts the bytes of a .tapp tar archive (as produced by
//! `tilo-app-package pack`), pulls every blob into the daemon's on-disk
//! `FsBlobStore`, unpacks the payload eagerly to
//! `<store>/payloads/<pkg-id-hex>/`, then writes a `RegistryEntry`
//! with the requested pinning and the manifest text.
//!
//! The .tapp wire format is a tar with:
//!
//! ```text
//! tapp.toml                # tiny header naming the PackageId + format
//! signed-root              # the 173-byte signed-root blob
//! manifest.toml            # the manifest (UTF-8 TOML)
//! collection.qcl           # the encoded payload Collection (binary)
//! blobs/<hex>              # each payload blob, named by its BLAKE3 hex
//! ```
//!
//! signed-root / manifest / collection are duplicated under
//! `blobs/<hex>` too by the CLI packager, but we ingest generically —
//! any entry whose bytes hash matches its content-address is welcome.

use thiserror::Error;
use tilo_app_package::blobs::BlobStore;
use tilo_app_package::id::PackageId;

use crate::registry::{Pinning, RegistryEntry};
use crate::store::{Store, StoreError};

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("parse .tapp: {0}")]
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

/// Registry-authenticated identity that downloaded bytes must match.
///
/// This is deliberately supplied by the privileged Store capability, not
/// inferred from the archive: an archive is allowed to describe itself, but
/// it is not allowed to choose the identity the user approved in the
/// catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallExpectation {
    pub package_root: PackageId,
    pub publisher: [u8; 32],
    pub app_id: String,
    pub version: String,
    pub size_bytes: u64,
}

/// Verify a registry download against every signed catalogue identity before
/// ingesting it into the host store.
///
/// # Errors
///
/// Returns an error when the bytes do not match the expected catalogue identity
/// or cannot be installed into the host store.
pub fn install_verified_tapp(
    store: &Store,
    bytes: &[u8],
    pinning: Pinning,
    expected: &InstallExpectation,
) -> Result<PackageId, InstallError> {
    let actual_size = u64::try_from(bytes.len())
        .map_err(|_| InstallError::Verify("package length does not fit u64".into()))?;
    if actual_size != expected.size_bytes {
        return Err(InstallError::Verify(format!(
            "package size mismatch: catalogue says {}, download has {actual_size}",
            expected.size_bytes
        )));
    }
    app_registry::verify_bytes(bytes, expected.package_root, expected.publisher)
        .map_err(|error| InstallError::Verify(error.to_string()))?;
    let meta =
        app_registry::tapp::load(bytes).map_err(|error| InstallError::Verify(error.to_string()))?;
    if meta.manifest.package.id != expected.app_id
        || meta.manifest.package.version != expected.version
    {
        return Err(InstallError::Verify(format!(
            "manifest identity mismatch: expected {}@{}, got {}@{}",
            expected.app_id,
            expected.version,
            meta.manifest.package.id,
            meta.manifest.package.version
        )));
    }
    install_tapp(store, bytes, pinning)
}

/// Ingest a .tapp-shaped byte blob into `store`.
///
/// - Verifies the signature (best-effort: self-signed is accepted; the
///   trust-set is currently empty pending publisher-trust integration).
/// - Extracts the payload eagerly to `<store>/payloads/<pkg-id-hex>/`.
/// - Inserts a `RegistryEntry` with the requested pinning.
///
/// Returns the resolved [`PackageId`].
///
/// # Errors
///
/// Returns [`InstallError::Parse`] if the tar isn't a well-formed
/// `.tapp` (missing `tapp.toml`, malformed TOML, or a bad blob),
/// [`InstallError::Verify`] if the signature check fails outright,
/// [`InstallError::Unpack`] if extracting the payload fails, or
/// [`InstallError::Store`] / [`InstallError::Io`] if writing to the
/// on-disk store fails.
pub fn install_tapp(
    store: &Store,
    bytes: &[u8],
    pinning: Pinning,
) -> Result<PackageId, InstallError> {
    // Parse every tar entry into a scratch store before touching durable
    // state. The shared parser is also the single authority for archive-layout
    // version checks.
    let app_registry::tapp::TappMeta {
        package_id: id,
        store: mem,
        ..
    } = app_registry::tapp::load(bytes).map_err(|error| InstallError::Parse(error.to_string()))?;

    // Verify — self-signed acceptable (empty trust set → UntrustedPublisher).
    let manifest = match tilo_app_package::verify(id, &mem, &[]) {
        tilo_app_package::Verification::Trusted { manifest }
        | tilo_app_package::Verification::UntrustedPublisher { manifest, .. } => manifest,
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
    // has the same content, but tilo_app_package::unpack takes `&impl BlobStore`
    // and the memory store is Send + Sync-friendly.
    tilo_app_package::unpack(id, &mem, &payload_root)
        .map_err(|e| InstallError::Unpack(e.to_string()))?;

    let manifest_text = manifest
        .to_toml_string()
        .map_err(|e| InstallError::Unpack(e.to_string()))?;
    let entry = RegistryEntry::new_now(id, pinning, payload_root, manifest_text);
    store.put_entry(entry)?;
    Ok(id)
}
