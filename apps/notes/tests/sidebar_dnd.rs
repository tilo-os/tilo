//! Drag-to-reorder / re-parent and the right-click context menu, driven
//! through `NotesApp::handle_event` on a live, settled taffy layout — the
//! same five-pass cycle `ui::App` runs. These exercise the real pointer
//! hit-tests + the toolkit `TreeView` reorder, not internal helpers.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::framework_support::{add_note, notes, open, root_order, send as dispatch, surface};
use notes::sidebar::OutEvent;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::NamedKey;

use ui::synthetic_input::cursor_moved;

fn mouse(state: ElementState, button: MouseButton) -> WindowEvent {
    ui::synthetic_input::mouse_input(state, button)
}

use ui::synthetic_input::named_key;

/// Notes with three root notes: [Welcome, One, Two].
fn three_page_app() -> (TestApp<NotesShell>, PageId, PageId, PageId) {
    let mut app = notes();
    let welcome = add_note(&mut app, "Welcome", "");
    let one = add_note(&mut app, "One", "");
    let two = add_note(&mut app, "Two", "");
    open(&mut app, two);
    (app, welcome, one, two)
}

fn row(app: &mut TestApp<NotesShell>, index: usize) -> vello::kurbo::Point {
    surface(app, |notes| notes.sidebar_row_center_for_test(index)).unwrap()
}

fn drag_active(app: &mut TestApp<NotesShell>) -> bool {
    surface(app, |notes| notes.sidebar_drag_active_for_test())
}

fn menu_open(app: &mut TestApp<NotesShell>) -> bool {
    surface(app, |notes| notes.sidebar_menu_open_for_test())
}

#[test]
fn sidebar_drags_preview_cancel_and_reparent() {
    eprintln!("phase drag_previews_live_commits_only_on_drop");
    {
        let (mut app, welcome, one, two) = three_page_app();
        assert_eq!(root_order(&app), vec![welcome, one, two]);

        let r0 = row(&mut app, 0);
        let r2 = row(&mut app, 2);

        // Press on row 0 (Welcome), then drag down below the last row.
        dispatch(&mut app, &cursor_moved(r0.x, r0.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Left));
        dispatch(&mut app, &cursor_moved(r0.x, r2.y + 8.0));

        // Mid-drag: the tree shows where the page would land (an insertion line
        // below the last row) but the model is NOT mutated yet — commit only
        // happens on drop.
        assert!(drag_active(&mut app), "drag promoted past the threshold");
        let indicator = surface(&mut app, |notes| notes.sidebar_drop_indicator_for_test())
            .expect("the toolkit shows the live drop position");
        assert!(
            indicator.height() < 4.0 && indicator.center().y > r2.y,
            "an insertion line below the last row, not a nest target: {indicator:?}"
        );
        assert_eq!(
            root_order(&app),
            vec![welcome, one, two],
            "the CRDT order is untouched until the drop"
        );

        // Drop → the move commits.
        dispatch(&mut app, &mouse(ElementState::Released, MouseButton::Left));
        assert!(!drag_active(&mut app), "the drag session cleared on drop");
        assert_eq!(
            root_order(&app),
            vec![one, two, welcome],
            "Welcome dropped to the end of its sibling group"
        );
    }

    eprintln!("phase drag_cancel_with_escape_restores_order");
    {
        let (mut app, welcome, one, two) = three_page_app();

        let r0 = row(&mut app, 0);
        let r2 = row(&mut app, 2);
        dispatch(&mut app, &cursor_moved(r0.x, r0.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Left));
        dispatch(&mut app, &cursor_moved(r0.x, r2.y + 8.0));
        assert!(drag_active(&mut app));

        // Escape aborts — the order snaps back and no move commits.
        let handled = dispatch(&mut app, &named_key(NamedKey::Escape));
        assert!(handled, "Escape consumes the in-flight drag");
        assert!(!drag_active(&mut app));
        assert_eq!(
            root_order(&app),
            vec![welcome, one, two],
            "cancel restores the order"
        );
    }

    eprintln!("phase drag_onto_a_row_reparents_it_as_a_child");
    {
        let (mut app, welcome, one, _two) = three_page_app();

        // Drag row 1 (One) up onto the middle of row 0 (Welcome) → nest it.
        let r1 = row(&mut app, 1);
        let r0 = row(&mut app, 0);
        dispatch(&mut app, &cursor_moved(r1.x, r1.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Left));
        // Land squarely on Welcome's centre band (the "into this page" zone).
        dispatch(&mut app, &cursor_moved(r0.x, r0.y));
        assert!(drag_active(&mut app));
        dispatch(&mut app, &mouse(ElementState::Released, MouseButton::Left));

        let pages = crate::framework_support::shown(&app).pages().to_vec();
        let one_meta = pages.iter().find(|p| p.id == one).unwrap();
        assert_eq!(
            one_meta.parent,
            Some(welcome),
            "dropping One onto Welcome re-parents it as Welcome's child"
        );
    }
}

#[test]
fn sidebar_context_menu_deletes_and_deep_copies_pages() {
    eprintln!("phase context_menu_delete_removes_the_page");
    {
        let (mut app, welcome, one, _two) = three_page_app();
        let before = crate::framework_support::shown(&app).pages().len();

        // Right-click row 1 (One) → the context menu opens.
        let r1 = row(&mut app, 1);
        dispatch(&mut app, &cursor_moved(r1.x, r1.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Right));
        assert!(menu_open(&mut app), "right-click opens the context menu");

        // ArrowUp wraps to the last action row (Delete); Enter fires it.
        dispatch(&mut app, &named_key(NamedKey::ArrowUp));
        dispatch(&mut app, &named_key(NamedKey::Enter));
        // Undo takes a deleted page back, so nobody is asked.
        assert!(app.asking().is_none(), "nothing asks first");

        let pages = crate::framework_support::shown(&app).pages().to_vec();
        assert_eq!(pages.len(), before - 1, "Delete removed exactly one page");
        assert!(
            pages.iter().all(|p| p.id != one),
            "the targeted page is gone"
        );
        assert!(
            pages.iter().any(|p| p.id == welcome),
            "other pages untouched"
        );
        assert!(!menu_open(&mut app), "the menu closed after selection");
    }

    eprintln!("phase context_menu_duplicate_deep_copies_the_page");
    {
        let (mut app, _welcome, one, _two) = three_page_app();
        // Rename One so the copy is identifiable, and give it a child so the
        // duplicate must deep-copy the subtree.
        surface(&mut app, |notes| {
            notes.apply_event_for_test(OutEvent::Rename {
                id: one,
                new_title: "Report".into(),
            });
        });
        surface(&mut app, |notes| {
            notes.apply_event_for_test(OutEvent::CreateChild { parent: Some(one) });
        });
        let before = crate::framework_support::shown(&app).pages().len();

        // Right-click row 1 (Report) → menu; ArrowDown ×3 lands on Duplicate.
        let r1 = row(&mut app, 1);
        dispatch(&mut app, &cursor_moved(r1.x, r1.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Right));
        assert!(menu_open(&mut app));
        for _ in 0..3 {
            dispatch(&mut app, &named_key(NamedKey::ArrowDown));
        }
        dispatch(&mut app, &named_key(NamedKey::Enter));

        let pages = crate::framework_support::shown(&app).pages().to_vec();
        assert_eq!(
            pages.len(),
            before + 2,
            "Duplicate deep-copies the page AND its one child (+2)"
        );
        assert!(
            pages.iter().any(|p| p.title == "Report copy"),
            "the duplicate carries a ' copy' title; titles: {:?}",
            pages.iter().map(|p| p.title.as_str()).collect::<Vec<_>>()
        );
    }
}
