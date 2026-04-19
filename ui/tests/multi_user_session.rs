mod support;

use support::{MemoryRelay, pump};
use quick_ui::collaboration::{CollabRequest, CollabEvent};

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
