//! Threshold-signature interop with `collab-types`.
//!
//! A signature produced by
//! collaborative threshold signing across multiple devices must verify
//! byte-for-byte through the standard Ed25519 path in `collab_types`,
//! with no special "threshold mode" needed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::SystemTime;

use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, DEFAULT_WORKSPACE, InboundState, OpKind, Role,
    Signature, SignedOp, canonical_bytes, validate_admitted_inbound_op, verify_event,
};
use threshold_identity::{
    EnrolmentRound2, ThresholdIdentity, signing::SigningRound1, signing::SigningRound1Commitment,
    signing::SigningRound2Partial,
};
use tilo_action_policy::{Authority, Grant, Grantee, Party, roles};

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

// `_a`/`_b` names two independent DKG devices — the clearest available
// convention for a 2-device protocol test, used the same way across
// every multi-device test in this crate.
#[allow(
    clippy::similar_names,
    reason = "_a/_b device-pair naming is deliberate and used throughout this crate's multi-device tests"
)]
fn dkg_two() -> (ThresholdIdentity, ThresholdIdentity) {
    let mut rng_a = ChaCha20Rng::from_seed([31u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([32u8; 32]);
    let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();
    let (mut s_a, r1_a) = a.begin_enrolment(1, 2, 2, &mut rng_a).unwrap();
    let (mut s_b, r1_b) = b.begin_enrolment(2, 2, 2, &mut rng_b).unwrap();
    let r2_a_out = s_a.receive_round1(&[r1_b]).unwrap();
    let r2_b_out = s_b.receive_round1(&[r1_a]).unwrap();
    let r2_for_a: Vec<EnrolmentRound2> = vec![r2_b_out[&1].clone()];
    let r2_for_b: Vec<EnrolmentRound2> = vec![r2_a_out[&2].clone()];
    s_a.receive_round2(&r2_for_a).unwrap();
    s_b.receive_round2(&r2_for_b).unwrap();
    a.adopt_dkg_outcome(s_a).unwrap();
    b.adopt_dkg_outcome(s_b).unwrap();
    (a, b)
}

fn threshold_sign(a: &ThresholdIdentity, b: &ThresholdIdentity, canon: &[u8]) -> Signature {
    let mut rng_a = ChaCha20Rng::from_seed([40u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([41u8; 32]);
    let r1_a: SigningRound1 = a.sign_round1(&mut rng_a).unwrap();
    let r1_b: SigningRound1 = b.sign_round1(&mut rng_b).unwrap();
    let commitments: Vec<SigningRound1Commitment> =
        vec![r1_a.commitment.clone(), r1_b.commitment.clone()];
    let p_a: SigningRound2Partial = a.sign_round2_raw(r1_a.nonces, canon, &commitments).unwrap();
    let p_b: SigningRound2Partial = b.sign_round2_raw(r1_b.nonces, canon, &commitments).unwrap();
    a.aggregate_signature(&[p_a, p_b]).unwrap()
}

/// A threshold signature verifies on raw bytes and through the inbound
/// `SignedOp` validator.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn threshold_signatures_pass_every_collab_types_verification_path() {
    threshold_sig_verifies_via_tilo_collab_types_verify_event_on_bytes();
    threshold_signed_op_passes_inbound_validator();
}

fn threshold_sig_verifies_via_tilo_collab_types_verify_event_on_bytes() {
    let (a, b) = dkg_two();
    let value: Vec<u8> = b"hello threshold".to_vec();
    let canon = canonical_bytes(&value).unwrap();
    let sig = threshold_sign(&a, &b, &canon);
    verify_event(&value, &sig, &a.group_public_key)
        .expect("HEADLINE: threshold sig must verify via verify_event");
}

/// A `SignedOp` the threshold identity authored passes the inbound validator
/// the production document path runs.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "a phase of a test: clock offsets are fixed small durations"
)]
fn threshold_signed_op_passes_inbound_validator() {
    let (a, b) = dkg_two();
    let now = SystemTime::UNIX_EPOCH;
    let artifact = ArtifactId::new();
    let mut op = SignedOp::new_unsigned(
        b"some-automerge-change".to_vec(),
        artifact,
        a.group_public_key,
        AttestationId::new(),
        OpKind::Change,
        now + std::time::Duration::from_secs(60),
        1,
    );
    // Sign the op via threshold.
    let op_signing_bytes = op.signing_bytes().unwrap();
    op.signature = threshold_sign(&a, &b, &op_signing_bytes);

    // The admitted path: a certified device join under the workspace's live
    // roster, and a Note artifact the validator knows.
    let mut state = InboundState::default();
    state.artifacts.insert(artifact, ArtifactKind::Note);
    let grants: Vec<Grant> =
        roles::workspace_role_grants(DEFAULT_WORKSPACE, &Grantee::role(Role::Owner));
    let author = Authority::person(Party::new(vec![Grantee::role(Role::Editor)], &grants));
    validate_admitted_inbound_op(
        &op,
        &author,
        &mut state,
        op.timestamp,
        std::time::Duration::from_secs(300),
    )
    .expect("HEADLINE: a threshold-signed SignedOp must pass the inbound validator");
}
