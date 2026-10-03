//! Drag-to-reorder / re-parent and the right-click context menu, driven
//! through `NotesApp::handle_event` on a live, settled taffy layout — the
//! same five-pass cycle `ui::App` runs. These exercise the real pointer
//! hit-tests + the toolkit `TreeView` reorder, not internal helpers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use ui::headless::{FrameOptions, Framed};

use notes::NotesApp;
use notes::sidebar::OutEvent;
use tempfile::TempDir;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

fn cursor_moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

fn mouse(state: ElementState, button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state,
        button,
    }
}

fn named_key(key: NamedKey) -> WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Named(key);
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// Mount `app` over `state` and run the production frame at `w`x`h`.
fn settle_layout(app: NotesApp, state: ui::State, w: u16, h: u16) -> Framed<NotesApp> {
    Framed::with_options(
        state,
        app,
        (u32::from(w), u32::from(h)),
        FrameOptions {
            ..FrameOptions::default()
        },
    )
}

fn dispatch(app: &mut Framed<NotesApp>, event: &WindowEvent) -> bool {
    Framed::harness_mut(app).dispatch_to_root_with(event, &Modifiers::default())
}

/// Build a 3-root-page workspace: [Welcome, One, Two].
fn three_page_app() -> Result<(NotesApp, uuid::Uuid, uuid::Uuid, uuid::Uuid), String> {
    let tmp = TempDir::new().map_err(|e| e.to_string())?;
    // Keep the temp dir alive for the whole process by leaking it — the
    // app's background savers hold the path; a hermetic per-test dir.
    let path = tmp.keep();
    let mut app = NotesApp::spawn_with_starter_page(path)?;
    let welcome = app
        .workspace_pages_for_test()
        .first()
        .ok_or_else(|| "no welcome page".to_string())?
        .id;
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let one = app
        .selected_for_test()
        .ok_or_else(|| "no selection after create".to_string())?;
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let two = app
        .selected_for_test()
        .ok_or_else(|| "no selection after create".to_string())?;
    app.sync_sidebar_for_test();
    Ok((app, welcome, one, two))
}

fn root_order(app: &NotesApp) -> Vec<uuid::Uuid> {
    let mut ps = app.workspace_pages_for_test();
    ps.retain(|p| p.parent.is_none());
    ps.sort_by_key(|p| p.order);
    ps.into_iter().map(|p| p.id).collect()
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn sidebar_drags_preview_cancel_and_reparent() {
    eprintln!("phase drag_previews_live_commits_only_on_drop");
    {
        let (app, welcome, one, two) = three_page_app().unwrap();
        let state = ui::State::new();
        let mut app = settle_layout(app, state, 1200, 800);
        assert_eq!(root_order(&app), vec![welcome, one, two]);

        let r0 = app.sidebar_row_center_for_test(0).unwrap();
        let r2 = app.sidebar_row_center_for_test(2).unwrap();

        // Press on row 0 (Welcome), then drag down below the last row.
        dispatch(&mut app, &cursor_moved(r0.x, r0.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Left));
        dispatch(&mut app, &cursor_moved(r0.x, r2.y + 8.0));

        // Mid-drag: the tree shows where the page would land (an insertion line
        // below the last row) but the model is NOT mutated yet — commit only
        // happens on drop.
        assert!(
            app.sidebar_drag_active_for_test(),
            "drag promoted past the threshold"
        );
        let indicator = app
            .sidebar_drop_indicator_for_test()
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
        assert!(
            !app.sidebar_drag_active_for_test(),
            "the drag session cleared on drop"
        );
        assert_eq!(
            root_order(&app),
            vec![one, two, welcome],
            "Welcome dropped to the end of its sibling group"
        );
    }

    eprintln!("phase drag_cancel_with_escape_restores_order");
    {
        let (app, welcome, one, two) = three_page_app().unwrap();
        let state = ui::State::new();
        let mut app = settle_layout(app, state, 1200, 800);

        let r0 = app.sidebar_row_center_for_test(0).unwrap();
        let r2 = app.sidebar_row_center_for_test(2).unwrap();
        dispatch(&mut app, &cursor_moved(r0.x, r0.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Left));
        dispatch(&mut app, &cursor_moved(r0.x, r2.y + 8.0));
        assert!(app.sidebar_drag_active_for_test());

        // Escape aborts — the order snaps back and no move commits.
        let handled = dispatch(&mut app, &named_key(NamedKey::Escape));
        assert!(handled, "Escape consumes the in-flight drag");
        assert!(!app.sidebar_drag_active_for_test());
        assert_eq!(
            root_order(&app),
            vec![welcome, one, two],
            "cancel restores the order"
        );
    }

    eprintln!("phase drag_onto_a_row_reparents_it_as_a_child");
    {
        let (app, welcome, one, _two) = three_page_app().unwrap();
        let state = ui::State::new();
        let mut app = settle_layout(app, state, 1200, 800);

        // Drag row 1 (One) up onto the middle of row 0 (Welcome) → nest it.
        let r1 = app.sidebar_row_center_for_test(1).unwrap();
        let r0 = app.sidebar_row_center_for_test(0).unwrap();
        dispatch(&mut app, &cursor_moved(r1.x, r1.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Left));
        // Land squarely on Welcome's centre band (the "into this page" zone).
        dispatch(&mut app, &cursor_moved(r0.x, r0.y));
        assert!(app.sidebar_drag_active_for_test());
        dispatch(&mut app, &mouse(ElementState::Released, MouseButton::Left));

        let pages = app.workspace_pages_for_test();
        let one_meta = pages.iter().find(|p| p.id == one).unwrap();
        assert_eq!(
            one_meta.parent,
            Some(welcome),
            "dropping One onto Welcome re-parents it as Welcome's child"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn sidebar_context_menu_deletes_and_deep_copies_pages() {
    eprintln!("phase context_menu_delete_removes_the_page");
    {
        let (app, welcome, one, _two) = three_page_app().unwrap();
        let state = ui::State::new();
        let mut app = settle_layout(app, state, 1200, 800);
        let before = app.workspace_pages_for_test().len();

        // Right-click row 1 (One) → the context menu opens.
        let r1 = app.sidebar_row_center_for_test(1).unwrap();
        dispatch(&mut app, &cursor_moved(r1.x, r1.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Right));
        assert!(
            app.sidebar_menu_open_for_test(),
            "right-click opens the context menu"
        );

        // ArrowUp wraps to the last action row (Delete); Enter fires it.
        dispatch(&mut app, &named_key(NamedKey::ArrowUp));
        dispatch(&mut app, &named_key(NamedKey::Enter));

        let pages = app.workspace_pages_for_test();
        assert_eq!(pages.len(), before - 1, "Delete removed exactly one page");
        assert!(
            pages.iter().all(|p| p.id != one),
            "the targeted page is gone"
        );
        assert!(
            pages.iter().any(|p| p.id == welcome),
            "other pages untouched"
        );
        assert!(
            !app.sidebar_menu_open_for_test(),
            "the menu closed after selection"
        );
    }

    eprintln!("phase context_menu_duplicate_deep_copies_the_page");
    {
        let (mut app, _welcome, one, _two) = three_page_app().unwrap();
        // Rename One so the copy is identifiable, and give it a child so the
        // duplicate must deep-copy the subtree.
        app.apply_event_for_test(OutEvent::Rename {
            id: one,
            new_title: "Report".into(),
        });
        app.apply_event_for_test(OutEvent::CreateChild { parent: Some(one) });
        app.sync_sidebar_for_test();
        let before = app.workspace_pages_for_test().len();

        let state = ui::State::new();
        let mut app = settle_layout(app, state, 1200, 800);

        // Right-click row 1 (Report) → menu; ArrowDown ×3 lands on Duplicate.
        let r1 = app.sidebar_row_center_for_test(1).unwrap();
        dispatch(&mut app, &cursor_moved(r1.x, r1.y));
        dispatch(&mut app, &mouse(ElementState::Pressed, MouseButton::Right));
        assert!(app.sidebar_menu_open_for_test());
        for _ in 0..3 {
            dispatch(&mut app, &named_key(NamedKey::ArrowDown));
        }
        dispatch(&mut app, &named_key(NamedKey::Enter));

        let pages = app.workspace_pages_for_test();
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
