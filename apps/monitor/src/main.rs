//! Monitor — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// Monitor's binary entry point — see the module docs for the supported modes.
///
/// # Errors
///
/// Returns an error when `--shots` or `--bench-interact` fails to build or
/// lay out the offscreen widget tree (a Taffy tree-construction or
/// layout-compute failure).
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    ui::launch::run(&launch_spec(monitor::app_root))?;
    Ok(())
}

fn launch_spec(root: fn() -> Box<dyn Widget>) -> LaunchSpec {
    launch_spec_base(root).history(&monitor::framework_history::HISTORY)
}

fn launch_spec_base(root: fn() -> Box<dyn Widget>) -> LaunchSpec {
    LaunchSpec::new("so.tilo.monitor", root)
        .shot_handler(render_shot)
        .scenarios(&[
            "detail",
            "interval-popover",
            "resources",
            "resources-many",
            "empty",
            "process-loading",
            "process-permission-error",
            "no-results",
            "tree-collapsed",
            "tree-expanded",
            "per-process",
            "per-process-collapsed",
            "context-menu",
            "kill-confirm",
        ])
        .configure_app(configure_app)
}

fn configure_app(app: &mut ui::App) {
    let _ = app;
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    let mut app = monitor::MonitorApp::new();
    if let Some(scenario) = args.scenario.as_deref() {
        app.seed_shot_scenario(scenario);
    }
    render_shot_to_png(
        Box::new(app),
        args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
        ui::launch::shot_theme(args),
        out,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
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

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
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

    let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
    // SAFETY: the bound nodes come from this exact exclusively owned build.
    unsafe {
        bindings.rebuild(widget.as_ref(), &taffy, child, &node_map);
        ui::widget::compute_widget_layout(
            &mut taffy,
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            &node_map,
            &bindings,
            &mut text_renderer,
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

/// [`BenchHarness::frame`]'s result: the drawn scene, the live taffy tree +
/// root child node (to dispatch events into the just-laid-out tree), and
/// per-pass microseconds `[sync, build, layout, apply, draw]`.
type FramePass = (Scene, TaffyTree<()>, NodeId, [f64; 5]);

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
    fn frame(&mut self) -> Result<FramePass, Box<dyn std::error::Error>> {
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

#[cfg(test)]
mod launch_tests {
    use super::*;
    use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryScopeDefinition};

    static FOREIGN_HISTORY: HistoryDefinition = HistoryDefinition {
        app_id: "so.tilo.foreign",
        scopes: &[HistoryScopeDefinition {
            key: "foreign",
            title: "Foreign",
            presentation: HistoryPresentation::External { reason: "foreign" },
            content_schema: None,
        }],
    };

    fn inert_root() -> Box<dyn Widget> {
        Box::new(monitor::MonitorApp::new())
    }

    fn memory_state(_: &'static str) -> (ui::State, ui::persist::Autosave) {
        (
            ui::State::new(),
            ui::persist::Autosave::disabled("controlled Monitor launch"),
        )
    }

    #[test]
    fn production_launch_spec_installs_the_root_history_definition() {
        let spec = launch_spec(inert_root).open_app_global_state(memory_state);
        spec.validate_history_declaration().unwrap();
        let mut app = spec.build_controlled_app_for_test().unwrap();
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(std::ptr::eq(
            installed.unwrap(),
            &monitor::framework_history::HISTORY
        ));
        assert!(std::ptr::eq(
            root.unwrap(),
            &monitor::framework_history::HISTORY
        ));
        assert!(
            app.build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("History"))
        );
    }

    #[test]
    fn missing_and_foreign_launch_declarations_are_not_admitted() {
        let missing = launch_spec_base(inert_root).open_app_global_state(memory_state);
        assert_eq!(
            missing.validate_history_declaration(),
            Err(ui::history_mode::HistoryRegistrationError::Missing)
        );
        let app = missing.build_controlled_app_for_test().unwrap();
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(installed.is_none());
        assert!(std::ptr::eq(
            root.unwrap(),
            &monitor::framework_history::HISTORY
        ));

        let foreign = launch_spec_base(inert_root)
            .history(&FOREIGN_HISTORY)
            .open_app_global_state(memory_state);
        assert_eq!(
            foreign.validate_history_declaration(),
            Err(ui::history_mode::HistoryRegistrationError::ForeignSource)
        );
        assert!(matches!(
            foreign.build_controlled_app_for_test(),
            Err(LaunchError::Operation(message)) if message.contains("different installed application")
        ));
    }
}
