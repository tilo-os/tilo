//! Dashboard-style perf benchmark for the incremental renderer.
//!
//! Builds a *large* widget tree (~250 widgets — many labels and
//! buttons across 8 columns, a couple of complex widgets per
//! column) and drives a realistic mix of events at it:
//!
//!   - 70 % `CursorMoved` events that DON'T cross widget edges —
//!     these must be no-ops with the redraw gate.
//!   - 20 % hover-edge crossings — these mutate hover state on
//!     exactly one widget per event, so only that widget's cache
//!     should invalidate.
//!   -  5 % clicks on a ToggleGroup — mutates `selected`, re-
//!     emits the picker, every other widget hits its cache.
//!   -  5 % "complex" events (spinner-style animation ticks where
//!      every animating widget marks itself dirty).
//!
//! The bench tallies:
//!   - "no-op" events that didn't request a redraw (proves Unit 2).
//!   - draw_to_scene mean cost across redrawing events.
//!   - vello render mean cost.
//!   - effective throughput Hz = 1 / total mean per event.
//!
//! Run with:
//! ```
//! cargo run -p quick-ui --release --example dashboard_perf_bench
//! ```
//! Override iterations: `QUICK_UI_DASH_ITERS=2000`.

use std::collections::HashMap;
use std::time::Instant;

use automerge::AutoCommit;
use quick_ui::i18n::I18nManager;
use quick_ui::offscreen::OffscreenRenderer;
use quick_ui::permissions::PermissionContext;
use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::toggle_group::Segment;
use quick_ui::widget::{
    Avatar, Badge, Button, Column, ProgressBar, Row, ScrollAxis, Scrollable, Spinner, Text,
    ToggleGroup, Widget,
};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};

/// One panel of the dashboard: header + a column of mixed widgets.
fn panel(title_key: &'static str, idx: usize) -> Box<dyn Widget> {
    let mut col = Column::new()
        .push(Box::new(Text::new(title_key).with_size(18.0)));

    // Twenty rows. To exercise the Unit 2 redraw gate, only
    // widgets whose `needs_redraw` is genuinely opt-in show up
    // here — Toggle / Checkbox / Slider / DatePicker / etc. still
    // default to `true` and would peg the gate forever. Once
    // those are migrated this can grow back.
    for i in 0..20 {
        let row_idx = idx * 20 + i;
        let label_key = leak_label(&format!("dash_lbl_{row_idx}"));
        match i % 3 {
            0 => col = col.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(label_key)))
                    .push(Box::new(Button::new(
                        leak_label(&format!("dash_btn_{row_idx}")),
                        leak_label(&format!("dash_btnlbl_{row_idx}")),
                        None,
                    ))),
            )),
            1 => col = col.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(label_key)))
                    .push(Box::new(Badge::count((i as u32) * 3))),
            )),
            _ => col = col.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(label_key)))
                    .push(Box::new(Avatar::from_initials(
                        &format!("U{i}"),
                        24.0,
                    ))),
            )),
        }
    }

    Box::new(col)
}

/// Leak short strings so we can hand them to widget constructors
/// that want `&'static str`. This is a benchmark; never do this in
/// real code. Capped at a few hundred labels by the panel /
/// per-panel-row counts so the leak is bounded.
fn leak_label(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn build_tree(include_animated: bool) -> Box<dyn Widget> {
    // 8 columns × 20 rows × (label + control) ≈ 320 widgets.
    // Plus a header row of complex widgets.
    let mut header = Row::new()
        .push(Box::new(Text::new("dash_title").with_size(24.0)))
        .push(Box::new(ToggleGroup::new(
            vec![
                Segment::new("Day"),
                Segment::new("Week"),
                Segment::new("Month"),
            ],
            1,
        )));
    if include_animated {
        header = header
            .push(Box::new(ProgressBar::new(0.4)))
            .push(Box::new(ProgressBar::indeterminate()))
            .push(Box::new(ProgressBar::indeterminate()))
            .push(Box::new(Spinner::with_size(24.0)))
            .push(Box::new(Spinner::with_size(24.0)))
            .push(Box::new(Spinner::with_size(24.0)))
            .push(Box::new(Spinner::with_size(24.0)))
            .push(Box::new(Spinner::with_size(24.0)))
            .push(Box::new(Spinner::with_size(24.0)))
            .push(Box::new(Spinner::with_size(24.0)))
            .push(Box::new(Spinner::with_size(24.0)));
    }

    let mut panels = Row::new();
    let titles = [
        "dash_p0", "dash_p1", "dash_p2", "dash_p3",
        "dash_p4", "dash_p5", "dash_p6", "dash_p7",
    ];
    for (i, t) in titles.iter().enumerate() {
        panels = panels.push(panel(t, i));
    }

    // Footer kept light — only migrated widgets so the static-mode
    // gate experiment isn't poisoned by Breadcrumb / Slider / etc.
    // still returning `needs_redraw = true` by default.
    let footer = Row::new()
        .push(Box::new(Text::new("dash_title")))
        .push(Box::new(Avatar::from_initials("DB", 28.0)))
        .push(Box::new(Avatar::from_initials("QD", 28.0)))
        .push(Box::new(Badge::count(42)));

    Box::new(
        Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(
            Column::new()
                .with_intrinsic_height()
                .push(Box::new(header))
                .push(Box::new(panels))
                .push(Box::new(footer)),
        )),
    )
}

#[derive(Default, Clone)]
struct Phase {
    handle_us: u128,
    layout_us: u128,
    draw_us: u128,
    render_us: u128,
    total_us: u128,
}

fn full_draw(
    root: &mut Box<dyn Widget>,
    taffy: &mut TaffyTree<()>,
    node_map: &HashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
    text_renderer: &mut TextRenderer,
    presence: &PresenceCtx,
    perms: &PermissionContext,
    theme: &Theme,
    scene: &mut Scene,
    renderer: &mut OffscreenRenderer,
    canvas_w: u32,
    canvas_h: u32,
    phase: &mut Phase,
) {
    let t_layout = Instant::now();
    if root.is_layout_dirty() {
        let _ = taffy.compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(canvas_w as f32),
                height: AvailableSpace::Definite(canvas_h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, text_renderer) }
                } else {
                    Size::ZERO
                }
            },
        );
        root.apply_layout(taffy, root_node, Point::ZERO);
    }
    phase.layout_us = t_layout.elapsed().as_micros();

    let t_draw = Instant::now();
    scene.reset();
    root.draw_with_presence(scene, Affine::IDENTITY, text_renderer, theme, presence, perms);
    phase.draw_us = t_draw.elapsed().as_micros();

    let t_render = Instant::now();
    let _ = renderer.render(scene, theme.tokens.colors.surfaces.base);
    phase.render_us = t_render.elapsed().as_micros();

    root.mark_drawn();
}

fn dispatch(
    root: &mut Box<dyn Widget>,
    taffy: &TaffyTree<()>,
    root_node: NodeId,
    doc: &mut AutoCommit,
    event: &WindowEvent,
) {
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    let modifiers = Modifiers::default();
    root.handle_event(
        event, 1.0, &modifiers, doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, taffy,
    );
}

fn make_cursor(x: f64, y: f64) -> WindowEvent {
    let device_id: DeviceId = unsafe { std::mem::zeroed() };
    WindowEvent::CursorMoved { device_id, position: PhysicalPosition::new(x, y) }
}

fn make_click_press() -> WindowEvent {
    let device_id: DeviceId = unsafe { std::mem::zeroed() };
    WindowEvent::MouseInput { device_id, state: ElementState::Pressed, button: MouseButton::Left }
}

fn make_click_release() -> WindowEvent {
    let device_id: DeviceId = unsafe { std::mem::zeroed() };
    WindowEvent::MouseInput { device_id, state: ElementState::Released, button: MouseButton::Left }
}

fn make_wheel(dy: f32) -> WindowEvent {
    let device_id: DeviceId = unsafe { std::mem::zeroed() };
    WindowEvent::MouseWheel {
        device_id,
        delta: MouseScrollDelta::LineDelta(0.0, dy),
        phase: TouchPhase::Moved,
    }
}

fn main() {
    I18nManager::init("en-US");
    // Register every label key so `Text::new(key)` resolves
    // through i18n without panicking.
    let mut ftl = String::new();
    ftl.push_str("dash_title = QuickDesktop Dashboard\n");
    for i in 0..8 {
        ftl.push_str(&format!("dash_p{i} = Panel {i}\n"));
    }
    for row_idx in 0..160 {
        ftl.push_str(&format!("dash_lbl_{row_idx} = Metric {row_idx}\n"));
        ftl.push_str(&format!("dash_btn_{row_idx} = btn_{row_idx}\n"));
        ftl.push_str(&format!("dash_btnlbl_{row_idx} = Open\n"));
    }
    I18nManager::add_resource("en-US", ftl);

    let iters: usize = std::env::var("QUICK_UI_DASH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    let static_mode = std::env::var("QUICK_UI_DASH_STATIC").is_ok();
    let include_animated = !static_mode;
    let (canvas_w, canvas_h) = (1800u32, 1200u32);
    println!("mode: {}", if static_mode {
        "static (no spinners / indeterminate bars — gate-skipped events should dominate)"
    } else {
        "animated (header contains spinners + indeterminate progress)"
    });

    let mut renderer = OffscreenRenderer::new(canvas_w, canvas_h);
    let theme = Theme::dark();
    let mut text_renderer = TextRenderer::new();
    let presence = PresenceCtx::default();
    let perms = PermissionContext::permissive();
    let mut doc = AutoCommit::new();

    let mut root = build_tree(include_animated);
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);
    let mut scene = Scene::new();

    // Count the total widget tree size for the report.
    fn count_widgets(w: &dyn Widget) -> usize {
        1 + w.children().iter().map(|c| count_widgets(*c)).sum::<usize>()
    }
    let tree_size = count_widgets(root.as_ref());
    println!("dashboard widget tree: {tree_size} widgets");

    // Two warm-up frames: first compute_layout, second post-cache.
    for _ in 0..2 {
        let mut warm = Phase::default();
        full_draw(
            &mut root, &mut taffy, &node_map, root_node,
            &mut text_renderer, &presence, &perms, &theme,
            &mut scene, &mut renderer, canvas_w, canvas_h, &mut warm,
        );
    }
    println!(
        "after warm-up: root.needs_redraw()={} root.is_layout_dirty()={}",
        root.needs_redraw(), root.is_layout_dirty()
    );
    fn dirty_culprits(w: &dyn Widget, depth: usize, out: &mut Vec<(String, usize)>) {
        if w.needs_redraw() {
            out.push((w.name().to_string(), depth));
        }
        for c in w.children() {
            dirty_culprits(c, depth + 1, out);
        }
    }
    let mut culprits = Vec::new();
    dirty_culprits(root.as_ref(), 0, &mut culprits);
    println!("dirty culprits after warmup: {} widgets", culprits.len());
    for (name, d) in culprits.iter().take(8) {
        println!("  {} (depth {})", name, d);
    }
    let (warm_hits, warm_misses) = text_renderer.cache_stats();
    println!(
        "after warm-up: measure-cache hits={} misses={} hit-rate={:.1}%",
        warm_hits,
        warm_misses,
        (warm_hits as f64) / ((warm_hits + warm_misses).max(1) as f64) * 100.0,
    );

    // Event mix. We hand-craft 5 distinct cursor positions that
    // exercise different parts of the tree:
    //   - A: empty area inside a panel (no widget edge crossing)
    //   - B: hover edge of a button (state change)
    //   - C: hover edge of a toggle (state change)
    //   - D: inside the header ToggleGroup ("Week" segment)
    //   - E: inside the header ToggleGroup ("Month" segment)
    // Click events fire on D/E alternately so each click changes
    // the ToggleGroup's selected segment.
    let pa = (400.0, 300.0);
    let pb = (200.0, 200.0);
    let pc = (240.0, 200.0);
    let pd = (380.0, 50.0);
    let pe = (520.0, 50.0);

    let mut total_redraws = 0usize;
    let mut total_skipped = 0usize;
    let mut histo: Vec<Phase> = Vec::with_capacity(iters);

    // FROZEN — identical no-op CursorMoved every event. Proves
    // the Unit 1+2 gate skips when nothing in the tree mutates.
    // SCROLL — fire a wheel event every iteration; exercises the
    // translation-invariant cache + Scrollable's scroll-dirty
    // signal. The 2026-05-23 user report ("scrolling does
    // something after a few seconds but isn't smooth") was the
    // motivation for this mode.
    let frozen = std::env::var("QUICK_UI_DASH_FROZEN").is_ok();
    let scroll_mode = std::env::var("QUICK_UI_DASH_SCROLL").is_ok();
    if frozen {
        // Prime the cursor at the frozen position so the first
        // event doesn't trigger a hover-state change.
        let p = (canvas_w as f64 - 5.0, canvas_h as f64 - 5.0);
        dispatch(&mut root, &taffy, root_node, &mut doc, &make_cursor(p.0, p.1));
        // Draw one frame to clear pending dirty flags.
        let mut warm = Phase::default();
        full_draw(
            &mut root, &mut taffy, &node_map, root_node,
            &mut text_renderer, &presence, &perms, &theme,
            &mut scene, &mut renderer, canvas_w, canvas_h, &mut warm,
        );
    }

    for i in 0..iters {
        let t_total = Instant::now();
        let mut phase = Phase::default();

        let t_handle = Instant::now();
        if scroll_mode {
            // Alternate scroll direction every ~50 events so we
            // exercise both clamp-eats-it (at the edges) and
            // dirty-signal-fires paths.
            let dy = if (i / 50) % 2 == 0 { -3.0 } else { 3.0 };
            dispatch(&mut root, &taffy, root_node, &mut doc, &make_wheel(dy));
        } else if frozen {
            // Identical no-op event — see comment above.
            let p = (canvas_w as f64 - 5.0, canvas_h as f64 - 5.0);
            dispatch(&mut root, &taffy, root_node, &mut doc, &make_cursor(p.0, p.1));
        } else {
            let pick = i % 20;
            match pick {
                0..=13 => dispatch(&mut root, &taffy, root_node, &mut doc, &make_cursor(pa.0 + (i as f64).sin() * 4.0, pa.1)),
                14 | 15 => dispatch(&mut root, &taffy, root_node, &mut doc, &make_cursor(pb.0, pb.1)),
                16 => dispatch(&mut root, &taffy, root_node, &mut doc, &make_cursor(pc.0, pc.1)),
                17 => {
                    let p = if i % 40 < 20 { pd } else { pe };
                    dispatch(&mut root, &taffy, root_node, &mut doc, &make_cursor(p.0, p.1));
                    dispatch(&mut root, &taffy, root_node, &mut doc, &make_click_press());
                    dispatch(&mut root, &taffy, root_node, &mut doc, &make_click_release());
                }
                _ => dispatch(&mut root, &taffy, root_node, &mut doc, &make_cursor(pa.0 + (i as f64).cos() * 4.0, pa.1)),
            }
        }
        phase.handle_us = t_handle.elapsed().as_micros();

        // App-style gate: only draw if the tree signals dirty.
        if root.needs_redraw() {
            total_redraws += 1;
            full_draw(
                &mut root, &mut taffy, &node_map, root_node,
                &mut text_renderer, &presence, &perms, &theme,
                &mut scene, &mut renderer, canvas_w, canvas_h, &mut phase,
            );
        } else {
            total_skipped += 1;
        }
        phase.total_us = t_total.elapsed().as_micros();
        histo.push(phase);
    }

    fn pct(hist: &[Phase], pct_v: f64, f: impl Fn(&Phase) -> u128) -> u128 {
        let mut v: Vec<u128> = hist.iter().map(&f).collect();
        v.sort_unstable();
        let idx = ((v.len() as f64 - 1.0) * pct_v / 100.0).round() as usize;
        v[idx]
    }
    fn mean(hist: &[Phase], f: impl Fn(&Phase) -> u128) -> u128 {
        if hist.is_empty() { return 0; }
        hist.iter().map(&f).sum::<u128>() / hist.len() as u128
    }

    println!();
    println!("=== Dashboard bench ({iters} events, canvas {canvas_w}×{canvas_h}) ===");
    println!("redrawn events:  {total_redraws}  ({:.1}%)",
        (total_redraws as f64) / (iters as f64) * 100.0);
    println!("skipped events:  {total_skipped}  (no widget signalled dirty)");
    println!();
    println!("{:<20} {:>8} {:>8} {:>8}", "phase", "mean us", "p95 us", "p99 us");
    println!("{:<20} {:>8} {:>8} {:>8}", "handle_event",
        mean(&histo, |p| p.handle_us),
        pct(&histo, 95.0, |p| p.handle_us),
        pct(&histo, 99.0, |p| p.handle_us));
    println!("{:<20} {:>8} {:>8} {:>8}", "layout (gated)",
        mean(&histo, |p| p.layout_us),
        pct(&histo, 95.0, |p| p.layout_us),
        pct(&histo, 99.0, |p| p.layout_us));
    println!("{:<20} {:>8} {:>8} {:>8}", "draw_to_scene",
        mean(&histo, |p| p.draw_us),
        pct(&histo, 95.0, |p| p.draw_us),
        pct(&histo, 99.0, |p| p.draw_us));
    println!("{:<20} {:>8} {:>8} {:>8}", "vello render",
        mean(&histo, |p| p.render_us),
        pct(&histo, 95.0, |p| p.render_us),
        pct(&histo, 99.0, |p| p.render_us));
    println!("{:<20} {:>8} {:>8} {:>8}", "TOTAL/event",
        mean(&histo, |p| p.total_us),
        pct(&histo, 95.0, |p| p.total_us),
        pct(&histo, 99.0, |p| p.total_us));

    let mean_total = mean(&histo, |p| p.total_us).max(1);
    let mean_redraw_only: u128 = if total_redraws == 0 {
        0
    } else {
        histo.iter()
            .filter(|p| p.render_us > 0 || p.draw_us > 0)
            .map(|p| p.total_us)
            .sum::<u128>() / total_redraws as u128
    };
    println!();
    println!(
        "throughput (mean across mixed events):     {:>5} Hz   ({:.1} ms / event)",
        1_000_000 / mean_total, (mean_total as f64) / 1000.0,
    );
    if total_redraws > 0 {
        println!(
            "throughput (redrawing events only):        {:>5} Hz   ({:.1} ms / redraw)",
            1_000_000_u128.checked_div(mean_redraw_only).unwrap_or(0),
            (mean_redraw_only as f64) / 1000.0,
        );
    }
    let (hits_end, misses_end) = text_renderer.cache_stats();
    println!(
        "measure-cache final: hits={} misses={} hit-rate={:.1}%",
        hits_end,
        misses_end,
        (hits_end as f64) / ((hits_end + misses_end).max(1) as f64) * 100.0,
    );
}
