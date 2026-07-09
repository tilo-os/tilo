//! Reading + verifying `.qpkg` tar bytes.
//!
//! Both the publish side (build an index entry from a qpkg's manifest)
//! and the fetch side (verify downloaded bytes before install) need to
//! crack open the `.qpkg` tar, so the logic lives here once. The wire
//! format is the one `pkg-cli` writes:
//!
//! ```text
//! qpkg.toml          # header: package_id (qpkg:<hex>) + format
//! signed-root        # the signed-root blob
//! manifest.toml      # the manifest (UTF-8 TOML)
//! collection.qcl     # the encoded payload Collection
//! blobs/<hex>        # each payload blob, named by its BLAKE3 hex
//! ```

use std::io::Read;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use pkg::blobs::{BlobHash, BlobStore};
use pkg::signed::SignedRoot;
use pkg::{Manifest, MemoryBlobStore, PackageId, Verification, verify};

#[derive(Debug, Deserialize)]
struct QpkgHeader {
    package_id: String,
    #[allow(dead_code)]
    format: u32,
}

/// The pieces of a `.qpkg` a caller commonly needs.
pub struct QpkgMeta {
    /// Content-addressed package identity (from the header, matched
    /// against the signed-root during [`verify_bytes`]).
    pub package_id: PackageId,
    /// The decoded manifest.
    pub manifest: Manifest,
    /// Every in-band blob, ready to hand to [`pkg::verify`].
    pub store: MemoryBlobStore,
}

/// Parse a `.qpkg` tar into its blobs + manifest. Does NOT verify the
/// signature — call [`verify_bytes`] for that.
pub fn load(bytes: &[u8]) -> Result<QpkgMeta> {
    let mut store = MemoryBlobStore::new();
    let mut header: Option<QpkgHeader> = None;

    let mut ar = tar::Archive::new(bytes);
    for entry in ar.entries().context("read qpkg tar entries")? {
        let mut entry = entry.context("read qpkg tar entry")?;
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("read qpkg tar entry body")?;
        match name.as_str() {
            "qpkg.toml" => {
                let s = String::from_utf8(buf).context("qpkg.toml not UTF-8")?;
                header = Some(toml::from_str(&s).context("decode qpkg.toml")?);
            }
            n if n.starts_with("blobs/") => {
                store.put(&buf).map_err(|e| anyhow!("blob store: {e}"))?;
            }
            "signed-root" | "manifest.toml" | "collection.qcl" => {
                store.put(&buf).map_err(|e| anyhow!("blob store: {e}"))?;
            }
            _ => {}
        }
    }

    let header = header.ok_or_else(|| anyhow!(".qpkg is missing qpkg.toml"))?;
    let package_id: PackageId = header
        .package_id
        .parse()
        .map_err(|e| anyhow!("bad package id {:?}: {e:?}", header.package_id))?;

    let signed_bytes = store
        .get(&BlobHash(*package_id.as_bytes()))
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("signed-root blob missing"))?;
    let signed = SignedRoot::decode(&signed_bytes).context("decode signed-root")?;
    let manifest_bytes = store
        .get(&signed.manifest_hash)
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("manifest blob missing"))?;
    let manifest_str = std::str::from_utf8(&manifest_bytes).context("manifest not UTF-8")?;
    let manifest = Manifest::from_toml_str(manifest_str).context("decode manifest")?;

    Ok(QpkgMeta {
        package_id,
        manifest,
        store,
    })
}

/// Verify downloaded `.qpkg` bytes fail-closed:
///
/// - the header package-id must equal `expected_id`;
/// - [`pkg::verify`] against `[expected_publisher]` must return
///   `Trusted` (or `AttestedBy`) — i.e. every hash re-derives, the
///   signed-root signature validates, AND the qpkg's embedded
///   publisher key equals the one the registry entry vouched for.
///
/// Any other outcome (missing/tampered blob, bad signature, publisher
/// mismatch) is a hard error — the bytes are never accepted.
pub fn verify_bytes(
    bytes: &[u8],
    expected_id: PackageId,
    expected_publisher: [u8; 32],
) -> Result<PackageId> {
    let meta = load(bytes)?;
    if meta.package_id != expected_id {
        bail!(
            "package id mismatch: qpkg is {}, index promised {}",
            meta.package_id,
            expected_id
        );
    }
    match verify(meta.package_id, &meta.store, &[expected_publisher]) {
        Verification::Trusted { .. } => Ok(meta.package_id),
        Verification::AttestedBy { .. } => Ok(meta.package_id),
        Verification::UntrustedPublisher { identity, .. } => bail!(
            "qpkg publisher ed25519:{} does not match the registry entry's publisher",
            hex::encode(identity)
        ),
        other => bail!("qpkg failed verification: {other:?}"),
    }
}
