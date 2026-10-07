//! pack → archive → unpack → verify integration tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;

use crate::fixtures;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::manifest::Manifest;
use tilo_app_package::{
    BlobHash, Collection, MetaError, PackageMeta, SignedRoot, SigningIdentity, Verification, pack,
    tapp, unpack, validate_materialized_payload, verify,
};

fn fresh_identity() -> (SigningKey, String) {
    let mut rng = UnwrapErr(SysRng);
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, pub_hex)
}

fn manifest_with_key(pub_hex: &str) -> Manifest {
    let src = fixtures::render_template(fixtures::SAMPLE_APP_MANIFEST_TEMPLATE, pub_hex);
    Manifest::from_toml_str(&src).expect("decode")
}

fn packed_fixture() -> (SigningKey, tempfile::TempDir, tilo_app_package::Packed) {
    let (sk, pub_hex) = fresh_identity();
    let manifest = manifest_with_key(&pub_hex);
    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();
    let identity = SigningIdentity::from_signing_key(sk.clone());
    let packed = pack(payload_dir.path(), &manifest, &identity).expect("pack");
    (sk, payload_dir, packed)
}

#[test]
fn unpacking_verifies_bytes_and_enforces_materialised_limits() {
    // phase: pack_archive_unpack_roundtrip
    {
        let (_sk, payload_dir, packed) = packed_fixture();

        // Through a full archive and back, with the payload held only by the
        // decoder's sink.
        let archive = tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        let mut blobs = BTreeMap::new();
        let decoded = tapp::read(archive.as_slice(), &mut blobs).unwrap();
        assert!(decoded.is_complete());

        let dest = tempfile::tempdir().unwrap();
        let out = dest.path().join("tree");
        unpack(&decoded.meta, &blobs, &out).expect("unpack");

        // Manifests are identical apart from the signature field, which pack
        // clears before encoding the manifest blob.
        let mut expected = decoded.meta.manifest.clone();
        expected.publisher.signature.clear();
        assert_eq!(decoded.meta.manifest, expected);

        for rel in ["bin/editor", "share/splash.png"] {
            assert_eq!(
                std::fs::read(payload_dir.path().join(rel)).unwrap(),
                std::fs::read(out.join(rel)).unwrap(),
                "{rel}"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(out.join("bin/editor"))
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0, "the entrypoint is executable");
        }
    }
    // phase: unpack_refuses_bytes_that_do_not_match_the_collection
    {
        let (_sk, _payload_dir, packed) = packed_fixture();
        let mut blobs = BTreeMap::new();
        tapp::read(
            tapp::to_bytes(&packed.meta, Some(&packed))
                .unwrap()
                .as_slice(),
            &mut blobs,
        )
        .unwrap();
        let (target, bytes) = blobs.iter_mut().next().unwrap();
        let target = *target;
        bytes[0] ^= 0xff;
        let dest = tempfile::tempdir().unwrap();
        let error = unpack(&packed.meta, &blobs, &dest.path().join("tree")).unwrap_err();
        assert!(
            matches!(error, tilo_app_package::UnpackError::PayloadHashMismatch(_)),
            "{error} ({target:?})"
        );
    }
    // phase: deduplicated_blob_references_cannot_expand_an_app_past_the_materialized_limit
    {
        let (_sk, _payload_dir, packed) = packed_fixture();
        // Past the materialized limit when referenced 2,049 times, while
        // remaining below the independent 4,096-entry ceiling so this
        // specifically exercises the materialized-byte guard.
        let blob_len = tilo_app_package::MAX_APP_UNPACKED_BYTES / 2048;
        let repeated = BlobHash::of(&vec![0x5a; usize::try_from(blob_len).unwrap()]);
        let mut collection = Collection::default();
        for index in 0..=2048 {
            collection
                .entries
                .insert(format!("share/repeated-{index}"), repeated);
        }
        assert!(matches!(
            validate_materialized_payload(&packed.meta.manifest, &collection, |_| Some(blob_len)),
            Err(tilo_app_package::UnpackError::PayloadTooLarge { .. })
        ));
    }
    // phase: empty_files_cannot_expand_an_app_past_the_inode_budget
    {
        let (_sk, _payload_dir, packed) = packed_fixture();
        let empty = BlobHash::of(&[]);
        let mut collection = Collection::default();
        for index in 0..=tilo_app_package::MAX_APP_PAYLOAD_ENTRIES {
            collection
                .entries
                .insert(format!("share/empty-{index}"), empty);
        }
        assert!(matches!(
            validate_materialized_payload(&packed.meta.manifest, &collection, |_| Some(0)),
            Err(tilo_app_package::UnpackError::TooManyPayloadEntries { .. })
        ));
    }
}

#[test]
fn package_signatures_bind_the_hash_chain_and_root() {
    // phase: sign_verify_symmetric
    {
        let (sk_a, _payload_dir, packed) = packed_fixture();

        // Verify against A's pubkey → Trusted.
        let v_a = verify(&packed.meta, &[sk_a.verifying_key().to_bytes()]);
        assert!(matches!(v_a, Verification::Trusted { .. }), "got {v_a:?}");

        // Verify against B's pubkey → UntrustedPublisher (signature is still
        // valid; the publisher just isn't in the trust set).
        let (sk_b, _) = fresh_identity();
        let v_b = verify(&packed.meta, &[sk_b.verifying_key().to_bytes()]);
        assert!(
            matches!(v_b, Verification::UntrustedPublisher { .. }),
            "got {v_b:?}"
        );
    }
    // phase: tampered_manifest_breaks_the_hash_chain
    {
        let (_sk, _payload_dir, packed) = packed_fixture();
        let mut manifest_bytes = packed.meta.manifest_bytes.clone();
        manifest_bytes[10] ^= 0xff;
        let error = PackageMeta::from_parts(
            packed.meta.id,
            &packed.meta.signed_root_bytes(),
            manifest_bytes,
            packed.meta.collection_bytes,
        )
        .unwrap_err();
        assert!(matches!(error, MetaError::ManifestHashMismatch), "{error}");
    }
    // phase: a_root_that_is_not_the_package_id_is_refused
    {
        let (_sk, _payload_dir, packed) = packed_fixture();
        let error = PackageMeta::from_parts(
            tilo_app_package::PackageId::from_bytes([9; 32]),
            &packed.meta.signed_root_bytes(),
            packed.meta.manifest_bytes.clone(),
            packed.meta.collection_bytes,
        )
        .unwrap_err();
        assert!(matches!(error, MetaError::IdMismatch { .. }), "{error}");
    }
    // phase: a_forged_signature_fails_verify
    {
        let (sk, _payload_dir, packed) = packed_fixture();
        let mut root = packed.meta.root.clone();
        let mut signature = root.signature.unwrap();
        signature[0] ^= 0xff;
        root.signature = Some(signature);
        let forged = PackageMeta::from_root(
            root,
            packed.meta.manifest_bytes.clone(),
            packed.meta.collection_bytes,
        )
        .unwrap();
        let v = verify(&forged, &[sk.verifying_key().to_bytes()]);
        assert!(matches!(v, Verification::SignatureMismatch), "got {v:?}");
    }
    // phase: unsigned_root_fails_verify_with_not_signed
    {
        let (sk, _payload_dir, packed) = packed_fixture();
        let unsigned = SignedRoot {
            signature: None,
            ..packed.meta.root
        };
        let meta = PackageMeta::from_root(
            unsigned,
            packed.meta.manifest_bytes.clone(),
            packed.meta.collection_bytes,
        )
        .unwrap();
        let v = verify(&meta, &[sk.verifying_key().to_bytes()]);
        assert!(matches!(v, Verification::NotSigned), "got {v:?}");
    }
}

#[cfg(unix)]
#[test]
fn signal_runtime_loader_executes_after_signed_thin_package_unpack() {
    use std::os::unix::fs::PermissionsExt;
    let (key, public) = fresh_identity();
    let mut manifest = manifest_with_key(&public);
    manifest.package.kind = tilo_app_package::Kind::Runtime;
    manifest.package.runtime = None;
    manifest.package.provides = Some(format!(
        "tilo-runtime/2026.10.1-signal+native.{}",
        "a".repeat(32)
    ));
    let loader = "lib/lib64/ld-linux-x86-64.so.2";
    manifest.entrypoint.as_mut().unwrap().bin = loader.into();
    let payload = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(payload.path().join("lib/lib64")).unwrap();
    std::fs::write(
        payload.path().join(loader),
        b"#!/bin/sh\nprintf 'loader started\\n'\n",
    )
    .unwrap();
    std::fs::write(payload.path().join("lib/java"), b"fixture Java executable").unwrap();
    let packed = pack(
        payload.path(),
        &manifest,
        &SigningIdentity::from_signing_key(key.clone()),
    )
    .unwrap();
    let thin = tapp::to_bytes(&packed.meta, None).unwrap();
    let decoded = tapp::read(thin.as_slice(), &mut BTreeMap::new()).unwrap();
    assert!(matches!(
        verify(&decoded.meta, &[key.verifying_key().to_bytes()]),
        Verification::Trusted { .. }
    ));
    let output = tempfile::tempdir().unwrap();
    let root = output.path().join("runtime");
    unpack(&decoded.meta, &packed, &root).unwrap();
    let process = std::process::Command::new(root.join(loader))
        .output()
        .unwrap();
    assert!(process.status.success());
    assert_eq!(process.stdout, b"loader started\n");
    assert_eq!(
        std::fs::metadata(root.join("lib/java"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
}

#[cfg(unix)]
#[test]
fn thin_runtime_restores_only_its_signed_entrypoint_execute_bits() {
    use std::os::unix::fs::PermissionsExt;
    let (key, public) = fresh_identity();
    let mut manifest = manifest_with_key(&public);
    manifest.package.kind = tilo_app_package::Kind::Runtime;
    manifest.package.runtime = None;
    manifest.package.provides = Some("tilo-runtime/2026.10.2-engine-test".into());
    manifest.entrypoint.as_mut().unwrap().bin = "engine-host".into();
    let payload = tempfile::tempdir().unwrap();
    std::fs::write(
        payload.path().join("engine-host"),
        b"#!/bin/sh\nprintf 'thin engine started\\n'\n",
    )
    .unwrap();
    std::fs::write(payload.path().join("resources.pak"), b"resource").unwrap();
    let identity = SigningIdentity::from_signing_key(key.clone());
    let packed = pack(payload.path(), &manifest, &identity).unwrap();
    assert!(
        !packed
            .meta
            .collection
            .entries
            .contains_key(tilo_app_package::image::TREE_FILE),
        "existing runtime payload format must remain compatible"
    );
    let thin = tapp::to_bytes(&packed.meta, None).unwrap();
    let decoded = tapp::read(thin.as_slice(), &mut BTreeMap::new()).unwrap();
    assert!(!decoded.is_complete());
    assert!(matches!(
        verify(&decoded.meta, &[key.verifying_key().to_bytes()]),
        Verification::Trusted { .. }
    ));
    let output = tempfile::tempdir().unwrap();
    let root = output.path().join("runtime");
    unpack(&decoded.meta, &packed, &root).unwrap();
    assert_eq!(
        std::fs::metadata(root.join("resources.pak"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    let process = std::process::Command::new(root.join("engine-host"))
        .output()
        .unwrap();
    assert!(process.status.success());
    assert_eq!(process.stdout, b"thin engine started\n");

    // Changing which file is executable breaks the signed manifest hash.
    let mut forged = decoded.meta.manifest.clone();
    forged.entrypoint.as_mut().unwrap().bin = "resources.pak".into();
    assert!(matches!(
        PackageMeta::from_root(
            decoded.meta.root,
            forged.to_toml_string().unwrap().into_bytes(),
            decoded.meta.collection_bytes
        ),
        Err(MetaError::ManifestHashMismatch)
    ));

    for path in ["../engine-host", "/engine-host", "missing-host"] {
        manifest.entrypoint.as_mut().unwrap().bin = path.into();
        assert!(matches!(
            pack(payload.path(), &manifest, &identity),
            Err(tilo_app_package::PackError::Manifest(
                tilo_app_package::ManifestError::RuntimeEntrypoint(_)
            ))
        ));
    }
}
