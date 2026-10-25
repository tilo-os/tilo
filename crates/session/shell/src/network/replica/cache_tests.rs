use super::*;
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use tilo_automerge::ApplyIncoming as _;
use tilo_peer_state::room::media;

fn setup() -> (WorkspaceTreeHandle, WorkspaceTreeReplica, RoomId) {
    let mut tree = WorkspaceTree::default();
    let workspace = tree.append_empty_workspace("One room");
    let room = tree.room_of(workspace).unwrap();
    let governor = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
    let device = iroh::SecretKey::from_bytes(&[92; 32]).public();
    let owner = hex::encode(governor.verifying_key().as_bytes());
    tree.desktop.workspaces[0].set_owner(owner.clone());
    assert!(tree.name_peer(room, &owner, &device.to_string(), "Governor"));
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: governor.verifying_key().to_bytes(),
        device_public_key: *device.as_bytes(),
        device_label: "Governor device".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &governor).unwrap();
    assert!(tree.certify_peer_identity(room, &device.to_string(), cert));
    tree.sync_room_doc(room);
    let doc = tree.room_docs.get_mut(&room).unwrap();
    let policy = media::RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(&room.to_string()).unwrap().as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(doc, &[]).unwrap(),
        calls: std::collections::BTreeMap::new(),
    };
    doc.put(
        ROOT,
        media::FIELD,
        ScalarValue::Bytes(media::encode(&policy).unwrap()),
    )
    .unwrap();
    doc.commit();
    assert!(media::validate_transition(None, doc, media::TransitionMode::Restore).is_ok());
    let tree = tree.shared();
    let replica = WorkspaceTreeReplica::new(7, tree.clone());
    (tree, replica, room)
}

fn request(replica: &WorkspaceTreeReplica, room: RoomId) -> Vec<automerge::ChangeHash> {
    replica.request_sync_of(&[room]);
    let messages = replica.drain_outgoing();
    assert_eq!(messages.len(), 1);
    let NeighbourMsg::SyncReq {
        since, room: named, ..
    } = &messages[0]
    else {
        panic!("expected a scoped sync request");
    };
    assert_eq!(named, &room.to_string());
    serde_json::from_slice(since).unwrap()
}

#[test]
fn exact_committed_heads_are_reused_but_pending_and_changed_policy_are_not() {
    let (tree, replica, room) = setup();
    let heads = request(&replica, room);
    assert!(!heads.is_empty());
    assert_eq!(replica.sync_heads.lock_recover().get(&room), Some(&heads));
    let generation = tree.generation();
    assert_eq!(request(&replica, room), heads);
    assert_eq!(tree.generation(), generation);
    {
        let mut state = tree.write();
        state
            .room_docs
            .get_mut(&room)
            .unwrap()
            .put(ROOT, media::FIELD, ScalarValue::Bytes(vec![0xff]))
            .unwrap();
    }
    assert_eq!(tree.read().room_docs[&room].committed_heads(), heads);
    assert!(
        request(&replica, room).is_empty(),
        "an open transaction cannot inherit validation"
    );
    tree.write().room_docs.get_mut(&room).unwrap().commit();
    assert_ne!(tree.read().room_docs[&room].committed_heads(), heads);
    drop(tree);
    assert!(
        request(&replica, room).is_empty(),
        "new committed heads must validate again"
    );
    drop(replica);
}

#[test]
fn unapplied_dependencies_cannot_inherit_a_validated_head_set() {
    let (tree, replica, room) = setup();
    let heads = request(&replica, room);
    assert!(!heads.is_empty());
    {
        let mut state = tree.write();
        let doc = state.room_docs.get_mut(&room).unwrap();
        let mut remote = doc.fork();
        remote.put(ROOT, "first", true).unwrap();
        remote.commit();
        remote.put(ROOT, "second", true).unwrap();
        remote.commit();
        let child = (*remote.get_changes(&heads).last().unwrap()).clone();
        doc.apply_incoming([child]).unwrap();
        assert_eq!(doc.committed_heads(), heads);
        assert!(!ReadDoc::get_missing_deps(doc, &[]).is_empty());
        drop(state);
    }
    drop(tree);
    assert!(
        request(&replica, room).is_empty(),
        "a missing dependency changes validation without changing heads"
    );
    drop(replica);
}
