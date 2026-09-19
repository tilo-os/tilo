//! Drives the responsive sidebar through the real `handle_event` path:
//! a wide window keeps the page tree inline, a narrow window collapses
//! it into a closed overlay drawer, the top-bar button toggles it, and
//! picking a page selects it and auto-closes the drawer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
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

/// Build + lay out `app` at `w`x`h`, looping until the responsive
/// structure settles, and return the final taffy tree + child node so
/// the caller can dispatch pointer events against a live layout.
fn settle_layout(
    app: &mut NotesApp,
    state: &mut ui::State,
    w: u16,
    h: u16,
) -> Result<(TaffyTree<()>, NodeId), String> {
    settle_layout_with_theme(app, state, w, h, &Theme::dark())
}

/// [`settle_layout`] under an explicit theme, so a test can lay the app out
/// at an accessibility text scale.
fn settle_layout_with_theme(
    app: &mut NotesApp,
    state: &mut ui::State,
    w: u16,
    h: u16,
    theme: &Theme,
) -> Result<(TaffyTree<()>, NodeId), String> {
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
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
fn narrow_window_collapses_sidebar_into_toggleable_drawer() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let welcome = app.workspace_pages_for_test()[0].id;
    // A second root page so a selection change is observable.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let other = app.selected_for_test().unwrap();
    assert_ne!(welcome, other);
    app.sync_sidebar_for_test();

    let mut state = ui::State::new();

    // Wide window: the sidebar stays inline; no top bar / drawer.
    let _ = settle_layout(&mut app, &mut state, 1280, 720).unwrap();
    assert!(
        !app.built_narrow_for_test(),
        "1280px window should keep the sidebar inline"
    );

    // Narrow window: collapses to a closed overlay drawer.
    let (taffy, child) = settle_layout(&mut app, &mut state, 560, 720).unwrap();
    assert!(
        app.built_narrow_for_test(),
        "560px window should collapse the sidebar into the drawer"
    );
    assert!(
        !app.drawer_open_for_test(),
        "the drawer must start closed when a window becomes narrow"
    );

    // The top-bar toggle opens the drawer.
    let btn = app.menu_btn_center_for_test();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &cursor_moved(btn.x, btn.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, &mouse_press());
    assert!(
        app.drawer_open_for_test(),
        "clicking the top-bar button must open the drawer"
    );

    // Picking a page from the drawer selects it and closes the drawer.
    assert_eq!(app.selected_for_test(), Some(other));
    let row = app
        .sidebar_row_center_for_test(0)
        .expect("row 0 should be laid out");
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &cursor_moved(row.x, row.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, &mouse_press());
    assert_eq!(
        app.selected_for_test(),
        Some(welcome),
        "clicking a drawer row must select that page"
    );
    assert!(
        !app.drawer_open_for_test(),
        "selecting a page must auto-close the drawer"
    );
}

/// At 2× text the scaled 264px drawer would be 528px wide — wider than a
/// 480px phone window. The drawer must clamp to the window and keep a scrim
/// band of the page visible, so its own trailing "+" button stays reachable.
#[test]
fn scaled_drawer_stays_inside_a_narrow_window() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    app.sync_sidebar_for_test();
    let mut state = ui::State::new();
    let theme = Theme::dark().with_text_scale(2.0);
    let window_w: u16 = 480;

    let (taffy, child) =
        settle_layout_with_theme(&mut app, &mut state, window_w, 900, &theme).unwrap();
    assert!(
        app.built_narrow_for_test(),
        "480px at 2× is a drawer layout"
    );
    let btn = app.menu_btn_center_for_test();
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &cursor_moved(btn.x, btn.y),
    );
    dispatch(&mut app, &mut state, &taffy, child, &mouse_press());
    assert!(app.drawer_open_for_test(), "the toggle opens the drawer");
    // The open drawer's structure is laid out on the next settled pass.
    let _ = settle_layout_with_theme(&mut app, &mut state, window_w, 900, &theme).unwrap();

    let drawer = app.sidebar_rect_for_test();
    let reveal = f64::from(theme.tokens.spacing.s12);
    assert!(
        drawer.x1 <= f64::from(window_w) - reveal + 0.5,
        "drawer must leave a {reveal}px band of the page visible, got right edge {}",
        drawer.x1
    );
    assert!(
        drawer.width() > 264.0,
        "the clamp must not undo the text-scale growth entirely: {drawer:?}"
    );
    let plus = app.sidebar_new_page_btn_center_for_test();
    assert!(
        plus.x < drawer.x1 && plus.x < f64::from(window_w),
        "the drawer's own new-page button must stay on screen: {plus:?} in {drawer:?}"
    );
}
