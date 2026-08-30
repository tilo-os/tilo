//! Trust-chain integration tests for [`pkg::verify_with_trust`]:
//!
//! - Attested package by a trusted attestor → `AttestedBy`.
//! - Attested by an unknown attestor → falls back to
//!   `UntrustedPublisher`.
//! - Revoked publisher → `Revoked` (even with a valid signature).
//! - Scope check: attestor's `prefix:` scope must cover the
//!   package's declared id.
//!
//! Attestation signatures are produced inline with a real
//! Ed25519 key — matches the format `verify_with_trust` walks.
//! The signature covers `(payload_root_hash, publisher_pubkey)`,
//! which we compute cheaply via one throwaway pack pass.

use crate::fixtures;
use ed25519_dalek::{Signer, SigningKey};
use pkg::manifest::{Manifest, TrustAttestation};
use pkg::trust::{
    AttestorScope, RevocationList, RevokedKey, TrustedAttestor, attestation_preimage,
};
use pkg::{
    BlobHash, BlobStore, MemoryBlobStore, PackageId, SignedRoot, SigningIdentity, Verification,
    pack, verify_with_trust,
};
use rand::rngs::OsRng;

fn fresh_key() -> (SigningKey, String) {
    let mut rng = OsRng;
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, pub_hex)
}

fn manifest_with_key(pub_hex: &str) -> Manifest {
    let src = fixtures::render_template(fixtures::SAMPLE_APP_MANIFEST_TEMPLATE, pub_hex);
    Manifest::from_toml_str(&src).expect("decode")
}

/// Sign the domain-separated attestation preimage.
fn attest(
    payload_root_hash: &BlobHash,
    publisher_pubkey: &[u8; 32],
    attestor_sk: &SigningKey,
) -> [u8; 64] {
    let preimage = attestation_preimage(payload_root_hash, publisher_pubkey);
    attestor_sk.sign(&preimage).to_bytes()
}

/// Compute the `payload_root_hash` for a payload directory by doing
/// a throwaway pack. The signature depends only on payload bytes,
/// so it's stable across the final pack that embeds it.
fn payload_root_of(payload_dir: &std::path::Path, publisher_sk: &SigningKey) -> BlobHash {
    let manifest = manifest_with_key(&hex::encode(publisher_sk.verifying_key().to_bytes()));
    let identity = SigningIdentity::from_signing_key(publisher_sk.clone());
    let mut throwaway = MemoryBlobStore::new();
    let id = pack(payload_dir, &manifest, &identity, &mut throwaway).expect("throwaway pack");
    let signed_bytes = throwaway.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    SignedRoot::decode(&signed_bytes).unwrap().payload_root_hash
}

/// Pack with an attestation signature spliced into
/// `[[trust.attested_by]]`. Because the preimage is
/// `(payload_root, publisher_pubkey)` and neither depends on the
/// attestation itself, one pass suffices.
fn pack_with_attestation(
    manifest: &mut Manifest,
    payload_dir: &std::path::Path,
    publisher_sk: &SigningKey,
    attestor_id: &str,
    attestor_sk: &SigningKey,
) -> (PackageId, MemoryBlobStore) {
    let payload_root = payload_root_of(payload_dir, publisher_sk);
    let publisher_pk = publisher_sk.verifying_key().to_bytes();
    let sig = attest(&payload_root, &publisher_pk, attestor_sk);
    manifest
        .trust
        .attested_by
        .retain(|a| a.attestor != attestor_id);
    manifest.trust.attested_by.push(TrustAttestation {
        attestor: attestor_id.into(),
        claim_hex: String::new(),
        signature_hex: hex::encode(sig),
    });

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(publisher_sk.clone());
    let id = pack(payload_dir, manifest, &identity, &mut store).expect("pack");
    (id, store)
}

#[test]
fn attested_by_trusted_attestor_returns_attested_by() {
    let (publisher_sk, publisher_pub_hex) = fresh_key();
    let mut manifest = manifest_with_key(&publisher_pub_hex);
    // Clear the fixture's placeholder claim row.
    manifest.trust.attested_by.clear();

    let (attestor_sk, attestor_pub_hex) = fresh_key();

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let (id, store) = pack_with_attestation(
        &mut manifest,
        payload_dir.path(),
        &publisher_sk,
        "qos-curated/2026.07",
        &attestor_sk,
    );

    let trusted = vec![TrustedAttestor {
        id: "qos-curated/2026.07".into(),
        pubkey_hex: attestor_pub_hex,
        scope: AttestorScope::All,
    }];

    let v = verify_with_trust(id, &store, &[], &trusted, &RevocationList::empty());
    match v {
        Verification::AttestedBy { attestor, .. } => {
            assert_eq!(attestor, "qos-curated/2026.07");
        }
        other => panic!("expected AttestedBy, got {other:?}"),
    }
}

#[test]
fn attested_by_unknown_attestor_falls_back_to_untrusted() {
    let (publisher_sk, publisher_pub_hex) = fresh_key();
    let mut manifest = manifest_with_key(&publisher_pub_hex);
    manifest.trust.attested_by.clear();

    let (attestor_sk, _attestor_pub_hex) = fresh_key();

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let (id, store) = pack_with_attestation(
        &mut manifest,
        payload_dir.path(),
        &publisher_sk,
        "unknown-attestor/1",
        &attestor_sk,
    );

    // Registry is empty — the attestor isn't trusted.
    let v = verify_with_trust(id, &store, &[], &[], &RevocationList::empty());
    match v {
        Verification::UntrustedPublisher { .. } => {}
        other => panic!("expected UntrustedPublisher, got {other:?}"),
    }
}

#[test]
fn revoked_publisher_returns_revoked() {
    let (publisher_sk, publisher_pub_hex) = fresh_key();
    let manifest = manifest_with_key(&publisher_pub_hex);

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(publisher_sk.clone());
    let id = pack(payload_dir.path(), &manifest, &identity, &mut store).expect("pack");

    let revocation = RevocationList {
        revoked: vec![RevokedKey {
            pubkey_hex: publisher_pub_hex,
            reason: "compromised 2026-07-05".into(),
        }],
    };

    let v = verify_with_trust(id, &store, &[], &[], &revocation);
    match v {
        Verification::Revoked { pubkey, reason, .. } => {
            assert_eq!(pubkey, publisher_sk.verifying_key().to_bytes());
            assert_eq!(reason, "compromised 2026-07-05");
        }
        other => panic!("expected Revoked, got {other:?}"),
    }
}

#[test]
fn revoked_publisher_overrides_attestor_trust() {
    // Even if the package is signed by a trusted attestor, a
    // revoked publisher key still refuses. Revocation is the
    // strongest override.
    let (publisher_sk, publisher_pub_hex) = fresh_key();
    let mut manifest = manifest_with_key(&publisher_pub_hex);
    manifest.trust.attested_by.clear();
    let (attestor_sk, attestor_pub_hex) = fresh_key();

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let (id, store) = pack_with_attestation(
        &mut manifest,
        payload_dir.path(),
        &publisher_sk,
        "qos-curated/2026.07",
        &attestor_sk,
    );

    let trusted = vec![TrustedAttestor {
        id: "qos-curated/2026.07".into(),
        pubkey_hex: attestor_pub_hex,
        scope: AttestorScope::All,
    }];
    let revocation = RevocationList {
        revoked: vec![RevokedKey {
            pubkey_hex: publisher_pub_hex,
            reason: "compromised".into(),
        }],
    };

    let v = verify_with_trust(id, &store, &[], &trusted, &revocation);
    assert!(matches!(v, Verification::Revoked { .. }), "got {v:?}");
}

#[test]
fn attestor_scope_mismatch_falls_back_to_untrusted() {
    // Attestor is trusted for `prefix:com.example.` but the
    // package declares id `dev.qos.editor` — scope doesn't cover.
    let (publisher_sk, publisher_pub_hex) = fresh_key();
    let mut manifest = manifest_with_key(&publisher_pub_hex);
    manifest.trust.attested_by.clear();
    let (attestor_sk, attestor_pub_hex) = fresh_key();

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let (id, store) = pack_with_attestation(
        &mut manifest,
        payload_dir.path(),
        &publisher_sk,
        "qos-curated/2026.07",
        &attestor_sk,
    );

    let trusted = vec![TrustedAttestor {
        id: "qos-curated/2026.07".into(),
        pubkey_hex: attestor_pub_hex,
        scope: AttestorScope::Prefix(vec!["com.example.".into()]),
    }];

    let v = verify_with_trust(id, &store, &[], &trusted, &RevocationList::empty());
    assert!(
        matches!(v, Verification::UntrustedPublisher { .. }),
        "got {v:?}"
    );
}

#[test]
fn tampered_attestation_signature_falls_back_to_untrusted() {
    // Registry says the attestor's pubkey is DIFFERENT from the
    // one that actually signed — the sig doesn't verify against
    // the registered key → falls back to UntrustedPublisher.
    let (publisher_sk, publisher_pub_hex) = fresh_key();
    let mut manifest = manifest_with_key(&publisher_pub_hex);
    manifest.trust.attested_by.clear();
    let (attestor_sk, _actual_attestor_pub_hex) = fresh_key();

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let (id, store) = pack_with_attestation(
        &mut manifest,
        payload_dir.path(),
        &publisher_sk,
        "qos-curated/2026.07",
        &attestor_sk,
    );

    let (_, wrong_hex) = fresh_key();
    let trusted = vec![TrustedAttestor {
        id: "qos-curated/2026.07".into(),
        pubkey_hex: wrong_hex,
        scope: AttestorScope::All,
    }];

    let v = verify_with_trust(id, &store, &[], &trusted, &RevocationList::empty());
    assert!(
        matches!(v, Verification::UntrustedPublisher { .. }),
        "got {v:?}"
    );
}

#[test]
fn multiple_attestations_first_trusted_wins() {
    // Manifest has two attestations. Only the second attestor is
    // trusted. verify walks in order, skips the first (not in
    // trust registry), matches the second.
    let (publisher_sk, publisher_pub_hex) = fresh_key();
    let mut manifest = manifest_with_key(&publisher_pub_hex);
    manifest.trust.attested_by.clear();

    let (attestor_a_sk, _) = fresh_key();
    let (attestor_b_sk, attestor_b_pub_hex) = fresh_key();

    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let payload_root = payload_root_of(payload_dir.path(), &publisher_sk);
    let publisher_pk = publisher_sk.verifying_key().to_bytes();

    // Sign both attestations up-front (they don't depend on
    // each other).
    let sig_a = attest(&payload_root, &publisher_pk, &attestor_a_sk);
    let sig_b = attest(&payload_root, &publisher_pk, &attestor_b_sk);
    manifest.trust.attested_by.push(TrustAttestation {
        attestor: "attestor-a/1".into(),
        claim_hex: String::new(),
        signature_hex: hex::encode(sig_a),
    });
    manifest.trust.attested_by.push(TrustAttestation {
        attestor: "attestor-b/1".into(),
        claim_hex: String::new(),
        signature_hex: hex::encode(sig_b),
    });

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(publisher_sk);
    let id = pack(payload_dir.path(), &manifest, &identity, &mut store).expect("pack");

    let trusted = vec![TrustedAttestor {
        id: "attestor-b/1".into(),
        pubkey_hex: attestor_b_pub_hex,
        scope: AttestorScope::All,
    }];
    let v = verify_with_trust(id, &store, &[], &trusted, &RevocationList::empty());
    match v {
        Verification::AttestedBy { attestor, .. } => assert_eq!(attestor, "attestor-b/1"),
        other => panic!("expected AttestedBy(attestor-b/1), got {other:?}"),
    }
}
