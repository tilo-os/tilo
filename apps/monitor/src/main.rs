//! Monitor — thin binary entry point. All logic lives in the library.
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
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    // `--bench-interact` drives real input events (clicks, tab switches,
    // scrolls) through the production `handle_event` path and times the full
    // event -> frame-ready compute cost (handle_event + sync + layout + draw +
    // scene encode) — everything EXCEPT the window-system present. This isolates
    // app/toolkit latency from the compositor/DisplayLink present cost.
    if args.iter().any(|a| a == "--bench-interact") {
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        return run_interaction_bench(viewport);
    }
    if let Some(out) = parse_shots_arg(&args) {
        // `--shots` renders one offscreen frame. A `--viewport WxH` arg
        // overrides the default desktop viewport so the same binary can
        // capture the narrow responsive layout (single-pane Processes push +
        // stacked dashboard). `--scenario NAME` snaps a deterministic
        // interaction state (`resources` = Resources dashboard, `detail` =
        // pushed detail pane) since a single frame has no event loop.
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let mut app = monitor::MonitorApp::new();
        if let Some(scenario) = parse_scenario_arg(&args) {
            app.seed_shot_scenario(&scenario);
        }
        render_shot_to_png(Box::new(app), viewport, &out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            spec,
            monitor::app_root(),
            "dev.qos.monitor",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.monitor");
    #[allow(unused_mut)]
    let mut app = ui::App::new(state, monitor::app_root()).with_autosave(autosave);
    // Widget-debug overlays (bounds outline + widget-name labels) are a
    // development aid the toolkit enables from the `QUICK_UI_DEBUG` env var
    // alone. Force them off in release so a var inherited from the launching
    // environment can never leak the overlay into a shipped Monitor; debug
    // builds keep the env-driven toggle.
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
    app.run();
    Ok(())
}

/// Parse `--viewport WxH` (e.g. `--viewport 480x900`) for `--shots`.
fn parse_viewport_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let spec = if arg == "--viewport" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--viewport=")
        };
        if let Some(spec) = spec {
            let (w, h) = spec.split_once(['x', 'X'])?;
            return Some((w.trim().parse().ok()?, h.trim().parse().ok()?));
        }
    }
    None
}

/// Parse `--scenario NAME` for a deterministic `--shots` interaction state.
fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_string());
        }
    }
    None
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
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme
    // so the same frame can be graded in both light and dark (default dark).
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let presence = PresenceCtx::empty();
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
    println!("monitor: wrote shot to {}", out.display());
    Ok(())
}

/// One full frame pass (sync → `build_layout` → compute → apply → draw), mirroring
/// `ui::App`. Returns the drawn scene, the live taffy tree + root child node
/// (to dispatch events into the just-laid-out tree) and per-pass microseconds
/// `[sync, build, layout, apply, draw]`.
fn bench_frame(
    widget: &mut Box<dyn Widget>,
    state: &mut ui::State,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    permission: &PermissionContext,
    w: u32,
    h: u32,
) -> (Scene, TaffyTree<()>, NodeId, [f64; 5]) {
    let us = |t: Instant| t.elapsed().as_secs_f64() * 1e6;

    let t = Instant::now();
    widget.sync(&mut state.doc, &state.root);
    let sync_us = us(t);

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let t = Instant::now();
    let child = widget.build_layout(&mut taffy, theme, &mut node_map);
    let build_us = us(t);

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

    let t = Instant::now();
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
    let layout_us = us(t);

    let t = Instant::now();
    widget.apply_layout(&taffy, child, Point::ORIGIN);
    let apply_us = us(t);

    let mut scene = Scene::new();
    let t = Instant::now();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        text_renderer,
        theme,
        presence,
        permission,
    );
    let draw_us = us(t);

    (
        scene,
        taffy,
        child,
        [sync_us, build_us, layout_us, apply_us, draw_us],
    )
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
fn synth_wheel(dy: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, dy),
        phase: TouchPhase::Moved,
    }
}

enum BenchAction {
    Click(f64, f64),
    Scroll(f64, f64, f32),
}

/// Drive real input through the production `handle_event` path and time the full
/// event → frame-ready compute cost. Everything here runs on the CPU/GPU exactly
/// as the live app, MINUS the window-system present — so a fast result here with
/// a slow live app points at the compositor/DisplayLink present, not the app.
fn run_interaction_bench(viewport: (u32, u32)) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut state = ui::State::new();
    let mut widget: Box<dyn Widget> = Box::new(monitor::MonitorApp::new());
    let modifiers = winit::event::Modifiers::default();

    // Warm caches (the first frames pay cold text-shape + allocation cost).
    for _ in 0..3 {
        let _ = bench_frame(
            &mut widget,
            &mut state,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
            w,
            h,
        );
    }
    let mut renderer = OffscreenRenderer::new(w, h);

    let actions: Vec<(&str, BenchAction)> = vec![
        ("click Resources tab", BenchAction::Click(705.0, 42.0)),
        ("click Processes tab", BenchAction::Click(575.0, 42.0)),
        (
            "scroll process table down",
            BenchAction::Scroll(700.0, 300.0, -8.0),
        ),
        (
            "scroll process table up",
            BenchAction::Scroll(700.0, 300.0, 8.0),
        ),
        ("click a process row", BenchAction::Click(700.0, 240.0)),
        ("toggle kernel threads", BenchAction::Click(1205.0, 102.0)),
        ("toggle Tree/flat", BenchAction::Click(985.0, 102.0)),
    ];

    println!(
        "monitor interaction bench @ {w}x{h}  (microseconds; compute-only, excludes window present)"
    );
    println!(
        "{:<28} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9} {:>9}",
        "action", "event", "sync", "layout", "draw", "encode", "frame", "TOTAL"
    );

    let mut totals = Vec::new();
    for (label, action) in &actions {
        // Lay out the current state so events dispatch into a real tree.
        let (_s, taffy, child, _) = bench_frame(
            &mut widget,
            &mut state,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
            w,
            h,
        );
        let dispatch =
            |widget: &mut Box<dyn Widget>, state: &mut ui::State, ev: &WindowEvent| -> f64 {
                let mut handled = false;
                let mut focus = None;
                let t = Instant::now();
                widget.handle_event(
                    ev,
                    1.0,
                    &modifiers,
                    &mut state.doc,
                    &state.root,
                    &mut handled,
                    &mut focus,
                    child,
                    &taffy,
                );
                t.elapsed().as_secs_f64() * 1e6
            };
        let event_us = match action {
            BenchAction::Click(x, y) => {
                dispatch(&mut widget, &mut state, &synth_cursor(*x, *y))
                    + dispatch(&mut widget, &mut state, &synth_click(ElementState::Pressed))
                    + dispatch(
                        &mut widget,
                        &mut state,
                        &synth_click(ElementState::Released),
                    )
            }
            BenchAction::Scroll(x, y, dy) => {
                dispatch(&mut widget, &mut state, &synth_cursor(*x, *y))
                    + dispatch(&mut widget, &mut state, &synth_wheel(*dy))
            }
        };
        // The repaint reflecting the event: full frame + GPU scene encode.
        let (scene, _t, _c, ft) = bench_frame(
            &mut widget,
            &mut state,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
            w,
            h,
        );
        let te = Instant::now();
        let _rgba = renderer.render_rgba(&scene);
        let encode_us = te.elapsed().as_secs_f64() * 1e6;
        let frame_us = ft.iter().sum::<f64>();
        let total = event_us + frame_us + encode_us;
        totals.push(total);
        println!(
            "{:<28} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>9.0} {:>9.0}",
            label, event_us, ft[0], ft[2], ft[4], encode_us, frame_us, total
        );
    }

    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| totals[(((totals.len() - 1) as f64) * q).round() as usize];
    println!("---");
    println!(
        "event->frame-ready TOTAL: p50={:.0}us  p95={:.0}us  max={:.0}us   (60fps budget = 16667us)",
        p(0.5),
        p(0.95),
        p(1.0)
    );
    Ok(())
}
