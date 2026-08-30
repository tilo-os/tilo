//! Drives `MainPanel` vertical scrolling through the real
//! `NotesApp::handle_event` → `MouseWheel` path plus the `apply_layout`
//! overflow clamp, on a live settled layout. Guards the hand-rolled
//! `scroll_y` math (wheel-to-px conversion, top clamp, content-extent
//! clamp) that has no test today.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::collections::HashMap;
use std::fmt::Write as _;

use notes::NotesApp;
use taffy::prelude::*;
use tempfile::TempDir;
use ui::text_render::TextRenderer;
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
fn mouse_wheel_scrolls_and_clamps_to_content() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let id = app.selected_for_test().unwrap();
    app.main_panel_mut().load_page(id, "Long page", tall_body());

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

    // Scroll back up past the top: clamps at 0.
    dispatch(&mut app, &mut state, &taffy, child, &wheel(1000.0));
    let _ = settle(&mut app, &mut state, 1000, 300).unwrap();
    assert_eq!(
        app.main_panel_mut().scroll_y_for_test(),
        0.0,
        "wheel-up past the top must clamp scroll_y to 0"
    );
}
