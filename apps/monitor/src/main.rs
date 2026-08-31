//! Monitor — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).

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
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
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

/// Monitor's binary entry point — see the module docs for the supported modes.
///
/// # Errors
///
/// Returns an error when `--shots` or `--bench-interact` fails to build or
/// lay out the offscreen widget tree (a Taffy tree-construction or
/// layout-compute failure).
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
            &spec,
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

/// The seven interactions `--bench-interact` times: two tab switches, a
/// scroll pair, a row click and two toggles — the everyday inputs the
/// process/resources views take.
fn bench_actions() -> Vec<(&'static str, BenchAction)> {
    vec![
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
    ]
}

/// Bundles the rig `--bench-interact` drives frames and events through, so
/// each step's dependency on "the whole app" doesn't spill into a long
/// parameter list on every helper.
struct BenchHarness {
    widget: Box<dyn Widget>,
    state: ui::State,
    text_renderer: TextRenderer,
    theme: Theme,
    presence: PresenceCtx,
    permission: PermissionContext,
    renderer: OffscreenRenderer,
    modifiers: winit::event::Modifiers,
    viewport: (u32, u32),
}

impl BenchHarness {
    fn new(viewport: (u32, u32)) -> Self {
        let (w, h) = viewport;
        Self {
            widget: Box::new(monitor::MonitorApp::new()),
            state: ui::State::new(),
            text_renderer: TextRenderer::new(),
            theme: Theme::dark(),
            presence: PresenceCtx::empty(),
            permission: PermissionContext::permissive(),
            renderer: OffscreenRenderer::new(w, h),
            modifiers: winit::event::Modifiers::default(),
            viewport,
        }
    }

    /// One full frame pass (sync → `build_layout` → compute → apply →
    /// draw), mirroring `ui::App`. Returns the drawn scene, the live taffy
    /// tree + root child node (to dispatch events into the just-laid-out
    /// tree) and per-pass microseconds `[sync, build, layout, apply, draw]`.
    fn frame(
        &mut self,
    ) -> Result<(Scene, TaffyTree<()>, NodeId, [f64; 5]), Box<dyn std::error::Error>> {
        let (w, h) = self.viewport;
        let us = |t: Instant| t.elapsed().as_secs_f64() * 1e6;

        let t = Instant::now();
        self.widget.sync(&mut self.state.doc, &self.state.root);
        let sync_us = us(t);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let t = Instant::now();
        let child = self
            .widget
            .build_layout(&mut taffy, &self.theme, &mut node_map);
        let build_us = us(t);

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

        let t = Instant::now();
        {
            let measure_map = node_map.clone();
            let tr = &mut self.text_renderer;
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
        let layout_us = us(t);

        let t = Instant::now();
        self.widget.apply_layout(&taffy, child, Point::ORIGIN);
        let apply_us = us(t);

        let mut scene = Scene::new();
        let t = Instant::now();
        self.widget.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
            &self.presence,
            &self.permission,
        );
        let draw_us = us(t);

        Ok((
            scene,
            taffy,
            child,
            [sync_us, build_us, layout_us, apply_us, draw_us],
        ))
    }

    /// Dispatch one synthetic event through the production `handle_event`
    /// path against the tree from the most recent [`Self::frame`] and time it.
    fn dispatch(&mut self, ev: &WindowEvent, child: NodeId, taffy: &TaffyTree<()>) -> f64 {
        let mut handled = false;
        let mut focus = None;
        let t = Instant::now();
        self.widget.handle_event(
            ev,
            1.0,
            &self.modifiers,
            &mut self.state.doc,
            &self.state.root,
            &mut handled,
            &mut focus,
            child,
            taffy,
        );
        t.elapsed().as_secs_f64() * 1e6
    }

    /// Drive `action`'s synthetic events into the currently laid-out tree,
    /// repaint (full frame + GPU scene encode), and return the event
    /// microseconds, the repaint's per-pass breakdown, and the encode
    /// microseconds.
    fn run_action(
        &mut self,
        action: &BenchAction,
    ) -> Result<(f64, [f64; 5], f64), Box<dyn std::error::Error>> {
        // Lay out the current state so events dispatch into a real tree.
        let (_scene, taffy, child, _) = self.frame()?;
        let event_us = match *action {
            BenchAction::Click(x, y) => {
                self.dispatch(&synth_cursor(x, y), child, &taffy)
                    + self.dispatch(&synth_click(ElementState::Pressed), child, &taffy)
                    + self.dispatch(&synth_click(ElementState::Released), child, &taffy)
            }
            BenchAction::Scroll(x, y, dy) => {
                self.dispatch(&synth_cursor(x, y), child, &taffy)
                    + self.dispatch(&synth_wheel(dy), child, &taffy)
            }
        };

        // The repaint reflecting the event: full frame + GPU scene encode.
        let (scene, _taffy, _child, frame_us) = self.frame()?;
        let te = Instant::now();
        let _rgba = self.renderer.render_rgba(&scene);
        let encode_us = te.elapsed().as_secs_f64() * 1e6;

        Ok((event_us, frame_us, encode_us))
    }
}

/// Print the p50/p95/max event→frame-ready totals across all bench actions.
fn report_bench_totals(totals: &[f64]) {
    let mut totals = totals.to_vec();
    totals.sort_by(f64::total_cmp);
    let percentile = |q: f64| {
        let idx = (((totals.len() - 1) as f64) * q).round() as usize;
        totals.get(idx).copied().unwrap_or(0.0)
    };
    println!("---");
    println!(
        "event->frame-ready TOTAL: p50={:.0}us  p95={:.0}us  max={:.0}us   (60fps budget = 16667us)",
        percentile(0.5),
        percentile(0.95),
        percentile(1.0)
    );
}

/// Drive real input through the production `handle_event` path and time the full
/// event → frame-ready compute cost. Everything here runs on the CPU/GPU exactly
/// as the live app, MINUS the window-system present — so a fast result here with
/// a slow live app points at the compositor/DisplayLink present, not the app.
fn run_interaction_bench(viewport: (u32, u32)) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let mut bench = BenchHarness::new(viewport);

    // Warm caches (the first frames pay cold text-shape + allocation cost).
    for _ in 0..3 {
        bench.frame()?;
    }

    println!(
        "monitor interaction bench @ {w}x{h}  (microseconds; compute-only, excludes window present)"
    );
    println!(
        "{:<28} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9} {:>9}",
        "action", "event", "sync", "layout", "draw", "encode", "frame", "TOTAL"
    );

    let mut totals = Vec::new();
    for (label, action) in bench_actions() {
        let (event_us, ft, encode_us) = bench.run_action(&action)?;
        let frame_us = ft.iter().sum::<f64>();
        let total = event_us + frame_us + encode_us;
        totals.push(total);
        println!(
            "{:<28} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>9.0} {:>9.0}",
            label, event_us, ft[0], ft[2], ft[4], encode_us, frame_us, total
        );
    }

    report_bench_totals(&totals);
    Ok(())
}
