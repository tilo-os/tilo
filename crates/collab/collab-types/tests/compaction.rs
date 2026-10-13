//! Op-log compaction + scrollback integration tests.
//!
//! Exercises [`ChannelState::record_validated_op`],
//! [`ChannelState::apply_compaction`], and
//! [`ChannelState::scrollback_for`] against the validator tests' fixture
//! shape. Live in `tests/` (not unit tests) because they cover the
//! validator's *public* API.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::SystemTime;

use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, ChannelState, CompactionCheckpoint, CompactionError,
    Ed25519PublicKey, OpKind, Role, RoleAttestation, SignedOp, encode_compaction_payload,
    generate_keypair, hash_op_prefix, sign_event, sign_op,
};
use ed25519_dalek::SigningKey;

/// Reuse the validator-test convention: a minimal channel state with one
/// owner, one Editor author, one Note artifact, pre-registered signer.
fn minimal_state() -> (
    ChannelState,
    SigningKey,
    Ed25519PublicKey,
    ArtifactId,
    AttestationId,
) {
    let (owner_sk, owner_pub) = generate_keypair();
    let (author_sk, author_pub) = generate_keypair();
    let artifact = ArtifactId::new();

    // Mint + sign the Editor attestation against the owner.
    let mut att = collab_types::Attestation {
        id: AttestationId::new(),
        issued_by: owner_pub,
        subject: author_pub,
        claims: vec![],
        valid_from: SystemTime::UNIX_EPOCH,
        valid_until: SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(100_000))
            .expect("valid_until in range"),
        signature: collab_types::Signature::from_bytes([0u8; 64]),
    };
    let bytes = att.signing_bytes().expect("signing_bytes");
    att.signature = sign_event(&bytes, &owner_sk).expect("sign attestation");
    let ra = RoleAttestation {
        channel: collab_types::ChannelId::new(),
        role: Role::Editor,
        attestation: att.clone(),
    };
    let att_id = att.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pub);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    (state, author_sk, author_pub, artifact, att_id)
}

fn make_signed_op(
    sk: &SigningKey,
    author: Ed25519PublicKey,
    artifact: ArtifactId,
    att_id: AttestationId,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        format!("payload-{seq}").into_bytes(),
        artifact,
        author,
        att_id,
        OpKind::Change,
        SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(seq))
            .expect("timestamp in range"),
        seq,
    );
    sign_op(&mut op, sk).expect("sign");
    op
}

/// A minimal state whose op log holds `artifact`'s ops with seqs `seqs`.
fn state_with_ops(
    seqs: std::ops::RangeInclusive<u64>,
) -> (
    ChannelState,
    SigningKey,
    Ed25519PublicKey,
    ArtifactId,
    AttestationId,
) {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    for seq in seqs {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    (state, sk, author, artifact, att_id)
}

/// Adds a second artifact B and interleaves ops for both artifacts: A gets
/// seqs 1..=3, B gets 101..=103.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test fixture: seq offsets on small fixed values cannot overflow"
)]
fn interleave_second_artifact(
    state: &mut ChannelState,
    sk: &SigningKey,
    author: Ed25519PublicKey,
    artifact_a: ArtifactId,
    att_id: AttestationId,
) -> ArtifactId {
    let artifact_b = ArtifactId::new();
    state.artifacts.insert(artifact_b, ArtifactKind::Note);
    for seq in 1..=3 {
        state.record_validated_op(make_signed_op(sk, author, artifact_a, att_id, seq));
        state.record_validated_op(make_signed_op(sk, author, artifact_b, att_id, seq + 100));
    }
    artifact_b
}

/// A checkpoint over `artifact`'s logged ops up to `up_to_seq`, with the
/// correct prefix hash.
fn checkpoint(state: &ChannelState, artifact: ArtifactId, up_to_seq: u64) -> CompactionCheckpoint {
    let prefix: Vec<SignedOp> = state
        .op_log
        .iter()
        .filter(|op| op.artifact == artifact && op.seq <= up_to_seq)
        .cloned()
        .collect();
    CompactionCheckpoint {
        artifact,
        up_to_seq,
        prefix_hash: hash_op_prefix(&prefix).expect("hash"),
    }
}

fn seqs_of<'a>(ops: impl IntoIterator<Item = &'a SignedOp>) -> Vec<u64> {
    ops.into_iter().map(|op| op.seq).collect()
}

// ---------- record_validated_op ----------

/// `record_validated_op` keeps arrival order and evicts FIFO at the bound,
/// which defaults to `DEFAULT_MAX_IN_MEMORY_OPS`.
#[test]
fn the_op_log_keeps_order_and_evicts_the_oldest_at_its_bound() {
    // Case: record_validated_op preserves order.
    let (state, ..) = state_with_ops(1..=5);
    assert_eq!(seqs_of(&state.op_log), vec![1, 2, 3, 4, 5]);

    // Case: the op log evicts the oldest at capacity. FIFO eviction: oldest
    // two (1, 2) dropped, log holds 3, 4, 5.
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    state.max_in_memory_ops = 3;
    for seq in 1..=5 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    assert_eq!(seqs_of(&state.op_log), vec![3, 4, 5]);

    // Case: ChannelState::default() must use DEFAULT_MAX_IN_MEMORY_OPS so a
    // fresh state has a sensible bound — surfaced as a test so the
    // constant + Default impl stay in sync.
    let state = ChannelState::default();
    assert_eq!(
        state.max_in_memory_ops,
        collab_types::validator::DEFAULT_MAX_IN_MEMORY_OPS
    );
    assert!(state.op_log.is_empty());
}

// ---------- apply_compaction ----------

/// A checkpoint evicts exactly the hash-verified prefix of its own artifact
/// (an empty prefix evicts nothing), a wrong hash changes nothing, and the
/// checkpoint round-trips through a Compaction op's payload.
#[test]
fn compaction_evicts_exactly_a_verified_prefix_of_one_artifact() {
    // Case: apply_compaction evicts the matching prefix (seqs 1..=3); the
    // remaining log holds seqs 4, 5.
    let (mut state, _, _, artifact, _) = state_with_ops(1..=5);
    let cp = checkpoint(&state, artifact, 3);
    let evicted = state.apply_compaction(&cp).expect("apply");
    assert_eq!(evicted, 3);
    assert_eq!(seqs_of(&state.op_log), vec![4, 5]);

    // Case: apply_compaction rejects a wrong hash; the log is unchanged.
    let (mut state, _, _, artifact, _) = state_with_ops(1..=5);
    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 3,
        prefix_hash: [0xFF; 32], // wrong on purpose
    };
    match state.apply_compaction(&cp) {
        Err(CompactionError::PrefixHashMismatch) => {}
        other => panic!("expected PrefixHashMismatch, got {other:?}"),
    }
    assert_eq!(state.op_log.len(), 5);

    // Case: apply_compaction only touches the matching artifact. The log
    // holds ops for both artifacts; the checkpoint covers artifact A up to
    // seq 3; artifact B's ops are untouched.
    let (mut state, sk, author, artifact_a, att_id) = minimal_state();
    let artifact_b = interleave_second_artifact(&mut state, &sk, author, artifact_a, att_id);
    let cp = checkpoint(&state, artifact_a, 3);
    let evicted = state.apply_compaction(&cp).expect("apply");
    assert_eq!(evicted, 3);
    let b_left = state
        .op_log
        .iter()
        .filter(|op| op.artifact == artifact_b)
        .count();
    assert_eq!(b_left, 3);

    // Case: an empty prefix succeeds. A checkpoint covering up_to_seq=0 (the
    // first a channel might issue before any mutation) must succeed with
    // zero evictions, not error.
    let (mut state, _, _, artifact, _) = state_with_ops(5..=5);
    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 0,
        prefix_hash: hash_op_prefix(&[]).expect("empty hash"),
    };
    let evicted = state.apply_compaction(&cp).expect("apply");
    assert_eq!(evicted, 0);
    assert_eq!(state.op_log.len(), 1);

    // Case: parse_compaction_payload round trip. The op's `artifact` should
    // match the checkpoint's `artifact`; a Compaction op should be
    // Owner-authored (not validated here — just round-tripping payload bytes
    // through `parse_compaction_payload`).
    let cp = CompactionCheckpoint {
        artifact: ArtifactId::new(),
        up_to_seq: 42,
        prefix_hash: [0xCD; 32],
    };
    let bytes = encode_compaction_payload(&cp).expect("encode");
    let (_, sk, author, _artifact, att_id) = minimal_state();
    let mut op = SignedOp::new_unsigned(
        bytes,
        cp.artifact,
        author,
        att_id,
        OpKind::Compaction,
        SystemTime::UNIX_EPOCH,
        99,
    );
    sign_op(&mut op, &sk).expect("sign");
    let parsed = collab_types::parse_compaction_payload(&op).expect("parse");
    assert_eq!(parsed, cp);
}

// ---------- scrollback_for ----------

/// (case, ops logged, `before_seq`, max, expected seqs, expected exhaustion when
/// the case pins it, why)
type ScrollCase = (
    &'static str,
    u64,
    u64,
    u16,
    Vec<u64>,
    Option<bool>,
    &'static str,
);

/// Scrollback pages backwards from `before_seq`, returns at most `max` of
/// the most recent ops in seq order, filters by artifact, and reports
/// exhaustion — including after the prefix was compacted away.
#[test]
fn scrollback_pages_backwards_per_artifact_until_exhausted() {
    // (case, ops logged, before_seq, max, expected seqs, expected exhausted
    // or None when the case does not pin it, why)
    let cases: [ScrollCase; 3] = [
        (
            "returns recent ops in seq order",
            5,
            6,
            10,
            vec![1, 2, 3, 4, 5],
            Some(true),
            "all 5 ops returned; nothing left in the log",
        ),
        (
            // Caller is paging backwards from the present → most recent 3.
            "caps at max returning tail",
            10,
            11,
            3,
            vec![8, 9, 10],
            Some(false),
            "10 ops > max of 3 → caller needs to page further; not exhausted",
        ),
        (
            // before_seq = 3 → seqs strictly less than 3.
            "respects before_seq bound",
            5,
            3,
            10,
            vec![1, 2],
            None,
            "",
        ),
    ];
    for (case, logged, before_seq, max, want, want_exhausted, why) in cases {
        let (state, _, _, artifact, _) = state_with_ops(1..=logged);
        let (ops, exhausted) = state.scrollback_for(artifact, before_seq, max);
        assert_eq!(seqs_of(&ops), want, "case {case}");
        if let Some(want_exhausted) = want_exhausted {
            assert_eq!(exhausted, want_exhausted, "case {case}: {why}");
        }
    }

    // Case: the artifact filter — only artifact A's ops are returned.
    let (mut state, sk, author, artifact_a, att_id) = minimal_state();
    interleave_second_artifact(&mut state, &sk, author, artifact_a, att_id);
    let (ops, _) = state.scrollback_for(artifact_a, 1000, 10);
    assert_eq!(seqs_of(&ops), vec![1, 2, 3], "only artifact A ops returned");

    // Case: exhausted after compaction. Compact seqs 1..=3 away; asking for
    // seqs < 4 → returns empty + exhausted.
    let (mut state, _, _, artifact, _) = state_with_ops(1..=5);
    let cp = checkpoint(&state, artifact, 3);
    state.apply_compaction(&cp).expect("apply");
    let (ops, exhausted) = state.scrollback_for(artifact, 4, 10);
    assert!(ops.is_empty(), "compacted prefix has been evicted");
    assert!(exhausted, "the requested range is fully exhausted locally");
}
