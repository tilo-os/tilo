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
                order: order as u32,
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
    assert!(app.navigate_history(ui::NavAction::Back, &doc).unwrap());
    // Canonical navigation queues the real Select event for the owner model pass.
    app.sync(&mut doc, &automerge::ROOT);
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
    assert!(app.navigate_history(ui::NavAction::Back, &doc).unwrap());
    app.sync(&mut doc, &automerge::ROOT);
    assert_eq!(app.workspace.selected(), Some(c));
}

fn pending() -> PendingMarkdownImport {
    PendingMarkdownImport {
        path: PathBuf::from("same-selected-file.md"),
        body: "same loaded bytes".into(),
        assets: Some(Vec::new()),
    }
}

#[test]
fn continuation_replaced_identical_pending_import_is_a_different_job() {
    let (mut doc, mut app, _) = canonical();
    app.stage_pending_import(pending()).unwrap();
    let first = app.notes_invocation_domain(&doc);
    let bytes = doc.clone().save();
    app.pending_markdown_source.take();
    app.stage_pending_import(pending()).unwrap();
    assert_ne!(first, app.notes_invocation_domain(&doc));
    assert_eq!(
        doc.save(),
        bytes,
        "job identity is local, not a notebook write"
    );
}

#[test]
fn continuation_pending_import_generation_overflow_refuses_without_replacement() {
    let (doc, mut app, _) = canonical();
    app.pending_import_generation = u64::MAX;
    let original = app.notes_invocation_domain(&doc);
    assert_eq!(
        app.stage_pending_import(pending()).unwrap_err(),
        "Markdown import generation exhausted"
    );
    assert!(app.pending_markdown_source.is_none());
    assert_eq!(app.pending_import_generation, u64::MAX);
    assert_eq!(app.notes_invocation_domain(&doc), original);
}
