//! The publish side of the registry protocol.
//!
//! Publishing is: (1) build a per-entry attestation from the tapp's
//! own manifest and sign it with the publisher identity, (2) upload
//! the tapp blob to iroh-blobs, (3) upsert the entry into the target
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

use tilo_app_package::PackageId;
use tilo_app_package::manifest::Arch;
use tilo_app_package::pack::SigningIdentity;

use crate::index::{IndexEntry, RegistryIndex};
use crate::registry_dir;
use crate::tapp;
use crate::transport::BlobNode;

/// What `publish` produced.
#[derive(Clone, Debug)]
pub struct PublishReceipt {
    pub app_id: String,
    pub version: String,
    pub package_id: PackageId,
    /// blake3 hex of the tapp (== iroh content hash).
    pub tapp_hash: String,
    pub registry_id: String,
}

/// Build + publisher-sign an [`IndexEntry`] from a `.tapp`, WITHOUT
/// uploading anything.
///
/// - `arch` selects which arch this build targets; when `None` it is
///   inferred only when the manifest declares exactly one architecture.
/// - `ticket` fills the entry's fetch source; pass `None` for a
///   dry-run (the entry can still be inspected / diffed).
///
/// Fails closed if the `publisher` key does not match the manifest's
/// declared `publisher.identity` — the entry must be vouched for by
/// the same key that signed the package.
///
/// # Errors
///
/// Returns an error if the `.tapp` fails to load, `publisher` doesn't
/// match the manifest's declared identity, the target arch can't be
/// inferred, or signing the built entry fails.
pub fn build_entry(
    tapp_bytes: &[u8],
    publisher: &SigningIdentity,
    arch: Option<Arch>,
    ticket: Option<&str>,
) -> Result<(IndexEntry, PackageId)> {
    let meta = tapp::load(tapp_bytes)?;
    tapp::validate_payload_shape(&meta)?;

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

    // Publishing is a trust boundary too: a manifest that merely names this
    // key is not proof that the signed root authenticates the in-band blobs.
    // Fail before upload/index mutation even for a self-hosted registry.
    tapp::verify_bytes(tapp_bytes, meta.package_id, publisher.public_key_bytes())?;

    if !tilo_app_package::tapp_size_allowed(
        meta.manifest.package.kind,
        u64::try_from(tapp_bytes.len()).unwrap_or(u64::MAX),
    ) {
        bail!(
            "application .tapp is {} bytes; limit is {} bytes (put shared dependencies in the runtime bundle)",
            tapp_bytes.len(),
            tilo_app_package::MAX_APP_TAPP_BYTES
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
            [] => bail!("manifest declares no target architecture"),
            _ => bail!(
                "manifest declares ISA for multiple arches ({candidates:?}); pass an explicit arch"
            ),
        },
    };
    let isa =
        meta.manifest.package.isa.for_arch(arch).ok_or_else(|| {
            anyhow!("manifest declares no build for target architecture {arch:?}")
        })?;

    let tapp_hash = *blake3::hash(tapp_bytes).as_bytes();

    let mut entry = IndexEntry {
        app_id: meta.manifest.package.id.clone(),
        version: meta.manifest.package.version.clone(),
        package_id: meta.package_id.to_string(),
        tapp_hash: hex::encode(tapp_hash),
        size_bytes: tapp_bytes.len() as u64,
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

/// Publish a `.tapp` to a `file://` registry directory: upload the
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
    tapp_bytes: &[u8],
    publisher: &SigningIdentity,
    registry_key: &SigningIdentity,
    registry_id: &str,
    arch: Option<Arch>,
) -> Result<PublishReceipt> {
    // Authenticate and enforce payload budgets before mutating blob storage.
    let (mut entry, package_id) = build_entry(tapp_bytes, publisher, arch, None)?;

    // Upload only accepted bytes, then bind the resulting fetch source into
    // the publisher-signed entry.
    let (hash, ticket) = node.add_and_ticket(tapp_bytes.to_vec()).await?;
    entry.ticket = ticket.to_string();
    entry.sign(publisher)?;

    // The uploaded blob's hash must match the entry's advertised hash.
    if entry.tapp_hash_bytes()? != *hash.as_bytes() {
        bail!("internal: uploaded blob hash disagrees with the built entry");
    }

    let mut index = registry_dir::load_or_new_index(dir, registry_id)?;
    let app_id = entry.app_id.clone();
    let version = entry.version.clone();
    let tapp_hash = entry.tapp_hash.clone();
    index.upsert(entry);
    index
        .sign(registry_key)
        .map_err(|e| anyhow!("sign index: {e}"))?;
    registry_dir::write_index(dir, &index)?;

    Ok(PublishReceipt {
        app_id,
        version,
        package_id,
        tapp_hash,
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
        .header("x-tilo-publisher", &entry.publisher_identity)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tapp::{self, test_fixture::signed_tapp};
    use tilo_app_package::{BlobStore, MemoryBlobStore, SignedRoot};

    #[test]
    fn build_entry_accepts_a_cryptographically_valid_bundle() {
        let publisher = SigningIdentity::generate();
        let (bytes, id) = signed_tapp(&publisher);
        let (entry, actual) = build_entry(&bytes, &publisher, None, None).unwrap();
        assert_eq!(actual, id);
        assert_eq!(entry.package_id, id.to_string());
    }

    #[test]
    fn build_entry_rejects_a_manifest_that_names_publisher_but_has_bad_signature() {
        let publisher = SigningIdentity::generate();
        let (bytes, id) = signed_tapp(&publisher);
        let meta = tapp::load(&bytes).unwrap();
        let root_bytes = meta
            .store
            .get(&tilo_app_package::BlobHash(*id.as_bytes()))
            .unwrap()
            .unwrap();
        let mut root = SignedRoot::decode(&root_bytes).unwrap();
        let mut signature = root.signature.unwrap();
        signature[0] ^= 1;
        root.signature = Some(signature);

        let mut tampered_store = MemoryBlobStore::new();
        for (hash, blob) in meta.store.iter() {
            if hash != &tilo_app_package::BlobHash(*id.as_bytes()) {
                tampered_store.put(blob).unwrap();
            }
        }
        tampered_store.put(&root.encode()).unwrap();
        let tampered = tapp::write_bytes(root.package_id(), &tampered_store).unwrap();

        let error = build_entry(&tampered, &publisher, None, None).unwrap_err();
        let message = error.to_string().to_ascii_lowercase();
        assert!(
            message.contains("signature") || message.contains("verify"),
            "unexpected rejection: {error:#}"
        );
    }

    #[test]
    fn build_entry_rejects_signed_deduplication_amplification() {
        let publisher = SigningIdentity::generate();
        let (bytes, id) = signed_tapp(&publisher);
        let meta = tapp::load(&bytes).unwrap();
        let root_bytes = meta
            .store
            .get(&tilo_app_package::BlobHash(*id.as_bytes()))
            .unwrap()
            .unwrap();
        let mut root = SignedRoot::decode(&root_bytes).unwrap();

        let mut malicious_store = MemoryBlobStore::new();
        for (hash, blob) in meta.store.iter() {
            if hash != &tilo_app_package::BlobHash(*id.as_bytes()) {
                malicious_store.put(blob).unwrap();
            }
        }
        // 2,049 references expand beyond 5 MiB while remaining under the
        // independent 4,096-entry ceiling, so this specifically exercises
        // the materialized-byte boundary.
        let repeated = malicious_store.put(&vec![0x5a; 3072]).unwrap();
        let mut collection = tilo_app_package::Collection::default();
        for index in 0..=2048 {
            collection
                .entries
                .insert(format!("share/repeated-{index}"), repeated);
        }
        root.payload_root_hash = malicious_store.put(&collection.encode()).unwrap();
        root.signature = Some(publisher.sign_signed_root(&root).unwrap());
        malicious_store.put(&root.encode()).unwrap();
        let malicious = tapp::write_bytes(root.package_id(), &malicious_store).unwrap();
        assert!(
            u64::try_from(malicious.len()).unwrap_or(u64::MAX)
                < tilo_app_package::MAX_APP_TAPP_BYTES,
            "fixture must exercise materialization rather than wire-size rejection"
        );

        let error = build_entry(&malicious, &publisher, None, None).unwrap_err();
        assert!(
            error.to_string().contains("materializes"),
            "unexpected rejection: {error:#}"
        );
    }
}
