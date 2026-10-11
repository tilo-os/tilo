use super::*;
use crate::StatePeer;
use crate::TransportOrigin;
use crate::state::DurableV4;
use automerge::AutoCommit;
use automerge::ROOT;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use collab_types::CommitNamed as _;
use tilo_user_identity::RevocationList;
use web_time::SystemTime;

fn deferred() -> (StatePeer, NeighbourMsg, SystemTime) {
    let (mut source, _, member, cert, now) = crate::admission_tests::seeded();
    let mut receiver = source.clone();
    let mut doc = AutoCommit::load(&source.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "pending-predecessor", true).unwrap();
    doc.commit_edit("fixture", "pending-predecessor");
    let predecessor = source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "pending-descendant", true).unwrap();
    doc.commit_edit("fixture", "pending-descendant");
    let descendant = source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(cert.device_public_key),
            descendant,
            now,
        )
        .unwrap();
    assert_eq!(receiver.pending_count(), 1);
    (receiver, predecessor, now)
}

#[test]
fn bounded_pending_counts_actual_signed_deferred_message() {
    let (peer, predecessor, now) = deferred();
    let bytes = peer.durable().unwrap();
    let durable: DurableV4 = postcard::from_bytes(&bytes).unwrap();
    validate(&durable.pending, durable.pending_bytes).unwrap();
    let mut restored = StatePeer::restore(
        &bytes,
        peer.room(),
        peer.authority().clone(),
        peer.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(restored.pending_count(), 1);
    let source = match &predecessor {
        NeighbourMsg::AppDocument { envelope, .. } => *envelope.author.as_bytes(),
        _ => panic!("unexpected fixture shape"),
    };
    restored
        .receive_from(TransportOrigin::RelayDelta(source), predecessor, now)
        .unwrap();
    assert_eq!(restored.pending_count(), 0);
    let doc = AutoCommit::load(&restored.document("test.app", "tab:1").unwrap()).unwrap();
    assert!(doc.get(ROOT, "pending-descendant").unwrap().is_some());
}

#[test]
fn bounded_pending_preserves_exact_count_and_declared_byte_denials() {
    let (peer, _, now) = deferred();
    let mut durable: DurableV4 = postcard::from_bytes(&peer.durable().unwrap()).unwrap();
    assert_eq!(
        validate(&durable.pending, durable.pending_bytes + 1),
        Err(PendingError::Bytes)
    );
    let item = durable.pending.front().unwrap().clone();
    durable.pending = std::iter::repeat_n(item, 65).collect();
    assert_eq!(
        validate(&durable.pending, usize::MAX),
        Err(PendingError::Count)
    );
    let bytes = postcard::to_allocvec(&durable).unwrap();
    assert_eq!(
        StatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority().clone(),
            peer.certificates(),
            RevocationList::new(),
            now
        )
        .err()
        .unwrap(),
        "durable pending queue exceeds entry limit"
    );
    let empty = VecDeque::new();
    assert_eq!(validate(&empty, 0), Ok(()));
    assert_eq!(
        validate(&empty, 4 * 1024 * 1024 + 1),
        Err(PendingError::Bytes)
    );
}

#[test]
fn bounded_pending_enforces_four_mib_at_literal_serialized_boundary() {
    let limit = 4 * 1024 * 1024;
    let mut message = NeighbourMsg::SyncResp {
        room: String::new(),
        changes: vec![vec![0; limit]],
    };
    let overhead = postcard::to_allocvec(&message).unwrap().len() - limit;
    if let NeighbourMsg::SyncResp { changes, .. } = &mut message {
        changes[0].truncate(limit - overhead);
    }
    assert_eq!(postcard::to_allocvec(&message).unwrap().len(), limit);
    let mut queue = VecDeque::from([([0; 32], message, false)]);
    validate(&queue, limit).unwrap();
    if let NeighbourMsg::SyncResp { changes, .. } = &mut queue[0].1 {
        changes[0].push(0);
    }
    assert_eq!(validate(&queue, limit + 1), Err(PendingError::Bytes));
}
