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

fn mouse_press_right() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: ElementState::Pressed,
        button: MouseButton::Right,
    }
}

fn mouse_release() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: ElementState::Released,
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
    modifiers: Modifiers,
    event: &WindowEvent,
) -> bool {
    let mut handled = false;
    let mut focus_target = None;
    app.handle_event(
        event,
        1.0,
        &modifiers,
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
) -> Result<(), String> {
    let no_mods = Modifiers::default();
    let c = app
        .sidebar_row_center_for_test(idx)
        .ok_or_else(|| "row laid out".to_string())?;
    dispatch(app, state, taffy, child, no_mods, &cursor_moved(c.x, c.y));
    dispatch(app, state, taffy, child, no_mods, &mouse_press());
    Ok(())
}

#[test]
fn question_mark_types_into_focused_note_and_does_not_open_help() {
    // Regression guard: `?` must be a literal character in a note. The
    // editor holds focus after load, so the global help shortcut must
    // yield to typing.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert!(app.main_panel_mut().editor_mut().is_focused());

    let no_mods = Modifiers::default();
    let handled = dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key("?"));

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
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    let before = app.editor_text_for_test();
    app.blur_editor_for_test();
    let no_mods = Modifiers::default();
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key("?"));

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
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    app.shortcuts_mut().set_visible(true);
    let before = app.editor_text_for_test();
    let no_mods = Modifiers::default();
    let handled = dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key("x"));

    assert!(handled, "the overlay must consume keystrokes while visible");
    assert_eq!(
        app.editor_text_for_test(),
        before,
        "no keystroke may reach the editor while the overlay is up"
    );
    assert!(app.shortcuts_visible_for_test());

    // Escape closes it.
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Escape),
    );
    assert!(
        !app.shortcuts_visible_for_test(),
        "Esc must close the overlay"
    );
}

#[test]
fn cmd_n_creates_child_even_while_editor_is_focused() {
    // The core reachability bug: Cmd-N used to no-op unless the sidebar
    // was focused, but the editor holds focus on load. Handled at the
    // app level it must work regardless.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let root = app.workspace_pages_for_test()[0].id;
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert!(app.main_panel_mut().editor_mut().is_focused());
    assert_eq!(app.selected_for_test(), Some(root));

    let ctrl = Modifiers::from(ModifiersState::CONTROL);
    let handled = dispatch(&mut app, &mut state, &taffy, child, ctrl, &char_key("n"));

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
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert_eq!(app.workspace_pages_for_test().len(), 1);

    let no_mods = Modifiers::default();
    let btn = app.sidebar_new_page_btn_center_for_test();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(btn.x, btn.y),
    );
    let handled = dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());

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
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let victim = app.selected_for_test().unwrap();
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    // Row 1 is the newly-created second root page. Click focuses the
    // tree AND selects it; then Delete removes it through the gated
    // keyboard path.
    click_row(&mut app, &mut state, &taffy, child, 1).unwrap();
    assert_eq!(app.selected_for_test(), Some(victim));

    let no_mods = Modifiers::default();
    let handled = dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Delete),
    );

    assert!(handled, "Delete must be consumed by the focused tree");
    let pages = app.workspace_pages_for_test();
    assert_eq!(pages.len(), 1, "Delete must remove the page");
    assert_eq!(pages[0].id, welcome);
    assert_eq!(
        app.selected_for_test(),
        Some(welcome),
        "selection falls back"
    );
}

#[test]
fn delete_key_is_gated_on_tree_focus() {
    // Without focusing the tree (editor holds focus after load), Delete
    // must NOT delete a page — it belongs to the editor. This proves the
    // focus gate is real, not bypassed.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();
    let before = app.workspace_pages_for_test().len();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    let no_mods = Modifiers::default();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Delete),
    );

    assert_eq!(
        app.workspace_pages_for_test().len(),
        before,
        "Delete must not delete a page while the editor holds focus"
    );
}

#[test]
fn f2_opens_inline_rename_after_focusing_tree() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    click_row(&mut app, &mut state, &taffy, child, 0).unwrap();
    assert!(!app.sidebar_is_renaming_for_test());

    let no_mods = Modifiers::default();
    let handled = dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::F2),
    );
    assert!(handled, "F2 must be consumed by the focused tree");
    assert!(
        app.sidebar_is_renaming_for_test(),
        "F2 must open the inline rename editor"
    );
}

#[test]
fn arrow_keys_move_selection_after_focusing_tree() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let second = app.selected_for_test().unwrap();
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    click_row(&mut app, &mut state, &taffy, child, 0).unwrap();
    assert_eq!(app.selected_for_test(), Some(welcome));

    let no_mods = Modifiers::default();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::ArrowDown),
    );
    assert_eq!(
        app.selected_for_test(),
        Some(second),
        "ArrowDown moves selection to the next row"
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::ArrowUp),
    );
    assert_eq!(
        app.selected_for_test(),
        Some(welcome),
        "ArrowUp moves selection back up"
    );
}

#[test]
fn disclosure_triangle_click_toggles_child_visibility() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    // Give `welcome` a child so it becomes a collapsible branch.
    app.apply_event_for_test(OutEvent::CreateChild {
        parent: Some(welcome),
    });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    // Creating a child reveals it (its branch expands), so both rows show.
    assert!(app.sidebar_row_center_for_test(0).is_some());
    assert!(
        app.sidebar_row_center_for_test(1).is_some(),
        "the newly-created child is revealed under its parent"
    );

    // Click the disclosure triangle (leftmost glyph of row 0) to COLLAPSE.
    let row0 = app.sidebar_row_center_for_test(0).unwrap();
    let glyph_x = app.sidebar_disclosure_x_for_test(0).unwrap();
    let no_mods = Modifiers::default();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(glyph_x, row0.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    // Re-settle so the re-flattened rows lay out.
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert!(
        app.sidebar_row_center_for_test(1).is_none(),
        "collapsing must hide the child row through the event path"
    );

    // Click again → the child row reappears.
    let row0 = app.sidebar_row_center_for_test(0).unwrap();
    let glyph_x = app.sidebar_disclosure_x_for_test(0).unwrap();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(glyph_x, row0.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    let _ = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert!(
        app.sidebar_row_center_for_test(1).is_some(),
        "expanding must reveal the child row again"
    );
}

#[test]
fn double_click_row_opens_inline_rename() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    let c = app.sidebar_row_center_for_test(0).expect("row 0 laid out");
    let no_mods = Modifiers::default();
    // Two presses at the same spot in quick succession → double-click.
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(c.x, c.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    assert!(
        !app.sidebar_is_renaming_for_test(),
        "single click must not rename"
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(c.x, c.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    assert!(
        app.sidebar_is_renaming_for_test(),
        "double-click on a row must open the inline rename editor"
    );
}

#[test]
fn multiline_code_block_renders_as_one_contiguous_unit() {
    // Bug: code blocks rendered as separate lines with gaps. A multi-line
    // fenced block must render as ONE contiguous unit — consecutive code
    // lines butt together (no inter-line gap), each in the monospace face.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let id = app.workspace_pages_for_test()[0].id;
    let body = "Intro line.\n\n```rust\nlet a = 1;\nlet b = 2;\nlet c = 3;\n```\n\nAfter.\n";
    app.main_panel_mut().load_page(id, "T", body.to_string());

    let mut state = ui::State::new();
    let _ = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    let editor = app.main_panel_mut().editor();
    let blocks = editor.blocks();
    let code_idx: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.kind, ui::widget::BlockKind::CodeBlock { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(code_idx.len(), 3, "a 3-line fence yields 3 code cells");

    // Consecutive code lines are contiguous — no 8px inter-block gap.
    for w in code_idx.windows(2) {
        let a = blocks[w[0]].line.get_rect();
        let b = blocks[w[1]].line.get_rect();
        let gap = b.y0 - a.y1;
        assert!(
            gap.abs() < 1.0,
            "code lines must be contiguous (one visual block), gap was {gap}"
        );
    }
    // Each code cell shapes its text in the monospace face.
    for &i in &code_idx {
        assert!(
            blocks[i].line.inner().is_mono(),
            "code cells render in the monospace face"
        );
    }
    // The paragraph AFTER the code block keeps its inter-block gap, proving
    // the no-gap rule is code-specific, not global.
    let last = *code_idx.last().unwrap();
    if last + 1 < blocks.len() {
        let a = blocks[last].line.get_rect();
        let p = blocks[last + 1].line.get_rect();
        assert!(
            p.y0 - a.y1 > 4.0,
            "a normal block after the code run keeps its gap"
        );
    }
}

#[test]
fn backspace_at_note_start_does_not_delete_or_switch_page() {
    // Regression: with a page selected from the tree, the tree and the
    // editor both hold focus. A Backspace at the very start of the note
    // used to fall through to the tree's page-delete binding, deleting the
    // note and jumping the user to another one. It must be a no-op now.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    // A second root page so a stray delete would visibly "switch" pages.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    // Select the first page from the tree — this is the dual-focus state
    // (tree focused for its shortcuts, editor focused for typing).
    click_row(&mut app, &mut state, &taffy, child, 0).unwrap();
    assert_eq!(app.selected_for_test(), Some(welcome));
    // Put the caret at the very start of the first block.
    app.main_panel_mut().editor_mut().blocks_mut()[0]
        .line
        .set_cursor_pos(0);

    let before = app.workspace_pages_for_test().len();
    let no_mods = Modifiers::default();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Backspace),
    );

    assert_eq!(
        app.workspace_pages_for_test().len(),
        before,
        "Backspace at the start of a note must NOT delete a page"
    );
    assert_eq!(
        app.selected_for_test(),
        Some(welcome),
        "Backspace at the start of a note must NOT switch pages"
    );
}

#[test]
fn right_click_row_opens_context_menu_and_new_child_action_works() {
    // Bug: right-click on a page row did nothing. It must open the context
    // menu, and its actions must actually run.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    let c = app.sidebar_row_center_for_test(0).expect("row 0 laid out");
    let no_mods = Modifiers::default();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(c.x, c.y),
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &mouse_press_right(),
    );
    assert!(
        app.sidebar_menu_open_for_test(),
        "right-click on a page row must open the context menu"
    );

    // Drive the menu through its real key path: ArrowDown highlights the
    // first action ("New child"), Enter fires it.
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::ArrowDown),
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Enter),
    );

    let pages = app.workspace_pages_for_test();
    assert_eq!(pages.len(), 2, "the New child action must create a page");
    let children_count = pages.iter().filter(|p| p.parent == Some(welcome)).count();
    assert_eq!(
        children_count, 1,
        "New child must nest the new page under the right-clicked page"
    );
    assert!(
        !app.sidebar_menu_open_for_test(),
        "the menu closes after an action is chosen"
    );
}

#[test]
fn nested_child_renders_indented_under_its_parent() {
    // Bug: the sidebar rendered a flat list despite the model's hierarchy.
    // A child of the selected page must show one indent level below it.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    // Creating a child auto-selects it, so revealing it proves the tree
    // expands its ancestors (not a collapsed flat list).
    app.apply_event_for_test(OutEvent::CreateChild {
        parent: Some(welcome),
    });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let _ = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    assert_eq!(
        app.sidebar_row_depth_for_test(0),
        Some(0),
        "parent sits at depth 0"
    );
    assert_eq!(
        app.sidebar_row_has_children_for_test(0),
        Some(true),
        "the parent shows it has children (disclosure triangle)"
    );
    assert_eq!(
        app.sidebar_row_depth_for_test(1),
        Some(1),
        "the child renders one level indented under its parent — a real tree"
    );
}

#[test]
fn inline_rename_is_plain_and_commits_on_click_outside() {
    // Bug: the rename field was a jarring boxed input that never deselected.
    // It must be plain-chrome (styled like the row) and commit on focus-out.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    // Second root page so we can click a DIFFERENT row to blur + commit.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    // Double-click row 0 to open the in-place rename field.
    let c = app.sidebar_row_center_for_test(0).expect("row 0 laid out");
    let no_mods = Modifiers::default();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(c.x, c.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(c.x, c.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    assert!(
        app.sidebar_is_renaming_for_test(),
        "double-click opens the rename field"
    );
    assert_eq!(
        app.sidebar_rename_is_plain_for_test(),
        Some(true),
        "the rename field is plain-chrome (styled like the row, not a boxed input)"
    );

    // Type an edit into the field, then click a DIFFERENT row → focus-out
    // must COMMIT (not discard) and end editing.
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key("!"));
    let c1 = app.sidebar_row_center_for_test(1).expect("row 1 laid out");
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(c1.x, c1.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());

    assert!(
        !app.sidebar_is_renaming_for_test(),
        "clicking outside the field must end editing (it deselects/blurs)"
    );
    let pages = app.workspace_pages_for_test();
    let renamed = pages.iter().find(|p| p.id == welcome).unwrap();
    assert_eq!(
        renamed.title, "Welcome!",
        "clicking outside must COMMIT the edit, not discard it"
    );
}

#[test]
fn double_click_in_editor_selects_a_word() {
    // Regression guard for "double-click gives a weird selection": a
    // double-click inside a note block must select exactly the word under
    // the cursor (via the ui TextInput's multi-click ladder), never a
    // stray partial or cross-block range.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let id = app.workspace_pages_for_test()[0].id;
    app.main_panel_mut()
        .load_page(id, "T", "the quick brown fox\n".into());

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    // Click a point a few characters into the first block.
    let rect = app.main_panel_mut().editor().blocks()[0].line.get_rect();
    let x = rect.x0 + 60.0;
    let y = rect.y0 + rect.height() / 2.0;
    let no_mods = Modifiers::default();

    // Single click: caret only, no selection.
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(x, y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &mouse_release(),
    );
    assert!(
        app.main_panel_mut().editor().blocks()[0]
            .line
            .selection_range()
            .is_none(),
        "a single click places the caret without selecting a word"
    );

    // Double click at the same spot: exactly the word under the cursor.
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(x, y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &mouse_release(),
    );

    let block = &app.main_panel_mut().editor().blocks()[0];
    let text = block.text().to_string();
    let (s, e) = block
        .line
        .selection_range()
        .expect("double-click selects a word");
    assert!(e > s, "the selection is non-empty");
    let word = text
        .get(s..e)
        .expect("selection range lands on a char boundary");
    assert!(
        !word.trim().is_empty() && !word.contains(' '),
        "the selection is exactly one word (no spaces), got {word:?}"
    );
    assert!(
        text.split_whitespace().any(|w| w == word),
        "the selected span is a whole word from the line, got {word:?}"
    );
}

#[test]
fn slash_menu_creates_numbered_list_that_continues_on_enter() {
    // Drive the real path: `/number` + Enter converts the focused block to
    // a numbered list; pressing Enter continues the list; the rendered
    // ordinals increment and the saved markdown numbers sequentially.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    // A fresh, empty page so the slash command has a clean paragraph.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert!(app.main_panel_mut().editor_mut().is_focused());

    let no_mods = Modifiers::default();
    // Open the slash menu and filter to "Numbered list".
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key("/"));
    assert!(
        app.editor_slash_menu_visible_for_test(),
        "typing `/` opens the block menu"
    );
    for c in ["n", "u", "m", "b", "e", "r"] {
        dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(c));
    }
    // Enter commits the highlighted row (the sole match) → Numbered.
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Enter),
    );
    assert!(
        !app.editor_slash_menu_visible_for_test(),
        "committing the menu closes it"
    );
    {
        let blocks = app.main_panel_mut().editor().blocks();
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(
                blocks[0].kind,
                ui::widget::BlockKind::Numbered { indent: 0 }
            ),
            "the `/number` command converts the block to a numbered list"
        );
        assert_eq!(
            blocks[0].text(),
            "",
            "the `/number` filter text is stripped"
        );
    }

    // Type the first item, then Enter to continue the list twice more.
    for c in ["o", "n", "e"] {
        dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(c));
    }
    let _ = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Enter),
    );
    for c in ["t", "w", "o"] {
        dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(c));
    }
    let _ = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Enter),
    );
    for c in ["s", "i", "x"] {
        dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(c));
    }

    let editor = app.main_panel_mut().editor();
    let blocks = editor.blocks();
    assert_eq!(blocks.len(), 3, "each Enter continues the numbered list");
    for b in blocks {
        assert!(
            matches!(b.kind, ui::widget::BlockKind::Numbered { indent: 0 }),
            "Enter on a numbered item yields another numbered item"
        );
    }
    assert_eq!(
        ui::widget::block_editor::numbered_ordinals(blocks),
        vec![1, 2, 3],
        "the rendered ordinals increment down the list"
    );

    // The saved markdown numbers the list sequentially.
    let md = app.main_panel_mut().take_body_snapshot();
    assert_eq!(md, "1. one\n\n2. two\n\n3. six\n");
}

#[test]
fn editing_the_in_page_title_renames_the_page_and_updates_the_sidebar() {
    // Clicking the prominent title opens an inline rename; committing it renames
    // the page in the model AND re-syncs the sidebar row — the in-note title and
    // the tree stay in step.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert_eq!(app.sidebar_row_title_for_test(0), Some("Welcome"));

    // Click the title band → begin editing (whole title selected).
    let c = app.main_title_center_for_test();
    let no_mods = Modifiers::default();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(c.x, c.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    assert!(
        app.main_title_editing_for_test(),
        "clicking the title opens the inline rename"
    );

    // Type a new name — select-all means it replaces the old value — then Enter.
    for ch in ["R", "o", "a", "d", "m", "a", "p"] {
        dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(ch));
    }
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Enter),
    );

    assert!(
        !app.main_title_editing_for_test(),
        "Enter commits and ends the edit"
    );
    let pages = app.workspace_pages_for_test();
    let renamed = pages.iter().find(|p| p.id == welcome).unwrap();
    assert_eq!(
        renamed.title, "Roadmap",
        "committing the title renames the page"
    );
    assert_eq!(
        app.main_panel_mut().title(),
        Some("Roadmap"),
        "the panel shows the new title"
    );
    assert_eq!(
        app.sidebar_row_title_for_test(0),
        Some("Roadmap"),
        "the rename propagates into the sidebar row"
    );

    // Escape must cancel without renaming.
    let c = app.main_title_center_for_test();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(c.x, c.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());
    for ch in ["N", "o", "p", "e"] {
        dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(ch));
    }
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &named_key(NamedKey::Escape),
    );
    assert!(!app.main_title_editing_for_test(), "Escape ends the edit");
    let pages = app.workspace_pages_for_test();
    let after = pages.iter().find(|p| p.id == welcome).unwrap();
    assert_eq!(
        after.title, "Roadmap",
        "Escape cancels — the title is unchanged"
    );
}

#[test]
fn hash_space_promotes_paragraph_to_heading_through_the_event_path() {
    // The markdown shortcut, proven on the real keystroke flow: typing `#`
    // then a space at the start of an empty paragraph turns it into an H1 and
    // strips the `# ` prefix. This is the sequence a user actually types —
    // the old code only fired when the block was empty *before* the keystroke,
    // so the space (typed into a non-empty "#") never promoted.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    // A fresh, empty page so the first block is an empty paragraph.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert!(app.main_panel_mut().editor_mut().is_focused());

    let no_mods = Modifiers::default();
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key("#"));
    // A lone `#` must NOT promote — the prefix isn't complete yet.
    assert!(
        matches!(
            app.main_panel_mut().editor().blocks()[0].kind,
            ui::widget::BlockKind::Paragraph
        ),
        "a bare `#` stays a paragraph until the space completes the prefix"
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(" "));

    {
        let blocks = app.main_panel_mut().editor().blocks();
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(blocks[0].kind, ui::widget::BlockKind::Heading(1)),
            "`# ` promotes the paragraph to an H1"
        );
        assert_eq!(
            blocks[0].text(),
            "",
            "the `# ` prefix is stripped on promotion"
        );
    }

    // `## ` and `### ` promote to H2 / H3 on their own fresh pages.
    for (hashes, level) in [("##", 2u8), ("###", 3u8)] {
        app.apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.sync_sidebar_for_test();
        let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
        for ch in hashes.chars() {
            dispatch(
                &mut app,
                &mut state,
                &taffy,
                child,
                no_mods,
                &char_key(&ch.to_string()),
            );
        }
        dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(" "));
        let blocks = app.main_panel_mut().editor().blocks();
        assert!(
            matches!(blocks[0].kind, ui::widget::BlockKind::Heading(l) if l == level),
            "`{hashes} ` promotes to a level-{level} heading"
        );
        assert_eq!(blocks[0].text(), "");
    }
}

#[test]
fn slash_command_click_applies_kind_and_strips_the_trigger() {
    // Selecting a slash command with the MOUSE must delete the `/…` trigger
    // and apply the block kind — the old menu had no mouse handler, so a click
    // just dismissed it and left the literal `/` behind.
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    assert!(app.main_panel_mut().editor_mut().is_focused());

    let no_mods = Modifiers::default();
    // Open the menu and filter to the single "Heading 1" row.
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key("/"));
    assert!(
        app.editor_slash_menu_visible_for_test(),
        "`/` opens the block menu"
    );
    for c in ["h", "1"] {
        dispatch(&mut app, &mut state, &taffy, child, no_mods, &char_key(c));
    }
    assert_eq!(
        app.main_panel_mut().editor().slash_menu().visible_len(),
        1,
        "`/h1` narrows to exactly the Heading 1 row"
    );

    // Click that row through the real pointer path.
    let (cx, cy) = {
        let rect = app
            .main_panel_mut()
            .editor()
            .slash_menu()
            .row_rect(0)
            .expect("the sole filtered command has a painted row");
        (rect.center().x, rect.center().y)
    };
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        no_mods,
        &cursor_moved(cx, cy),
    );
    dispatch(&mut app, &mut state, &taffy, child, no_mods, &mouse_press());

    assert!(
        !app.editor_slash_menu_visible_for_test(),
        "clicking a row closes the menu"
    );
    let blocks = app.main_panel_mut().editor().blocks();
    assert_eq!(blocks.len(), 1);
    assert!(
        matches!(blocks[0].kind, ui::widget::BlockKind::Heading(1)),
        "the clicked command converts the block to a Heading 1"
    );
    assert_eq!(
        blocks[0].text(),
        "",
        "the `/` trigger (and filter) is stripped — no literal slash left behind"
    );
}

#[test]
fn alt_arrow_reorders_selected_page_among_siblings() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
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
    let (taffy, child) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();

    // Focus the tree with `welcome` selected, then Alt-↓ once.
    click_row(&mut app, &mut state, &taffy, child, 0).unwrap();
    assert_eq!(app.selected_for_test(), Some(welcome));

    let alt = Modifiers::from(ModifiersState::ALT);
    let handled = dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        alt,
        &named_key(NamedKey::ArrowDown),
    );
    assert!(handled, "Alt-↓ must be consumed by the focused tree");
    assert_eq!(
        order_ids(&app),
        vec![p1, welcome, p2],
        "Alt-↓ must move `welcome` down one slot among its siblings"
    );

    // And Alt-↑ moves it back.
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        alt,
        &named_key(NamedKey::ArrowUp),
    );
    assert_eq!(
        order_ids(&app),
        vec![welcome, p1, p2],
        "Alt-↑ must move `welcome` back up"
    );
}
