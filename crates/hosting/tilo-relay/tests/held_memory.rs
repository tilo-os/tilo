//! What the relay holds and allocates, measured with a counting global allocator.
//!
//! A pinned workspace materializes a long run of remote deltas: a device types and writes
//! settings in 20 rounds of 1000 ops; the relay takes each round in through anti-entropy ingest,
//! as it would from a peer. Its growth is compared with the same document loaded from a full
//! save. Separately, classifying a routed multi-megabyte delta must not copy it.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_automerge::heap::{self, ThreadCounting};
use tilo_relay::workspace::{Membership, WorkspaceRuntime};

#[global_allocator]
static HEAP: ThreadCounting = ThreadCounting;

const WORKSPACE: u64 = 7;
const ROUNDS: u64 = 20;
const OPS_PER_ROUND: usize = 1000;
const SETTINGS_EVERY: usize = 10;
const TYPED_RUN: &str = "xxxxxxxxx";
// Every round is still 1000 ops: 100 settings writes and 900 typed characters.
const _: () = assert!(TYPED_RUN.len() == SETTINGS_EVERY - 1);

#[test]
fn a_pinned_workspace_holds_what_its_saved_document_holds() {
    let state = tempfile::tempdir().unwrap();
    let mut workspace =
        WorkspaceRuntime::open(state.path(), WORKSPACE, true, Membership::default()).unwrap();

    let mut remote = AutoCommit::new();
    let text = remote.put_object(ROOT, "text", ObjType::Text).unwrap();
    let settings = remote.put_object(ROOT, "settings", ObjType::Map).unwrap();
    let mut grown = 0isize;
    let mut typed = 0usize;
    for round in 0..ROUNDS {
        // Each settings write is followed by nine typed characters. One splice of the
        // nine records the same nine single-character inserts, each after the last, as
        // nine one-character splices do, without locating the insertion point nine times.
        for op in (0..OPS_PER_ROUND).step_by(SETTINGS_EVERY) {
            remote
                .put(&settings, format!("k{}", op % 50), round)
                .unwrap();
            remote.splice_text(&text, typed, 0, TYPED_RUN).unwrap();
            typed += TYPED_RUN.len();
        }
        let delta = remote.save_incremental();
        let before = heap::held();
        assert!(
            workspace
                .ingest_sync_changes(WORKSPACE, &delta, round + 1)
                .unwrap()
        );
        grown += heap::held() - before;
    }
    assert_eq!(
        workspace.current_heads(),
        remote
            .get_heads()
            .into_iter()
            .map(|h| h.0)
            .collect::<Vec<_>>()
    );

    let saved = remote.save();
    let loaded = heap::held_by(|| AutoCommit::load(&saved).unwrap());
    eprintln!("relay workspace: grown {grown} bytes, loaded {loaded} bytes");
    assert!(
        grown < loaded * 2,
        "a workspace fed 20,000 remote ops holds {grown} bytes against {loaded} loaded"
    );
}

/// A document delta as a native desktop publishes its idle checkpoint: a
/// signed `AppDocument` of `size` payload bytes inside a co-edit checkpoint.
fn app_document_checkpoint(size: usize) -> Vec<u8> {
    let op = collab_types::SignedOp::new_unsigned(
        vec![0x5a; size],
        collab_types::document_artifact_id("so.tilo.notes", "cluster", "note-1"),
        collab_types::Ed25519PublicKey::from_bytes([7; 32]),
        collab_types::document_attestation_id("so.tilo.notes", "cluster", "note-1"),
        collab_types::OpClass::Splice,
        std::time::UNIX_EPOCH,
        1,
    );
    let body = postcard::to_stdvec(&tilo_peer_state::NeighbourMsg::AppDocument {
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
    assert!(tilo_relay::compaction::Compactor::observe(&delta).is_some());
    let observed = heap::allocated_by(|| tilo_relay::compaction::Compactor::observe(&delta));
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
