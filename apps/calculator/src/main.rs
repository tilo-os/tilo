//! Calculator — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]

use std::collections::HashMap;
use std::time::Instant;

use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    // `--bench-clicks [N]` drives real Calc-keypad button clicks through the
    // production `handle_event` path and times each event + repaint frame,
    // printing per-click the expression length, handle-event µs, frame µs and
    // CRDT document size — so a runaway (freeze) shows up as a climbing number.
    if args.iter().any(|a| a == "--bench-clicks") {
        let n = parse_bench_n(&args).unwrap_or(400);
        return run_clicks_bench(shots_viewport(), n);
    }
    if let Some(out) = parse_shots_arg(&args) {
        // The headless screenshot shows the lived-in demo state; the live loop
        // (below) boots empty via `app_root`. `QOS_CALC_EMPTY` lets a shot
        // capture that real empty-launch state for verification.
        let root: Box<dyn Widget> = if std::env::var_os("QOS_CALC_EMPTY").is_some() {
            calculator::app_root()
        } else {
            // Shots-only overrides so one binary can capture every state:
            //   QOS_CALC_MODE=calc|2d|3d      — which screen to render
            //   QOS_CALC_HISTORY=open|closed  — force the history sidebar state
            let mut app = calculator::CalculatorApp::seeded();
            match std::env::var("QOS_CALC_MODE").ok().as_deref() {
                Some("2d" | "graph2d") => {
                    app.shots_set_mode(calculator::model::Mode::Graph2d);
                }
                Some("3d" | "graph3d") => {
                    app.shots_set_mode(calculator::model::Mode::Graph3d);
                }
                _ => {}
            }
            match std::env::var("QOS_CALC_HISTORY").ok().as_deref() {
                Some("open") => app.shots_set_history_open(true),
                Some("closed" | "collapsed") => app.shots_set_history_open(false),
                _ => {}
            }
            Box::new(app)
        };
        render_shot_to_png(root, shots_viewport(), &out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            spec,
            calculator::app_root(),
            "dev.qos.calculator",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.calculator");
    #[allow(unused_mut)]
    let mut app = ui::App::new(state, calculator::app_root()).with_autosave(autosave);
    // Register the calculator's action decoder so the shell's Ctrl+K palette and
    // remote replay can decode + replay the same history / graph actions the
    // on-screen affordances dispatch.
    calculator::actions::register(app.action_registry_mut());
    // Widget-debug overlays (bounds outline + widget-name labels) are a
    // development aid only. The toolkit's `App` turns them on from the
    // `QUICK_UI_DEBUG` env var alone, so a var inherited from the launching
    // environment would leak the overlay into a shipped calculator. Force it
    // off in release builds so a plain run is always clean; debug builds keep
    // the env-driven toggle for development.
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
    app.run();
    Ok(())
}

/// Parse the optional click count after `--bench-clicks`.
fn parse_bench_n(args: &[String]) -> Option<usize> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--bench-clicks" {
            return iter.next().and_then(|s| s.parse().ok());
        }
        if let Some(rest) = arg.strip_prefix("--bench-clicks=") {
            return rest.parse().ok();
        }
    }
    None
}

fn synth_cursor(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}
fn synth_click(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

/// One full frame pass (sync → build → compute → apply → draw), mirroring
/// `ui::App`. Returns the live taffy tree + root child node and the total
/// frame microseconds. Kept concrete over `CalculatorApp` so the bench can
/// call the app's `bench_*` accessors between frames.
fn bench_frame(
    app: &mut calculator::CalculatorApp,
    state: &mut ui::State,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    permission: &PermissionContext,
    w: u32,
    h: u32,
) -> (TaffyTree<()>, NodeId, f64) {
    let t0 = Instant::now();
    app.sync(&mut state.doc, &state.root);

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = app.build_layout(&mut taffy, theme, &mut node_map);
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
        let tr = &mut *text_renderer;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
    }
    app.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        text_renderer,
        theme,
        presence,
        permission,
    );
    let us = t0.elapsed().as_secs_f64() * 1e6;
    (taffy, child, us)
}

/// Drive real Calc-keypad clicks through `handle_event` and report per-click
/// cost. This is the reproduction harness for the "freezes after a few button
/// presses" report — headless, no window, identical passes to the live app.
fn run_clicks_bench(viewport: (u32, u32), n: usize) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut state = ui::State::new();
    let mut app = calculator::CalculatorApp::new();
    let modifiers = winit::event::Modifiers::default();

    let dispatch = |app: &mut calculator::CalculatorApp,
                    state: &mut ui::State,
                    taffy: &TaffyTree<()>,
                    child: NodeId,
                    ev: &WindowEvent|
     -> f64 {
        let mut handled = false;
        let mut focus = None;
        let t = Instant::now();
        app.handle_event(
            ev,
            1.0,
            &modifiers,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            child,
            taffy,
        );
        t.elapsed().as_secs_f64() * 1e6
    };

    // Warm caches.
    for _ in 0..3 {
        let _ = bench_frame(
            &mut app,
            &mut state,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
            w,
            h,
        );
    }

    // Switch to Calc mode by clicking its header segment (production path).
    let (mut taffy, mut child, _) = bench_frame(
        &mut app,
        &mut state,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
        w,
        h,
    );
    let seg = app.bench_seg_center(0);
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &synth_cursor(seg.x, seg.y),
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &synth_click(ElementState::Pressed),
    );
    dispatch(
        &mut app,
        &mut state,
        &taffy,
        child,
        &synth_click(ElementState::Released),
    );
    let (t2, c2, _) = bench_frame(
        &mut app,
        &mut state,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
        w,
        h,
    );
    taffy = t2;
    child = c2;
    assert!(
        app.bench_mode_is_calc(),
        "bench should have switched to Calc mode"
    );

    // A never-ending expression: digits + operators, no `=`, so it grows
    // exactly like the user "1 2 3 + 4 5 6 * 7 ..." mashing report.
    let cycle = ["1", "2", "3", "+", "4", "5", "6", "×", "7", "8", "9", "−"];

    println!("calculator --bench-clicks @ {w}x{h}  ({n} clicks; µs, compute-only)");
    println!(
        "{:>5}  {:<4} {:>8} {:>10} {:>10} {:>12}",
        "click", "key", "exprlen", "handle_us", "frame_us", "doc_bytes"
    );

    let mut max_handle = 0.0f64;
    let mut max_frame = 0.0f64;
    for idx in 0..n {
        let label = cycle[idx % cycle.len()];
        let Some(pt) = app.bench_calc_key_center(label) else {
            eprintln!("click {idx}: key {label:?} not found in layout");
            continue;
        };
        let handle_us = dispatch(
            &mut app,
            &mut state,
            &taffy,
            child,
            &synth_cursor(pt.x, pt.y),
        ) + dispatch(
            &mut app,
            &mut state,
            &taffy,
            child,
            &synth_click(ElementState::Pressed),
        ) + dispatch(
            &mut app,
            &mut state,
            &taffy,
            child,
            &synth_click(ElementState::Released),
        );
        let (t2, c2, frame_us) = bench_frame(
            &mut app,
            &mut state,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
            w,
            h,
        );
        taffy = t2;
        child = c2;
        max_handle = max_handle.max(handle_us);
        max_frame = max_frame.max(frame_us);

        // Print every click for the first 20, then sample, plus the doc size.
        if idx < 20 || idx % 20 == 0 || idx == n - 1 {
            let doc_bytes = state.doc.save().len();
            println!(
                "{:>5}  {:<4} {:>8} {:>10.0} {:>10.0} {:>12}",
                idx,
                label,
                app.bench_expr_len(),
                handle_us,
                frame_us,
                doc_bytes
            );
        }
    }
    println!("---");
    println!("max handle_us = {max_handle:.0}   max frame_us = {max_frame:.0}");
    Ok(())
}

/// Viewport for the headless screenshot. Defaults to a wide desktop frame;
/// `QOS_CALC_SHOTS_SIZE=WxH` overrides it (e.g. to capture the compact keypad
/// + overlay-history behaviour at a narrow size).
fn shots_viewport() -> (u32, u32) {
    if let Some(s) = std::env::var_os("QOS_CALC_SHOTS_SIZE") {
        let s = s.to_string_lossy();
        if let Some((w, h)) = s.split_once(['x', 'X', '*'])
            && let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse())
        {
            return (w, h);
        }
    }
    DEFAULT_SHOTS_VIEWPORT
}

fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme so
    // theme-driven chrome can be proven across palettes; defaults to dark.
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let mut presence = PresenceCtx::empty();
    // Shots-only: seed a remote collaborator's cursor at `x,y` so the headless
    // frame shows a peer caret mapped into the active math field's 2D layout.
    if let Some(spec) = std::env::var_os("QOS_CALC_REMOTE_CURSOR") {
        let spec = spec.to_string_lossy();
        if let Some((sx, sy)) = spec.split_once(',')
            && let (Ok(x), Ok(y)) = (sx.trim().parse::<f32>(), sy.trim().parse::<f32>())
        {
            presence.upsert_remote_cursor("alice".into(), [x, y], Some("Alice"));
        }
    }
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;

    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("calculator: wrote shot to {}", out.display());
    Ok(())
}
