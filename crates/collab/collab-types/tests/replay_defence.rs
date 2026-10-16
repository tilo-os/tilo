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

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Claim,
    Ed25519PublicKey, OpKind, Role, RoleAttestation, Signature, SignedOp, ValidationError,
    generate_keypair, sign_event, sign_op, validate_inbound_op,
};
use ed25519_dalek::SigningKey;

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

fn make_attestation(
    issuer_sk: &SigningKey,
    issuer_pub: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    claims: Vec<Claim>,
) -> RoleAttestation {
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pub,
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
    author_pub: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        format!("seq-{seq}").into_bytes(),
        artifact,
        author_pub,
        attestation_id,
        OpKind::Change,
        t(seq.saturating_add(10)),
        seq,
    );
    sign_op(&mut op, author_sk).unwrap();
    op
}

/// A channel with one owner-signer, one Note artifact and one Editor author.
fn one_author_channel() -> (
    ChannelState,
    SigningKey,
    Ed25519PublicKey,
    ArtifactId,
    AttestationId,
) {
    let (owner_sk, owner_pub) = generate_keypair();
    let (author_sk, author_pub) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let ra = make_attestation(&owner_sk, owner_pub, author_pub, channel, vec![]);
    let aid = ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pub);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);
    (state, author_sk, author_pub, artifact, aid)
}

/// What step 7 should say about one submitted seq.
#[derive(Clone, Copy, Debug)]
enum Expect {
    Accepted,
    Replay { seen: u64, incoming: u64 },
    AnyReplay,
}

/// (case, submissions with what step 7 should say, final seq slot)
type SeqCase = (&'static str, Vec<(u64, Expect)>, u64);

/// Seq is tracked per author: one author's seq must strictly increase on
/// arrival (gaps allowed, back-fills and repeats rejected), while different
/// authors each have their own seq space.
#[test]
fn seq_must_strictly_increase_per_author_but_may_skip() {
    let monotonic: Vec<(u64, Expect)> = (1..=10).map(|n| (n, Expect::Accepted)).collect();
    let cases: [SeqCase; 4] = [
        (
            "same seq twice, second rejected",
            vec![
                (1, Expect::Accepted),
                (
                    1,
                    Expect::Replay {
                        seen: 1,
                        incoming: 1,
                    },
                ),
            ],
            1,
        ),
        (
            "smaller seq rejected",
            vec![
                (100, Expect::Accepted),
                (
                    50,
                    Expect::Replay {
                        seen: 100,
                        incoming: 50,
                    },
                ),
            ],
            100,
        ),
        ("strictly monotonic increment succeeds", monotonic, 10),
        // Holes are fine — out-of-order is reordered by sender; the
        // validator only insists on strict-monotone-on-arrival, not
        // gapless. Then seq=2 (a back-filled gap) is rejected because
        // 2 <= 100.
        (
            "gap in seq is allowed",
            vec![
                (1, Expect::Accepted),
                (100, Expect::Accepted),
                (2, Expect::AnyReplay),
            ],
            100,
        ),
    ];
    for (case, submissions, final_seq) in cases {
        let (mut state, author_sk, author_pub, artifact, aid) = one_author_channel();
        for (seq, expect) in submissions {
            let op = build_and_sign(&author_sk, author_pub, artifact, aid, seq);
            let got = validate_inbound_op(&op, &mut state);
            match expect {
                Expect::Accepted => assert!(got.is_ok(), "case {case}: seq {seq}: {got:?}"),
                Expect::Replay { seen, incoming } => assert_eq!(
                    got,
                    Err(ValidationError::ReplaySeq { seen, incoming }),
                    "case {case}: seq {seq}"
                ),
                Expect::AnyReplay => assert!(
                    matches!(got, Err(ValidationError::ReplaySeq { .. })),
                    "case {case}: seq {seq}: {got:?}"
                ),
            }
        }
        assert_eq!(
            state.last_seq_per_author.get(&author_pub).copied(),
            Some(final_seq),
            "case {case}"
        );
    }

    // Case: different authors share seq space independently. Author A and
    // Author B both submit seq=1. Both must succeed. After both succeed,
    // last_seq_per_author has both keys at 1.
    let (owner_sk, owner_pub) = generate_keypair();
    let (a_sk, a_pub) = generate_keypair();
    let (b_sk, b_pub) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let ra_a = make_attestation(&owner_sk, owner_pub, a_pub, channel, vec![]);
    let ra_b = make_attestation(&owner_sk, owner_pub, b_pub, channel, vec![]);
    let aid_a = ra_a.attestation.id;
    let aid_b = ra_b.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pub);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra_a);
    state.register_attestation(ra_b);

    let op_a = build_and_sign(&a_sk, a_pub, artifact, aid_a, 1);
    let op_b = build_and_sign(&b_sk, b_pub, artifact, aid_b, 1);

    assert!(validate_inbound_op(&op_a, &mut state).is_ok());
    assert!(validate_inbound_op(&op_b, &mut state).is_ok());
    assert_eq!(state.last_seq_per_author.get(&a_pub).copied(), Some(1));
    assert_eq!(state.last_seq_per_author.get(&b_pub).copied(), Some(1));
}
