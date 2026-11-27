//! What a window does with the workspace's committed copy of a record: merging it, deferring it
//! until its owner is here, and going on with what it holds when the copy cannot be read.

use super::framework_history_tests::RoutedOwners;

/// The workspace's copy of a record arrives as bytes this app does not read: the window says the
/// shared copy could not be read here, keeps the record it holds, and does not go on waiting on
/// bytes no pass could merge, which would stop the route delivering anything else.
#[test]
fn an_unreadable_committed_copy_is_refused_and_said_in_the_status() {
    let mut fixture = RoutedOwners::new();
    let route = fixture.bindings[0].broker_route().unwrap();
    let read = tilo_ui_base::collaboration::CommittedDocument::for_test(vec![0xff; 8], None);
    fixture
        .app
        .queue_committed_document(route.clone(), read.clone());
    assert!(
        !read.was_applied(),
        "a copy it cannot read is never acknowledged"
    );
    assert!(
        read.was_refused(),
        "the owner tells the reader it cannot read the copy"
    );
    assert!(
        fixture.app.committed_documents.is_empty(),
        "and never keeps it to try the same bytes again"
    );
    let said = tilo_ui_base::i18n::I18nManager::lookup_plain("history-sync-unreadable");
    assert_eq!(
        fixture.app.document_sync_status().as_deref(),
        Some(said.as_str()),
        "the window says the shared copy could not be read here"
    );
}

/// The workspace's copy of a resource, whole, delivered a second time after the window took it in:
/// the window has it already, so the second delivery is passed over, never refused as a copy it
/// cannot read.
#[test]
fn a_whole_copy_delivered_again_after_it_was_taken_in_is_not_refused() {
    use automerge::transaction::Transactable as _;
    let mut fixture = RoutedOwners::new();
    let id = fixture.bindings[0].checkout.id.clone();
    let route = fixture.bindings[0].broker_route().unwrap();
    let mut doc = fixture.records.read_checked(&id, Clone::clone).unwrap();
    doc.set_actor(automerge::ActorId::from(b"committed-peer".as_slice()));
    doc.put(automerge::ROOT, "remote", "written elsewhere")
        .unwrap();
    doc.commit();
    let read = tilo_ui_base::collaboration::CommittedDocument::for_test(doc.save(), None);
    fixture
        .app
        .queue_committed_document(route.clone(), read.clone());
    assert!(read.was_applied(), "the window takes the copy in");
    fixture.app.queue_committed_document(route, read.clone());
    assert!(!read.was_refused(), "the second delivery is not refused");
    assert_eq!(
        fixture.app.document_sync_status(),
        None,
        "nor does the window say the copy could not be read"
    );
}

/// A record no owner holds, which this window asked the workspace for (a record an app keeps
/// beside its own): its committed read arrives, so the stores look again and what asked for it
/// opens it. The read waits here for an owner the record itself would bring, so without the wake
/// the record never opens — and what an app keeps beside its own is never published.
#[test]
fn a_record_no_owner_holds_that_arrives_is_looked_for_again() {
    let mut fixture = RoutedOwners::new();
    let workspace = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000077").unwrap();
    let route = tilo_ui_base::collaboration::RouteSpec::document(
        "so.tilo.test-probe",
        workspace,
        "uuid:587bfde5-3070-4a56-9b1b-bb5abd3a599b",
    );
    let read = tilo_ui_base::collaboration::CommittedDocument::for_test(vec![0xff; 8], None);
    let before = fixture.records.kept_revision();
    fixture.app.queue_committed_document(route, read);
    assert!(
        fixture.records.kept_revision() > before,
        "the stores look again for the record that arrived"
    );
}
