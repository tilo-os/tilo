use crate::support::{document_mutation, typed_collab::TypedBroker};
use tilo_ui::collaboration::{CollabEvent, CollabRequest, PresencePayload, RouteSpec};
use tilo_ui::presence::PresenceKey;
use uuid::Uuid;

#[test]
fn typed_socket_fans_out_on_its_own_route_only() {
    // Case `typed_socket_fans_out_incremental_and_presence`.
    {
        let broker = TypedBroker::new();
        let mut peer_a = broker.manager("peer-a");
        let mut peer_b = broker.manager("peer-b");

        peer_a.send(CollabRequest::document_incremental(b"hello".to_vec()));
        let mut events = Vec::new();
        tilo_test_env::wait_until("the incremental at peer B", || {
            events.extend(peer_b.poll());
            events.iter().any(|event| {
                matches!(
                    document_mutation(event),
                    Some(collab_types::DocumentMutation::Incremental(data)) if data == b"hello"
                )
            })
        });
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        document_mutation(event),
                        Some(collab_types::DocumentMutation::Incremental(data)) if data == b"hello"
                    )
                })
                .count(),
            1,
            "peer B receives the incremental exactly once: {events:?}",
        );
        assert!(
            peer_a.poll().iter().all(|event| {
                !matches!(
                    document_mutation(event),
                    Some(collab_types::DocumentMutation::Incremental(data)) if data == b"hello"
                )
            }),
            "the sender is not echoed"
        );

        peer_a.send(CollabRequest::Presence {
            key: PresenceKey::Workspace { id: "77".into() },
            payload: PresencePayload::Arrived,
        });
        tilo_test_env::wait_until("peer A's typed presence at peer B", || {
            peer_b.poll().iter().any(|event| matches!(
                event,
                CollabEvent::RemotePresenceUpdate { user_id, key: PresenceKey::Workspace { id }, payload: PresencePayload::Arrived }
                    if user_id == "peer-a" && id == "77"
            ))
        });
    }

    // Case `route_specific_send_never_leaks_onto_the_primary_route`.
    {
        let broker = TypedBroker::new();
        let mut sender = broker.manager("agent");
        let mut primary_observer = broker.manager("primary-observer");
        let mut photos = broker.route_manager("photos", "photos");

        // A route refuses documents until it is joined and its document mode is
        // resolved; the refused attempt starts that join.
        tilo_test_env::wait_until("the photos route to become publishable", || {
            sender
                .try_send_to_route(
                    RouteSpec::app("photos", Uuid::nil()),
                    CollabRequest::document_incremental(b"photo-edit".to_vec()),
                )
                .is_ok()
        });
        tilo_test_env::wait_until("the photo edit on the photos route", || {
            photos.poll().iter().any(|event| {
                matches!(
                    document_mutation(event),
                    Some(collab_types::DocumentMutation::Incremental(data)) if data == b"photo-edit"
                )
            })
        });
        assert!(primary_observer.poll().iter().all(|event| !matches!(
            document_mutation(event),
            Some(collab_types::DocumentMutation::Incremental(data)) if data == b"photo-edit"
        )));
    }
}
