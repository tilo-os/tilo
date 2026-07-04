//! Interaction coverage that drives `NotesApp::handle_event` on a live,
//! settled taffy layout — the same five-pass cycle `ui::App` runs — so
//! the tests exercise the *real* event path (focus gates, pointer
//! hit-tests, keyboard routing) rather than internal helper methods.
//!
//! Covers the reachability fixes:
//!   - `?` types into a focused note but opens help when it isn't focused,
//!   - Cmd-N creates a child regardless of which pane holds focus,
//!   - the sidebar "+" button creates a root page,
//!   - Delete / F2 / Arrow keys work once the tree is focused,
//!   - Alt-↑ / Alt-↓ reorder a page among its siblings.

use std::collections::HashMap;

use notes::NotesApp;
use notes::sidebar::OutEvent;
use taffy::prelude::*;
use tempfile::TempDir;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};

fn cursor_moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

fn mouse_press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

/// A printable-character keyboard event (mirrors `text_input`'s test
/// helper — `KeyEvent` has a `pub(crate)` field so it can't be built by
/// struct literal).
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

/// Build + lay out `app` at `w`x`h`, looping until the responsive
/// structure settles, returning the final taffy tree + child node so
/// the caller can dispatch events against a live layout.
fn settle_layout(
    app: &mut NotesApp,
    state: &mut ui::State,
    w: u32,
    h: u32,
) -> (TaffyTree<()>, NodeId) {
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
                        width: Dimension::length(w as f32),
                        height: Dimension::length(h as f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let tr = &mut text_renderer;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w as f32),
                        height: AvailableSpace::Definite(h as f32),
                    },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        let settled = !app.take_structure_dirty();
        result = Some((taffy, child));
        if settled {
            break;
        }
    }
    result.unwrap()
}

fn dispatch(
    app: &mut NotesApp,
    state: &mut ui::State,
    taffy: &TaffyTree<()>,
    child: NodeId,
    modifiers: &Modifiers,
    event: &WindowEvent,
) -> bool {
    let mut handled = false;
    let mut focus_target = None;
    app.handle_event(
        event,
        1.0,
        modifiers,
        &mut state.doc,
        &state.root,
        &mut handled,
        &mut focus_target,
        child,
        taffy,
    );
    handled
}

/// Click the centre of sidebar row `idx` to select it AND give the
/// sidebar keyboard focus (the gate the tree's key commands require).
fn click_row(
    app: &mut NotesApp,
    state: &mut ui::State,
    taffy: &TaffyTree<()>,
    child: NodeId,
    idx: usize,
) {
    let no_mods = Modifiers::default();
    let c = app.sidebar_row_center_for_test(idx).expect("row laid out");
    dispatch(app, state, taffy, child, &no_mods, &cursor_moved(c.x, c.y));
    dispatch(app, state, taffy, child, &no_mods, &mouse_press());
}

#[test]
fn question_mark_types_into_focused_note_and_does_not_open_help() {
    // Regression guard: `?` must be a literal character in a note. The
    // editor holds focus after load, so the global help shortcut must
    // yield to typing.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);
    assert!(app.main_panel_mut().editor_mut().is_focused());

    let no_mods = Modifiers::default();
    let handled = dispatch(&mut app, &mut state, &taffy, child, &no_mods, &char_key("?"));

    assert!(
        !app.shortcuts_visible_for_test(),
        "`?` must NOT open the shortcuts overlay while a note is focused"
    );
    assert!(
        app.editor_text_for_test().contains('?'),
        "`?` must be typed into the focused note; got {:?}",
        app.editor_text_for_test()
    );
    assert!(handled, "the editor should consume the typed `?`");
}

#[test]
fn question_mark_opens_help_when_no_note_is_focused() {
    // With the tree (not the editor) focused, `?` is the help shortcut.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);

    let before = app.editor_text_for_test();
    app.blur_editor_for_test();
    let no_mods = Modifiers::default();
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &char_key("?"));

    assert!(
        app.shortcuts_visible_for_test(),
        "`?` must open the shortcuts overlay when no note is focused"
    );
    assert_eq!(
        app.editor_text_for_test(),
        before,
        "opening help must not mutate the note body"
    );
}

#[test]
fn open_help_overlay_blocks_the_editor() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);

    app.shortcuts_mut().set_visible(true);
    let before = app.editor_text_for_test();
    let no_mods = Modifiers::default();
    let handled = dispatch(&mut app, &mut state, &taffy, child, &no_mods, &char_key("x"));

    assert!(handled, "the overlay must consume keystrokes while visible");
    assert_eq!(
        app.editor_text_for_test(),
        before,
        "no keystroke may reach the editor while the overlay is up"
    );
    assert!(app.shortcuts_visible_for_test());

    // Escape closes it.
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &named_key(NamedKey::Escape));
    assert!(!app.shortcuts_visible_for_test(), "Esc must close the overlay");
}

#[test]
fn cmd_n_creates_child_even_while_editor_is_focused() {
    // The core reachability bug: Cmd-N used to no-op unless the sidebar
    // was focused, but the editor holds focus on load. Handled at the
    // app level it must work regardless.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let root = app.workspace_pages_for_test()[0].id;
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);
    assert!(app.main_panel_mut().editor_mut().is_focused());
    assert_eq!(app.selected_for_test(), Some(root));

    let ctrl = Modifiers::from(ModifiersState::CONTROL);
    let handled = dispatch(&mut app, &mut state, &taffy, child, &ctrl, &char_key("n"));

    assert!(handled, "Cmd-N must be consumed at the app level");
    let pages = app.workspace_pages_for_test();
    assert_eq!(pages.len(), 2, "Cmd-N must add exactly one page");
    let new_id = app.selected_for_test().expect("new child selected");
    assert_ne!(new_id, root);
    let new_meta = pages.iter().find(|p| p.id == new_id).unwrap();
    assert_eq!(
        new_meta.parent,
        Some(root),
        "Cmd-N must create a CHILD of the selected page"
    );
}

#[test]
fn plus_button_creates_a_root_page() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);
    assert_eq!(app.workspace_pages_for_test().len(), 1);

    let no_mods = Modifiers::default();
    let btn = app.sidebar_new_page_btn_center_for_test();
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &cursor_moved(btn.x, btn.y));
    let handled = dispatch(&mut app, &mut state, &taffy, child, &no_mods, &mouse_press());

    assert!(handled, "clicking the + button must be consumed");
    let pages = app.workspace_pages_for_test();
    assert_eq!(pages.len(), 2, "the + button must add exactly one page");
    let new_id = app.selected_for_test().unwrap();
    let new_meta = pages.iter().find(|p| p.id == new_id).unwrap();
    assert_eq!(
        new_meta.parent, None,
        "the + button must create a ROOT-level page"
    );
}

#[test]
fn delete_key_removes_selected_page_after_focusing_tree() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let victim = app.selected_for_test().unwrap();
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);

    // Row 1 is the newly-created second root page. Click focuses the
    // tree AND selects it; then Delete removes it through the gated
    // keyboard path.
    click_row(&mut app, &mut state, &taffy, child, 1);
    assert_eq!(app.selected_for_test(), Some(victim));

    let no_mods = Modifiers::default();
    let handled =
        dispatch(&mut app, &mut state, &taffy, child, &no_mods, &named_key(NamedKey::Delete));

    assert!(handled, "Delete must be consumed by the focused tree");
    let pages = app.workspace_pages_for_test();
    assert_eq!(pages.len(), 1, "Delete must remove the page");
    assert_eq!(pages[0].id, welcome);
    assert_eq!(app.selected_for_test(), Some(welcome), "selection falls back");
}

#[test]
fn delete_key_is_gated_on_tree_focus() {
    // Without focusing the tree (editor holds focus after load), Delete
    // must NOT delete a page — it belongs to the editor. This proves the
    // focus gate is real, not bypassed.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();
    let before = app.workspace_pages_for_test().len();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);
    let no_mods = Modifiers::default();
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &named_key(NamedKey::Delete));

    assert_eq!(
        app.workspace_pages_for_test().len(),
        before,
        "Delete must not delete a page while the editor holds focus"
    );
}

#[test]
fn f2_opens_inline_rename_after_focusing_tree() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);

    click_row(&mut app, &mut state, &taffy, child, 0);
    assert!(!app.sidebar_is_renaming_for_test());

    let no_mods = Modifiers::default();
    let handled =
        dispatch(&mut app, &mut state, &taffy, child, &no_mods, &named_key(NamedKey::F2));
    assert!(handled, "F2 must be consumed by the focused tree");
    assert!(
        app.sidebar_is_renaming_for_test(),
        "F2 must open the inline rename editor"
    );
}

#[test]
fn arrow_keys_move_selection_after_focusing_tree() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let second = app.selected_for_test().unwrap();
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);

    click_row(&mut app, &mut state, &taffy, child, 0);
    assert_eq!(app.selected_for_test(), Some(welcome));

    let no_mods = Modifiers::default();
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &named_key(NamedKey::ArrowDown));
    assert_eq!(
        app.selected_for_test(),
        Some(second),
        "ArrowDown moves selection to the next row"
    );
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &named_key(NamedKey::ArrowUp));
    assert_eq!(
        app.selected_for_test(),
        Some(welcome),
        "ArrowUp moves selection back up"
    );
}

#[test]
fn disclosure_triangle_click_toggles_child_visibility() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    // Give `welcome` a child so it becomes a collapsible branch.
    app.apply_event_for_test(OutEvent::CreateChild { parent: Some(welcome) });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);

    // Collapsed by default: only the `welcome` row is visible.
    assert!(app.sidebar_row_center_for_test(0).is_some());
    assert!(
        app.sidebar_row_center_for_test(1).is_none(),
        "child must be hidden while the branch is collapsed"
    );

    // Click the disclosure triangle (leftmost glyph of row 0).
    let row0 = app.sidebar_row_center_for_test(0).unwrap();
    let no_mods = Modifiers::default();
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &cursor_moved(8.0, row0.y));
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &mouse_press());
    // Re-settle so the re-flattened rows lay out.
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);
    assert!(
        app.sidebar_row_center_for_test(1).is_some(),
        "expanding must reveal the child row through the event path"
    );

    // Collapse again → the child row disappears.
    let row0 = app.sidebar_row_center_for_test(0).unwrap();
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &cursor_moved(8.0, row0.y));
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &mouse_press());
    let _ = settle_layout(&mut app, &mut state, 1200, 800);
    assert!(
        app.sidebar_row_center_for_test(1).is_none(),
        "collapsing must hide the child row again"
    );
}

#[test]
fn double_click_row_opens_inline_rename() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);

    let c = app.sidebar_row_center_for_test(0).expect("row 0 laid out");
    let no_mods = Modifiers::default();
    // Two presses at the same spot in quick succession → double-click.
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &cursor_moved(c.x, c.y));
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &mouse_press());
    assert!(!app.sidebar_is_renaming_for_test(), "single click must not rename");
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &cursor_moved(c.x, c.y));
    dispatch(&mut app, &mut state, &taffy, child, &no_mods, &mouse_press());
    assert!(
        app.sidebar_is_renaming_for_test(),
        "double-click on a row must open the inline rename editor"
    );
}

#[test]
fn alt_arrow_reorders_selected_page_among_siblings() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let p1 = app.selected_for_test().unwrap();
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let p2 = app.selected_for_test().unwrap();
    app.sync_sidebar_for_test();

    // Sanity: creation order is [welcome, p1, p2].
    let order_ids = |app: &NotesApp| -> Vec<uuid::Uuid> {
        let mut ps = app.workspace_pages_for_test();
        ps.retain(|p| p.parent.is_none());
        ps.sort_by_key(|p| p.order);
        ps.into_iter().map(|p| p.id).collect()
    };
    assert_eq!(order_ids(&app), vec![welcome, p1, p2]);

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800);

    // Focus the tree with `welcome` selected, then Alt-↓ once.
    click_row(&mut app, &mut state, &taffy, child, 0);
    assert_eq!(app.selected_for_test(), Some(welcome));

    let alt = Modifiers::from(ModifiersState::ALT);
    let handled =
        dispatch(&mut app, &mut state, &taffy, child, &alt, &named_key(NamedKey::ArrowDown));
    assert!(handled, "Alt-↓ must be consumed by the focused tree");
    assert_eq!(
        order_ids(&app),
        vec![p1, welcome, p2],
        "Alt-↓ must move `welcome` down one slot among its siblings"
    );

    // And Alt-↑ moves it back.
    dispatch(&mut app, &mut state, &taffy, child, &alt, &named_key(NamedKey::ArrowUp));
    assert_eq!(
        order_ids(&app),
        vec![welcome, p1, p2],
        "Alt-↑ must move `welcome` back up"
    );
}
