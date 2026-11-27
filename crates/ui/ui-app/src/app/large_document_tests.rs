//! A local change reaches the record's store before the route's frontier
//! moves, whatever its size. A change the transport can never carry is reported
//! and kept, and collaboration stays connected.
#![cfg(unix)]

use super::*;
use automerge::transaction::Transactable;
use collab_types::CommitNamed as _;

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

/// Each write of record changes the store took, in order: the changes it carried.
type Writes = Box<dyn Fn() -> Vec<Vec<Vec<u8>>>>;

fn connected_app() -> (App, Writes) {
    let (manager, probe) = CollaborationManager::capturing_for_test();
    let mut app =
        App::new_with_controlled_inputs(State::new(), Box::new(crate::widget::Column::new()));
    app.collaboration.manager = manager;
    tilo_test_env::wait_until("the capturing broker connects", || {
        app.collaboration.manager.is_connected()
    });
    (app, probe)
}

/// The changes of the next write after the `seen` the store had already
/// taken, as one run.
fn wait_for_write(probe: &dyn Fn() -> Vec<Vec<Vec<u8>>>, seen: usize) -> Vec<u8> {
    tilo_test_env::wait_for("the store to take the complete change", || {
        probe().get(seen).map(|write| write.concat())
    })
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
    app.state.doc.commit_edit("fixture", "fixture", &[1; 32]);
    assert!(
        app.pending_primary_document()
            .unwrap()
            .is_some_and(|(mutation, _, _)| {
                mutation.data().len() > collab_types::MAX_DOCUMENT_MUTATION_BYTES
            }),
        "precondition: the change does not fit one signed frame"
    );

    app.broadcast_doc_delta();

    assert!(
        app.pending_primary_document().unwrap().is_none(),
        "an accepted change advances the frontier"
    );
    let whole = wait_for_write(&*probe, 0);
    let mut peer = automerge::AutoCommit::new();
    peer.load_incremental(&whole).unwrap();
    assert_eq!(
        automerge::ReadDoc::get(&peer, automerge::ROOT, "body")
            .unwrap()
            .and_then(|(value, _)| value.as_bytes().map(<[u8]>::to_vec)),
        Some(body),
        "the peer receives the whole record"
    );

    // The edit made once the route holds the record travels on as a change.
    let edited = bytes(7 * MIB / 2, 12);
    app.state
        .doc
        .put(
            automerge::ROOT,
            "body",
            automerge::ScalarValue::Bytes(edited.clone()),
        )
        .unwrap();
    app.state.doc.commit_edit("fixture", "fixture", &[1; 32]);
    assert!(
        app.pending_primary_document()
            .unwrap()
            .is_some_and(|(mutation, _, _)| {
                mutation.data().len() > collab_types::MAX_DOCUMENT_MUTATION_BYTES
            }),
        "precondition: the live edit does not fit one signed frame either"
    );

    app.broadcast_doc_delta();

    assert!(
        app.pending_primary_document().unwrap().is_none(),
        "an accepted change advances the frontier"
    );
    let delta = wait_for_write(&*probe, 1);
    peer.load_incremental(&delta).unwrap();
    assert_eq!(
        automerge::ReadDoc::get(&peer, automerge::ROOT, "body")
            .unwrap()
            .and_then(|(value, _)| value.as_bytes().map(<[u8]>::to_vec)),
        Some(edited),
        "the peer receives the whole change"
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
    app.state.doc.commit_edit("fixture", "fixture", &[1; 32]);

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
        app.pending_primary_document().unwrap().is_some(),
        "the refused change stays pending instead of being marked sent"
    );
    // Ordinary traffic still flows on the same bridge, and the bridge keeps
    // order: what the broker sees first is the ordinary change, so no part
    // of the refused one went ahead of it.
    let mut ordinary = automerge::AutoCommit::new();
    ordinary.put(automerge::ROOT, "ordinary", 1).unwrap();
    ordinary.commit();
    assert!(
        app.collaboration
            .manager
            .send(::collaboration::CollabRequest::document_incremental(
                ordinary.save_after(&[])
            ))
    );
    tilo_test_env::wait_until("the ordinary change reaches the broker", || {
        !probe().is_empty()
    });
    assert_eq!(probe().len(), 1, "no partial change reached the broker");
}
