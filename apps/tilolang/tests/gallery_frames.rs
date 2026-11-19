//! The gallery (`apps/tl/gallery`) in a real window's frame loop (`ui::app::EmbeddedApp`, the
//! app as the shell shows it in-process): switching its page, dragging its sidebar's edge and
//! turning the wheel over its content each do the work of what changed. What is held is counted,
//! not timed, so the check reads the same on any machine at any load: a page switch rebuilds the
//! window's structure once or twice and costs no more after many switches than after one, a drag
//! step of the sidebar's edge is one layout with no rebuild and no view evaluated, and a wheel
//! step is one layout with no component synced.

use std::collections::HashMap;

use tilolang_app::app::TlApp;
use ui::app::EmbeddedApp;
use ui::text_render::TextRenderer;
use ui::tilo_resource::profiling::{Sample, observe};
use ui::winit::event::{ElementState, MouseButton};

/// Where the sidebar's rows stand in a 1280×800 window, from the top: Basics, Controls,
/// Feedback, Backlog, Release board, Data, Media, Places.
const ROWS: [f64; 8] = [104.0, 136.0, 168.0, 260.0, 292.0, 308.0, 384.0, 400.0];
/// Just past the sidebar's edge, where the grip that resizes it stands.
const EDGE: f64 = 282.0;

/// One frame as the shell draws the app: laid out at the desktop size, then drawn.
fn frame(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    use ui::widget::{DrawChild as _, Widget as _};
    let theme = tile.app().theme.clone();
    let mut taffy = ui::taffy::TaffyTree::new();
    let node = tile.build_layout(&mut taffy, &theme, &mut HashMap::new());
    let _ = taffy.compute_layout(
        node,
        ui::taffy::Size {
            width: ui::taffy::AvailableSpace::Definite(1280.0),
            height: ui::taffy::AvailableSpace::Definite(800.0),
        },
    );
    tile.apply_layout(&taffy, node, ui::kurbo::Point::ZERO);
    let mut scene = ui::vello::Scene::new();
    tile.draw_child(
        &mut scene,
        ui::kurbo::Affine::IDENTITY,
        text,
        &theme,
        &ui::presence::PresenceCtx::empty(),
    );
}

/// A click at `(x, y)` and the frames that settle it.
fn click(tile: &mut EmbeddedApp, text: &mut TextRenderer, (x, y): (f64, f64)) {
    tile.input_for_test(ui::synthetic_input::cursor_moved(x, y));
    frame(tile, text);
    tile.input_for_test(ui::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Left,
    ));
    frame(tile, text);
    tile.input_for_test(ui::synthetic_input::mouse_input(
        ElementState::Released,
        MouseButton::Left,
    ));
    for _ in 0..3 {
        frame(tile, text);
    }
}

/// How often `scope` ran.
fn calls(scopes: &[Sample], scope: &str) -> usize {
    scopes
        .iter()
        .find(|sample| sample.phase == scope)
        .map_or(0, |sample| sample.calls)
}

/// What a failure says the step did.
fn said(scopes: &[Sample]) -> String {
    let mut said: Vec<String> = scopes
        .iter()
        .map(|sample| format!("{}={}", sample.phase, sample.calls))
        .collect();
    said.sort();
    said.join(" ")
}

/// Where each named control stands.
fn placed(tile: &mut EmbeddedApp) -> HashMap<String, f64> {
    tile.named_controls()
        .into_iter()
        .map(|(name, rect)| (name, rect.x0))
        .collect()
}

#[test]
fn switching_pages_resizing_the_sidebar_and_scrolling_do_only_what_changed() {
    crate::support::installed("gallery");
    ui::framework::testing::drive_live_embedded::<TlApp, _>(|tile| {
        let mut text = TextRenderer::new();
        for _ in 0..6 {
            frame(tile, &mut text);
        }
        switches(tile, &mut text);
        drags(tile, &mut text);
        scrolls(tile, &mut text);
    })
    .expect("the gallery opens in a window");
}

/// Every page once, and then again: the second round costs what the first did.
fn switches(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    let mut rounds = Vec::new();
    for _ in 0..2 {
        let ((), scopes) = observe(|| {
            for y in ROWS {
                click(tile, text, (120.0, y));
            }
        });
        let rebuilds = calls(&scopes, "ui.rebuild_structure");
        assert!(
            rebuilds <= ROWS.len().saturating_mul(2),
            "a page switch rebuilds the window's structure at most twice: {}",
            said(&scopes)
        );
        rounds.push(calls(&scopes, "ui.callback_scope"));
    }
    let (first, second) = (rounds[0], rounds[1]);
    assert!(
        second <= first.saturating_add(first / 10),
        "switching pages again costs what it did the first time: {first} then {second} \
         component scopes"
    );
}

/// A drag of the sidebar's edge: each step lays the window out once, nothing more.
fn drags(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    click(tile, text, (120.0, ROWS[0]));
    let before = placed(tile);
    tile.input_for_test(ui::synthetic_input::cursor_moved(EDGE, 400.0));
    frame(tile, text);
    tile.input_for_test(ui::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Left,
    ));
    frame(tile, text);
    for step in 1..=4 {
        let ((), scopes) = observe(|| {
            tile.input_for_test(ui::synthetic_input::cursor_moved(
                EDGE + f64::from(step) * 7.0,
                400.0,
            ));
            frame(tile, text);
        });
        let evaluated = scopes
            .iter()
            .filter(|sample| sample.phase.starts_with("tl."))
            .count();
        assert_eq!(
            (
                calls(&scopes, "ui.rebuild_structure"),
                calls(&scopes, "ui.layout"),
                evaluated,
            ),
            (0, 1, 0),
            "a drag step is one layout, without a rebuild or a view evaluated: {}",
            said(&scopes)
        );
    }
    tile.input_for_test(ui::synthetic_input::mouse_input(
        ElementState::Released,
        MouseButton::Left,
    ));
    frame(tile, text);
    let after = placed(tile);
    assert!(
        before.iter().any(|(name, x)| after
            .get(name)
            .is_some_and(|moved| (moved - x - 28.0).abs() < 0.5)),
        "the drag moved the sidebar's edge: {before:?} then {after:?}"
    );
}

/// The wheel over the content: one layout a step, no component synced.
fn scrolls(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    tile.input_for_test(ui::synthetic_input::cursor_moved(800.0, 400.0));
    frame(tile, text);
    for _ in 0..3 {
        let ((), scopes) = observe(|| {
            tile.input_for_test(ui::synthetic_input::mouse_wheel(0.0, -2.0));
            frame(tile, text);
        });
        assert_eq!(
            (
                calls(&scopes, "ui.rebuild_structure"),
                calls(&scopes, "ui.layout"),
                calls(&scopes, "ui.component.sync"),
                calls(&scopes, "framework.action_bindings"),
            ),
            (0, 1, 0, 0),
            "a wheel step is one layout: {}",
            said(&scopes)
        );
    }
}
