//! Op-log compaction + scrollback integration tests.
//!
//! Exercises [`ChannelState::record_validated_op`],
//! [`ChannelState::apply_compaction`], and
//! [`ChannelState::scrollback_for`] against the validator tests' fixture
//! shape. Live in `tests/` (not unit tests) because they cover the
//! validator's *public* API.

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
    ArtifactId, ArtifactKind, AttestationId, ChannelState, CompactionCheckpoint, CompactionError,
    Ed25519PublicKey, OpClass, Role, RoleAttestation, SignedOp, encode_compaction_payload,
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
    let (owner_sk, owner_pk) = generate_keypair();
    let (author_sk, author_pk) = generate_keypair();
    let artifact = ArtifactId::new();

    // Mint + sign the Editor attestation against the owner.
    let mut att = collab_types::Attestation {
        id: AttestationId::new(),
        issued_by: owner_pk,
        subject: author_pk,
        claims: vec![],
        valid_from: SystemTime::UNIX_EPOCH,
        valid_until: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100_000),
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
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    (state, author_sk, author_pk, artifact, att_id)
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
        OpClass::Splice,
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seq),
        seq,
    );
    sign_op(&mut op, sk).expect("sign");
    op
}

// ---------- record_validated_op ----------

#[test]
fn record_validated_op_preserves_order() {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    for seq in 1..=5 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    let seqs: Vec<u64> = state.op_log.iter().map(|op| op.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
}

#[test]
fn op_log_evicts_oldest_at_capacity() {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    state.max_in_memory_ops = 3;
    for seq in 1..=5 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    let seqs: Vec<u64> = state.op_log.iter().map(|op| op.seq).collect();
    // FIFO eviction: oldest two (1, 2) dropped, log holds 3, 4, 5.
    assert_eq!(seqs, vec![3, 4, 5]);
}

#[test]
fn default_max_in_memory_ops_matches_constant() {
    // ChannelState::default() must use DEFAULT_MAX_IN_MEMORY_OPS so a
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

#[test]
fn apply_compaction_evicts_matching_prefix() {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    for seq in 1..=5 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    // Build a checkpoint covering seqs 1..=3.
    let prefix: Vec<SignedOp> = state
        .op_log
        .iter()
        .filter(|op| op.seq <= 3)
        .cloned()
        .collect();
    let prefix_hash = hash_op_prefix(&prefix).expect("hash");
    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 3,
        prefix_hash,
    };

    let evicted = state.apply_compaction(&cp).expect("apply");
    assert_eq!(evicted, 3);
    // The remaining log holds seqs 4, 5.
    let seqs: Vec<u64> = state.op_log.iter().map(|op| op.seq).collect();
    assert_eq!(seqs, vec![4, 5]);
}

#[test]
fn apply_compaction_rejects_wrong_hash() {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    for seq in 1..=5 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 3,
        prefix_hash: [0xFF; 32], // wrong on purpose
    };
    match state.apply_compaction(&cp) {
        Err(CompactionError::PrefixHashMismatch) => {}
        other => panic!("expected PrefixHashMismatch, got {other:?}"),
    }
    // Log unchanged.
    assert_eq!(state.op_log.len(), 5);
}

#[test]
fn apply_compaction_only_touches_matching_artifact() {
    let (mut state, sk, author, artifact_a, att_id) = minimal_state();
    let artifact_b = ArtifactId::new();
    state.artifacts.insert(artifact_b, ArtifactKind::Note);

    // Log holds ops for both artifacts.
    for seq in 1..=3 {
        state.record_validated_op(make_signed_op(&sk, author, artifact_a, att_id, seq));
        state.record_validated_op(make_signed_op(&sk, author, artifact_b, att_id, seq + 100));
    }
    // Checkpoint covers artifact A up to seq 3.
    let prefix_a: Vec<SignedOp> = state
        .op_log
        .iter()
        .filter(|op| op.artifact == artifact_a && op.seq <= 3)
        .cloned()
        .collect();
    let cp = CompactionCheckpoint {
        artifact: artifact_a,
        up_to_seq: 3,
        prefix_hash: hash_op_prefix(&prefix_a).expect("hash"),
    };
    let evicted = state.apply_compaction(&cp).expect("apply");
    assert_eq!(evicted, 3);
    // Artifact B's ops are untouched.
    let b_left = state
        .op_log
        .iter()
        .filter(|op| op.artifact == artifact_b)
        .count();
    assert_eq!(b_left, 3);
}

#[test]
fn apply_compaction_empty_prefix_succeeds() {
    // A checkpoint covering up_to_seq=0 (the first a channel might issue
    // before any mutation) must succeed with zero evictions, not error.
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, 5));

    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 0,
        prefix_hash: hash_op_prefix(&[]).expect("empty hash"),
    };
    let evicted = state.apply_compaction(&cp).expect("apply");
    assert_eq!(evicted, 0);
    assert_eq!(state.op_log.len(), 1);
}

// ---------- scrollback_for ----------

#[test]
fn scrollback_for_returns_recent_ops_in_seq_order() {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    for seq in 1..=5 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    let (ops, exhausted) = state.scrollback_for(artifact, 6, 10);
    let seqs: Vec<u64> = ops.iter().map(|op| op.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    assert!(exhausted, "all 5 ops returned; nothing left in the log");
}

#[test]
fn scrollback_for_caps_at_max_returning_tail() {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    for seq in 1..=10 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    let (ops, exhausted) = state.scrollback_for(artifact, 11, 3);
    let seqs: Vec<u64> = ops.iter().map(|op| op.seq).collect();
    // Caller is paging backwards from the present → most recent 3.
    assert_eq!(seqs, vec![8, 9, 10]);
    assert!(
        !exhausted,
        "10 ops > max of 3 → caller needs to page further; not exhausted"
    );
}

#[test]
fn scrollback_for_respects_before_seq_bound() {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    for seq in 1..=5 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    let (ops, _exhausted) = state.scrollback_for(artifact, 3, 10);
    let seqs: Vec<u64> = ops.iter().map(|op| op.seq).collect();
    // before_seq = 3 → seqs strictly less than 3.
    assert_eq!(seqs, vec![1, 2]);
}

#[test]
fn scrollback_for_artifact_filter() {
    let (mut state, sk, author, artifact_a, att_id) = minimal_state();
    let artifact_b = ArtifactId::new();
    state.artifacts.insert(artifact_b, ArtifactKind::Note);
    for seq in 1..=3 {
        state.record_validated_op(make_signed_op(&sk, author, artifact_a, att_id, seq));
        state.record_validated_op(make_signed_op(&sk, author, artifact_b, att_id, seq + 100));
    }
    let (ops, _) = state.scrollback_for(artifact_a, 1000, 10);
    let seqs: Vec<u64> = ops.iter().map(|op| op.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3], "only artifact A ops returned");
}

#[test]
fn scrollback_for_exhausted_after_compaction() {
    let (mut state, sk, author, artifact, att_id) = minimal_state();
    for seq in 1..=5 {
        state.record_validated_op(make_signed_op(&sk, author, artifact, att_id, seq));
    }
    // Compact seqs 1..=3 away.
    let prefix: Vec<SignedOp> = state
        .op_log
        .iter()
        .filter(|op| op.seq <= 3)
        .cloned()
        .collect();
    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 3,
        prefix_hash: hash_op_prefix(&prefix).expect("hash"),
    };
    state.apply_compaction(&cp).expect("apply");

    // Asking for seqs < 4 → returns empty + exhausted.
    let (ops, exhausted) = state.scrollback_for(artifact, 4, 10);
    assert!(ops.is_empty(), "compacted prefix has been evicted");
    assert!(exhausted, "the requested range is fully exhausted locally");
}

// ---------- parse_compaction_payload integration ----------

#[test]
fn parse_compaction_payload_round_trip() {
    let cp = CompactionCheckpoint {
        artifact: ArtifactId::new(),
        up_to_seq: 42,
        prefix_hash: [0xCD; 32],
    };
    let bytes = encode_compaction_payload(&cp).expect("encode");
    let (_, sk, author, _artifact, att_id) = minimal_state();
    // The op's `artifact` should match the checkpoint's `artifact`; a
    // Compaction op should be Owner-authored (not validated here — just
    // round-tripping payload bytes through `parse_compaction_payload`).
    let mut op = SignedOp::new_unsigned(
        bytes,
        cp.artifact,
        author,
        att_id,
        OpClass::Compaction,
        SystemTime::UNIX_EPOCH,
        99,
    );
    sign_op(&mut op, &sk).expect("sign");
    let parsed = collab_types::parse_compaction_payload(&op).expect("parse");
    assert_eq!(parsed, cp);
}
