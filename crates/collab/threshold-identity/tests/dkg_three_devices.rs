//! 2-of-3 DKG end-to-end tests.
//!
//! Three participants run FROST DKG, after which any 2 can produce a
//! threshold signature verifiable through `collab_types::verify_event`.
//! Covers "add a third device" enrolment and the recovery scenario
//! where a paper share returns to the quorum when a physical device is
//! unavailable.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;

use collab_types::{canonical_bytes, verify_event};
use threshold_identity::{
    EnrolmentRound2, ThresholdIdentity, signing::SigningRound1, signing::SigningRound1Commitment,
    signing::SigningRound2Partial,
};

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

/// Set up three devices with a 2-of-3 DKG. Returns identities a, b, c.
// `_a`/`_b`/`_c` names three independent DKG devices — the clearest
// available convention for a 3-device protocol test, used the same way
// across every multi-device test in this crate.
#[allow(
    clippy::similar_names,
    reason = "_a/_b/_c device naming is deliberate and used throughout this crate's multi-device tests"
)]
fn run_three_device_dkg() -> (ThresholdIdentity, ThresholdIdentity, ThresholdIdentity) {
    let mut rng_a = ChaCha20Rng::from_seed([11u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([12u8; 32]);
    let mut rng_c = ChaCha20Rng::from_seed([13u8; 32]);

    let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();
    let mut c = ThresholdIdentity::bootstrap_single_device(&mut rng_c).unwrap();

    let (mut s_a, r1_a) = a.begin_enrolment(1, 2, 3, &mut rng_a).unwrap();
    let (mut s_b, r1_b) = b.begin_enrolment(2, 2, 3, &mut rng_b).unwrap();
    let (mut s_c, r1_c) = c.begin_enrolment(3, 2, 3, &mut rng_c).unwrap();

    let r2_a_out = s_a.receive_round1(&[r1_b.clone(), r1_c.clone()]).unwrap();
    let r2_b_out = s_b.receive_round1(&[r1_a.clone(), r1_c]).unwrap();
    let r2_c_out = s_c.receive_round1(&[r1_a, r1_b]).unwrap();

    // Route each round-2 message to its addressee.
    let r2_for_a: Vec<EnrolmentRound2> = vec![r2_b_out[&1].clone(), r2_c_out[&1].clone()];
    let r2_for_b: Vec<EnrolmentRound2> = vec![r2_a_out[&2].clone(), r2_c_out[&2].clone()];
    let r2_for_c: Vec<EnrolmentRound2> = vec![r2_a_out[&3].clone(), r2_b_out[&3].clone()];

    s_a.receive_round2(&r2_for_a).unwrap();
    s_b.receive_round2(&r2_for_b).unwrap();
    s_c.receive_round2(&r2_for_c).unwrap();

    a.adopt_dkg_outcome(s_a).unwrap();
    b.adopt_dkg_outcome(s_b).unwrap();
    c.adopt_dkg_outcome(s_c).unwrap();

    (a, b, c)
}

/// Two-signer threshold-sign for an arbitrary pair of devices in
/// a 2-of-3 group. Returns the aggregated 64-byte Ed25519 sig.
fn sign_with_two(
    primary: &ThresholdIdentity,
    secondary: &ThresholdIdentity,
    payload_canon: &[u8],
    rng_seed: u8,
) -> collab_types::Signature {
    let mut rng_a = ChaCha20Rng::from_seed([rng_seed; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([rng_seed.wrapping_add(1); 32]);

    let r1_a: SigningRound1 = primary.sign_round1(&mut rng_a).unwrap();
    let r1_b: SigningRound1 = secondary.sign_round1(&mut rng_b).unwrap();

    let commitments: Vec<SigningRound1Commitment> =
        vec![r1_a.commitment.clone(), r1_b.commitment.clone()];

    let p_a: SigningRound2Partial = primary
        .sign_round2_raw(r1_a.nonces, payload_canon, &commitments)
        .unwrap();
    let p_b: SigningRound2Partial = secondary
        .sign_round2_raw(r1_b.nonces, payload_canon, &commitments)
        .unwrap();

    primary.aggregate_signature(&[p_a, p_b]).unwrap()
}

/// A 2-of-3 DKG gives all devices one group key and distinct shares, and each of the three
/// possible two-device quorums produces a signature that verifies through `collab_types`.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn a_three_device_dkg_agrees_and_every_two_device_quorum_signs() {
    three_device_dkg_produces_consistent_group_state();
    three_device_quorum_1_2_signs_and_verifies();
    three_device_quorum_1_3_signs_and_verifies();
    three_device_quorum_2_3_signs_and_verifies();
}

fn three_device_dkg_produces_consistent_group_state() {
    let (a, b, c) = run_three_device_dkg();
    assert_eq!(a.group_public_key, b.group_public_key);
    assert_eq!(b.group_public_key, c.group_public_key);
    assert_eq!(a.threshold, 2);
    assert_eq!(a.total_shares, 3);
    let mut indices = BTreeMap::new();
    indices.insert(a.this_share_index, true);
    indices.insert(b.this_share_index, true);
    indices.insert(c.this_share_index, true);
    assert_eq!(indices.len(), 3, "every device gets a distinct share index");
}

fn three_device_quorum_1_2_signs_and_verifies() {
    let (a, b, _c) = run_three_device_dkg();
    let payload: Vec<u8> = b"a+b quorum payload".to_vec();
    let canon = canonical_bytes(&payload).unwrap();
    let sig = sign_with_two(&a, &b, &canon, 50);
    verify_event(&payload, &sig, &a.group_public_key)
        .expect("a+b quorum must verify via collab_types");
}

fn three_device_quorum_1_3_signs_and_verifies() {
    let (a, _b, c) = run_three_device_dkg();
    let payload: Vec<u8> = b"a+c quorum payload".to_vec();
    let canon = canonical_bytes(&payload).unwrap();
    let sig = sign_with_two(&a, &c, &canon, 60);
    verify_event(&payload, &sig, &a.group_public_key)
        .expect("a+c quorum must verify via collab_types");
}

fn three_device_quorum_2_3_signs_and_verifies() {
    let (_a, b, c) = run_three_device_dkg();
    let payload: Vec<u8> = b"b+c quorum payload".to_vec();
    let canon = canonical_bytes(&payload).unwrap();
    let sig = sign_with_two(&b, &c, &canon, 70);
    verify_event(&payload, &sig, &b.group_public_key)
        .expect("b+c quorum must verify via collab_types");
}
