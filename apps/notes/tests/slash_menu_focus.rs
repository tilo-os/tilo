//! The `/` block menu must capture keyboard focus: while it's open, ↑/↓
//! navigate the menu — not the page list behind it — Enter inserts the
//! chosen block, Esc closes. Regression guard for the case where the
//! sidebar retained focus (after a row click) and swallowed the arrows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::collections::HashMap;

use notes::NotesApp;
use notes::sidebar::OutEvent;
use taffy::prelude::*;
use tempfile::TempDir;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{BlockKind, Widget};
use vello::kurbo::Point;
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

fn settle_layout(
    app: &mut NotesApp,
    state: &mut ui::State,
    w: u16,
    h: u16,
) -> Result<(TaffyTree<()>, NodeId), String> {
    let theme = Theme::dark();
    let mut text_renderer = TextRenderer::new();
    app.sync(&mut state.doc, &state.root);
    let mut result: Option<(TaffyTree<()>, NodeId)> = None;
    for _ in 0..5 {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(f32::from(w)),
                        height: Dimension::length(f32::from(h)),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .map_err(|e| e.to_string())?;
        {
            let measure_map = node_map.clone();
            let tr = &mut text_renderer;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(f32::from(w)),
                        height: AvailableSpace::Definite(f32::from(h)),
                    },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        let settled = !app.take_structure_dirty();
        result = Some((taffy, child));
        if settled {
            break;
        }
    }
    result.ok_or_else(|| "layout never settled".to_string())
}

fn dispatch(
    app: &mut NotesApp,
    state: &mut ui::State,
    taffy: &TaffyTree<()>,
    child: NodeId,
    event: &WindowEvent,
) -> bool {
    let no_mods = Modifiers::default();
    let mut handled = false;
    let mut focus_target = None;
    app.handle_event(
        event,
        1.0,
        &no_mods,
        &mut state.doc,
        &state.root,
        &mut handled,
        &mut focus_target,
        child,
        taffy,
    );
    handled
}

#[test]
fn slash_menu_arrows_navigate_the_menu_not_the_page_list() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    // A second page so a stray ArrowDown on the page list would be visible
    // (it would move the selection off Welcome).
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    // Click Welcome's row: this selects it AND gives the sidebar focus,
    // while loading the page focuses the editor too — the exact state in
    // which the sidebar used to steal the arrows.
    let r0 = app.sidebar_row_center_for_test(0).unwrap();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &cursor_moved(r0.x, r0.y),
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &mouse(ElementState::Pressed),
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &mouse(ElementState::Released),
    );
    assert_eq!(app.selected_for_test(), Some(welcome));

    // Type `/` in the editor → the block menu opens.
    dispatch(&mut app, &mut state, &taffy, child, &char_key("/"));
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
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &named_key(NamedKey::ArrowDown),
    );
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
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &named_key(NamedKey::Escape),
    );
    assert!(
        !app.editor_slash_menu_visible_for_test(),
        "Escape closes the block menu"
    );
}

#[test]
fn slash_menu_enter_inserts_the_chosen_block() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    // A fresh empty child page gives a clean single-paragraph editor.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    // Open the menu, arrow to Heading 1, Enter to convert the block.
    dispatch(&mut app, &mut state, &taffy, child, &char_key("/"));
    assert!(app.editor_slash_menu_visible_for_test());
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &named_key(NamedKey::ArrowDown),
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &named_key(NamedKey::Enter),
    );

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
