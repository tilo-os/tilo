//! A local change reaches the transport before the route's frontier moves,
//! whatever its size. A change the transport can never carry is reported
//! and kept, and collaboration stays connected.
#![cfg(unix)]

use super::*;
use automerge::transaction::Transactable;
use collab_types::CommitNamed as _;
use std::time::{Duration, Instant};

const MIB: usize = 1024 * 1024;

fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn connected_app() -> (App, Box<dyn Fn() -> Vec<collab_types::SignedOp>>) {
    let (manager, probe) = CollaborationManager::capturing_for_test();
    let mut app =
        App::new_with_controlled_inputs(State::new(), Box::new(crate::widget::Column::new()));
    app.collaboration.manager = manager;
    let started = Instant::now();
    while !app.collaboration.manager.is_connected() && started.elapsed() < crate::HANG_BOUND {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.collaboration.manager.is_connected());
    (app, probe)
}

/// Rebuild the complete mutations a broker received, exactly as a peer's
/// endpoint does: single frames pass through, segments are reassembled and
/// checked against their signed digest.
fn received(ops: &[collab_types::SignedOp]) -> Vec<collab_types::DocumentMutation> {
    let mut reassembler = collab_types::DocumentReassembler::default();
    let mut out = Vec::new();
    for op in ops {
        collab_types::verify_op(op).expect("every frame is individually signed");
        match collab_types::DocumentFrame::decode(&op.payload).expect("typed frame") {
            collab_types::DocumentFrame::Whole(mutation) => out.push(mutation),
            collab_types::DocumentFrame::Segment(segment) => {
                if let Some(done) = reassembler
                    .accept(op, segment, Instant::now())
                    .expect("consistent transfer")
                {
                    out.push(collab_types::DocumentMutation::decode(&done.payload).unwrap());
                }
            }
        }
    }
    out
}

fn wait_for_mutation(
    probe: &dyn Fn() -> Vec<collab_types::SignedOp>,
) -> collab_types::DocumentMutation {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        if let Some(mutation) = received(&probe()).pop() {
            return mutation;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the broker never received the complete change");
}

#[test]
fn a_change_larger_than_one_frame_reaches_the_broker_before_the_frontier_moves() {
    let (mut app, probe) = connected_app();
    let body = bytes(7 * MIB / 2, 11);
    app.state
        .doc
        .put(
            automerge::ROOT,
            "body",
            automerge::ScalarValue::Bytes(body.clone()),
        )
        .unwrap();
    app.state.doc.commit_edit("fixture", "fixture");
    assert!(
        app.pending_primary_document_delta().unwrap().0.len()
            > collab_types::MAX_DOCUMENT_MUTATION_BYTES,
        "precondition: the change does not fit one signed frame"
    );

    app.broadcast_doc_delta();

    assert!(
        app.pending_primary_document_delta().unwrap().0.is_empty(),
        "an accepted change advances the frontier"
    );
    let collab_types::DocumentMutation::Incremental(delta) = wait_for_mutation(&*probe) else {
        panic!("a live edit travels as an incremental change");
    };
    let mut peer = automerge::AutoCommit::new();
    peer.load_incremental(&delta).unwrap();
    assert_eq!(
        automerge::ReadDoc::get(&peer, automerge::ROOT, "body")
            .unwrap()
            .and_then(|(value, _)| value.as_bytes().map(<[u8]>::to_vec)),
        Some(body),
        "the peer receives the whole change"
    );
    assert!(probe().len() > 1, "it travelled as several signed segments");
    assert!(
        probe()
            .iter()
            .all(|op| op.op_class == collab_types::OpClass::Splice),
        "app document changes carry the one coarse class the collaboration docs describe"
    );
    assert!(app.collaboration.manager.is_connected());
    assert!(
        app.document_routes.outbound.is_none(),
        "an accepted change reports no outbound failure"
    );
}

#[test]
fn an_over_limit_change_is_reported_and_kept_without_disconnecting() {
    let (mut app, probe) = connected_app();
    app.state
        .doc
        .put(
            automerge::ROOT,
            "body",
            automerge::ScalarValue::Bytes(bytes(collab_types::MAX_DOCUMENT_TRANSFER_BYTES + 1, 5)),
        )
        .unwrap();
    app.state.doc.commit_edit("fixture", "fixture");

    app.broadcast_doc_delta();

    assert!(
        app.collaboration.manager.is_connected(),
        "an oversized change never shuts collaboration down"
    );
    let status = app
        .document_sync_status()
        .expect("the refusal is visible to the user");
    assert!(status.contains("no longer shared"), "{status}");
    assert!(
        !app.pending_primary_document_delta().unwrap().0.is_empty(),
        "the refused change stays pending instead of being marked sent"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(probe().is_empty(), "no partial change reached the broker");

    // Ordinary traffic still flows on the same bridge.
    assert!(app.collaboration.manager.send(
        crate::collaboration::CollabRequest::document_incremental(vec![1; 16])
    ));
}
