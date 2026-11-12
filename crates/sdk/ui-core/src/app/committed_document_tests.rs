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
    let read = crate::collaboration::CommittedDocument::for_test(vec![0xff; 8], None);
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
    let said = crate::i18n::I18nManager::lookup_plain("history-sync-unreadable");
    assert_eq!(
        fixture.app.document_sync_status().as_deref(),
        Some(said.as_str()),
        "the window says the shared copy could not be read here"
    );
}
