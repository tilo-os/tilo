//! Reading + verifying `.tapp` bytes on the registry protocol's edges.
//!
//! Both the publish side (build an index entry from a tapp's manifest)
//! and the fetch side (verify downloaded bytes before install) need to
//! open a `.tapp`; the archive format itself lives in
//! [`tilo_app_package::tapp`], and this module adds the registry's
//! checks on top of it.

use anyhow::{Context, Result, anyhow, bail};

use tilo_app_package::{DiscardBlobs, Manifest, PackageId, Tapp, Verification, verify};

pub use tilo_app_package::tapp::TAPP_FORMAT_VERSION;

/// The pieces of a `.tapp` a caller commonly needs.
pub struct TappMeta {
    /// Content-addressed package identity.
    pub package_id: PackageId,
    /// The decoded manifest.
    pub manifest: Manifest,
    /// The decoded archive: metadata plus the sizes of carried blobs, each
    /// already checked against its hash.
    pub tapp: Tapp,
}

/// Enforce the application materialization and entry-count budgets without
/// writing the payload to disk. Needs the full form, which carries every
/// payload size.
///
/// # Errors
///
/// Returns an error when the archive lacks payload blobs or the decoded
/// application exceeds the materialization limits.
pub fn validate_payload_shape(meta: &TappMeta) -> Result<()> {
    let tapp = &meta.tapp;
    let missing = tapp.missing();
    if !missing.is_empty() {
        bail!(
            "the payload shape needs the full .tapp; {} payload blob(s) are not carried",
            missing.len()
        );
    }
    let payload_paths: Vec<std::path::PathBuf> = tapp
        .meta
        .collection
        .entries
        .keys()
        .map(std::path::PathBuf::from)
        .collect();
    tilo_app_package::validate_manifest_with_payload(&meta.manifest, &payload_paths)
        .map_err(|error| anyhow!("manifest/payload validation: {error}"))?;
    tilo_app_package::validate_materialized_payload(
        &meta.manifest,
        &tapp.meta.collection,
        |hash| tapp.carried.get(hash).copied(),
    )?;
    Ok(())
}

/// Decode a `.tapp`, checking the metadata hash chain and every carried
/// payload blob. Does NOT check the signature — call [`verify_bytes`] for
/// that.
///
/// # Errors
///
/// Returns an error if the archive is malformed, its metadata links are
/// broken, or a carried blob does not match its hash.
pub fn load(bytes: &[u8]) -> Result<TappMeta> {
    let tapp = tilo_app_package::tapp::read(bytes, &mut DiscardBlobs).context("decode .tapp")?;
    Ok(TappMeta {
        package_id: tapp.meta.id,
        manifest: tapp.meta.manifest.clone(),
        tapp,
    })
}

/// Verify downloaded `.tapp` bytes fail-closed:
///
/// - the package id must equal `expected_id`;
/// - the metadata hash chain and every carried payload blob must check;
/// - the signature must be valid and made by `expected_publisher`, the
///   key the registry entry vouched for.
///
/// A thin archive passes on its metadata; its payload is checked blob by
/// blob by whoever fetches it. Any other outcome is a hard error — the
/// bytes are never accepted.
///
/// # Errors
///
/// Returns an error if `bytes` fails to decode, the package id doesn't
/// match `expected_id`, or verification against `expected_publisher`
/// doesn't come back `Trusted`/`AttestedBy`.
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
    if !meta.tapp.carried.is_empty() && !meta.tapp.is_complete() {
        bail!("tapp carries only part of its payload");
    }
    match verify(&meta.tapp.meta, &[expected_publisher]) {
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
    use tilo_app_package::pack::{SigningIdentity, pack};
    use tilo_app_package::{Manifest, PackageId};

    /// A signed, full `.tapp` for `so.tilo.editor@1.0.0`. Returns the bytes
    /// and its [`PackageId`].
    pub fn signed_tapp(identity: &SigningIdentity) -> (Vec<u8>, PackageId) {
        let packed = signed_package(identity);
        let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        (bytes, packed.id())
    }

    /// The packed fixture itself, for tests that re-sign or rewrite it.
    /// The payload directory lives in a leaked temp dir so the result can
    /// still stream its files.
    pub fn signed_package(identity: &SigningIdentity) -> tilo_app_package::Packed {
        let pubhex = hex::encode(identity.public_key_bytes());
        let dir = tempfile::tempdir().unwrap().keep();
        let payload = dir.join("payload");
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
        pack(&payload, &manifest, identity).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixture::{signed_package, signed_tapp};
    use super::*;
    use tilo_app_package::PackageMeta;
    use tilo_app_package::pack::SigningIdentity;

    #[test]
    fn tapps_load_and_verify_only_against_their_real_publisher_and_id() {
        let identity = SigningIdentity::generate();
        let publisher = identity.public_key_bytes();
        let (bytes, id) = signed_tapp(&identity);

        // Load reads the manifest and id.
        let meta = load(&bytes).unwrap();
        assert_eq!(meta.package_id, id);
        assert_eq!(meta.manifest.package.id, "so.tilo.editor");
        assert_eq!(meta.manifest.package.version, "1.0.0");
        assert!(meta.tapp.is_complete());

        // Verify accepts the real publisher, and rejects a publisher the entry
        // claims but the tapp was NOT signed by, and a mismatched package id.
        assert_eq!(verify_bytes(&bytes, id, publisher).unwrap(), id);
        let other = SigningIdentity::generate().public_key_bytes();
        assert!(verify_bytes(&bytes, id, other).is_err());
        let wrong = PackageId::from_bytes([3u8; 32]);
        assert!(verify_bytes(&bytes, wrong, publisher).is_err());

        // A thin archive verifies on its metadata but has no payload.
        let packed = signed_package(&identity);
        let thin = tilo_app_package::tapp::to_bytes(&packed.meta, None).unwrap();
        assert_eq!(
            verify_bytes(&thin, packed.id(), publisher).unwrap(),
            packed.id()
        );
        assert!(validate_payload_shape(&load(&thin).unwrap()).is_err());

        // Rebuild the package with one bit of the signature flipped. Its (now
        // different) package id is consistent throughout, so verify reaches —
        // and fails — the signature check.
        let mut root = packed.meta.root.clone();
        let mut sig = root.signature.unwrap();
        sig[0] ^= 0xff;
        root.signature = Some(sig);
        let tampered = PackageMeta::from_root(
            root,
            packed.meta.manifest_bytes.clone(),
            packed.meta.collection_bytes.clone(),
        )
        .unwrap();
        let bytes = tilo_app_package::tapp::to_bytes(&tampered, Some(&packed)).unwrap();
        assert!(verify_bytes(&bytes, tampered.id, publisher).is_err());
    }
}
