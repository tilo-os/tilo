//! What the relay allocates, measured with a counting global allocator: classifying a routed
//! multi-megabyte delta must not copy it.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_automerge::heap::{self, ThreadCounting};

#[global_allocator]
static HEAP: ThreadCounting = ThreadCounting;

/// A document delta as a native desktop publishes its idle checkpoint: a
/// signed `AppDocument` of `size` payload bytes inside a co-edit checkpoint.
fn app_document_checkpoint(size: usize) -> Vec<u8> {
    let op = collab_types::SignedOp::new_unsigned(
        vec![0x5a; size],
        collab_types::document_artifact_id("so.tilo.notes", "cluster", "note-1"),
        collab_types::Ed25519PublicKey::from_bytes([7; 32]),
        collab_types::document_attestation_id("so.tilo.notes", "cluster", "note-1"),
        collab_types::OpKind::Change,
        std::time::UNIX_EPOCH,
        1,
    );
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::AppDocument {
        peer: 3,
        app_id: "so.tilo.notes".into(),
        cluster_id: "cluster".into(),
        record_id: "note-1".into(),
        envelope: Box::new(op),
    })
    .unwrap();
    tilo_relay::CoeditPayload::Checkpoint {
        channel: [9; 32],
        body,
    }
    .encode()
    .unwrap()
}

#[test]
fn classifying_a_routed_checkpoint_does_not_copy_it() {
    const SIZE: usize = 4 * 1024 * 1024;
    const BUDGET: usize = 4 * 1024;
    let delta = app_document_checkpoint(SIZE);
    assert!(delta.len() > SIZE);

    // The compactor sees every delta a session admits.
    assert!(
        tilo_relay::compaction::Compactor::observe_with(
            &tilo_relay::payloads::NeighbourPayloads,
            &delta
        )
        .is_some()
    );
    let observed = heap::allocated_by(|| {
        tilo_relay::compaction::Compactor::observe_with(
            &tilo_relay::payloads::NeighbourPayloads,
            &delta,
        )
    });
    assert!(
        observed < BUDGET,
        "observing a {SIZE}-byte checkpoint allocated {observed} bytes"
    );

    // Persistence and fan-out route it by channel and kind.
    assert_eq!(
        tilo_relay::coedit::retention_metadata(&delta),
        Some(([9; 32], tilo_relay::coedit::RetentionKind::Checkpoint))
    );
    let routed = heap::allocated_by(|| tilo_relay::coedit::retention_metadata(&delta));
    assert!(
        routed < BUDGET,
        "routing a {SIZE}-byte checkpoint allocated {routed} bytes"
    );

    // The counter does see a copy, so a zero above is a measurement.
    let copied = heap::allocated_by(|| tilo_relay::CoeditPayload::decode(&delta));
    assert!(
        copied >= SIZE,
        "an owned decode allocated only {copied} bytes"
    );
}
