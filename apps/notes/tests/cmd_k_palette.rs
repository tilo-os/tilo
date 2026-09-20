//! Cmd-K / Ctrl-K opens the page jump palette and Enter navigates to the
//! selected page.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use notes::NotesApp;
use tempfile::TempDir;

#[test]
fn cmd_k_palette_lists_workspace_pages() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    // After spawn the workspace has the Welcome page; the palette
    // should have it seeded.
    let pal = app.palette_mut();
    pal.set_visible(true);
    assert!(pal.is_visible());
    assert_eq!(pal.visible_len(), 1);
    assert_eq!(pal.highlighted().unwrap().title, "Welcome");
}

#[test]
fn selecting_a_page_changes_app_selected() {
    use notes::sidebar::OutEvent;
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let root_id = app.workspace_pages_for_test()[0].id;
    // Create a second page so the palette has two entries.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    // Now the workspace has two pages — Welcome + Untitled.
    assert_eq!(app.workspace_pages_for_test().len(), 2);

    // Navigate explicitly to the root via the public API.
    app.navigate_to(root_id);
    assert_eq!(app.selected_for_test(), Some(root_id));
}

#[test]
fn body_search_opens_matching_block_without_authoring_a_change() {
    use crate::component_support::*;
    use notes::{actions, doc::NotesDoc};
    let f = LegacyFixture::rich();
    let body = format!(
        "{}\n\nThe zirconium migration checklist",
        "Ordinary paragraph.\n\n".repeat(45)
    );
    f.write_page(f.child, body.as_bytes());
    let checkout = imported(&f);
    let (heads, matching) = f
        .records
        .read(&checkout.id, |doc| {
            (
                doc.base_heads(),
                NotesDoc::page_block_snapshots(doc, f.child)
                    .into_iter()
                    .find(|(_, text)| text.contains("zirconium"))
                    .unwrap()
                    .0,
            )
        })
        .unwrap();
    let mut a = driver(&f, 91, &checkout);
    invoke(&mut a, 91, actions::page_resource_id(f.root), "open", None);
    a.set_modifiers(&["ctrl".into()]);
    a.key("k");
    a.set_modifiers(&[]);
    a.type_text("zirconium");
    let visible = a.inspection_json().unwrap();
    assert!(
        visible.contains("Launch — The zirconium migration checklist"),
        "body hit must name its note and matching content: {visible}"
    );
    a.key("Enter");
    a.relayout();
    a.expect(&ui::headless::SemanticExpectation::named("note_title").value("Launch"))
        .unwrap();
    assert_eq!(revision(&f, &checkout), 0);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| doc.base_heads())
            .unwrap(),
        heads
    );
    a.close_components();
    let reports = a.take_component_close_reports();
    assert_eq!(reports.len(), 1);
    assert_closed(&reports[0]);
    let saved = reports[0].recovery.as_ref().unwrap();
    let local: notes::local_view::NotesLocalViewV1 =
        serde_json::from_slice(&saved.application.as_ref().unwrap().payload).unwrap();
    assert_eq!(local.selected_page, Some(f.child));
    assert_eq!(local.caret.as_ref().unwrap().block_id, matching);
    assert!(
        local.editor_scroll > 0.0,
        "a late body hit must actually scroll into view"
    );
}
