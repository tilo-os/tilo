use super::*;
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use collab_types::caps::{CapOverride, Expiry, Subject};
use std::cell::RefCell;

thread_local! {
    static BEFORE_DOCUMENT: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}
pub(super) fn before_document() {
    if let Some(hook) = BEFORE_DOCUMENT.with(|slot| slot.borrow_mut().take()) {
        hook();
    }
}

#[test]
fn cloud_policy_clock_expiry_during_guest_writer_preparation_denies_actual_effect() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    let raw = qos_relay::clock::now_ns();
    let expiry = raw / 1_000_000_000 + 100;
    let owner_device = SigningKey::from_bytes(&[8; 32]);
    let guest_root = SigningKey::from_bytes(&[9; 32]);
    let guest_device = SigningKey::from_bytes(&[10; 32]);
    let (guest, _) = crate::tests::invite_and_admit(
        &mut store,
        &owner_device,
        &guest_root,
        &guest_device,
        "timed-guest",
        [71; 32],
        raw,
    );
    let mut peer = store.peer().unwrap();
    peer.select_admission_v5().unwrap();
    let mut selected = store.state.clone();
    selected.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(selected).unwrap();
    let mut doc = store.peer().unwrap().admitted_room().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(
        store.key().verifying_key().to_bytes().to_vec(),
    ));
    let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
    let keys: Vec<_> = doc.keys(&caps).collect();
    for key in keys {
        doc.delete(&caps, key).unwrap();
    }
    for (key, cap) in [
        (
            "guest-read",
            CapOverride::membership(
                Subject::User(collab_types::Ed25519PublicKey(guest.user_public_key)),
                &store.state.room,
                Verb::Read,
            ),
        ),
        (
            "guest-write",
            CapOverride::membership(
                Subject::User(collab_types::Ed25519PublicKey(guest.user_public_key)),
                &store.state.room,
                Verb::Write,
            )
            .expiring(Expiry::AtUnix(expiry)),
        ),
    ] {
        doc.put(
            &caps,
            key,
            ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
        )
        .unwrap();
    }
    doc.commit();
    let message = NeighbourMsg::SyncResp {
        room: store.state.room.clone(),
        changes: doc
            .get_changes(&[])
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    };
    store
        .receive(
            TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
            message,
            raw,
        )
        .unwrap();
    let (clock, control) = qos_action_broker::peer_actions::OwnerPolicyClock::for_test(raw);
    let source = clock.new_origin();
    store.bind_policy_time(source.capture().unwrap()).unwrap();
    let peer = store.peer().unwrap();
    assert_eq!(
        peer.permissions(guest.device_public_key, now_time(raw))
            .unwrap()
            .0,
        collab_types::Role::Editor
    );
    assert_eq!(
        peer.permissions(
            guest.device_public_key,
            now_time((expiry + 1) * 1_000_000_000)
        )
        .unwrap()
        .0,
        collab_types::Role::Viewer
    );
    assert_eq!(
        peer.permissions(
            peer.authority().device_public_key,
            now_time((expiry + 1) * 1_000_000_000)
        )
        .unwrap()
        .0,
        collab_types::Role::Owner
    );
    let mut action = ActionRequest {
        version: 1,
        room: store.state.room.clone(),
        invocation_id: "valid-guest-write".into(),
        operation: NativeOperation::WriterInsert {
            text: "valid guest".into(),
        },
    };
    assert_eq!(
        store
            .action(guest.device_public_key, &action, raw)
            .unwrap()
            .status,
        "completed"
    );
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    action.invocation_id = "expired-during-preparation".into();
    BEFORE_DOCUMENT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            control.advance_monotonic(std::time::Duration::from_nanos(
                (expiry + 1) * 1_000_000_000 - raw,
            ));
            control.set_wall_ns(raw);
        }))
    });
    let result = store.action(guest.device_public_key, &action, raw);
    assert!(
        result.is_err(),
        "cached Editor role admitted actual expired guest effect: {result:?}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}
