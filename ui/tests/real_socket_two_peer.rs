//! Two-peer RPC integration test for the production typed collaboration client.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::thread;
use std::time::Duration;

use crate::support::typed_collab::TypedBroker;
use ui::collaboration::{
    CollabEvent, CollabRequest, CollaborationManager, PresencePayload, RouteSpec,
};
use ui::presence::PresenceKey;
use uuid::Uuid;

#[test]
fn typed_socket_fans_out_incremental_and_presence() {
    let broker = TypedBroker::new();
    let mut peer_a = broker.manager("peer-a");
    let mut peer_b = broker.manager("peer-b");
    thread::sleep(Duration::from_millis(100));

    peer_a.send(CollabRequest::SyncIncremental {
        data: b"hello".to_vec(),
    });
    thread::sleep(Duration::from_millis(100));
    let events = peer_b.poll();
    assert_eq!(
        events
            .iter()
            .filter(
                |event| matches!(event, CollabEvent::CRDTIncremental { data } if data == b"hello")
            )
            .count(),
        1,
        "peer B receives the incremental exactly once: {events:?}",
    );
    assert!(
        peer_a.poll().iter().all(
            |event| !matches!(event, CollabEvent::CRDTIncremental { data } if data == b"hello")
        ),
        "the sender is not echoed"
    );

    peer_a.send(CollabRequest::Presence {
        key: PresenceKey::Workspace { id: "77".into() },
        payload: PresencePayload::Arrived,
    });
    thread::sleep(Duration::from_millis(100));
    let events = peer_b.poll();
    assert!(events.iter().any(|event| matches!(
        event,
        CollabEvent::RemotePresenceUpdate { user_id, key: PresenceKey::Workspace { id }, payload: PresencePayload::Arrived }
            if user_id == "peer-a" && id == "77"
    )), "typed presence keeps attribution and key: {events:?}");
}

#[test]
fn route_specific_send_never_leaks_onto_the_primary_route() {
    let broker = TypedBroker::new();
    let mut sender = broker.manager("agent");
    let mut primary_observer = broker.manager("primary-observer");
    let mut photos = CollaborationManager::for_route_fd(
        broker.connect("photos"),
        "photos",
        Uuid::nil(),
        None,
        Some("photos".into()),
    )
    .expect("photos route connects");
    thread::sleep(Duration::from_millis(100));

    sender.send_to_route(
        RouteSpec::app("photos", Uuid::nil()),
        CollabRequest::SyncIncremental {
            data: b"photo-edit".to_vec(),
        },
    );
    thread::sleep(Duration::from_millis(100));

    assert!(photos.poll().iter().any(
        |event| matches!(event, CollabEvent::CRDTIncremental { data } if data == b"photo-edit")
    ));
    assert!(primary_observer.poll().iter().all(
        |event| !matches!(event, CollabEvent::CRDTIncremental { data } if data == b"photo-edit")
    ));
}
