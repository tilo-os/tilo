use super::*;
use ui::permissions::UserId;
use uuid::Uuid;

fn canonical() -> (AutoCommit, NotesApp, [PageId; 3]) {
    let mut doc = AutoCommit::new();
    crate::notebook::initialize_empty(&mut doc).unwrap();
    let pages = [
        Uuid::from_u128(11),
        Uuid::from_u128(12),
        Uuid::from_u128(13),
    ];
    for (order, id) in pages.iter().copied().enumerate() {
        NotesDoc::upsert_page_meta(
            &mut doc,
            &crate::model::PageMeta {
                id,
                title: format!("Page {order}"),
                parent: None,
                order: format!("{}", char::from(b'a' + order as u8)),
            },
        );
    }
    doc.commit();
    let app = NotesApp::from_document(&doc, &UserId("continuation-author".into())).unwrap();
    (doc, app, pages)
}

#[test]
fn continuation_navigation_identity_covers_actual_destination_not_availability() {
    let (mut doc, mut app, [a, b, c]) = canonical();
    app.route_history = ui::NavStack::new(None);
    app.route_history.push(Some(a));
    app.route_history.push(Some(b));
    app.workspace.set_selected(Some(b));
    let original = app.notes_invocation_domain(&doc);
    let available = (
        app.route_history.can_go_back(),
        app.route_history.can_go_forward(),
    );
    assert!(app.navigate_history(ui::NavAction::Back));
    // Canonical navigation queues the real Select event for the owner model pass.
    app.frame(&mut doc, &automerge::ROOT);
    assert_eq!(app.workspace.selected(), Some(a));
    app.route_history = ui::NavStack::new(None);
    app.route_history.push(Some(c));
    app.route_history.push(Some(b));
    app.workspace.set_selected(Some(b));
    assert_eq!(
        available,
        (
            app.route_history.can_go_back(),
            app.route_history.can_go_forward()
        )
    );
    assert_ne!(original, app.notes_invocation_domain(&doc));
    assert!(app.navigate_history(ui::NavAction::Back));
    app.frame(&mut doc, &automerge::ROOT);
    assert_eq!(app.workspace.selected(), Some(c));
}
