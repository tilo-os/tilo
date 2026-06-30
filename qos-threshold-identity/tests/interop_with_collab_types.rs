//! HEADLINE INTEROP — threshold sigs ↔ `qos-collab-types`.
//!
//! This is the test the whole crate was built for: a signature
//! produced by collaborative threshold signing across multiple
//! devices must verify byte-for-byte through the standard
//! Ed25519 verification path in `qos_collab_types`, with no
//! special "threshold mode" needed.
//!
//! If any test here fails, the report back to the orchestrator
//! must call out the precise verifier path that rejected the
//! signature — silently shipping a quietly-broken signing path
//! is much worse than no signing path at all.

use std::time::SystemTime;

use qos_collab_types::{
    ArtifactId, ArtifactKind, ChannelEvent, ChannelId, ChannelState, Ed25519PublicKey, Identity,
    OpClass, Role, SignedOp, Signature, SpaceId, canonical_bytes, verify_event,
};
use qos_threshold_identity::{
    EnrolmentRound2, ThresholdIdentity, signing::SigningRound1, signing::SigningRound1Commitment,
    signing::SigningRound2Partial,
};

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

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

fn threshold_sign(
    a: &ThresholdIdentity,
    b: &ThresholdIdentity,
    canon: &[u8],
) -> Signature {
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

#[test]
fn threshold_sig_verifies_via_qos_collab_types_verify_event_on_bytes() {
    let (a, b) = dkg_two();
    let value: Vec<u8> = b"hello threshold".to_vec();
    let canon = canonical_bytes(&value).unwrap();
    let sig = threshold_sign(&a, &b, &canon);
    verify_event(&value, &sig, &a.group_public_key)
        .expect("HEADLINE: threshold sig must verify via verify_event");
}

#[test]
fn threshold_sig_verifies_over_channel_event_genesis() {
    // A more realistic semantic test: sign a `ChannelEvent::Genesis`
    // with the threshold identity and verify through the same path
    // the broker uses. The signed bytes flow through the variant's
    // `signing_bytes()` helper so we exercise the same encoder as
    // the production signing path in qos-collab-types.
    let (a, b) = dkg_two();
    let mut genesis = ChannelEvent::Genesis {
        channel_id: ChannelId::new(),
        space: SpaceId(uuid::Uuid::new_v4()),
        name: "engineering".into(),
        owner: a.group_public_key,
        created_at: SystemTime::UNIX_EPOCH,
        signature: Signature::from_bytes([0u8; 64]),
    };
    // `qos_collab_types`'s convention for verifying an event:
    //   bytes = event.signing_bytes();
    //   verify_event(&bytes, &signature, &public_key);
    // and `verify_event` itself canonicalises its first argument
    // again — so the signed pre-image is `canonical_bytes(&bytes)`,
    // i.e. double-canonicalisation. Threshold-sign over that.
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
    // Now re-verify via verify_event, the same way the validator
    // would. `verify_event` consumes the signing-bytes view, not
    // the full enum, because the signature does not cover its own
    // signature field.
    if let ChannelEvent::Genesis {
        ref signature, ..
    } = genesis
    {
        let bytes = genesis.signing_bytes().unwrap();
        verify_event(&bytes, signature, &a.group_public_key)
            .expect("threshold-signed Genesis must verify via verify_event");
    }
}

#[test]
fn threshold_signed_op_passes_inbound_validator() {
    // The grand-finale test: build a SignedOp signed via threshold,
    // run it through the full eight-step inbound-op validator,
    // and confirm validation succeeds. This proves the threshold
    // signature is treated as a regular Ed25519 sig by every
    // production verification path.
    use std::collections::HashMap;

    use qos_collab_types::{Attestation, AttestationId, Claim, RoleAttestation, generate_keypair};

    let (a, b) = dkg_two();

    // The author of the SignedOp is the *group* threshold
    // identity. Its attestation, however, must be signed by an
    // authorised signer (the channel owner). For this test the
    // channel owner is a separate single-device key — the
    // typical case where the channel was created by a different
    // identity than the one whose ops are being validated.
    let (owner_sk, owner_pk) = generate_keypair();
    let channel_id = ChannelId::new();
    let space = SpaceId(uuid::Uuid::new_v4());

    // Build a RoleAttestation: the channel owner attests that
    // the threshold identity (`a.group_public_key`) has the
    // Editor role on this channel.
    let now = SystemTime::UNIX_EPOCH;
    let later = now + std::time::Duration::from_secs(60 * 60 * 24 * 365);
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
    let att_sig = qos_collab_types::sign_event(&sig_bytes, &owner_sk).unwrap();
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

    // We don't enforce clearance here — leave channel_min_clearance
    // as None. The validator should pass all 8 steps including
    // the cryptographic signature check on the threshold sig.
    qos_collab_types::validate_inbound_op(&op, &mut state)
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
