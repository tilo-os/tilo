use super::*;
use crate::BoundedStatePeer;
use crate::PeerLimits;
use crate::StatePeer;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use collab_types::Role;
use tilo_action_policy::{ActionKind, TreePath};
use tilo_user_identity::RevocationList;
fn fixture(role: Role) -> (StatePeer, [u8; 32], SystemTime) {
    let (mut peer, _, _, cert, now) = crate::admission_tests::seeded();
    let room = peer.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", "different owner").unwrap();
    let members = room.get(ROOT, "members").unwrap().unwrap().1;
    room.put(
        &members,
        hex::encode(cert.user_public_key),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&role).unwrap()),
    )
    .unwrap();
    room.commit();
    (peer, cert.device_public_key, now)
}
fn old_document(
    peer: &StatePeer,
    device: [u8; 32],
    app: &str,
    record: &str,
    now: SystemTime,
) -> Result<(), String> {
    let member = peer.permissions(device, now)?;
    if !member.allows(
        &collab_types::document_node(peer.room(), app, record),
        ActionKind::Read,
        now,
    ) {
        return Err(format!("record read forbidden: {app}/{record}"));
    }
    Ok(())
}
fn old_room(peer: &StatePeer, device: [u8; 32], now: SystemTime) -> (bool, bool) {
    let member = peer.permissions(device, now).unwrap();
    let room = TreePath::room(peer.room());
    (
        member.allows(&room, ActionKind::Read, now),
        member.allows(&room, ActionKind::Undoable, now),
    )
}
#[test]
fn native_current_access_matches_actual_legacy_room_and_record_decisions() {
    for (role, expected_room) in [
        (Role::Viewer, (true, false)),
        (Role::Commenter, (true, false)),
        (Role::Editor, (true, true)),
    ] {
        let (mut peer, device, now) = fixture(role);
        let bytes = peer.durable().unwrap();
        let expected = old_room(&peer, device, now);
        assert_eq!(expected, expected_room, "{role:?}");
        let restored = BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            &RevocationList::new(),
            now,
            &PeerLimits::default(),
        )
        .unwrap();
        let read = restored.read();
        let access = read.room_access(device, now).unwrap();
        assert_eq!((access.read(), access.write()), expected);
        assert_eq!(
            access.user_root(),
            peer.core.certificates.get(&device).unwrap().user_public_key
        );
        for (app, record) in [("test.app", "tab:1"), ("other.app", "missing"), ("", "")] {
            assert_eq!(
                peer.check_document_read(device, app, record, now),
                old_document(&peer, device, app, record, now)
            );
            assert_eq!(
                read.document_read_allowed(device, app, record, now)
                    .unwrap(),
                old_document(&peer, device, app, record, now).is_ok()
            );
        }
        let person = hex::encode(peer.core.certificates.get(&device).unwrap().user_public_key);
        let room = peer.core.room_doc.as_mut().unwrap();
        let members = room.get(ROOT, "members").unwrap().unwrap().1;
        room.delete(&members, person.as_str()).unwrap();
        room.commit();
        assert!(peer.permissions(device, now).is_err());
        let restored = BoundedStatePeer::restore(
            &peer.durable().unwrap(),
            peer.room(),
            peer.authority(),
            &[],
            &RevocationList::new(),
            now,
            &PeerLimits::default(),
        )
        .unwrap();
        let read = restored.read();
        assert!(read.room_access(device, now).is_err());
        assert!(
            read.document_read_allowed(device, "test.app", "tab:1", now)
                .is_err()
        );
    }
}
#[test]
fn access_evaluates_long_routes_and_refuses_unknown_devices() {
    let (peer, device, now) = fixture(Role::Editor);
    let bytes = peer.durable().unwrap();
    let restored = BoundedStatePeer::restore(
        &bytes,
        peer.room(),
        peer.authority(),
        &[],
        &RevocationList::new(),
        now,
        &PeerLimits::default(),
    )
    .unwrap();
    let read = restored.read();
    let app = "long-app".repeat(4000);
    let record = "long-record".repeat(4000);
    assert!(
        read.document_read_allowed(device, &app, &record, now)
            .unwrap()
    );
    assert!(read.room_access(device, now).unwrap().write());
    assert!(read.room_access([0x77; 32], now).is_err());
    assert_eq!(
        peer.check_document_read([0x77; 32], "a", "b", now),
        old_document(&peer, [0x77; 32], "a", "b", now)
    );
}
