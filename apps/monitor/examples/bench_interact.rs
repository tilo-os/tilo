//! Times Monitor's everyday interactions — two tab switches, a scroll pair,
//! a row click and two toggles — through the production `handle_event` path:
//! the full event → frame-ready compute cost (`handle_event` + sync + layout +
//! draw + scene encode), everything except the window-system present. This
//! isolates app/toolkit latency from the compositor's present cost.
//!
//! `cargo run -p monitor --example bench_interact [-- --viewport WxH]`

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "benchmark arithmetic over timings and pixel coordinates"
)]

use std::collections::HashMap;
use ui::time::Instant;

use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, WindowEvent};

const DEFAULT_VIEWPORT: (u32, u32) = (1280, 800);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    run_interaction_bench(parse_viewport_arg(&args).unwrap_or(DEFAULT_VIEWPORT))
}

/// Parse `--viewport WxH` (e.g. `--viewport 480x900`).
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

use ui::synthetic_input::cursor_moved;
fn synth_click(state: ElementState) -> WindowEvent {
    ui::synthetic_input::mouse_input(state, ui::winit::event::MouseButton::Left)
}
fn synth_wheel(dy: f32) -> WindowEvent {
    ui::synthetic_input::mouse_wheel(0.0, dy)
}

enum BenchAction {
    Click(f64, f64),
    Scroll(f64, f64, f32),
}

/// Where the control an accessibility node describes sits: the node whose
/// semantic key, or else whose name (and value, when given), matches.
fn control_center(
    node: &serde_json::Value,
    key: Option<&str>,
    name: &str,
    value: Option<&str>,
) -> Option<(f64, f64)> {
    let text = |field: &str| node.get(field).and_then(serde_json::Value::as_str);
    let matches = match key {
        Some(key) => text("semantic_key") == Some(key),
        None => {
            text("name") == Some(name) && value.is_none_or(|value| text("value") == Some(value))
        }
    };
    if matches && let Some(bounds) = node.get("bounds").filter(|bounds| !bounds.is_null()) {
        let coordinate = |field: &str| bounds.get(field).and_then(serde_json::Value::as_f64);
        return Some((
            f64::midpoint(coordinate("x0")?, coordinate("x1")?),
            f64::midpoint(coordinate("y0")?, coordinate("y1")?),
        ));
    }
    match node {
        serde_json::Value::Object(fields) => fields
            .values()
            .find_map(|child| control_center(child, key, name, value)),
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|child| control_center(child, key, name, value)),
        _ => None,
    }
}

/// The seven interactions this benchmark times: two tab switches, a
/// scroll pair, a row click and two toggles — the everyday inputs the
/// process/resources views take — aimed where the window lays out each
/// control at `viewport`.
fn bench_actions(
    viewport: (u32, u32),
) -> Result<Vec<(&'static str, BenchAction)>, Box<dyn std::error::Error>> {
    let mut harness = ui::headless::FrameHarness::for_root(
        ui::State::new(),
        monitor::MonitorApp::new(),
        viewport,
        Theme::dark(),
    );
    for _ in 0..4 {
        harness.relayout();
    }
    let inspection: serde_json::Value = serde_json::from_str(&harness.inspection_json()?)?;
    let at = |key: Option<&str>, name: &str, value: Option<&str>| {
        control_center(&inspection, key, name, value)
            .ok_or_else(|| format!("`{name}` is not on screen at {viewport:?}"))
    };
    let resources = at(Some("monitor.tabs:segment:1"), "Resources", None)?;
    let processes = at(Some("monitor.tabs:segment:0"), "Processes", None)?;
    let table = at(None, "Processes", None)?;
    let row = at(None, "Name", Some("tilo-fs"))?;
    let kernel = at(None, "Kernel threads", None)?;
    let tree = at(None, "Tree view", None)?;
    Ok(vec![
        (
            "click Resources tab",
            BenchAction::Click(resources.0, resources.1),
        ),
        (
            "click Processes tab",
            BenchAction::Click(processes.0, processes.1),
        ),
        (
            "scroll process table down",
            BenchAction::Scroll(table.0, table.1, -8.0),
        ),
        (
            "scroll process table up",
            BenchAction::Scroll(table.0, table.1, 8.0),
        ),
        ("click a process row", BenchAction::Click(row.0, row.1)),
        (
            "toggle kernel threads",
            BenchAction::Click(kernel.0, kernel.1),
        ),
        ("toggle Tree/flat", BenchAction::Click(tree.0, tree.1)),
    ])
}

/// [`BenchHarness::frame`]'s result: the drawn scene, the live taffy tree +
/// root child node (to dispatch events into the just-laid-out tree), and
/// per-pass microseconds `[sync, build, layout, apply, draw]`.
type FramePass = (Scene, TaffyTree<()>, NodeId, [f64; 5]);

/// Bundles the rig this benchmark drives frames and events through, so
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

        let t = ui::time::now();
        self.widget.sync(&self.state.doc, &self.state.root);
        let sync_us = us(t);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let t = ui::time::now();
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

        let t = ui::time::now();
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

        let t = ui::time::now();
        self.widget.apply_layout(&taffy, child, Point::ORIGIN);
        let apply_us = us(t);

        let mut scene = Scene::new();
        let t = ui::time::now();
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
        let t = ui::time::now();
        self.widget.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &self.modifiers,
                state: ui::widget::Edits::lend(&mut self.state.doc),
                path: &self.state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: child,
                taffy,
            },
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
                self.dispatch(&cursor_moved(x, y), child, &taffy)
                    + self.dispatch(&synth_click(ElementState::Pressed), child, &taffy)
                    + self.dispatch(&synth_click(ElementState::Released), child, &taffy)
            }
            BenchAction::Scroll(x, y, dy) => {
                self.dispatch(&cursor_moved(x, y), child, &taffy)
                    + self.dispatch(&synth_wheel(dy), child, &taffy)
            }
        };

        // The repaint reflecting the event: full frame + GPU scene encode.
        let (scene, _taffy, _child, frame_us) = self.frame()?;
        let te = ui::time::now();
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
    for (label, action) in bench_actions(viewport)? {
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
