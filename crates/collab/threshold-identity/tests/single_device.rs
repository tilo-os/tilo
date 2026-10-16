//! Single-device (1-of-1 bootstrap) end-to-end tests.
//!
//! Confirms the bootstrap path produces a working Ed25519 identity whose
//! signatures verify through both `collab_types::verify_event` (the
//! public consumer) and the crate's own `verify_threshold_signature` helper.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use collab_types::{canonical_bytes, verify_event};
use threshold_identity::{ThresholdIdentity, signing::verify_threshold_signature};

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

fn fixed_rng() -> ChaCha20Rng {
    ChaCha20Rng::from_seed([7u8; 32])
}

/// A bootstrapped identity is 1-of-1, distinct per seed, signs standard Ed25519 that both
/// verifiers accept and a tampered payload fails, and refuses the threshold signing rounds.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn a_bootstrapped_single_device_identity_is_a_fresh_1_of_1_standard_ed25519_signer() {
    bootstrap_initialises_in_single_device_mode();
    bootstrap_produces_distinct_keys_per_run();
    single_device_signs_and_verifies_via_tilo_collab_types();
    single_device_sig_also_passes_frost_verifier();
    tampered_payload_fails_verification();
    threshold_signing_calls_error_in_single_device_mode();
}

fn bootstrap_initialises_in_single_device_mode() {
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    assert_eq!(id.threshold, 1);
    assert_eq!(id.total_shares, 1);
    assert_eq!(id.this_share_index, 1);
    assert!(id.is_single_device());
}

fn bootstrap_produces_distinct_keys_per_run() {
    // Two independent ChaCha seeds → two different group keys.
    let mut rng_a = ChaCha20Rng::from_seed([1u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([2u8; 32]);
    let a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();
    assert_ne!(a.group_public_key, b.group_public_key);
}

fn single_device_signs_and_verifies_via_tilo_collab_types() {
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let payload: Vec<u8> = b"hello, threshold world".to_vec();
    let sig = id.sign(&payload, &mut rng).unwrap();
    // Headline assertion: the signature verifies through the standard
    // tilo-collab-types pathway — no special threshold verifier needed.
    verify_event(&payload, &sig, &id.group_public_key).expect("threshold sig must verify");
}

fn single_device_sig_also_passes_frost_verifier() {
    // Sanity: FROST's own verifier accepts the same signature bytes,
    // confirming FROST produces standard Ed25519 sigs, not a private variant.
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let payload: Vec<u8> = b"interop sanity".to_vec();
    let sig = id.sign(&payload, &mut rng).unwrap();
    let canon = canonical_bytes(&payload).unwrap();
    verify_threshold_signature(&id.group_public_key, &canon, &sig).unwrap();
}

fn tampered_payload_fails_verification() {
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let payload: Vec<u8> = b"approve transaction X".to_vec();
    let sig = id.sign(&payload, &mut rng).unwrap();
    let tampered: Vec<u8> = b"approve transaction Y".to_vec();
    assert!(verify_event(&tampered, &sig, &id.group_public_key).is_err());
}

/// The exported seed is a dalek seed that rebuilds the same signing identity; a threshold
/// identity exports none.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn the_single_device_seed_is_the_dalek_seed_that_rebuilds_the_identity() {
    bootstrap_single_device_seed_round_trips_via_dalek();
    single_device_seed_reconstructs_the_same_identity();
    single_device_seed_is_the_dalek_seed_the_disk_record_stores();
    threshold_mode_exports_no_single_device_seed();
}

fn bootstrap_single_device_seed_round_trips_via_dalek() {
    // The seed variant produces a dalek-shaped keypair: rebuilding a
    // `SigningKey` from the returned seed derives the same group public
    // key, verifiable via the standard Ed25519 verifier. The frost
    // variant of `bootstrap_single_device` cannot uphold this (frost
    // stores a scalar; dalek `from_bytes` treats input as a seed).
    use ed25519_dalek::Signer;
    use ed25519_dalek::Verifier;

    let mut rng = fixed_rng();
    let (seed, group_pk) = ThresholdIdentity::bootstrap_single_device_seed(&mut rng);
    let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
    let derived_pk = sk.verifying_key().to_bytes();
    assert_eq!(&derived_pk, group_pk.as_bytes());

    let msg = b"round-trip";
    let sig = sk.sign(msg);
    let vk = ed25519_dalek::VerifyingKey::from_bytes(group_pk.as_bytes()).unwrap();
    vk.verify(msg, &sig).expect("dalek verify must succeed");
}

fn single_device_seed_reconstructs_the_same_identity() {
    // The persistence round-trip the on-disk record depends on: export
    // the seed, rebuild from it, and the rebuilt identity must carry the
    // same public key and produce verifying signatures.
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let seed = id
        .single_device_seed()
        .expect("1-of-1 identity exports a seed");

    let back = ThresholdIdentity::from_single_device_seed(&seed);
    assert_eq!(back.group_public_key, id.group_public_key);
    assert!(back.is_single_device());

    let payload: Vec<u8> = b"reconstructed signer".to_vec();
    let sig = back.sign(&payload, &mut rng).unwrap();
    verify_event(&payload, &sig, &id.group_public_key)
        .expect("a rebuilt identity must sign under the original public key");
}

fn single_device_seed_is_the_dalek_seed_the_disk_record_stores() {
    use ed25519_dalek::Signer;

    // The persisted bytes must be the bytes `SigningKey::from_bytes`
    // expands back into the *same* key — the property a frost signing
    // scalar cannot satisfy, and whose absence made ceremony-minted
    // identities unable to sign for themselves.
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let seed = id.single_device_seed().unwrap();

    let dalek = ed25519_dalek::SigningKey::from_bytes(&seed);
    assert_eq!(
        &dalek.verifying_key().to_bytes(),
        id.group_public_key.as_bytes(),
        "the exported seed must derive the identity's own public key"
    );

    // …and a signature made by the dalek-side key verifies through
    // the identity's public key, which is what every app that loads
    // the record does.
    let payload: Vec<u8> = b"disk round-trip".to_vec();
    let canon = canonical_bytes(&payload).unwrap();
    let sig = collab_types::Signature::from_bytes(dalek.sign(&canon).to_bytes());
    verify_event(&payload, &sig, &id.group_public_key).expect("dalek-signed op must verify");
}

// `_a`/`_b` names two independent DKG devices throughout this test —
// the clearest available convention for a 2-device protocol test, used
// the same way across every multi-device test in this crate.
#[allow(
    clippy::similar_names,
    reason = "_a/_b device-pair naming is deliberate and used throughout this crate's multi-device tests"
)]
fn threshold_mode_exports_no_single_device_seed() {
    // A threshold identity has no whole secret to hand out — the
    // accessor must say so rather than inventing bytes.
    let mut rng_a = ChaCha20Rng::from_seed([21u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([22u8; 32]);
    let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();

    let (mut s_a, r1_a) = a.begin_enrolment(1, 2, 2, &mut rng_a).unwrap();
    let (mut s_b, r1_b) = b.begin_enrolment(2, 2, 2, &mut rng_b).unwrap();
    let r2_a_out = s_a.receive_round1(std::slice::from_ref(&r1_b)).unwrap();
    let r2_b_out = s_b.receive_round1(std::slice::from_ref(&r1_a)).unwrap();
    s_a.receive_round2(&[r2_b_out[&1].clone()]).unwrap();
    s_b.receive_round2(&[r2_a_out[&2].clone()]).unwrap();
    a.adopt_dkg_outcome(s_a).unwrap();
    b.adopt_dkg_outcome(s_b).unwrap();

    assert!(!a.is_single_device());
    assert_eq!(a.single_device_seed(), None);
    assert_eq!(b.single_device_seed(), None);
}

fn threshold_signing_calls_error_in_single_device_mode() {
    // The round-1/round-2 API doesn't apply in single-device mode (no
    // second signer) — calling it should error clearly, not return a
    // half-baked partial.
    let mut rng = fixed_rng();
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let Err(err) = id.sign_round1(&mut rng) else {
        panic!("sign_round1 must fail in single-device mode");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("threshold mode"),
        "expected threshold-mode error, got: {msg}"
    );
}
