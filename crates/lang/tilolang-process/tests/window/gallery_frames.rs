//! The gallery (`apps/tl/gallery`) in a real window's frame loop (`ui::app::EmbeddedApp`, the
//! app as the shell shows it in-process): switching its page, dragging its sidebar's edge and
//! turning the wheel over its content each do the work of what changed. What is held is counted,
//! not timed, so the check reads the same on any machine at any load: a page switch rebuilds none of
//! the window's structure, lays it out once, measures its leaves a bounded
//! number of times and costs no more time or memory after many switches than after one, a drag
//! step of the sidebar's edge is
//! one layout with no rebuild, no view evaluated and no sync, and a wheel step is one layout with
//! no component synced, and a resize (a window maximized, its edge dragged) is one layout that
//! rebuilds nothing and evaluates no view.

use std::collections::HashMap;

use tilolang_process::window::app::TlApp;
use ui::app::EmbeddedApp;
use ui::text_render::TextRenderer;
use ui::tilo_resource::profiling::{Sample, observe};
use ui::winit::event::{ElementState, MouseButton};

/// The middle of the sidebar's rows of the pages in a 1280×800 window, from the top: Basics,
/// Controls, Feedback, Backlog, Release board, Data, Media, Places (States, under Feedback, is
/// measured by `gallery_pages`).
const ROWS: [f64; 8] = [100.0, 132.0, 164.0, 276.0, 308.0, 340.0, 412.0, 444.0];
/// Just past the sidebar's edge, where the grip that resizes it stands.
const EDGE: f64 = 282.0;

/// One frame as the shell draws the app: laid out at the desktop size, then drawn.
fn frame(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    frame_at(tile, text, (1280.0, 800.0));
}

/// One frame of the app laid out at `size`, then drawn.
fn frame_at(tile: &mut EmbeddedApp, text: &mut TextRenderer, (width, height): (f32, f32)) {
    use ui::widget::{DrawChild as _, Widget as _};
    let theme = tile.app().theme.clone();
    let mut taffy = ui::taffy::TaffyTree::new();
    let node = tile.build_layout(&mut taffy, &theme, &mut HashMap::new());
    let _ = taffy.compute_layout(
        node,
        ui::taffy::Size {
            width: ui::taffy::AvailableSpace::Definite(width),
            height: ui::taffy::AvailableSpace::Definite(height),
        },
    );
    tile.apply_layout(&taffy, node, ui::kurbo::Point::ZERO);
    let mut scene = ui::vello::Scene::new();
    tile.draw_child(
        &mut scene,
        ui::kurbo::Affine::IDENTITY,
        text,
        &theme,
        &ui::presence::PresenceCtx::empty().draw_env(),
    );
}

/// Frames while the window has work pending, as a host draws: what opening it reads on the
/// process's reader threads (its index, its first publication), which another test's window
/// can keep busy, lands before the steps measured.
fn opened(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    frame(tile, text);
    while tile.app_mut().root_widget_mut().has_pending_visual_work() {
        std::thread::sleep(std::time::Duration::from_millis(5));
        frame(tile, text);
    }
}

/// A click at `(x, y)` and the frames that settle it: how many of those frames solved the window's
/// layout.
fn click(tile: &mut EmbeddedApp, text: &mut TextRenderer, (x, y): (f64, f64)) -> usize {
    let mut solved = 0_usize;
    let mut settle = |tile: &mut EmbeddedApp, text: &mut TextRenderer| {
        let before = ui::profiling::layout_counts::read().solves;
        frame(tile, text);
        if ui::profiling::layout_counts::read().solves > before {
            solved = solved.saturating_add(1);
        }
    };
    tile.input_for_test(ui::synthetic_input::cursor_moved(x, y));
    settle(tile, text);
    tile.input_for_test(ui::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Left,
    ));
    settle(tile, text);
    tile.input_for_test(ui::synthetic_input::mouse_input(
        ElementState::Released,
        MouseButton::Left,
    ));
    for _ in 0..3 {
        settle(tile, text);
    }
    solved
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
        opened(tile, &mut text);
        switches(tile, &mut text);
        drags(tile, &mut text);
        scrolls(tile, &mut text);
        resizes(tile, &mut text);
    })
    .expect("the gallery opens in a window");
}

#[test]
fn the_gallery_reads_only_the_faces_its_text_is_set_in() {
    // Every catalog face is listed and mapped before the first frame; a file is read only when text
    // needs one of its faces, and the gallery's first page is set in the interface face alone. The
    // list is the thread laying the window out, so a window another test opens in this binary adds
    // nothing to it.
    crate::support::installed("gallery");
    ui::framework::testing::drive_live_embedded::<TlApp, _>(|tile| {
        let mut text = TextRenderer::new();
        opened(tile, &mut text);
        let read: Vec<String> = ui::font_catalog::font_files_read()
            .iter()
            .filter_map(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(read, ["TiloSans[wdth,wght].ttf"]);
        // The media page sets its code in the bundled monospace face, which has every character
        // it shows: no other face is read for them.
        let _ = click(tile, &mut text, (120.0, ROWS[6]));
        let read: Vec<String> = ui::font_catalog::font_files_read()
            .iter()
            .filter_map(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(read, ["TiloSans[wdth,wght].ttf", "NotoSansMono[wght].ttf"]);
    })
    .expect("the gallery opens in a window");
}

/// What the process holds allocated, in MiB: the bytes its pages and caches keep, not the pages
/// the allocator has yet to hand back to the kernel, which grow and shrink with no leak.
fn held_mib() -> usize {
    tilo_test_env::allocation::held() / (1024 * 1024)
}

/// Every page once, and then again, twice more: the second round costs what the first did, each
/// switch solves its layout without measuring its leaves over and over, and coming back to pages
/// holds no more memory.
fn switches(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    let mut rounds = Vec::new();
    let mut held = vec![held_mib()];
    for _ in 0..4 {
        let _ = ui::profiling::layout_counts::take();
        let mut callbacks = 0_usize;
        for y in ROWS {
            let _ = ui::profiling::layout_counts::take_restyled();
            let (frames, scopes) = observe(|| click(tile, text, (120.0, y)));
            // What a control's layout follows from (its width, what it holds, where its rows
            // stand) is decided within the solve: nothing is styled again after it.
            let restyled = ui::profiling::layout_counts::take_restyled();
            assert!(
                restyled.is_empty(),
                "a page switch (the row at {y}) styles nothing again after its solve: {restyled:?}"
            );
            // A page shown in the content takes the place of the one before under the content's
            // own node: the rest of the window keeps its layout tree.
            assert_eq!(
                calls(&scopes, "ui.rebuild_structure"),
                0,
                "a page switch rebuilds none of the window's structure: {}",
                said(&scopes)
            );
            // Pressing a row changes no size: the press is drawn, not laid out, and the page the
            // release shows is laid out once. The frames after it solve nothing, and no control is
            // left asking for layout, which would lay the window out again on every input. A list
            // that measures its rows for the first time mounts enough of them and places them in
            // that one solve.
            assert!(
                frames <= 1 && calls(&scopes, "ui.solve") <= 1,
                "a page switch (the row at {y}) lays the window out in one frame: {frames} \
                 frames, {}",
                said(&scopes)
            );
            let asking =
                ui::profiling::layout_counts::asking_for_layout(tile.app_mut().root_widget_mut());
            assert!(
                asking.is_empty(),
                "a settled page asks for no layout: {asking:?}"
            );
            callbacks = callbacks.saturating_add(calls(&scopes, "ui.callback_scope"));
        }
        let solved = ui::profiling::layout_counts::take();
        held.push(held_mib());
        // A solve that answers a container from its cache measures each leaf a few times a
        // switch; one that solves nested grids again for every constraint probed measured
        // the Feedback page alone some 12,000 times, and one whose controls state their own
        // sizes only after a solve read them solved every page again for them.
        assert!(
            solved.measures <= 7_000,
            "a round of {} switches measures its leaves {} times",
            ROWS.len(),
            solved.measures
        );
        rounds.push(callbacks);
    }
    // The first round pays what showing a page costs once (its fonts, pictures and caches) and
    // the second what is first paid on coming back to one. Past them, two more rounds hold no
    // more than the second did: a page that kept what it showed would hold more each round. The
    // caches hold a few MiB more or less from round to round as they fill and let go.
    let shown = held[1].saturating_sub(held[0]);
    let again = held[4].saturating_sub(held[2]);
    assert!(
        shown <= 32 && again <= 4,
        "every page shown holds at most 32 MiB more, and showing them twice more holds no more: \
         {shown} MiB, then {again} MiB ({held:?} MiB after each round)"
    );
    let (first, second) = (rounds[0], rounds[1]);
    assert!(
        second <= first.saturating_add(first / 10),
        "switching pages again costs what it did the first time: {first} then {second} \
         component scopes"
    );
}

/// A drag of the sidebar's edge: each step lays the window out once, nothing more.
fn drags(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    let _ = click(tile, text, (120.0, ROWS[0]));
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
                calls(&scopes, "ui.component.sync"),
                calls(&scopes, "framework.action_bindings"),
                calls(&scopes, "framework.publication"),
            ),
            (0, 1, 0, 0, 0, 0),
            "a drag step is one layout, without a rebuild, a view evaluated, a sync or what the \
             app publishes read again: {}",
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

/// A scroller the app shows: its `[x, y, width, height]`, offset, content and viewport.
type Scroller = ([f64; 4], f64, f64, f64);

/// Every scroller the app shows.
fn scrollers(tile: &mut EmbeddedApp) -> Vec<Scroller> {
    fn walk(widget: &dyn ui::widget::Widget, out: &mut Vec<Scroller>) {
        if let Some(region) = widget.primary_vertical_scroll() {
            let rect = widget.get_rect();
            out.push((
                [rect.x0, rect.y0, rect.width(), rect.height()],
                region.metrics.offset,
                region.metrics.content,
                region.metrics.viewport,
            ));
        }
        for child in widget.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tile.app_mut().root_widget_mut(), &mut out);
    out
}

/// The wheel over the page's content moves it here too: the window the shell shows in-process and
/// the headless harness route it the same way.
#[test]
fn the_wheel_over_the_embedded_gallery_scrolls_its_content() {
    crate::support::installed("gallery");
    ui::framework::testing::drive_live_embedded::<TlApp, _>(|tile| {
        let mut text = TextRenderer::new();
        opened(tile, &mut text);
        let _ = click(tile, &mut text, (120.0, 132.0));
        tile.input_for_test(ui::synthetic_input::cursor_moved(1270.0, 400.0));
        frame(tile, &mut text);
        let before = scrollers(tile);
        tile.input_for_test(ui::synthetic_input::mouse_wheel(0.0, -3.0));
        frame(tile, &mut text);
        let after = scrollers(tile);
        let under_pointer = |shown: &[Scroller]| {
            shown
                .iter()
                .find(|(rect, ..)| {
                    let [x, y, width, height] = *rect;
                    (x..=x + width).contains(&1270.0) && (y..=y + height).contains(&400.0)
                })
                .map(|&(_, offset, content, viewport)| (offset, content, viewport))
        };
        assert!(
            under_pointer(&after).is_some_and(|(offset, content, viewport)| {
                content > viewport + 1.0 && offset > 0.0
            }),
            "the wheel moves the content under the pointer: before {before:?}, after {after:?}"
        );
    })
    .expect("the gallery opens in a window");
}

/// The window resized, as maximizing it and dragging its edge do: each size is one layout, with no
/// rebuild and no view evaluated, on every page that switches in place.
fn resizes(tile: &mut EmbeddedApp, text: &mut TextRenderer) {
    for y in ROWS {
        let _ = click(tile, text, (120.0, y));
        for size in [(2560.0, 1440.0), (1600.0, 900.0), (1280.0, 800.0)] {
            let ((), scopes) = observe(|| frame_at(tile, text, size));
            let evaluated = scopes
                .iter()
                .filter(|sample| sample.phase.starts_with("tl."))
                .count();
            // A larger window mounts the rows it brings into view, and solves once more for
            // them. The media page's grid takes another count of columns past a width, and
            // composes it anew; no page evaluates a view for a size.
            assert!(
                calls(&scopes, "ui.layout") == 1
                    && calls(&scopes, "ui.solve") <= 2
                    && calls(&scopes, "ui.rebuild_structure") <= 2
                    && evaluated == 0,
                "a resize to {size:?} on the page at {y} is one layout of at most two solves, \
                 without a view evaluated: {}",
                said(&scopes)
            );
        }
    }
}
