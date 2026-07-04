//! Single-device (1-of-1 bootstrap) end-to-end tests.
//!
//! Goal: confirm that the bootstrap path produces a working
//! Ed25519 identity whose signatures verify through both
//! `collab_types::verify_event` (the public consumer) and
//! the crate's own `verify_threshold_signature` helper.

use collab_types::{canonical_bytes, verify_event};
use threshold_identity::{ThresholdIdentity, signing::verify_threshold_signature};

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

fn fixed_rng() -> ChaCha20Rng {
    ChaCha20Rng::from_seed([7u8; 32])
}

#[test]
fn bootstrap_initialises_in_single_device_mode() {
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    assert_eq!(id.threshold, 1);
    assert_eq!(id.total_shares, 1);
    assert_eq!(id.this_share_index, 1);
    assert!(id.is_single_device());
}

#[test]
fn bootstrap_produces_distinct_keys_per_run() {
    // Two independent ChaCha seeds → two different group keys.
    let mut rng_a = ChaCha20Rng::from_seed([1u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([2u8; 32]);
    let a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();
    assert_ne!(a.group_public_key, b.group_public_key);
}

#[test]
fn single_device_signs_and_verifies_via_qos_collab_types() {
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let payload: Vec<u8> = b"hello, threshold world".to_vec();
    let sig = id.sign(&payload, &mut rng).unwrap();
    // Headline assertion: the signature verifies through the
    // standard qos-collab-types pathway. No special threshold
    // verifier needed.
    verify_event(&payload, &sig, &id.group_public_key).expect("threshold sig must verify");
}

#[test]
fn single_device_sig_also_passes_frost_verifier() {
    // Sanity check: the FROST library's own verifier accepts
    // the same signature bytes. Confirms FROST is producing
    // standard Ed25519 sigs (not a private variant).
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let payload: Vec<u8> = b"interop sanity".to_vec();
    let sig = id.sign(&payload, &mut rng).unwrap();
    let canon = canonical_bytes(&payload).unwrap();
    verify_threshold_signature(&id.group_public_key, &canon, &sig).unwrap();
}

#[test]
fn tampered_payload_fails_verification() {
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let payload: Vec<u8> = b"approve transaction X".to_vec();
    let sig = id.sign(&payload, &mut rng).unwrap();
    let tampered: Vec<u8> = b"approve transaction Y".to_vec();
    assert!(verify_event(&tampered, &sig, &id.group_public_key).is_err());
}

#[test]
fn bootstrap_single_device_seed_round_trips_via_dalek() {
    // The seed variant produces a dalek-shaped keypair: rebuilding
    // a `SigningKey` from the returned seed derives the returned
    // group public key, and signatures verify against it via the
    // standard Ed25519 verifier. This is the property the on-disk
    // identity record depends on — the frost variant of
    // `bootstrap_single_device` cannot uphold it (frost stores a
    // scalar, dalek `from_bytes` treats input as a seed).
    let mut rng = fixed_rng();
    let (seed, group_pk) = ThresholdIdentity::bootstrap_single_device_seed(&mut rng);
    let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
    let derived_pk = sk.verifying_key().to_bytes();
    assert_eq!(&derived_pk, group_pk.as_bytes());

    use ed25519_dalek::Signer;
    let msg = b"round-trip";
    let sig = sk.sign(msg);
    let vk = ed25519_dalek::VerifyingKey::from_bytes(group_pk.as_bytes()).unwrap();
    use ed25519_dalek::Verifier;
    vk.verify(msg, &sig).expect("dalek verify must succeed");
}

#[test]
fn threshold_signing_calls_error_in_single_device_mode() {
    // The round-1 / round-2 API doesn't apply when the identity
    // is in single-device mode — there is no second signer.
    // Calling them should produce a clear error rather than
    // returning a half-baked partial.
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let err = match id.sign_round1(&mut rng) {
        Ok(_) => panic!("sign_round1 must fail in single-device mode"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("threshold mode"),
        "expected threshold-mode error, got: {msg}"
    );
}
