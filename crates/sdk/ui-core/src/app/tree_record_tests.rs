use super::framework_history_tests::RoutedOwners;
use crate::collaboration::CollabRequest;
use automerge::{ROOT, transaction::Transactable as _};

/// A record of an open record's tree (a chat's message part, a container's
/// child) syncs on its own route though no view owns it, and starts with
/// its whole document: on a desktop a part one person wrote never left
/// their device, so nobody else read their messages.
#[test]
fn a_record_of_an_open_records_tree_syncs_on_its_own_route_from_a_snapshot() {
    let mut fixture = RoutedOwners::new();
    let base =
        crate::record::RecordId::derived("messages", &crate::record::RecordId::room_default());
    let part = crate::record::RecordCheckout::new(
        base.part(0),
        crate::record::RecordKind::new("test.part"),
    );
    fixture
        .records
        .reserve_record(&part)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "messages", "first")
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
    fixture.app.refresh_document_routes();
    let route = fixture
        .app
        .document_routes
        .owners
        .iter()
        .find(|entry| entry.route.record.as_deref() == Some(part.id.as_str()))
        .map(|entry| entry.route.clone())
        .expect("the part has a route of its own");
    // Made by a running action: the part's route only now joins. Finishing
    // the action does not wait for it, and nothing fails.
    assert!(
        fixture.app.queue_documents_before_terminal(),
        "a route still opening is no publication failure"
    );
    let deadline = tilo_test_env::hang_deadline();
    while !fixture
        .app
        .collaboration
        .manager
        .document_route_publishable_for_test(&route)
    {
        assert!(
            crate::time::now() < deadline,
            "the part's route never opened"
        );
        std::thread::sleep(crate::time::Duration::from_millis(5));
    }
    fixture.app.publish_owned_documents();
    let first = fixture
        .app
        .document_routes
        .sent
        .iter()
        .find(|(sent, _)| sent == &route)
        .map(|(_, request)| request.clone())
        .expect("the part is published");
    assert!(
        matches!(
            first,
            CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Snapshot(_),
                ..
            }
        ),
        "a new route starts with the whole document"
    );
}

/// The surface the person acts through never waits for the route of a
/// record held only for a tree: a chat's next part opening its route after
/// a post left every input meanwhile acting through a stale surface.
#[test]
fn a_record_held_for_a_tree_never_holds_back_the_published_surface() {
    let mut fixture = RoutedOwners::new();
    fixture.app.collaboration.manager =
        crate::collaboration::CollaborationManager::native_initial_named_fixture_for_test(
            fixture.bindings[0].broker_route().unwrap(),
            automerge::AutoCommit::new().save(),
        );
    fixture.app.actions.bound_broker_route_for_test = None;
    let deadline = tilo_test_env::hang_deadline();
    while !fixture.app.published_action_history_ready() {
        fixture.app.poll_and_apply_collab_events();
        assert!(
            crate::time::now() < deadline,
            "the window's own routes never opened"
        );
        std::thread::yield_now();
    }
    let base =
        crate::record::RecordId::derived("messages", &crate::record::RecordId::room_default());
    let part = crate::record::RecordCheckout::new(
        base.part(1),
        crate::record::RecordKind::new("test.part"),
    );
    fixture
        .records
        .reserve_record(&part)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "messages", "next")
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
    assert!(
        fixture.app.published_action_history_ready(),
        "a held part's route opening holds nothing back"
    );
    assert!(
        fixture
            .app
            .document_routes
            .owners
            .iter()
            .any(|entry| entry.route.record.as_deref() == Some(part.id.as_str())),
        "the part has a route of its own"
    );
}
