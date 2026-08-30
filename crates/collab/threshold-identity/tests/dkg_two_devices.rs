//! 2-of-2 DKG end-to-end tests.
//!
//! Simulates two devices running FROST DKG, then producing a threshold
//! signature that verifies through `collab_types::verify_event` — the
//! canonical first-enrolment flow.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use collab_types::{canonical_bytes, verify_event};
use threshold_identity::{
    EnrolmentRound2, ThresholdIdentity, signing::SigningRound1, signing::SigningRound1Commitment,
    signing::SigningRound2Partial, signing::verify_threshold_signature,
};

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

/// Run a 2-of-2 DKG in-memory. Returns the two device identities.
fn run_two_device_dkg() -> (ThresholdIdentity, ThresholdIdentity) {
    let mut rng_a = ChaCha20Rng::from_seed([1u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([2u8; 32]);

    // Bootstrap device A in single-device mode.
    let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    // Device B starts fresh — in production a factory-default device.
    // For DKG we just need a placeholder `ThresholdIdentity`, overwritten
    // via adopt_dkg_outcome.
    let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();

    // Both devices initiate DKG with new (t=2, n=2): A is share 1, B is share 2.
    let (mut s_a, r1_a) = a.begin_enrolment(1, 2, 2, &mut rng_a).unwrap();
    let (mut s_b, r1_b) = b.begin_enrolment(2, 2, 2, &mut rng_b).unwrap();

    // Round-1: each device receives the other's commitment.
    let r2_a_out = s_a.receive_round1(std::slice::from_ref(&r1_b)).unwrap();
    let r2_b_out = s_b.receive_round1(std::slice::from_ref(&r1_a)).unwrap();

    // Round-2: route each message to its addressee — A sends to B
    // (index 2), B sends to A (index 1).
    let r2_for_a: Vec<EnrolmentRound2> = vec![r2_b_out[&1].clone()];
    let r2_for_b: Vec<EnrolmentRound2> = vec![r2_a_out[&2].clone()];

    s_a.receive_round2(&r2_for_a).unwrap();
    s_b.receive_round2(&r2_for_b).unwrap();

    // Adopt outcomes — replaces each device's old key material.
    a.adopt_dkg_outcome(s_a).unwrap();
    b.adopt_dkg_outcome(s_b).unwrap();

    (a, b)
}

/// Drive a 2-of-2 threshold-sign across both devices.
fn threshold_sign(
    a: &ThresholdIdentity,
    b: &ThresholdIdentity,
    payload_bytes: &[u8],
) -> collab_types::Signature {
    let mut rng_a = ChaCha20Rng::from_seed([10u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([20u8; 32]);

    let r1_a: SigningRound1 = a.sign_round1(&mut rng_a).unwrap();
    let r1_b: SigningRound1 = b.sign_round1(&mut rng_b).unwrap();

    let commitments: Vec<SigningRound1Commitment> =
        vec![r1_a.commitment.clone(), r1_b.commitment.clone()];

    let p_a: SigningRound2Partial = a
        .sign_round2_raw(r1_a.nonces, payload_bytes, &commitments)
        .unwrap();
    let p_b: SigningRound2Partial = b
        .sign_round2_raw(r1_b.nonces, payload_bytes, &commitments)
        .unwrap();

    a.aggregate_signature(&[p_a, p_b]).unwrap()
}

#[test]
fn dkg_produces_matching_group_public_key_on_both_devices() {
    let (a, b) = run_two_device_dkg();
    assert_eq!(a.group_public_key, b.group_public_key);
    assert_eq!(a.threshold, 2);
    assert_eq!(b.threshold, 2);
    assert_eq!(a.total_shares, 2);
    assert_eq!(b.total_shares, 2);
    assert_eq!(a.this_share_index, 1);
    assert_eq!(b.this_share_index, 2);
    assert!(!a.is_single_device());
    assert!(!b.is_single_device());
}

#[test]
fn dkg_rotates_group_public_key_away_from_bootstrap() {
    // The 1-of-1 bootstrap key MUST differ from the 2-of-2 DKG key —
    // this rotation is accepted by the design.
    let mut rng_a = ChaCha20Rng::from_seed([1u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([2u8; 32]);
    let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let pre_dkg_key = a.group_public_key;
    let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();

    let (mut s_a, r1_a) = a.begin_enrolment(1, 2, 2, &mut rng_a).unwrap();
    let (mut s_b, r1_b) = b.begin_enrolment(2, 2, 2, &mut rng_b).unwrap();
    let r2_a_out = s_a.receive_round1(&[r1_b]).unwrap();
    let r2_b_out = s_b.receive_round1(&[r1_a]).unwrap();
    s_a.receive_round2(&[r2_b_out[&1].clone()]).unwrap();
    s_b.receive_round2(&[r2_a_out[&2].clone()]).unwrap();
    a.adopt_dkg_outcome(s_a).unwrap();
    b.adopt_dkg_outcome(s_b).unwrap();

    assert_ne!(a.group_public_key, pre_dkg_key);
}

#[test]
fn two_device_threshold_sign_verifies_via_qos_collab_types() {
    // HEADLINE INTEROP TEST.
    let (a, b) = run_two_device_dkg();
    let payload: Vec<u8> = b"approve cluster invite".to_vec();
    let canon = canonical_bytes(&payload).unwrap();
    let sig = threshold_sign(&a, &b, &canon);

    // Verify via qos-collab-types' Ed25519 path — the verifier the rest
    // of the system uses. If this passes, the threshold sig is
    // byte-compatible with ed25519-dalek.
    verify_event(&payload, &sig, &a.group_public_key)
        .expect("threshold-produced sig must verify via collab_types::verify_event");
}

#[test]
fn two_device_threshold_sign_also_passes_frost_verifier() {
    let (a, b) = run_two_device_dkg();
    let payload: Vec<u8> = b"some message".to_vec();
    let canon = canonical_bytes(&payload).unwrap();
    let sig = threshold_sign(&a, &b, &canon);
    verify_threshold_signature(&a.group_public_key, &canon, &sig).unwrap();
}

#[test]
fn single_partial_cannot_aggregate_below_threshold() {
    // To produce a partial at all FROST needs commitments from every
    // signer in the round-2 quorum (else round-2 fails with "Incorrect
    // number of commitments"). So drive both A and B through round-2
    // with the same commitment set, but aggregate using only A's
    // partial — the aggregator must reject `threshold - 1` as insufficient.
    let (a, b) = run_two_device_dkg();
    let mut rng_a = ChaCha20Rng::from_seed([3u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([4u8; 32]);
    let payload: Vec<u8> = b"only one device".to_vec();
    let canon = canonical_bytes(&payload).unwrap();

    let r1_a = a.sign_round1(&mut rng_a).unwrap();
    let r1_b = b.sign_round1(&mut rng_b).unwrap();
    let commitments = vec![r1_a.commitment.clone(), r1_b.commitment.clone()];
    let p_a = a
        .sign_round2_raw(r1_a.nonces, &canon, &commitments)
        .unwrap();
    // Drop B's partial — pretend it never made it back.
    let _drop_b_partial = b
        .sign_round2_raw(r1_b.nonces, &canon, &commitments)
        .unwrap();

    let err = a
        .aggregate_signature(&[p_a])
        .expect_err("must fail below threshold");
    let msg = err.to_string();
    assert!(
        msg.contains("not enough partial signatures") || msg.contains("InsufficientPartials"),
        "expected insufficient-partials error, got {msg}"
    );
}

#[test]
fn deterministic_dkg_for_fixed_seeds() {
    // Two independent runs with the same seeds must produce the same
    // group public key — useful for replay-based and reproducible CI tests.
    let (a1, _b1) = run_two_device_dkg();
    let (a2, _b2) = run_two_device_dkg();
    assert_eq!(a1.group_public_key, a2.group_public_key);
}
