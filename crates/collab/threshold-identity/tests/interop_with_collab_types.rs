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
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::SystemTime;

use collab_types::{
    ArtifactId, ArtifactKind, ChannelEvent, ChannelId, ChannelState, Ed25519PublicKey, Identity,
    OpClass, Role, Signature, SignedOp, SpaceId, canonical_bytes, verify_event,
};
use threshold_identity::{
    EnrolmentRound2, ThresholdIdentity, signing::SigningRound1, signing::SigningRound1Commitment,
    signing::SigningRound2Partial,
};

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

/// A threshold signature verifies on raw bytes, over a channel genesis event, and through the
/// full inbound `SignedOp` validator.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn threshold_signatures_pass_every_collab_types_verification_path() {
    threshold_sig_verifies_via_tilo_collab_types_verify_event_on_bytes();
    threshold_sig_verifies_over_channel_event_genesis();
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

fn threshold_sig_verifies_over_channel_event_genesis() {
    // A more realistic test: sign a `ChannelEvent::Genesis` with the
    // threshold identity and verify through the broker's own path, via
    // the variant's `signing_bytes()` helper.
    let (a, b) = dkg_two();
    let mut genesis = ChannelEvent::Genesis {
        channel_id: ChannelId::new(),
        space: SpaceId(uuid::Uuid::new_v4()),
        name: "engineering".into(),
        owner: a.group_public_key,
        created_at: SystemTime::UNIX_EPOCH,
        signature: Signature::from_bytes([0u8; 64]),
    };
    // collab_types verifies an event as bytes = event.signing_bytes();
    // verify_event(&bytes, &signature, &public_key) — and verify_event
    // canonicalises its argument again, so the pre-image is
    // canonical_bytes(&bytes). Threshold-sign over that.
    let signing_bytes = genesis.signing_bytes().unwrap();
    let double_canon = canonical_bytes(&signing_bytes).unwrap();
    let sig = threshold_sign(&a, &b, &double_canon);
    // Splice the signature into the event.
    if let ChannelEvent::Genesis {
        signature: ref mut s,
        ..
    } = genesis
    {
        *s = sig;
    }
    // Re-verify via verify_event, the way the validator would — it
    // consumes the signing-bytes view, not the full enum, since the
    // signature doesn't cover its own field.
    if let ChannelEvent::Genesis { ref signature, .. } = genesis {
        let bytes = genesis.signing_bytes().unwrap();
        verify_event(&bytes, signature, &a.group_public_key)
            .expect("threshold-signed Genesis must verify via verify_event");
    }
}

// `owner_sk`/`owner_pk` follows this crate's established signing-key /
// public-key naming convention; see the reason on `dkg_two`.
#[allow(
    clippy::similar_names,
    reason = "the _sk/_pk keypair naming convention is deliberate and used throughout this crate's tests"
)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "a phase of a test: clock offsets are fixed small durations"
)]
fn threshold_signed_op_passes_inbound_validator() {
    // The grand-finale test: build a SignedOp signed via threshold, run
    // it through the full eight-step validator, and confirm it passes —
    // proving threshold signatures are treated as regular Ed25519 by
    // every production path.
    use std::collections::HashMap;

    use collab_types::{Attestation, AttestationId, Claim, RoleAttestation, generate_keypair};

    let (a, b) = dkg_two();

    // The SignedOp's author is the *group* threshold identity, but its
    // attestation must be signed by an authorised signer (the channel
    // owner) — a separate single-device key, the typical case where the
    // channel creator differs from the identity whose ops are validated.
    let (owner_sk, owner_pk) = generate_keypair();
    let channel_id = ChannelId::new();
    let space = SpaceId(uuid::Uuid::new_v4());

    // Build a RoleAttestation: the channel owner attests that
    // the threshold identity (`a.group_public_key`) has the
    // Editor role on this channel.
    let now = SystemTime::UNIX_EPOCH;
    let later = now + std::time::Duration::from_secs(31_536_000);
    let attestation_id = AttestationId::new();
    let mut attestation = Attestation {
        id: attestation_id,
        issued_by: owner_pk,
        subject: a.group_public_key,
        claims: vec![Claim::MemberOf { space }],
        valid_from: now,
        valid_until: later,
        signature: Signature::from_bytes([0u8; 64]),
    };
    // Sign the attestation with the owner's key.
    let sig_bytes = attestation.signing_bytes().unwrap();
    let att_sig = collab_types::sign_event(&sig_bytes, &owner_sk).unwrap();
    attestation.signature = att_sig;

    // Build the SignedOp.
    let artifact = ArtifactId::new();
    let mut op = SignedOp::new_unsigned(
        b"some-automerge-change".to_vec(),
        artifact,
        a.group_public_key,
        attestation_id,
        OpClass::Splice,
        now + std::time::Duration::from_secs(60),
        1,
    );
    // Sign the op via threshold.
    let op_signing_bytes = op.signing_bytes().unwrap();
    let sig = threshold_sign(&a, &b, &op_signing_bytes);
    op.signature = sig;

    // Assemble a ChannelState and run the validator.
    let mut state = ChannelState::default();
    state.attestations.insert(
        attestation_id,
        RoleAttestation {
            channel: channel_id,
            role: Role::Editor,
            attestation,
        },
    );
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);

    // channel_min_clearance stays None — the validator should pass all
    // 8 steps including the threshold signature check.
    collab_types::validate_inbound_op(&op, &mut state)
        .expect("HEADLINE: threshold-signed SignedOp must pass validate_inbound_op");

    // Suppress unused warning for `b` — already consumed by
    // threshold_sign.
    let _ = &b;
    // `Identity` import is for symmetry with the spec example.
    let _ = Identity {
        public_key: a.group_public_key,
        label: "threshold-identity".into(),
        avatar: None,
        attestations: vec![],
    };
    // Suppress unused warning on the helpers map.
    let _: HashMap<Ed25519PublicKey, Vec<Attestation>> = state.identity_attestations.clone();
}
