//! Drives the responsive sidebar through the real `handle_event` path:
//! a wide window keeps the page tree inline, a narrow window collapses
//! it into a closed overlay drawer, the top-bar button toggles it, and
//! picking a page selects it and auto-closes the drawer.

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
