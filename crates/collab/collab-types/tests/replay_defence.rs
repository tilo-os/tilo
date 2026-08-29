//! Replay defence — focused tests for the step-7 sequence-number check.
//!
//! Replay is the most subtle of the eight checks because seq is
//! tracked per author, not per op or per artifact. These tests
//! exercise the boundary cases:
//!
//! - Re-submitting an op with the same `seq` as a prior successful op
//!   → rejected.
//! - Submitting an op with a smaller `seq` than the last-seen
//!   → rejected.
//! - Different authors maintaining independent seq spaces — author B
//!   can use seq=1 even after author A has used seq=1.

use std::time::{Duration, SystemTime};

use collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Claim,
    Ed25519PublicKey, OpClass, Role, RoleAttestation, Signature, SignedOp, ValidationError,
    generate_keypair, sign_event, sign_op, validate_inbound_op,
};
use ed25519_dalek::SigningKey;

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn make_attestation(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    claims: Vec<Claim>,
) -> RoleAttestation {
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pk,
        subject,
        claims,
        valid_from: t(0),
        valid_until: t(1_000_000),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = att.signing_bytes().unwrap();
    att.signature = sign_event(&sb, issuer_sk).unwrap();
    RoleAttestation {
        channel,
        role: Role::Editor,
        attestation: att,
    }
}

fn build_and_sign(
    author_sk: &SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        format!("seq-{seq}").into_bytes(),
        artifact,
        author_pk,
        attestation_id,
        OpClass::Splice,
        t(10 + seq),
        seq,
    );
    sign_op(&mut op, author_sk).unwrap();
    op
}

#[test]
fn same_seq_twice_second_rejected() {
    let (owner_sk, owner_pk) = generate_keypair();
    let (author_sk, author_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let ra = make_attestation(&owner_sk, owner_pk, author_pk, channel, vec![]);
    let aid = ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    let op1 = build_and_sign(&author_sk, author_pk, artifact, aid, 1);
    let op2 = build_and_sign(&author_sk, author_pk, artifact, aid, 1);

    assert!(validate_inbound_op(&op1, &mut state).is_ok());
    match validate_inbound_op(&op2, &mut state) {
        Err(ValidationError::ReplaySeq { seen, incoming }) => {
            assert_eq!(seen, 1);
            assert_eq!(incoming, 1);
        }
        other => panic!("expected ReplaySeq, got {other:?}"),
    }
}

#[test]
fn smaller_seq_rejected() {
    let (owner_sk, owner_pk) = generate_keypair();
    let (author_sk, author_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let ra = make_attestation(&owner_sk, owner_pk, author_pk, channel, vec![]);
    let aid = ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    let op_high = build_and_sign(&author_sk, author_pk, artifact, aid, 100);
    let op_low = build_and_sign(&author_sk, author_pk, artifact, aid, 50);

    assert!(validate_inbound_op(&op_high, &mut state).is_ok());
    match validate_inbound_op(&op_low, &mut state) {
        Err(ValidationError::ReplaySeq { seen, incoming }) => {
            assert_eq!(seen, 100);
            assert_eq!(incoming, 50);
        }
        other => panic!("expected ReplaySeq, got {other:?}"),
    }
}

#[test]
fn different_authors_share_seq_space_independently() {
    // Author A and Author B both submit seq=1. Both must succeed.
    // After both succeed, last_seq_per_author has both keys at 1.
    let (owner_sk, owner_pk) = generate_keypair();
    let (a_sk, a_pk) = generate_keypair();
    let (b_sk, b_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let ra_a = make_attestation(&owner_sk, owner_pk, a_pk, channel, vec![]);
    let ra_b = make_attestation(&owner_sk, owner_pk, b_pk, channel, vec![]);
    let aid_a = ra_a.attestation.id;
    let aid_b = ra_b.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra_a);
    state.register_attestation(ra_b);

    let op_a = build_and_sign(&a_sk, a_pk, artifact, aid_a, 1);
    let op_b = build_and_sign(&b_sk, b_pk, artifact, aid_b, 1);

    assert!(validate_inbound_op(&op_a, &mut state).is_ok());
    assert!(validate_inbound_op(&op_b, &mut state).is_ok());
    assert_eq!(state.last_seq_per_author.get(&a_pk).copied(), Some(1));
    assert_eq!(state.last_seq_per_author.get(&b_pk).copied(), Some(1));
}

#[test]
fn strictly_monotonic_increment_succeeds() {
    let (owner_sk, owner_pk) = generate_keypair();
    let (author_sk, author_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let ra = make_attestation(&owner_sk, owner_pk, author_pk, channel, vec![]);
    let aid = ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    for n in 1..=10 {
        let op = build_and_sign(&author_sk, author_pk, artifact, aid, n);
        assert!(validate_inbound_op(&op, &mut state).is_ok(), "seq {n}");
    }
    assert_eq!(state.last_seq_per_author.get(&author_pk).copied(), Some(10));
}

#[test]
fn gap_in_seq_is_allowed() {
    // Holes are fine — out-of-order is reordered by sender; the
    // validator only insists on strict-monotone-on-arrival, not
    // gapless.
    let (owner_sk, owner_pk) = generate_keypair();
    let (author_sk, author_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let ra = make_attestation(&owner_sk, owner_pk, author_pk, channel, vec![]);
    let aid = ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    let op_a = build_and_sign(&author_sk, author_pk, artifact, aid, 1);
    let op_b = build_and_sign(&author_sk, author_pk, artifact, aid, 100);
    assert!(validate_inbound_op(&op_a, &mut state).is_ok());
    assert!(validate_inbound_op(&op_b, &mut state).is_ok());
    // Now seq=2 (a back-filled gap) is rejected because 2 <= 100.
    let op_c = build_and_sign(&author_sk, author_pk, artifact, aid, 2);
    assert!(matches!(
        validate_inbound_op(&op_c, &mut state),
        Err(ValidationError::ReplaySeq { .. })
    ));
}
