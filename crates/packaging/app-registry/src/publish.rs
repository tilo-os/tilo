//! The publish side of the registry protocol.
//!
//! Publishing is: (1) build a per-entry attestation from the qpkg's
//! own manifest and sign it with the publisher identity, (2) upload
//! the qpkg blob to iroh-blobs, (3) upsert the entry into the target
//! registry's index and (for a self-hosted `file://` registry)
//! re-sign the index with the operator key.
//!
//! The `file://` path is fully implemented + tested here. The
//! `https://` path emits the signed entry to a registry server's
//! `/publish` endpoint; the server-side blob ingest + index re-sign is
//! the registry service's job (a `file://` directory is the reference
//! for what that server does).

use std::path::Path;

use anyhow::{Result, anyhow, bail};

use pkg::PackageId;
use pkg::manifest::{AArch64Level, Arch, IsaLevel, X86_64Level};
use pkg::pack::SigningIdentity;

use crate::index::{IndexEntry, RegistryIndex};
use crate::qpkg;
use crate::registry_dir;
use crate::transport::BlobNode;

/// What `publish` produced.
#[derive(Clone, Debug)]
pub struct PublishReceipt {
    pub app_id: String,
    pub version: String,
    pub package_id: PackageId,
    /// blake3 hex of the qpkg (== iroh content hash).
    pub qpkg_hash: String,
    pub registry_id: String,
}

/// The widest ISA floor for an arch — used when a manifest declares
/// no per-arch ISA requirement.
fn baseline_isa(arch: Arch) -> IsaLevel {
    match arch {
        Arch::X86_64 => IsaLevel::X86_64(X86_64Level::Baseline),
        Arch::AArch64 => IsaLevel::AArch64(AArch64Level::Armv8A),
    }
}

/// Build + publisher-sign an [`IndexEntry`] from a `.qpkg`, WITHOUT
/// uploading anything.
///
/// - `arch` selects which arch this build targets; when `None` it is
///   inferred (the single arch the manifest declares an ISA for, else
///   the host arch).
/// - `ticket` fills the entry's fetch source; pass `None` for a
///   dry-run (the entry can still be inspected / diffed).
///
/// Fails closed if the `publisher` key does not match the manifest's
/// declared `publisher.identity` — the entry must be vouched for by
/// the same key that signed the package.
///
/// # Errors
///
/// Returns an error if the `.qpkg` fails to load, `publisher` doesn't
/// match the manifest's declared identity, the target arch can't be
/// inferred, or signing the built entry fails.
pub fn build_entry(
    qpkg_bytes: &[u8],
    publisher: &SigningIdentity,
    arch: Option<Arch>,
    ticket: Option<&str>,
) -> Result<(IndexEntry, PackageId)> {
    let meta = qpkg::load(qpkg_bytes)?;

    let declared = meta
        .manifest
        .publisher
        .identity_bytes()
        .map_err(|e| anyhow!("manifest publisher identity: {e}"))?;
    if declared != publisher.public_key_bytes() {
        bail!(
            "signing identity ed25519:{} does not match the manifest publisher ed25519:{}",
            hex::encode(publisher.public_key_bytes()),
            hex::encode(declared)
        );
    }

    // Which arch does this build target?
    let mut candidates = Vec::new();
    if meta.manifest.package.isa.for_arch(Arch::X86_64).is_some() {
        candidates.push(Arch::X86_64);
    }
    if meta.manifest.package.isa.for_arch(Arch::AArch64).is_some() {
        candidates.push(Arch::AArch64);
    }
    let arch = match arch {
        Some(a) => a,
        None => match candidates.as_slice() {
            [only] => *only,
            [] => crate::index::HostProfile::detect().arch,
            _ => bail!(
                "manifest declares ISA for multiple arches ({candidates:?}); pass an explicit arch"
            ),
        },
    };
    let isa = meta
        .manifest
        .package
        .isa
        .for_arch(arch)
        .unwrap_or_else(|| baseline_isa(arch));

    let qpkg_hash = *blake3::hash(qpkg_bytes).as_bytes();

    let mut entry = IndexEntry {
        app_id: meta.manifest.package.id.clone(),
        version: meta.manifest.package.version.clone(),
        package_id: meta.package_id.to_string(),
        qpkg_hash: hex::encode(qpkg_hash),
        size_bytes: qpkg_bytes.len() as u64,
        arch,
        isa: isa.wire_label().to_string(),
        runtime: meta.manifest.package.runtime.clone(),
        publisher_identity: String::new(),
        publisher_signature: String::new(),
        ticket: ticket.unwrap_or_default().to_string(),
    };
    entry.sign(publisher)?;
    Ok((entry, meta.package_id))
}

/// Publish a `.qpkg` to a `file://` registry directory: upload the
/// blob into the registry's iroh-blobs store (served by `node`) and
/// upsert + re-sign the index.
///
/// `node` MUST be serving `<dir>/blobs` (see
/// [`crate::transport::BlobNode::persistent`]) so the ticket baked
/// into the entry actually resolves.
///
/// # Errors
///
/// Returns an error if the upload, [`build_entry`], the index
/// load/sign, or the index write fails; also if the uploaded blob's
/// hash disagrees with the built entry (an internal invariant).
pub async fn publish_to_dir(
    dir: &Path,
    node: &BlobNode,
    qpkg_bytes: &[u8],
    publisher: &SigningIdentity,
    registry_key: &SigningIdentity,
    registry_id: &str,
    arch: Option<Arch>,
) -> Result<PublishReceipt> {
    // Upload the blob; the returned hash is the iroh content hash.
    let (hash, ticket) = node.add_and_ticket(qpkg_bytes.to_vec()).await?;

    let (entry, package_id) = build_entry(qpkg_bytes, publisher, arch, Some(&ticket.to_string()))?;

    // The uploaded blob's hash must match the entry's advertised hash.
    if entry.qpkg_hash_bytes()? != *hash.as_bytes() {
        bail!("internal: uploaded blob hash disagrees with the built entry");
    }

    let mut index = registry_dir::load_or_new_index(dir, registry_id)?;
    let app_id = entry.app_id.clone();
    let version = entry.version.clone();
    let qpkg_hash = entry.qpkg_hash.clone();
    index.upsert(entry);
    index
        .sign(registry_key)
        .map_err(|e| anyhow!("sign index: {e}"))?;
    registry_dir::write_index(dir, &index)?;

    Ok(PublishReceipt {
        app_id,
        version,
        package_id,
        qpkg_hash,
        registry_id: registry_id.to_string(),
    })
}

/// Serialise a signed entry as the JSON body a registry server's
/// `/publish` endpoint accepts. The publisher signature in the entry
/// IS the authentication — no separate password.
///
/// # Errors
///
/// Returns an error if `entry` fails to serialise.
pub fn entry_publish_body(entry: &IndexEntry) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(entry).map_err(|e| anyhow!("encode entry: {e}"))
}

/// POST a signed entry to an `https://` registry's `/publish`
/// endpoint. The registry authenticates by verifying the entry's
/// publisher signature and ingests the blob out of band from the
/// entry's ticket. Not exercised in-tree (needs a live server); the
/// `file://` path is the tested reference.
///
/// # Errors
///
/// Returns an error if `entry` fails to serialise, the request fails,
/// or the registry responds with a non-success status.
pub async fn emit_entry_https(base_url: &str, entry: &IndexEntry) -> Result<()> {
    let url = format!("{}/publish", base_url.trim_end_matches('/'));
    let body = entry_publish_body(entry)?;
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header("x-qos-publisher", &entry.publisher_identity)
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        bail!("registry rejected publish: HTTP {}", resp.status());
    }
    Ok(())
}

/// Convenience for a signed index built off a fresh (non-directory)
/// context — used by CI / tooling that assembles an index in memory.
///
/// # Errors
///
/// Returns an error if signing the index fails.
pub fn sign_index(index: &mut RegistryIndex, registry_key: &SigningIdentity) -> Result<()> {
    index
        .sign(registry_key)
        .map_err(|e| anyhow!("sign index: {e}"))
}
