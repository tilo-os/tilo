//! Trust-chain integration tests for [`tilo_app_package::verify_with_trust`]:
//!
//! - Attested package by a trusted attestor → `AttestedBy`.
//! - Attested by an unknown attestor → falls back to
//!   `UntrustedPublisher`.
//! - Revoked publisher → `Revoked` (even with a valid signature).
//! - Scope check: attestor's `prefix:` scope must cover the
//!   package's declared id.
//!
//! Attestation signatures are produced inline with an Ed25519 key, matching
//! the format `verify_with_trust` walks.
//! The signature covers `(payload_root_hash, publisher_pubkey)`,
//! which we compute via a preliminary pack pass.

use crate::fixtures;
use ed25519_dalek::{Signer, SigningKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::manifest::{Manifest, TrustAttestation};
use tilo_app_package::trust::{
    AttestorScope, RevocationList, RevokedKey, TrustedAttestor, attestation_preimage,
};
use tilo_app_package::{BlobHash, Packed, SigningIdentity, Verification, pack, verify_with_trust};

fn fresh_key() -> (SigningKey, String) {
    let mut rng = UnwrapErr(SysRng);
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

/// Pack a fresh package with the sample payload, attested by `attestors`.
/// A throwaway pack gives the payload root the attestation signs; the
/// signature depends only on the payload bytes, so one pass suffices.
fn pack_attested(attestors: &[(&str, &SigningKey)]) -> (SigningKey, String, Packed) {
    let (publisher_sk, publisher_hex) = fresh_key();
    let mut manifest = manifest_with_key(&publisher_hex);
    manifest.trust.attested_by.clear();
    let payload_dir = tempfile::tempdir().unwrap();
    fixtures::write_app_payload(payload_dir.path()).unwrap();

    let identity = SigningIdentity::from_signing_key(publisher_sk.clone());
    let payload_root = pack(payload_dir.path(), &manifest, &identity)
        .expect("throwaway pack")
        .meta
        .root
        .payload_root_hash;
    let publisher_pub = publisher_sk.verifying_key().to_bytes();
    for (id, attestor_sk) in attestors {
        manifest.trust.attested_by.push(TrustAttestation {
            attestor: (*id).into(),
            claim_hex: String::new(),
            signature_hex: hex::encode(attest(&payload_root, &publisher_pub, attestor_sk)),
        });
    }
    let packed = pack(payload_dir.path(), &manifest, &identity).expect("pack");
    (publisher_sk, publisher_hex, packed)
}

fn trusted_attestor(id: &str, pubkey_hex: String, scope: AttestorScope) -> TrustedAttestor {
    TrustedAttestor {
        id: id.into(),
        pubkey_hex,
        scope,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled case per trust verdict"
)]
fn attestation_verdicts_resolve_trust_revocation_and_scope() {
    // A trusted attestor's signature opens the package.
    {
        let (attestor_sk, attestor_pub) = fresh_key();
        let (_, _, packed) = pack_attested(&[("tilo-curated/2026.07", &attestor_sk)]);
        let trusted = [trusted_attestor(
            "tilo-curated/2026.07",
            attestor_pub,
            AttestorScope::All,
        )];
        match verify_with_trust(&packed.meta, &[], &trusted, &RevocationList::default()) {
            Verification::AttestedBy { attestor, .. } => {
                assert_eq!(attestor, "tilo-curated/2026.07");
            }
            other => panic!("expected AttestedBy, got {other:?}"),
        }
    }
    // An attestor the caller does not trust vouches for nothing.
    {
        let (attestor_sk, _) = fresh_key();
        let (_, _, packed) = pack_attested(&[("unknown-attestor/1", &attestor_sk)]);
        let v = verify_with_trust(&packed.meta, &[], &[], &RevocationList::default());
        assert!(
            matches!(v, Verification::UntrustedPublisher { .. }),
            "got {v:?}"
        );
    }
    // A revoked publisher is refused even with a valid signature, and the
    // verdict names the key and the reason.
    {
        let (publisher_sk, publisher_hex, packed) = pack_attested(&[]);
        let revocation = RevocationList {
            revoked: vec![RevokedKey {
                pubkey_hex: publisher_hex,
                reason: "compromised 2026-07-05".into(),
            }],
        };
        match verify_with_trust(&packed.meta, &[], &[], &revocation) {
            Verification::Revoked { pubkey, reason, .. } => {
                assert_eq!(pubkey, publisher_sk.verifying_key().to_bytes());
                assert_eq!(reason, "compromised 2026-07-05");
            }
            other => panic!("expected Revoked, got {other:?}"),
        }
    }
    // Revocation is the strongest override: a trusted attestor does not lift it.
    {
        let (attestor_sk, attestor_pub) = fresh_key();
        let (_, publisher_hex, packed) = pack_attested(&[("tilo-curated/2026.07", &attestor_sk)]);
        let trusted = [trusted_attestor(
            "tilo-curated/2026.07",
            attestor_pub,
            AttestorScope::All,
        )];
        let revocation = RevocationList {
            revoked: vec![RevokedKey {
                pubkey_hex: publisher_hex,
                reason: "compromised".into(),
            }],
        };
        let v = verify_with_trust(&packed.meta, &[], &trusted, &revocation);
        assert!(matches!(v, Verification::Revoked { .. }), "got {v:?}");
    }
    // The attestor's scope must cover the package's declared id.
    {
        let (attestor_sk, attestor_pub) = fresh_key();
        let (_, _, packed) = pack_attested(&[("tilo-curated/2026.07", &attestor_sk)]);
        let trusted = [trusted_attestor(
            "tilo-curated/2026.07",
            attestor_pub,
            AttestorScope::Prefix(vec!["com.example.".into()]),
        )];
        let v = verify_with_trust(&packed.meta, &[], &trusted, &RevocationList::default());
        assert!(
            matches!(v, Verification::UntrustedPublisher { .. }),
            "got {v:?}"
        );
    }
    // The registry's key for that id is not the one that signed: no trust.
    {
        let (attestor_sk, _) = fresh_key();
        let (_, wrong_hex) = fresh_key();
        let (_, _, packed) = pack_attested(&[("tilo-curated/2026.07", &attestor_sk)]);
        let trusted = [trusted_attestor(
            "tilo-curated/2026.07",
            wrong_hex,
            AttestorScope::All,
        )];
        let v = verify_with_trust(&packed.meta, &[], &trusted, &RevocationList::default());
        assert!(
            matches!(v, Verification::UntrustedPublisher { .. }),
            "got {v:?}"
        );
    }
    // Two attestations, only the second trusted: verify walks in order and
    // matches the second.
    {
        let (first_sk, _) = fresh_key();
        let (second_sk, second_pub) = fresh_key();
        let (_, _, packed) =
            pack_attested(&[("attestor-a/1", &first_sk), ("attestor-b/1", &second_sk)]);
        let trusted = [trusted_attestor(
            "attestor-b/1",
            second_pub,
            AttestorScope::All,
        )];
        match verify_with_trust(&packed.meta, &[], &trusted, &RevocationList::default()) {
            Verification::AttestedBy { attestor, .. } => assert_eq!(attestor, "attestor-b/1"),
            other => panic!("expected AttestedBy(attestor-b/1), got {other:?}"),
        }
    }
}
