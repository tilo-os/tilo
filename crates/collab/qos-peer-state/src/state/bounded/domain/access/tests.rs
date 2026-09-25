use super::*;
use crate::BoundedStatePeer;
use crate::PeerLimits;
use crate::StatePeer;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use collab_types::caps::{
    Actor, CapDecision, CapOverride, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};
use collab_types::{ArtifactKind, Ed25519PublicKey};
use tilo_user_identity::RevocationList;
use web_time::Duration;
fn fixture(verb: Verb) -> (StatePeer, [u8; 32], SystemTime) {
    let (mut peer, _, _, cert, now) = crate::admission_tests::seeded();
    let room = peer.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", "different owner").unwrap();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    let existing: Vec<_> = room.keys(&caps).collect();
    for key in existing {
        room.delete(&caps, key).unwrap();
    }
    let cap = CapOverride::grant(
        Subject::User(Ed25519PublicKey::from_bytes(cert.user_public_key)),
        Scope::Workspace,
        verb,
    )
    .with_expiry(crate::state::now_secs(now).checked_add(1).unwrap());
    room.put(
        &caps,
        "member",
        automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
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
    let (role, caps, person) = peer.permissions(device, now)?;
    let actor = Actor::person(Some(role), &person)
        .with_principal(Principal::App(app.into()))
        .in_room(peer.room().to_owned());
    let resource = ResourceRef::Artifact {
        app: Some(app.into()),
        kind: ArtifactKind::Note,
        instance: collab_types::document_artifact_id(app, peer.room(), record),
    };
    if decide_capability(
        &actor,
        &resource,
        Verb::Read,
        &caps,
        Some(crate::state::now_secs(now)),
    ) != CapDecision::Allow
    {
        return Err(format!("record read forbidden: {app}/{record}"));
    }
    Ok(())
}
fn old_room(peer: &StatePeer, device: [u8; 32], now: SystemTime) -> (bool, bool) {
    let (role, caps, person) = peer.permissions(device, now).unwrap();
    let actor = Actor::person(Some(role), person).in_room(peer.room().to_owned());
    let resource = ResourceRef::Room(peer.room().to_owned());
    (
        decide_capability(
            &actor,
            &resource,
            Verb::Read,
            &caps,
            Some(crate::state::now_secs(now)),
        ) == CapDecision::Allow,
        decide_capability(
            &actor,
            &resource,
            Verb::Write,
            &caps,
            Some(crate::state::now_secs(now)),
        ) == CapDecision::Allow,
    )
}
#[test]
fn native_current_access_matches_actual_legacy_room_and_record_decisions() {
    for verb in [
        Verb::Read,
        Verb::Write,
        Verb::Comment,
        Verb::Invite,
        Verb::Admin,
    ] {
        let (peer, device, now) = fixture(verb);
        let bytes = peer.durable().unwrap();
        let expected = old_room(&peer, device, now);
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
        let later = now + Duration::from_secs(2);
        assert!(peer.permissions(device, later).is_err());
        assert!(read.room_access(device, later).is_err());
        assert!(
            read.document_read_allowed(device, "test.app", "tab:1", later)
                .is_err()
        );
    }
}
#[test]
fn access_evaluates_long_routes_and_refuses_unknown_devices() {
    let (peer, device, now) = fixture(Verb::Write);
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
