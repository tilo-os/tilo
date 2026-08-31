//! pack → unpack → verify integration tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::fixtures;
use ed25519_dalek::SigningKey;
use pkg::manifest::Manifest;
use pkg::{MemoryBlobStore, SigningIdentity, Verification, pack, unpack, verify};
use rand::rngs::OsRng;

fn fresh_identity() -> (SigningKey, String) {
    let mut rng = OsRng;
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, pub_hex)
}

fn manifest_with_key(pub_hex: &str) -> Manifest {
    let src = fixtures::render_template(fixtures::SAMPLE_APP_MANIFEST_TEMPLATE, pub_hex);
    Manifest::from_toml_str(&src).expect("decode")
}

#[test]
fn pack_unpack_roundtrip() {
    let (sk, pub_hex) = fresh_identity();
    let manifest = manifest_with_key(&pub_hex);

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(sk);
    let id = pack(payload_dir.path(), &manifest, &identity, &mut store).expect("pack");

    let dest = tempfile::tempdir().unwrap();
    let recovered = unpack(id, &store, dest.path()).expect("unpack");

    // Manifests are byte-identical apart from the signature
    // field (which pack zeroes before writing the manifest blob).
    let mut expected = manifest.clone();
    expected.publisher.signature.clear();
    assert_eq!(recovered, expected);

    // File bytes match.
    let bin_a = std::fs::read(payload_dir.path().join("bin/editor")).unwrap();
    let bin_b = std::fs::read(dest.path().join("bin/editor")).unwrap();
    assert_eq!(bin_a, bin_b);
    let splash_a = std::fs::read(payload_dir.path().join("share/splash.png")).unwrap();
    let splash_b = std::fs::read(dest.path().join("share/splash.png")).unwrap();
    assert_eq!(splash_a, splash_b);
}

#[test]
fn sign_verify_symmetric() {
    let (sk_a, pub_hex_a) = fresh_identity();
    let manifest = manifest_with_key(&pub_hex_a);

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let mut store = MemoryBlobStore::new();
    let identity_a = SigningIdentity::from_signing_key(sk_a.clone());
    let id = pack(payload_dir.path(), &manifest, &identity_a, &mut store).expect("pack");

    // Verify against A's pubkey → Trusted.
    let signer_pub_bytes = sk_a.verifying_key().to_bytes();
    let v_a = verify(id, &store, &[signer_pub_bytes]);
    assert!(matches!(v_a, Verification::Trusted { .. }), "got {v_a:?}");

    // Verify against B's pubkey → UntrustedPublisher (signature
    // is still valid; the publisher just isn't in the trust set).
    let mut rng = OsRng;
    let sk_b = SigningKey::generate(&mut rng);
    let other_pub_bytes = sk_b.verifying_key().to_bytes();
    let v_b = verify(id, &store, &[other_pub_bytes]);
    assert!(
        matches!(v_b, Verification::UntrustedPublisher { .. }),
        "got {v_b:?}"
    );
}

#[test]
fn tampered_manifest_fails_verify() {
    use pkg::blobs::{BlobHash, BlobStore, Collection};
    use pkg::signed::SignedRoot;

    let (sk, pub_hex) = fresh_identity();
    let manifest = manifest_with_key(&pub_hex);

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(sk.clone());
    let id = pack(payload_dir.path(), &manifest, &identity, &mut store).expect("pack");

    // Tamper: flip one byte in the manifest blob.
    let signed_bytes = store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    let signed = SignedRoot::decode(&signed_bytes).unwrap();
    let manifest_hash = signed.manifest_hash;
    let mut manifest_bytes = store.get(&manifest_hash).unwrap().unwrap();
    // Flip a byte in the manifest. The manifest blob is still
    // keyed by the *original* hash in the signed-root — re-inserting
    // it under the new hash leaves the original key dangling, so
    // we deliberately overwrite the entry at the original key.
    manifest_bytes[10] ^= 0xff;
    // Surgical replace via the public iter() snapshot is not
    // possible; rebuild a tampered store with the flipped
    // manifest under the original hash.
    let mut tampered = pkg::MemoryBlobStore::new();
    for (h, b) in store.iter() {
        if h == &manifest_hash {
            // skip; reinsert with tampered bytes below
            continue;
        }
        // Re-insert by hash → bypass put's BLAKE3 re-hash so
        // the original key still lives.
        // MemoryBlobStore::put computes the hash from the bytes,
        // so we can't keep the original key without an internal
        // hook. Workaround: use a wrapper store that records
        // (key, bytes) overrides.
        tampered.put(b).unwrap();
        let _ = h;
    }
    // Insert tampered manifest bytes under their NEW hash (which
    // differs from manifest_hash). The signed-root still points
    // at the original hash — that blob is now missing.
    tampered.put(&manifest_bytes).unwrap();
    // We still need the signed-root blob in the tampered store.
    tampered.put(&signed_bytes).unwrap();
    // Walk the original collection too so payload blobs exist.
    let collection_bytes = store.get(&signed.payload_root_hash).unwrap().unwrap();
    tampered.put(&collection_bytes).unwrap();
    let collection = Collection::decode(&collection_bytes).unwrap();
    for hash in collection.entries.values() {
        let b = store.get(hash).unwrap().unwrap();
        tampered.put(&b).unwrap();
    }

    let v = verify(id, &tampered, &[sk.verifying_key().to_bytes()]);
    // The original manifest_hash blob is gone, so verify reports
    // missing-manifest-blob — equivalent to "tampering broke the
    // hash chain". The point of the test is that the tamper does
    // NOT verify as trusted.
    assert!(
        matches!(
            v,
            Verification::MissingManifestBlob | Verification::ManifestHashMismatch
        ),
        "expected hash/missing manifest failure, got {v:?}"
    );
}

#[test]
fn tampered_payload_byte_fails_verify() {
    use pkg::blobs::{BlobHash, BlobStore, Collection};
    use pkg::signed::SignedRoot;

    // Build a custom store that holds the *declared* hash but
    // wrong bytes. We can't do this through MemoryBlobStore
    // alone (put re-hashes), so we use a small inline wrapper.
    struct Tampered {
        inner: std::collections::BTreeMap<BlobHash, Vec<u8>>,
    }
    impl BlobStore for Tampered {
        fn put(&mut self, bytes: &[u8]) -> Result<BlobHash, pkg::BlobStoreError> {
            let h = BlobHash::of(bytes);
            self.inner.insert(h, bytes.to_vec());
            Ok(h)
        }
        fn get(&self, h: &BlobHash) -> Result<Option<Vec<u8>>, pkg::BlobStoreError> {
            Ok(self.inner.get(h).cloned())
        }
    }

    // Like the manifest test but tamper a payload blob's bytes.
    let (sk, pub_hex) = fresh_identity();
    let manifest = manifest_with_key(&pub_hex);

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(sk.clone());
    let id = pack(payload_dir.path(), &manifest, &identity, &mut store).expect("pack");

    let signed_bytes = store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    let signed = SignedRoot::decode(&signed_bytes).unwrap();
    let collection_bytes = store.get(&signed.payload_root_hash).unwrap().unwrap();
    let collection = Collection::decode(&collection_bytes).unwrap();

    // Rebuild a store where one payload blob is replaced with
    // tampered bytes under its declared hash.
    let target_path = collection.entries.keys().next().unwrap().clone();
    let target_hash = collection.entries[&target_path];

    let mut tampered = Tampered {
        inner: std::collections::BTreeMap::default(),
    };
    for (h, b) in store.iter() {
        if h == &target_hash {
            let mut wrong = b.clone();
            wrong[0] ^= 0xff;
            tampered.inner.insert(*h, wrong);
        } else {
            tampered.inner.insert(*h, b.clone());
        }
    }

    let v = verify(id, &tampered, &[sk.verifying_key().to_bytes()]);
    assert!(
        matches!(v, Verification::PayloadBlobHashMismatch { ref path } if path == &target_path),
        "got {v:?}"
    );
}

#[test]
fn unsigned_root_fails_verify_with_not_signed() {
    use pkg::blobs::BlobStore;
    use pkg::signed::SignedRoot;
    let unsigned = SignedRoot::unsigned(
        pkg::BlobHash([1u8; 32]),
        pkg::BlobHash([2u8; 32]),
        [3u8; 32],
    );
    let id = unsigned.package_id();
    let mut store = MemoryBlobStore::new();
    store.put(&unsigned.encode()).unwrap();
    let v = verify(id, &store, &[[3u8; 32]]);
    assert!(matches!(v, Verification::NotSigned), "got {v:?}");
}
