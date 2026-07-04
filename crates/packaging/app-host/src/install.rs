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
use pkg::trust::{RevocationList, TrustedAttestors};
use serde::Deserialize;
use thiserror::Error;

use crate::registry::{Pinning, RegistryEntry};
use crate::store::{Store, StoreError};

/// One-line header at the top of every .qpkg tarball.
#[derive(Debug, Deserialize)]
struct QpkgHeader {
    /// `qpkg:<hex>` form of the PackageId.
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
    #[error("policy refused: --require-attestor {required:?} but package verified as {found}")]
    AttestorMismatch { required: String, found: String },
    #[error("policy refused: --require-attestor {required:?} and package is self-signed")]
    RequireAttestorWithoutAttestation { required: String },
    #[error("publisher key is revoked ({reason})")]
    Revoked { reason: String },
    #[error("unpack payload: {0}")]
    Unpack(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// Publisher-trust policy the daemon applies to every install.
///
/// Loaded once at daemon startup from the same
/// `~/.config/qos/trusted-attestors.toml` +
/// `~/.config/qos/revoked-keys.toml` files `qos-install` uses, so a
/// single trust config governs both entry points.
#[derive(Debug, Default, Clone)]
pub struct TrustPolicy {
    pub attestors: TrustedAttestors,
    pub revocation: RevocationList,
    /// If set, refuse any install that doesn't verify as attested by
    /// this attestor id. Default `None` — matches `qos-install`'s
    /// warn-then-install behaviour on self-signed packages.
    pub require_attestor: Option<String>,
}

impl TrustPolicy {
    /// Load the default on-disk trust config. Missing files degrade
    /// to an empty policy (attestors + revoked-keys empty). Failure
    /// to parse a file surfaces as an error.
    pub fn load_default() -> Result<Self, pkg::trust::TrustLoadError> {
        let attestors = match pkg::trust::default_trusted_attestors_path() {
            Some(p) => TrustedAttestors::load(&p)?,
            None => TrustedAttestors::empty(),
        };
        let revocation = match pkg::trust::default_revoked_keys_path() {
            Some(p) => RevocationList::load(&p)?,
            None => RevocationList::empty(),
        };
        Ok(Self { attestors, revocation, require_attestor: None })
    }

    /// Empty policy — nothing trusted, nothing revoked, no gate.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Ingest a .qpkg-shaped byte blob into `store` with no publisher-trust
/// policy applied. Kept for callers (tests, in-process integrations)
/// that don't want to plumb a policy through — see
/// [`install_qpkg_with_policy`] for the full API.
pub fn install_qpkg(
    store: &Store,
    bytes: &[u8],
    pinning: Pinning,
) -> Result<PackageId, InstallError> {
    install_qpkg_with_policy(store, bytes, pinning, &TrustPolicy::empty())
}

/// Ingest a .qpkg-shaped byte blob into `store`, gating the install on
/// `policy`.
///
/// - Runs [`pkg::verify_with_trust`] against the policy's attestor list
///   + revocation list. A revoked publisher aborts the install
///   regardless of the `require_attestor` field.
/// - When `policy.require_attestor` is set, refuses installs that don't
///   verify as attested by that id (or that verify as `Trusted` via a
///   trusted-key path, which bypasses the attestor chain and is
///   equally refused — mirrors `qos-install`'s strict-require gate).
/// - Otherwise: an untrusted (self-signed) publisher surfaces as a
///   `tracing::warn!` and the install proceeds, matching
///   `qos-install`'s warn-then-install default.
/// - Extracts the payload eagerly to `<store>/payloads/<pkg-id-hex>/`.
/// - Inserts a `RegistryEntry` with the requested pinning.
///
/// Returns the resolved [`PackageId`].
pub fn install_qpkg_with_policy(
    store: &Store,
    bytes: &[u8],
    pinning: Pinning,
    policy: &TrustPolicy,
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
        let entry_path = entry
            .path()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let name = entry_path.to_string_lossy().to_string();
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| InstallError::Parse(e.to_string()))?;
        match name.as_str() {
            "qpkg.toml" => {
                let s = String::from_utf8(buf).map_err(|e| {
                    InstallError::Parse(format!("qpkg.toml not UTF-8: {e}"))
                })?;
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
    let header = header
        .ok_or_else(|| InstallError::Parse(".qpkg is missing qpkg.toml".into()))?;
    let id: PackageId = header
        .package_id
        .parse()
        .map_err(|e| InstallError::Parse(format!("qpkg.toml package_id: {e:?}")))?;

    // Verify + apply the trust policy.
    let outcome = pkg::verify_with_trust(
        id,
        &mem,
        &[],
        policy.attestors.as_slice(),
        &policy.revocation,
    );
    let manifest = match outcome {
        pkg::Verification::AttestedBy { attestor, manifest } => {
            if let Some(required) = &policy.require_attestor {
                if required != &attestor {
                    return Err(InstallError::AttestorMismatch {
                        required: required.clone(),
                        found: attestor,
                    });
                }
            }
            manifest
        }
        pkg::Verification::Trusted { manifest } => {
            // Trusted-key path bypasses the attestor chain — with a
            // strict `require_attestor` gate this counts as a mismatch,
            // matching `qos-install`'s stricter behaviour.
            if let Some(required) = &policy.require_attestor {
                return Err(InstallError::AttestorMismatch {
                    required: required.clone(),
                    found: "trusted-key".into(),
                });
            }
            manifest
        }
        pkg::Verification::UntrustedPublisher { identity, manifest } => {
            if let Some(required) = &policy.require_attestor {
                return Err(InstallError::RequireAttestorWithoutAttestation {
                    required: required.clone(),
                });
            }
            tracing::warn!(
                publisher = %hex::encode(identity),
                "app-host install: publisher is untrusted (self-signed) — installing anyway; use --require-attestor to gate"
            );
            manifest
        }
        pkg::Verification::Revoked { reason, .. } => {
            return Err(InstallError::Revoked { reason });
        }
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
    pkg::unpack(id, &mem, &payload_root)
        .map_err(|e| InstallError::Unpack(e.to_string()))?;

    let manifest_text = manifest
        .to_toml_string()
        .map_err(|e| InstallError::Unpack(e.to_string()))?;
    let entry = RegistryEntry::new_now(id, pinning, payload_root, manifest_text);
    store.put_entry(entry)?;
    Ok(id)
}
