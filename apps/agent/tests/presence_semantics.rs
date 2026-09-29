//! Presence semantics through the production Cap'n Proto broker.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test setup and assertions"
)]

use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use ui::presence::PresenceKey;

use crate::typed_peer;
use typed_peer::TypedPeer;

fn hello(handle: &str) -> CollabRequest {
    CollabRequest::Hello {
        app_id: "so.tilo.word-processor".into(),
        cluster_id: typed_peer::room(),
        record: None,
        record_kind: None,
        handle: Some(handle.into()),
    }
}

fn send_until_presence(
    sender: &mut TypedPeer,
    receiver: &mut TypedPeer,
    request: &CollabRequest,
    expected: &(PresenceKey, PresencePayload),
) -> (PresenceKey, PresencePayload) {
    let deadline = tilo_test_env::hang_deadline();
    while ui::time::now() < deadline {
        sender.send(request).expect("publish typed presence");
        if let Some(CollabEvent::RemotePresenceUpdate { key, payload, .. }) = receiver
            .recv()
            .expect("receive from typed collaboration broker")
        {
            let observed = (key, payload);
            if &observed == expected {
                return observed;
            }
        }
    }
    panic!("timed out waiting for remote presence")
}

#[test]
fn keyed_pointer_and_typing_survive_the_native_broker() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("collab.capnp");
    let _broker = typed_peer::spawn(&socket);

    let mut alice = TypedPeer::connect(&socket);
    let mut bob = TypedPeer::connect(&socket);
    alice.announce(&socket, &hello("ada")).unwrap();
    bob.announce(&socket, &hello("Scribe")).unwrap();
    // Construction is the readiness boundary: this transient update is not
    // replayed, so receiving it proves both routes subscribed before their
    // constructors returned. No timing sleep may stand in for that contract.

    let pointer_key = PresenceKey::surface_pointer("document/surface-7");
    let pointer = CollabRequest::Presence {
        key: pointer_key.clone(),
        payload: PresencePayload::Pointer { x: 12.5, y: 48.0 },
    };
    let expected_pointer = (pointer_key, PresencePayload::Pointer { x: 12.5, y: 48.0 });
    assert_eq!(
        send_until_presence(&mut alice, &mut bob, &pointer, &expected_pointer),
        expected_pointer,
        "the keyed pointer must not be decoded as the keyless cursor variant"
    );

    let typing_key = PresenceKey::Tab {
        id: "chapter-2".into(),
    };
    let typing = CollabRequest::Presence {
        key: typing_key.clone(),
        payload: PresencePayload::Typing { start: 7, end: 13 },
    };
    let expected_typing = (typing_key, PresencePayload::Typing { start: 7, end: 13 });
    assert_eq!(
        send_until_presence(&mut alice, &mut bob, &typing, &expected_typing),
        expected_typing,
        "typing must remain a transient event rather than becoming a selection"
    );
}
