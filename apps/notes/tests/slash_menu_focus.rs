//! The `/` block menu must capture keyboard focus: while it's open, ↑/↓
//! navigate the menu — not the page list behind it — Enter inserts the
//! chosen block, Esc closes. Regression guard for the case where the
//! sidebar retained focus (after a row click) and swallowed the arrows.

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
use ui::widget::BlockKind;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey, SmolStr};

fn cursor_moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

fn mouse(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state,
        button: MouseButton::Left,
    }
}

fn char_key(s: &str) -> WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(SmolStr::new(s));
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(SmolStr::new(s));
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
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

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn slash_menu_owns_arrow_and_enter_keys() {
    eprintln!("phase slash_menu_arrows_navigate_the_menu_not_the_page_list");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let welcome = app.workspace_pages_for_test()[0].id;
        // A second page so a stray ArrowDown on the page list would be visible
        // (it would move the selection off Welcome).
        app.apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.sync_sidebar_for_test();

        let state = ui::State::new();
        let mut app = settle_layout(app, state, 1200, 800);

        // Click Welcome's row: this selects it AND gives the sidebar focus,
        // while loading the page focuses the editor too — the exact state in
        // which the sidebar used to steal the arrows.
        let r0 = app.sidebar_row_center_for_test(0).unwrap();
        dispatch(&mut app, &cursor_moved(r0.x, r0.y));
        dispatch(&mut app, &mouse(ElementState::Pressed));
        dispatch(&mut app, &mouse(ElementState::Released));
        assert_eq!(app.selected_for_test(), Some(welcome));

        // Type `/` in the editor → the block menu opens.
        dispatch(&mut app, &char_key("/"));
        assert!(
            app.editor_slash_menu_visible_for_test(),
            "typing `/` opens the block menu"
        );
        assert_eq!(
            app.main_panel_mut()
                .editor()
                .slash_menu()
                .highlighted_kind(),
            Some(&BlockKind::Paragraph),
            "the menu starts on the first row"
        );

        // ArrowDown must move the MENU highlight, not the page selection.
        dispatch(&mut app, &named_key(NamedKey::ArrowDown));
        assert_eq!(
            app.selected_for_test(),
            Some(welcome),
            "the page selection must NOT move while the `/` menu owns the arrows"
        );
        assert_eq!(
            app.main_panel_mut()
                .editor()
                .slash_menu()
                .highlighted_kind(),
            Some(&BlockKind::Heading(1)),
            "ArrowDown advances the block-menu highlight"
        );

        // Escape closes the menu.
        dispatch(&mut app, &named_key(NamedKey::Escape));
        assert!(
            !app.editor_slash_menu_visible_for_test(),
            "Escape closes the block menu"
        );
    }

    eprintln!("phase slash_menu_enter_inserts_the_chosen_block");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        // A fresh empty child page gives a clean single-paragraph editor.
        app.apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.sync_sidebar_for_test();

        let state = ui::State::new();
        let mut app = settle_layout(app, state, 1200, 800);

        // Open the menu, arrow to Heading 1, Enter to convert the block.
        dispatch(&mut app, &char_key("/"));
        assert!(app.editor_slash_menu_visible_for_test());
        dispatch(&mut app, &named_key(NamedKey::ArrowDown));
        dispatch(&mut app, &named_key(NamedKey::Enter));

        assert!(
            !app.editor_slash_menu_visible_for_test(),
            "Enter commits and closes the menu"
        );
        assert!(
            matches!(
                app.main_panel_mut().editor().blocks()[0].kind,
                BlockKind::Heading(1)
            ),
            "Enter inserted/converted the block to the chosen kind (Heading 1)"
        );
    }
}
