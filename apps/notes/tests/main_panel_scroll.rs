//! Drives `MainPanel` vertical scrolling through the real
//! `NotesApp::handle_event` → `MouseWheel` path plus the `apply_layout`
//! overflow clamp, on a live settled layout. Guards the hand-rolled
//! `scroll_y` math (wheel-to-px conversion, top clamp, content-extent
//! clamp) that has no test today.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::float_cmp,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::fmt::Write as _;

use notes::NotesApp;
use taffy::prelude::*;
use tempfile::TempDir;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{Modifiers, MouseScrollDelta, TouchPhase, WindowEvent};

fn wheel(lines: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: unsafe { std::mem::zeroed() },
        delta: MouseScrollDelta::LineDelta(0.0, lines),
        phase: TouchPhase::Moved,
    }
}

/// A tall body so the panel content overflows any short viewport.
fn tall_body() -> String {
    let mut s = String::from("# Long page\n\n");
    for i in 0..60 {
        let _ = writeln!(s, "Paragraph number {i} with some words.\n");
    }
    s
}

fn settle(
    app: &mut NotesApp,
    state: &mut ui::State,
    w: u16,
    h: u16,
) -> Result<(TaffyTree<()>, NodeId), String> {
    settle_scaled(app, state, w, h, 1.0)
}

fn settle_scaled(
    app: &mut NotesApp,
    state: &mut ui::State,
    w: u16,
    h: u16,
    scale: f32,
) -> Result<(TaffyTree<()>, NodeId), String> {
    let theme = Theme::dark().with_text_scale(scale);
    let mut text_renderer = crate::component_support::renderer();
    text_renderer.set_text_scale(scale);
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
) {
    let modifiers = Modifiers::default();
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
}

#[test]
fn mouse_wheel_scrolls_and_clamps_to_content() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let id = app.selected_for_test().unwrap();
    let body = tall_body();
    notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
    app.main_panel_mut().load_page(id, "Long page", &body);
    // Complete the normal legacy origin bind before measuring wheel behavior.
    // Otherwise its delayed first bind replaces the panel midway through this test.
    app.bypass_seed_grace_for_test();

    let mut state = ui::State::new();
    // Wide (stays inline) but short so the content overflows.
    let (taffy, child) = settle(&mut app, &mut state, 1000, 300).unwrap();
    assert_eq!(
        app.main_panel_mut().scroll_y_for_test(),
        0.0,
        "starts at top"
    );

    // Scroll down: negative LineDelta y → positive px offset.
    dispatch(&mut app, &mut state, &taffy, child, &wheel(-3.0));
    let (taffy, child) = settle(&mut app, &mut state, 1000, 300).unwrap();
    let scrolled = app.main_panel_mut().scroll_y_for_test();
    assert!(
        scrolled > 0.0,
        "wheel-down must advance scroll_y; got {scrolled}"
    );

    // Scroll far past the bottom: scroll_y must clamp to the content
    // extent — a second huge scroll can't push it further.
    dispatch(&mut app, &mut state, &taffy, child, &wheel(-1000.0));
    let (taffy, child) = settle(&mut app, &mut state, 1000, 300).unwrap();
    let at_max = app.main_panel_mut().scroll_y_for_test();
    dispatch(&mut app, &mut state, &taffy, child, &wheel(-1000.0));
    let (taffy, child) = settle(&mut app, &mut state, 1000, 300).unwrap();
    let still_max = app.main_panel_mut().scroll_y_for_test();
    assert!(
        at_max > scrolled,
        "scrolling further must increase offset up to the clamp"
    );
    assert_eq!(
        at_max, still_max,
        "scroll_y must clamp to the content extent (can't exceed it)"
    );
    let panel = app.main_panel_mut();
    assert!(
        panel.editor().content_bottom() <= panel.get_rect().y1 - 44.0,
        "the final row must remain above the save status, not underneath it"
    );

    // Scroll back up past the top: clamps at 0.
    dispatch(&mut app, &mut state, &taffy, child, &wheel(1000.0));
    let _ = settle(&mut app, &mut state, 1000, 300).unwrap();
    assert_eq!(
        app.main_panel_mut().scroll_y_for_test(),
        0.0,
        "wheel-up past the top must clamp scroll_y to 0"
    );
}

#[test]
fn bottom_scroll_reveals_the_last_row_just_above_the_actual_status_pill() {
    for scale in [1.0, 2.0] {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let id = app.selected_for_test().unwrap();
        let body = tall_body();
        notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
        app.main_panel_mut().load_page(id, "Long note", &body);
        app.bypass_seed_grace_for_test();
        let mut state = ui::State::new();
        let (taffy, child) = settle_scaled(&mut app, &mut state, 1000, 1000, scale).unwrap();
        dispatch(&mut app, &mut state, &taffy, child, &wheel(-1000.0));
        settle_scaled(&mut app, &mut state, 1000, 1000, scale).unwrap();
        let mut renderer = crate::component_support::renderer();
        renderer.set_text_scale(scale);
        let (_, pill_height) = ui::status_bubble::pill_size("Saved", &mut renderer, 12.0, 0.0);
        let pill_top = app.get_rect().y1 - 14.0 - pill_height;
        let content_bottom = app.main_panel_mut().editor().content_bottom();
        let gap = pill_top - content_bottom;
        let theme = Theme::dark().with_text_scale(scale);
        assert!(
            (gap - f64::from(theme.tokens.spacing.s2)).abs() <= 1.0,
            "at {scale}x, last row ends at {content_bottom}, pill starts at {pill_top}: {gap}px gap wastes space or overlaps the status"
        );
    }
}

fn scroll_control(app: &mut NotesApp) -> ui::a11y::AccessNode {
    app.main_panel_mut()
        .accessibility()
        .unwrap()
        .children
        .into_iter()
        .find(|node| node.action_target.as_deref() == Some("notes.scroll-position"))
        .expect("overflowing notes expose their visible scrollbar to accessibility")
}

fn pointer(point: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    }
}

fn mouse(state: winit::event::ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state,
        button: winit::event::MouseButton::Left,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end scrollbar test verifies pointer, keyboard, semantic, and reload paths"
)]
fn note_scrollbar_drags_pages_and_exposes_the_same_position_semantically() {
    use ui::a11y::Action;
    use winit::event::ElementState::{Pressed, Released};
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let id = app.selected_for_test().unwrap();
    let body = tall_body();
    notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
    app.main_panel_mut().load_page(id, "Long note", &body);
    app.bypass_seed_grace_for_test();
    let mut state = ui::State::new();
    let (taffy, child) = settle(&mut app, &mut state, 1000, 600).unwrap();
    let before = app.main_panel_mut().editor().to_markdown();
    let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
    assert_eq!(scroll_control(&mut app).value.as_deref(), Some("0"));
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &pointer(thumb.center()),
    );
    dispatch(&mut app, &mut state, &taffy, child, &mouse(Pressed));
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &pointer(Point::new(thumb.center().x, thumb.center().y + 100.0)),
    );
    let (taffy, child) = settle(&mut app, &mut state, 1000, 600).unwrap();
    assert!(
        app.main_panel_mut().scroll_y_for_test() > 0.0,
        "drag must move the note"
    );
    assert!(scroll_control(&mut app).bounds.unwrap().y0 > thumb.y0);
    dispatch(&mut app, &mut state, &taffy, child, &mouse(Released));
    let scrolled = app.main_panel_mut().scroll_y_for_test();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &pointer(Point::new(thumb.center().x, 10.0)),
    );
    settle(&mut app, &mut state, 1000, 600).unwrap();
    assert_eq!(
        app.main_panel_mut().scroll_y_for_test(),
        scrolled,
        "release ends the drag"
    );

    let root = state.root.clone();
    assert!(app.main_panel_mut().perform_semantic_action(
        "notes.scroll-position",
        &Action::SetValue("100".into()),
        &mut state.doc,
        &root
    ));
    let (taffy, child) = settle(&mut app, &mut state, 1000, 600).unwrap();
    assert_eq!(scroll_control(&mut app).value.as_deref(), Some("100"));
    let last = app
        .main_panel_mut()
        .editor()
        .blocks()
        .last()
        .unwrap()
        .line
        .get_rect();
    assert!(
        last.y1 < app.main_panel_mut().get_rect().y1,
        "semantic scroll reveals the last line"
    );
    let bottom = app.main_panel_mut().scroll_y_for_test();
    let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &pointer(Point::new(thumb.center().x, thumb.y0 - 20.0)),
    );
    dispatch(&mut app, &mut state, &taffy, child, &mouse(Pressed));
    dispatch(&mut app, &mut state, &taffy, child, &mouse(Released));
    settle(&mut app, &mut state, 1000, 600).unwrap();
    assert!(
        app.main_panel_mut().scroll_y_for_test() < bottom,
        "track click pages upward"
    );
    assert!(!app.main_panel_mut().perform_semantic_action(
        "notes.scroll-position",
        &Action::SetValue("NaN".into()),
        &mut state.doc,
        &root
    ));
    assert!(app.main_panel_mut().perform_semantic_action(
        "notes.scroll-position",
        &Action::Focus,
        &mut state.doc,
        &root
    ));
    let key = |key| {
        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = winit::keyboard::Key::Named(key);
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = Pressed;
        event.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
    };
    let (taffy, child) = settle(&mut app, &mut state, 1000, 600).unwrap();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &key(winit::keyboard::NamedKey::Home),
    );
    let (taffy, child) = settle(&mut app, &mut state, 1000, 600).unwrap();
    assert_eq!(app.main_panel_mut().scroll_y_for_test(), 0.0);
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &key(winit::keyboard::NamedKey::PageDown),
    );
    let (taffy, child) = settle(&mut app, &mut state, 1000, 600).unwrap();
    assert!(app.main_panel_mut().scroll_y_for_test() > 0.0);
    let thumb = scroll_control(&mut app).bounds.unwrap().to_rect();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &pointer(thumb.center()),
    );
    dispatch(&mut app, &mut state, &taffy, child, &mouse(Pressed));
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &WindowEvent::Focused(false),
    );
    let stopped = app.main_panel_mut().scroll_y_for_test();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &pointer(Point::new(thumb.center().x, 500.0)),
    );
    settle(&mut app, &mut state, 1000, 600).unwrap();
    assert_eq!(
        app.main_panel_mut().scroll_y_for_test(),
        stopped,
        "focus loss cancels dragging"
    );
    assert_eq!(
        app.main_panel_mut().editor().to_markdown(),
        before,
        "scrolling never edits the note"
    );
    // Build a real short-note fixture. Replacing just the live projection of
    // the long note would be overwritten by its canonical record on sync.
    let short = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(short.path().into()).unwrap();
    let id = app.selected_for_test().unwrap();
    notes::storage::write_page_body(short.path(), id, "Fits on screen").unwrap();
    app.main_panel_mut()
        .load_page(id, "Short note", "Fits on screen");
    app.bypass_seed_grace_for_test();
    let mut state = ui::State::new();
    settle(&mut app, &mut state, 1000, 600).unwrap();
    assert_eq!(app.main_panel_mut().editor().blocks().len(), 1);
    assert_eq!(
        app.main_panel_mut().editor().blocks()[0].text(),
        "Fits on screen"
    );
    assert!(
        !app.main_panel_mut()
            .accessibility()
            .unwrap()
            .children
            .iter()
            .any(|node| node.action_target.as_deref() == Some("notes.scroll-position")),
        "fitting content has no scrollbar"
    );
}

#[test]
fn revealing_a_deep_block_scrolls_its_caret_into_view_and_allows_manual_scrolling() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let id = app.selected_for_test().unwrap();
    let body = tall_body();
    notes::storage::write_page_body(tmp.path(), id, &body).unwrap();
    app.main_panel_mut().load_page(id, "Long note", &body);
    app.bypass_seed_grace_for_test();
    let mut state = ui::State::new();
    settle(&mut app, &mut state, 1000, 400).unwrap();

    let last = app.main_panel_mut().editor().blocks().len() - 1;
    assert!(app.main_panel_mut().reveal_block(last));
    let (taffy, child) = settle(&mut app, &mut state, 1000, 400).unwrap();
    let panel = app.main_panel_mut();
    assert!(
        panel.scroll_y_for_test() > 0.0,
        "revealing a deep block must scroll"
    );
    let caret = panel.editor().ime_cursor_rect().unwrap();
    assert!(
        caret.y0 >= panel.get_rect().y0 && caret.y1 <= panel.get_rect().y1,
        "revealed caret {caret:?} must be inside panel {:?}",
        panel.get_rect()
    );

    dispatch(&mut app, &mut state, &taffy, child, &wheel(1000.0));
    settle(&mut app, &mut state, 1000, 400).unwrap();
    assert_eq!(
        app.main_panel_mut().scroll_y_for_test(),
        0.0,
        "manual scrolling must be able to leave the caret behind"
    );

    assert!(app.main_panel_mut().reveal_block(last));
    settle(&mut app, &mut state, 1000, 400).unwrap();
    assert!(app.main_panel_mut().reveal_block(0));
    settle(&mut app, &mut state, 1000, 400).unwrap();
    assert_eq!(
        app.main_panel_mut().scroll_y_for_test(),
        0.0,
        "revealing the first block must return to the top"
    );

    let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    event.logical_key = winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown);
    event.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    event.location = winit::keyboard::KeyLocation::Standard;
    event.state = winit::event::ElementState::Pressed;
    event.text = None;
    let arrow_down = WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event,
        is_synthetic: false,
    };
    for _ in 0..last {
        let (taffy, child) = settle(&mut app, &mut state, 1000, 400).unwrap();
        dispatch(&mut app, &mut state, &taffy, child, &arrow_down);
    }
    settle(&mut app, &mut state, 1000, 400).unwrap();
    let panel = app.main_panel_mut();
    assert!(
        panel.scroll_y_for_test() > 0.0,
        "keyboard navigation must follow the caret"
    );
    let caret = panel.editor().ime_cursor_rect().unwrap();
    assert!(
        caret.y0 >= panel.get_rect().y0 && caret.y1 <= panel.get_rect().y1,
        "keyboard caret {caret:?} must stay within panel {:?}",
        panel.get_rect()
    );
}
