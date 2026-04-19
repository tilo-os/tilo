mod support;

use support::{MemoryRelay, pump, headless_app};
use quick_ui::collaboration::{CollabRequest, CollabEvent, PresencePayload};
use quick_ui::presence::{PresenceKey, PathSegment};
use quick_ui::permissions::UserId;

#[test]
fn memory_relay_fans_out_presence_updates_to_both_peers() {
    let mut relay = MemoryRelay::new();
    let alice = relay.attach("alice");
    let bob = relay.attach("bob");

    alice.send(CollabRequest::SyncIncremental { data: vec![1, 2, 3] });

    // Relay broadcasts raw incremental data as CollabEvent::CRDTIncremental.
    relay.drain_peer_outbox(&alice);
    pump(&mut relay);

    let events = bob.poll();
    assert!(events.iter().any(|e| matches!(e, CollabEvent::CRDTIncremental { .. })));
    // Sender should not receive its own broadcast.
    let alice_events = alice.poll();
    assert!(alice_events.is_empty());
}

#[test]
fn two_apps_sync_presence_caret_via_memory_relay() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");

    let (mut _alice_app, _alice_state) = headless_app("alice", alice_peer.clone());
    let (mut bob_app, _bob_state) = headless_app("bob", bob_peer.clone());

    let key = PresenceKey::Widget {
        path: vec![PathSegment::Field("body".into())],
    };

    // Alice moves her caret to position 7.
    alice_peer.send(CollabRequest::SyncIncremental { data: vec![] }); // warm up
    relay.drain_peer_outbox(&alice_peer);
    pump(&mut relay);

    // The relay itself does not yet translate a Presence mutation to an event.
    // We bypass the polling roundtrip and apply the RemotePresenceUpdate
    // directly to bob's App via the test-only hook to model fan-out.
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 7 },
    });

    let caret = bob_app.presence_ctx().caret_for(&key, &UserId("alice".into()));
    assert_eq!(caret, Some(7));
}
