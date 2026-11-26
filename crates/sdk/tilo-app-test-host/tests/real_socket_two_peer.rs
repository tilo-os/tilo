use crate::support_collab::{TypedBroker, change_carrying, changes_of};
use tilo_ui_base::{collaboration::CollabEvent, collaboration::CollabRequest, collaboration::PresencePayload, collaboration::RouteSpec};
use tilo_ui_base::presence::PresenceKey;
use uuid::Uuid;

#[test]
fn typed_socket_fans_out_on_its_own_route_only() {
    {
        let broker = TypedBroker::new();
        let mut peer_a = broker.manager("peer-a");
        let mut peer_b = broker.manager("peer-b");

        // A record takes real changes: the store reads every write as one.
        let hello = change_carrying(b"hello".to_vec());
        peer_a.send(CollabRequest::document_incremental(hello.clone()));
        let mut events = Vec::new();
        tilo_test_env::wait_until("the incremental at peer B", || {
            events.extend(peer_b.poll());
            events.iter().any(|event| {
                matches!(
                    event.document_mutation().cloned(),
                    Some(collab_types::DocumentMutation::Incremental(data)) if changes_of(&data) == changes_of(&hello)
                )
            })
        });
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event.document_mutation().cloned(),
                        Some(collab_types::DocumentMutation::Incremental(data)) if changes_of(&data) == changes_of(&hello)
                    )
                })
                .count(),
            1,
            "peer B receives the incremental exactly once: {events:?}",
        );
        assert!(
            peer_a.poll().iter().all(|event| {
                !matches!(
                    event.document_mutation().cloned(),
                    Some(collab_types::DocumentMutation::Incremental(data)) if changes_of(&data) == changes_of(&hello)
                )
            }),
            "the sender is not echoed"
        );

        peer_a.send(CollabRequest::Say {
            key: PresenceKey::Workspace { id: "77".into() },
            payload: PresencePayload::Arrived.to_live().unwrap(),
        });
        tilo_test_env::wait_until("peer A's typed presence at peer B", || {
            peer_b.poll().iter().any(|event| matches!(
                event,
                CollabEvent::RemotePresenceUpdate { member, key: PresenceKey::Workspace { id }, payload: PresencePayload::Arrived }
                    if *member == tilo_ui_base::permissions::MemberId::person("peer-a") && id == "77"
            ))
        });
    }

    {
        let broker = TypedBroker::new();
        let mut sender = broker.manager("agent");
        let mut primary_observer = broker.manager("primary-observer");
        let mut photos = broker.route_manager("photos", "photos");
        let photo_edit = change_carrying(b"photo-edit".to_vec());

        // A route refuses documents until it is joined and its document mode is
        // resolved; the refused attempt starts that join.
        tilo_test_env::wait_until("the photos route to become publishable", || {
            sender
                .try_send_to_route(
                    RouteSpec::app("photos", Uuid::nil()),
                    CollabRequest::document_incremental(photo_edit.clone()),
                )
                .is_ok()
        });
        tilo_test_env::wait_until("the photo edit on the photos route", || {
            photos.poll().iter().any(|event| {
                matches!(
                    event.document_mutation().cloned(),
                    Some(collab_types::DocumentMutation::Incremental(data)) if changes_of(&data) == changes_of(&photo_edit)
                )
            })
        });
        assert!(primary_observer.poll().iter().all(|event| !matches!(
            event.document_mutation().cloned(),
            Some(collab_types::DocumentMutation::Incremental(data)) if changes_of(&data) == changes_of(&photo_edit)
        )));
    }
}
