//! Replay defence — focused tests for the per-author sequence check.
//!
//! Replay is the most subtle of the checks because seq is tracked per author,
//! not per op or per artifact. These tests exercise the boundary cases:
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
    ArtifactId, ArtifactKind, AttestationId, DEFAULT_WORKSPACE, Ed25519PublicKey, InboundState,
    OpKind, Role, SignedOp, ValidationError, generate_keypair, sign_op,
    validate_replayed_inbound_op,
};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Authority, Grant, Grantee, Party, roles};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

fn build_and_sign(
    author_sk: &SigningKey,
    author_pub: Ed25519PublicKey,
    artifact: ArtifactId,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        format!("seq-{seq}").into_bytes(),
        artifact,
        author_pub,
        AttestationId::new(),
        OpKind::Change,
        t(seq.saturating_add(10)),
        seq,
    );
    sign_op(&mut op, author_sk).unwrap();
    op
}

/// One Note artifact in the default workspace.
fn one_author_state() -> (InboundState, ArtifactId) {
    let artifact = ArtifactId::new();
    let mut state = InboundState::default();
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.artifact_apps.insert(artifact, "so.tilo.notes".into());
    state.artifact_records.insert(artifact, "page".into());
    (state, artifact)
}

fn presets() -> Vec<Grant> {
    roles::workspace_role_grants(DEFAULT_WORKSPACE, &Grantee::role(Role::Owner))
}

fn editor(grants: &[Grant]) -> Authority<'_> {
    Authority::person(Party::new(vec![Grantee::role(Role::Editor)], grants))
}

/// Validate as history a host replays: age never refuses an op, only its seq.
fn replay(
    op: &SignedOp,
    state: &mut InboundState,
    grants: &[Grant],
) -> Result<(), ValidationError> {
    validate_replayed_inbound_op(op, &editor(grants), state, t(2_000_000), Duration::ZERO)
}

/// What the sequence check should say about one submitted seq.
#[derive(Clone, Copy, Debug)]
enum Expect {
    Accepted,
    Replay { seen: u64, incoming: u64 },
    AnyReplay,
}

/// (case, submissions with what the check should say, final seq slot)
type SeqCase = (&'static str, Vec<(u64, Expect)>, u64);

/// Seq is tracked per author: one author's seq must strictly increase on
/// arrival (gaps allowed, back-fills and repeats rejected), while different
/// authors each have their own seq space.
#[test]
fn seq_must_strictly_increase_per_author_but_may_skip() {
    let g = presets();
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
        let (mut state, artifact) = one_author_state();
        let (author_sk, author_pub) = generate_keypair();
        for (seq, expect) in submissions {
            let op = build_and_sign(&author_sk, author_pub, artifact, seq);
            let got = replay(&op, &mut state, &g);
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
    let (mut state, artifact) = one_author_state();
    let (a_sk, a_pub) = generate_keypair();
    let (b_sk, b_pub) = generate_keypair();

    let op_a = build_and_sign(&a_sk, a_pub, artifact, 1);
    let op_b = build_and_sign(&b_sk, b_pub, artifact, 1);

    assert!(replay(&op_a, &mut state, &g).is_ok());
    assert!(replay(&op_b, &mut state, &g).is_ok());
    assert_eq!(state.last_seq_per_author.get(&a_pub).copied(), Some(1));
    assert_eq!(state.last_seq_per_author.get(&b_pub).copied(), Some(1));
}
