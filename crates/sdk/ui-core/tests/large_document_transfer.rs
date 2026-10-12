//! Document size must not be a cliff. Edits and snapshots far larger than
//! one signed frame travel through the production SDK shovel as several
//! individually signed segments and arrive at the peer byte-for-byte, with
//! both bridges still connected afterwards.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::typed_collab::TypedBroker;
use collab_types::CommitNamed as _;
use tilo_ui::collaboration::{CollabRequest, CollaborationManager};

const MIB: usize = 1024 * 1024;

/// Incompressible, deterministic bytes: a transport that truncated, reordered
/// or duplicated a segment cannot produce the same buffer by accident.
fn payload(len: usize, seed: u64) -> Vec<u8> {
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

/// The next document mutation `peer` receives, which `what` describes.
fn receive(peer: &mut CollaborationManager, what: &str) -> collab_types::DocumentMutation {
    tilo_test_env::wait_for(what, || {
        peer.poll().into_iter().find_map(|event| {
            event
                .document_mutation()
                .map(|result| result.expect("a delivered document event decodes"))
        })
    })
}

fn connected_pair(broker: &TypedBroker) -> (CollaborationManager, CollaborationManager) {
    let mut sender = broker.manager("alice");
    let mut receiver = broker.manager("bob");
    let _ = sender.poll();
    let _ = receiver.poll();
    (sender, receiver)
}

/// A peer that joins after the document outgrew one frame is onboarded by
/// the production late-join path: its SDK asks the broker for a snapshot,
/// the holder's app answers, and the segmented snapshot merges into the
/// joiner's document.
#[test]
fn a_late_joiner_onboards_a_document_larger_than_one_frame() {
    use automerge::transaction::Transactable;
    use tilo_ui::app::App;
    use tilo_ui::state::State;
    use tilo_ui::widget::Container;

    let body = payload(4 * MIB + 123, 77);
    let mut holder_state = State::new();
    holder_state
        .doc
        .put(
            automerge::ROOT,
            "body",
            automerge::ScalarValue::Bytes(body.clone()),
        )
        .unwrap();
    holder_state.doc.commit_edit("fixture", "fixture");
    assert!(
        holder_state.doc.save().len() > collab_types::MAX_DOCUMENT_MUTATION_BYTES,
        "precondition: the snapshot does not fit one signed frame"
    );

    let broker = TypedBroker::new();
    let mut holder = App::with_collaboration_manager(
        holder_state,
        Box::new(Container::new()),
        broker.manager("holder"),
    );
    holder.pump_collab_presence();

    let mut joiner = App::with_collaboration_manager(
        State::new(),
        Box::new(Container::new()),
        broker.manager("joiner"),
    );
    let onboarded = |app: &mut App| {
        automerge::ReadDoc::get(&app.state_mut_for_test().doc, automerge::ROOT, "body")
            .unwrap()
            .and_then(|(value, _)| value.as_bytes().map(<[u8]>::to_vec))
    };
    let arrived = tilo_test_env::wait_for("the late joiner's onboarded document body", || {
        holder.pump_collab_presence();
        joiner.pump_collab_presence();
        onboarded(&mut joiner)
    });
    assert!(arrived == body, "the late joiner holds the whole document");
    assert_eq!(joiner.document_sync_status(), None);
}

#[test]
fn an_over_limit_mutation_is_refused_explicitly_and_collaboration_continues() {
    let broker = TypedBroker::new();
    let (mut sender, mut receiver) = connected_pair(&broker);
    let limit = collab_types::MAX_DOCUMENT_TRANSFER_BYTES;

    assert_eq!(
        sender.try_send(CollabRequest::document_incremental(vec![0; limit + 1])),
        Err(tilo_ui::collaboration::CollabSendError::TooLarge {
            actual: limit + 1,
            max: limit,
        })
    );
    assert!(
        sender.is_connected(),
        "the refusal is not a bridge shutdown"
    );

    let after = payload(2 * MIB, 3);
    sender
        .try_send(CollabRequest::document_incremental(after.clone()))
        .expect("later changes still flow");
    assert_eq!(
        receive(&mut receiver, "the change after the refused one"),
        collab_types::DocumentMutation::Incremental(after),
        "the peer sees the next change and nothing of the refused one"
    );
}

#[test]
fn large_edits_and_snapshots_replicate_between_two_peers() {
    let broker = TypedBroker::new();
    let (mut sender, mut receiver) = connected_pair(&broker);

    for (index, len) in [MIB, 7 * MIB / 2, 8 * MIB, 32 * MIB]
        .into_iter()
        .enumerate()
    {
        for snapshot in [false, true] {
            let seed = (index as u64) << 1 | u64::from(snapshot);
            let data = payload(len, seed);
            let request = if snapshot {
                CollabRequest::document_snapshot(data.clone())
            } else {
                CollabRequest::document_incremental(data.clone())
            };
            assert!(
                sender.send(request),
                "a {len}-byte mutation must be handed to the transport"
            );
            let arrived = receive(
                &mut receiver,
                &format!("the {len}-byte mutation (snapshot: {snapshot})"),
            );
            match (snapshot, arrived) {
                (true, collab_types::DocumentMutation::Snapshot(bytes))
                | (false, collab_types::DocumentMutation::Incremental(bytes)) => {
                    assert_eq!(bytes.len(), len, "no bytes are lost or added");
                    assert!(bytes == data, "the {len}-byte payload arrives intact");
                }
                (_, other) => panic!("mutation kind changed in transit: {other:?}"),
            }
            assert!(
                sender.is_connected() && receiver.is_connected(),
                "a large mutation must not shut collaboration down"
            );
        }
    }
    assert!(
        receiver
            .poll()
            .iter()
            .all(|e| e.document_mutation().is_none()),
        "every transfer is delivered exactly once"
    );
}
