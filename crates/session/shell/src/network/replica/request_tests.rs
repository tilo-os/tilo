//! A replication read must neither dirty the desktop nor change its document.
use super::*;
use automerge::transaction::Transactable;

#[test]
fn sync_request_snapshot_preserves_heads_without_announcing_a_tree_write() {
    let mut tree = WorkspaceTree::default();
    let workspace = tree.append_empty_workspace("One room");
    let room = tree.room_of(workspace).unwrap();
    tree.sync_room_doc(room);
    tree.room_docs
        .get_mut(&room)
        .unwrap()
        .put(automerge::ROOT, "pending", "edit")
        .unwrap();
    let expected = tree.clone().room_heads(room);
    let tree = tree.shared();
    let replica = WorkspaceTreeReplica::new(7, tree.clone());
    let generation = tree.generation();
    replica.request_sync_of(&[room]);
    assert_eq!(
        tree.generation(),
        generation,
        "requesting heads must not invalidate retained readers"
    );
    let messages = replica.drain_outgoing();
    assert_eq!(messages.len(), 1);
    let NeighbourMsg::SyncReq {
        since, room: named, ..
    } = &messages[0]
    else {
        panic!("expected a sync request");
    };
    assert_eq!(named, &room.to_string());
    assert_eq!(
        serde_json::from_slice::<Vec<automerge::ChangeHash>>(since).unwrap(),
        expected
    );
    assert!(
        tree.read().room_docs[&room].pending_ops() > 0,
        "the live transaction is untouched"
    );
}

#[test]
fn snapshot_sync_requests_still_refuse_malformed_media_policy() {
    let mut tree = WorkspaceTree::default();
    let workspace = tree.append_empty_workspace("One room");
    let room = tree.room_of(workspace).unwrap();
    tree.sync_room_doc(room);
    tree.room_docs
        .get_mut(&room)
        .unwrap()
        .put(
            automerge::ROOT,
            tilo_peer_state::room::media::FIELD,
            automerge::ScalarValue::Bytes(vec![0xff]),
        )
        .unwrap();
    let tree = tree.shared();
    let replica = WorkspaceTreeReplica::new(7, tree.clone());
    let generation = tree.generation();
    replica.request_sync_of(&[room]);
    assert_eq!(tree.generation(), generation);
    let messages = replica.drain_outgoing();
    let NeighbourMsg::SyncReq {
        since, room: named, ..
    } = &messages[0]
    else {
        panic!("expected a scoped sync request");
    };
    assert_eq!(named, &room.to_string());
    assert_eq!(since, b"[]", "invalid policy must not yield usable heads");
}
