use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use collab_types::caps::{
    Actor, CapDecision, CapOverride, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};
use collab_types::{ArtifactKind, Ed25519PublicKey};
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
    .with_expiry(crate::state::now_secs(now) + 1);
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
        let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        let restored = BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            &RevocationList::new(),
            now,
            &mut work,
        )
        .unwrap();
        let read = restored.read(&work).unwrap();
        let access = assert_precharged(&current, || read.room_access(device, now)).unwrap();
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
                assert_precharged(&current, || read
                    .document_read_allowed(device, app, record, now))
                .unwrap(),
                old_document(&peer, device, app, record, now).is_ok()
            );
        }
        let later = now + Duration::from_secs(2);
        assert!(peer.permissions(device, later).is_err());
        assert!(assert_precharged(&current, || read.room_access(device, later)).is_err());
        assert!(
            read.document_read_allowed(device, "test.app", "tab:1", later)
                .is_err()
        );
    }
}
#[test]
fn access_uses_current_credit_and_prepays_large_actor_outputs_before_evaluation() {
    let (peer, device, now) = fixture(Verb::Write);
    let bytes = peer.durable().unwrap();
    let resident = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let restored = {
        let mut turn = registry
            .begin_turn(resident.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            &RevocationList::new(),
            now,
            &mut work,
        )
        .unwrap()
    };
    let resident_before = resident.retained_bytes();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let read = restored.read(&work).unwrap();
    let app = "long-app".repeat(4000);
    let record = "long-record".repeat(4000);
    let before = current.retained_bytes();
    assert!(
        assert_precharged(&current, || read
            .document_read_allowed(device, &app, &record, now))
        .unwrap()
    );
    assert_eq!(current.retained_bytes(), before);
    assert_eq!(resident.retained_bytes(), resident_before);
    let all = work.reserve_output(current.available_bytes()).unwrap();
    assert!(read.room_access(device, now).is_err());
    drop(all);
    assert!(read.room_access(device, now).unwrap().write());
    assert!(read.room_access([0x77; 32], now).is_err());
    assert_eq!(
        peer.check_document_read([0x77; 32], "a", "b", now),
        old_document(&peer, [0x77; 32], "a", "b", now)
    );
}

#[test]
fn shared_evaluator_actor_growth_and_subject_temporary_fit_exact_source_budget() {
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let person = "ab".repeat(32);
    let room = "room-☺".repeat(4000);
    let app = "app-☺".repeat(4000);
    let caps = vec![CapOverride::grant(
        Subject::User(Ed25519PublicKey::from_bytes([0xab; 32])),
        Scope::Workspace,
        Verb::Write,
    )];
    let now = SystemTime::UNIX_EPOCH;
    for selected_app in [Some(app.as_str()), None] {
        let bytes = backend::access::temporary_bytes(&person, &room, selected_app).unwrap();
        assert_precharged(&current, || {
            let _retention = current.reserve(bytes).unwrap();
            if let Some(app) = selected_app {
                assert!(backend::access::document_read(
                    Role::Editor,
                    &caps,
                    &person,
                    &room,
                    app,
                    "record",
                    now
                ));
            } else {
                assert_eq!(
                    backend::access::room(Role::Editor, &caps, &person, &room, now),
                    (true, true)
                );
            }
        });
    }
}

#[test]
fn actor_copy_quota_refuses_large_route_before_growth_and_leaves_current_view_usable() {
    let (peer, device, now) = fixture(Verb::Write);
    let bytes = peer.durable().unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let restored = {
        let mut turn = registry
            .begin_turn(
                NativeAllowance::new(64 * 1024 * 1024).unwrap(),
                PeerLimits::default(),
            )
            .unwrap();
        let mut work = turn.work().unwrap();
        BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            &RevocationList::new(),
            now,
            &mut work,
        )
        .unwrap()
    };
    let current = NativeAllowance::new(2 * 1024 * 1024).unwrap();
    let limits = PeerLimits {
        governance_collection_bytes: 4096,
        ..PeerLimits::default()
    };
    let mut turn = registry.begin_turn(current.clone(), limits).unwrap();
    let work = turn.work().unwrap();
    let read = restored.read(&work).unwrap();
    assert!(
        read.document_read_allowed(device, "small", "record", now)
            .unwrap()
    );
    let large = "x".repeat(2 * 1024 * 1024);
    let before = current.retained_bytes();
    let error = assert_precharged(&current, || {
        read.document_read_allowed(device, &large, "record", now)
    });
    assert!(matches!(
        error,
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
    assert_eq!(current.retained_bytes(), before);
    assert!(
        read.document_read_allowed(device, "small", "record", now)
            .unwrap()
    );
}
