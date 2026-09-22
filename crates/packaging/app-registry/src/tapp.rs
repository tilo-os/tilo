//! Reading + verifying `.tapp` tar bytes.
//!
//! Both the publish side (build an index entry from a tapp's manifest)
//! and the fetch side (verify downloaded bytes before install) need to
//! crack open the `.tapp` tar, so the logic lives here once. The wire
//! format is the one `tilo-app-package-cli` writes:
//!
//! ```text
//! tapp.toml          # header: package_id (tapp:<hex>) + format
//! signed-root        # the signed-root blob
//! manifest.toml      # the manifest (UTF-8 TOML)
//! collection.qcl     # the encoded payload Collection
//! blobs/<hex>        # each payload blob, named by its BLAKE3 hex
//! ```

use std::io::Read;
use std::path::{Component, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use tilo_app_package::blobs::{BlobHash, BlobStore};
use tilo_app_package::signed::SignedRoot;
use tilo_app_package::{Manifest, MemoryBlobStore, PackageId, Verification, verify};

#[derive(Debug, Deserialize)]
struct TappHeader {
    package_id: String,
    format: u32,
}

/// Current version of the `.tapp` tar layout.
pub const TAPP_FORMAT_VERSION: u32 = 1;

/// The pieces of a `.tapp` a caller commonly needs.
pub struct TappMeta {
    /// Content-addressed package identity (from the header, matched
    /// against the signed-root during [`verify_bytes`]).
    pub package_id: PackageId,
    /// The decoded manifest.
    pub manifest: Manifest,
    /// Every in-band blob, ready to hand to [`tilo_app_package::verify`].
    pub store: MemoryBlobStore,
}

/// Enforce the application materialization and entry-count budgets without
/// writing the payload to disk.
///
/// # Errors
///
/// Returns an error when the package root or payload collection is missing or
/// malformed, the blob store cannot be read, or the decoded application
/// exceeds the materialization limits enforced by [`pkg`].
pub fn validate_payload_shape(meta: &TappMeta) -> Result<()> {
    let signed_bytes = meta
        .store
        .get(&BlobHash(*meta.package_id.as_bytes()))?
        .ok_or_else(|| anyhow!("signed-root blob missing"))?;
    let signed = SignedRoot::decode(&signed_bytes)?;
    let collection_bytes = meta
        .store
        .get(&signed.payload_root_hash)?
        .ok_or_else(|| anyhow!("payload collection missing"))?;
    let collection = tilo_app_package::Collection::decode(&collection_bytes)?;
    let payload_paths: Vec<PathBuf> = collection.entries.keys().map(PathBuf::from).collect();
    if payload_paths.iter().any(|path| {
        path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
    }) {
        bail!("payload collection contains an unsafe path");
    }
    tilo_app_package::validate_manifest_with_payload(&meta.manifest, &payload_paths)
        .map_err(|error| anyhow!("manifest/payload validation: {error}"))?;
    tilo_app_package::validate_materialized_payload(&meta.manifest, &collection, &meta.store)?;
    Ok(())
}

/// Parse a `.tapp` tar into its blobs + manifest. Does NOT verify the
/// signature — call [`verify_bytes`] for that.
///
/// # Errors
///
/// Returns an error if the tar can't be read, `tapp.toml` is missing
/// or malformed, or the signed-root / manifest blobs are missing or
/// fail to decode.
pub fn load(bytes: &[u8]) -> Result<TappMeta> {
    let mut store = MemoryBlobStore::new();
    let mut header: Option<TappHeader> = None;

    let mut ar = tar::Archive::new(bytes);
    for entry in ar.entries().context("read tapp tar entries")? {
        let mut entry = entry.context("read tapp tar entry")?;
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .context("read tapp tar entry body")?;
        match name.as_str() {
            "tapp.toml" => {
                let s = String::from_utf8(buf).context("tapp.toml not UTF-8")?;
                header = Some(toml::from_str(&s).context("decode tapp.toml")?);
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

    let header = header.ok_or_else(|| anyhow!(".tapp is missing tapp.toml"))?;
    if header.format != TAPP_FORMAT_VERSION {
        bail!(
            "unsupported tapp format {}; expected {}",
            header.format,
            TAPP_FORMAT_VERSION
        );
    }
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

    Ok(TappMeta {
        package_id,
        manifest,
        store,
    })
}

/// Serialise the blobs of a packed bundle into `.tapp` tar bytes —
/// the inverse of [`load`], matching the layout `tilo-app-package pack` writes.
/// Used by publish tooling + tests that build a bundle in memory.
///
/// # Errors
///
/// Returns an error if the signed-root, manifest, or collection blobs
/// are missing from `store`, or writing the tar fails.
pub fn write_bytes(id: PackageId, store: &MemoryBlobStore) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut buf);

        let header = format!("package_id = \"{id}\"\nformat = {TAPP_FORMAT_VERSION}\n");
        append(&mut tar, "tapp.toml", header.as_bytes())?;

        let signed_bytes = store
            .get(&BlobHash(*id.as_bytes()))
            .map_err(|e| anyhow!("store: {e}"))?
            .ok_or_else(|| anyhow!("signed-root blob missing"))?;
        append(&mut tar, "signed-root", &signed_bytes)?;
        let signed = SignedRoot::decode(&signed_bytes).context("decode signed-root")?;

        let manifest = store
            .get(&signed.manifest_hash)
            .map_err(|e| anyhow!("store: {e}"))?
            .ok_or_else(|| anyhow!("manifest blob missing"))?;
        append(&mut tar, "manifest.toml", &manifest)?;
        let collection = store
            .get(&signed.payload_root_hash)
            .map_err(|e| anyhow!("store: {e}"))?
            .ok_or_else(|| anyhow!("collection blob missing"))?;
        append(&mut tar, "collection.qcl", &collection)?;

        for (h, b) in store.iter() {
            if *h == BlobHash(*id.as_bytes())
                || *h == signed.manifest_hash
                || *h == signed.payload_root_hash
            {
                continue;
            }
            append(&mut tar, &format!("blobs/{h}"), b)?;
        }
        tar.finish().context("finalise tapp tar")?;
    }
    Ok(buf)
}

fn append<W: std::io::Write>(tar: &mut tar::Builder<W>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .with_context(|| format!("write tar entry {name}"))?;
    Ok(())
}

/// Verify downloaded `.tapp` bytes fail-closed:
///
/// - the header package-id must equal `expected_id`;
/// - [`tilo_app_package::verify`] against `[expected_publisher]` must return
///   `Trusted` (or `AttestedBy`) — i.e. every hash re-derives, the
///   signed-root signature validates, AND the tapp's embedded
///   publisher key equals the one the registry entry vouched for.
///
/// Any other outcome (missing/tampered blob, bad signature, publisher
/// mismatch) is a hard error — the bytes are never accepted.
///
/// # Errors
///
/// Returns an error if `bytes` fails to load, the header package-id
/// doesn't match `expected_id`, or verification against
/// `expected_publisher` doesn't come back `Trusted`/`AttestedBy`.
pub fn verify_bytes(
    bytes: &[u8],
    expected_id: PackageId,
    expected_publisher: [u8; 32],
) -> Result<PackageId> {
    let meta = load(bytes)?;
    if meta.package_id != expected_id {
        bail!(
            "package id mismatch: tapp is {}, index promised {}",
            meta.package_id,
            expected_id
        );
    }
    match verify(meta.package_id, &meta.store, &[expected_publisher]) {
        Verification::Trusted { .. } | Verification::AttestedBy { .. } => Ok(meta.package_id),
        Verification::UntrustedPublisher { identity, .. } => bail!(
            "tapp publisher ed25519:{} does not match the registry entry's publisher",
            hex::encode(identity)
        ),
        other => bail!("tapp failed verification: {other:?}"),
    }
}

#[cfg(test)]
pub(crate) mod test_fixture {
    //! Shared fixture builder for a signed `.tapp`. Also used
    //! by the crate's integration tests.
    use super::*;
    use tilo_app_package::pack::{SigningIdentity, pack};

    /// A signed `.tapp` for `so.tilo.editor@1.0.0`. Returns the bytes,
    /// its [`PackageId`], and the publisher's public key.
    pub fn signed_tapp(identity: &SigningIdentity) -> (Vec<u8>, PackageId) {
        let pubhex = hex::encode(identity.public_key_bytes());
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin").join("app"), b"#!/bin/true\ntilo app\n").unwrap();
        let manifest_toml = format!(
            r#"
[package]
id = "so.tilo.editor"
version = "1.0.0"
kind = "app"

[package.isa]
x86_64 = "v3"

[publisher]
identity = "ed25519:{pubhex}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#
        );
        let manifest = Manifest::from_toml_str(&manifest_toml).unwrap();
        let mut store = MemoryBlobStore::new();
        let id = pack(&payload, &manifest, identity, &mut store).unwrap();
        let bytes = write_bytes(id, &store).unwrap();
        (bytes, id)
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixture::signed_tapp;
    use super::*;
    use tilo_app_package::pack::SigningIdentity;

    #[test]
    fn load_reads_manifest_and_id() {
        let identity = SigningIdentity::generate();
        let (bytes, id) = signed_tapp(&identity);
        let meta = load(&bytes).unwrap();
        assert_eq!(meta.package_id, id);
        assert_eq!(meta.manifest.package.id, "so.tilo.editor");
        assert_eq!(meta.manifest.package.version, "1.0.0");
    }

    #[test]
    fn load_rejects_an_unsupported_archive_format() {
        let identity = SigningIdentity::generate();
        let (mut bytes, _) = signed_tapp(&identity);
        let marker = b"format = 1\n";
        let offset = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("fixture contains the format header");
        bytes[offset + marker.len() - 2] = b'2';

        let error = load(&bytes)
            .err()
            .expect("future archive formats must fail closed");
        assert!(error.to_string().contains("unsupported tapp format 2"));
    }

    #[test]
    fn writer_does_not_duplicate_named_metadata_under_blobs() {
        let identity = SigningIdentity::generate();
        let (bytes, id) = signed_tapp(&identity);
        let meta = load(&bytes).unwrap();
        let signed_bytes = meta.store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
        let signed = SignedRoot::decode(&signed_bytes).unwrap();
        let forbidden = [
            format!("blobs/{}", BlobHash(*id.as_bytes())),
            format!("blobs/{}", signed.manifest_hash),
            format!("blobs/{}", signed.payload_root_hash),
        ];
        let names: Vec<String> = tar::Archive::new(bytes.as_slice())
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        for duplicate in forbidden {
            assert!(
                !names.contains(&duplicate),
                "duplicated {duplicate}: {names:?}"
            );
        }
        for named in ["signed-root", "manifest.toml", "collection.qcl"] {
            assert!(names.iter().any(|name| name == named), "missing {named}");
        }
    }

    #[test]
    fn verify_accepts_the_real_publisher() {
        let identity = SigningIdentity::generate();
        let (bytes, id) = signed_tapp(&identity);
        assert_eq!(
            verify_bytes(&bytes, id, identity.public_key_bytes()).unwrap(),
            id
        );
    }

    #[test]
    fn verify_rejects_a_different_expected_publisher() {
        let identity = SigningIdentity::generate();
        let (bytes, id) = signed_tapp(&identity);
        // A publisher the entry claims but the tapp was NOT signed by.
        let other = SigningIdentity::generate().public_key_bytes();
        assert!(verify_bytes(&bytes, id, other).is_err());
    }

    #[test]
    fn verify_rejects_a_mismatched_package_id() {
        let identity = SigningIdentity::generate();
        let (bytes, _id) = signed_tapp(&identity);
        let wrong = PackageId::from_bytes([3u8; 32]);
        assert!(verify_bytes(&bytes, wrong, identity.public_key_bytes()).is_err());
    }

    #[test]
    fn verify_rejects_a_tampered_signature() {
        let identity = SigningIdentity::generate();
        let (bytes, id) = signed_tapp(&identity);

        // Rebuild the bundle with one bit of the signature flipped and
        // its (now different) package-id in the header. The id matches
        // the header, so verify reaches — and fails — the signature
        // check.
        let meta = load(&bytes).unwrap();
        let signed_bytes = meta.store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
        let mut signed = SignedRoot::decode(&signed_bytes).unwrap();
        let mut sig = signed.signature.unwrap();
        sig[0] ^= 0xff;
        signed.signature = Some(sig);

        let mut store = MemoryBlobStore::new();
        for (h, b) in meta.store.iter() {
            if h != &BlobHash(*id.as_bytes()) {
                store.put(b).unwrap();
            }
        }
        let tampered_signed = signed.encode();
        store.put(&tampered_signed).unwrap();
        let new_id = signed.package_id();

        let tampered = write_bytes(new_id, &store).unwrap();
        assert!(verify_bytes(&tampered, new_id, identity.public_key_bytes()).is_err());
    }
}
