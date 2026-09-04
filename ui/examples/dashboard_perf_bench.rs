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
//!   -  5 % clicks on a `ToggleGroup` — mutates `selected`, re-
//!      emits the picker, every other widget hits its cache.
//!   -  5 % "complex" events (spinner-style animation ticks where
//!      every animating widget marks itself dirty).
//!
//! The bench tallies:
//!   - "no-op" events that didn't request a redraw (proves Unit 2).
//!   - `draw_to_scene` mean cost across redrawing events.
//!   - vello render mean cost.
//!   - effective throughput Hz = 1 / total mean per event.
//!
//! Run with:
//! ```
//! cargo run -p ui --release --example ui-perf -- dashboard_perf_bench
//! ```
//! Override iterations: `QUICK_UI_DASH_ITERS=2000`.
//! Override the render target: `QUICK_UI_DASH_WIDTH=3840
//! QUICK_UI_DASH_HEIGHT=2160`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Instant;

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::i18n::I18nManager;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Avatar, Badge, Button, Column, ProgressBar, Row, ScrollAxis, Scrollable, Spinner, Text,
    ToggleGroup, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::dpi::PhysicalPosition;
use winit::event::{
    DeviceId, ElementState, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};

/// Narrows the canvas size (bounded, well under 10,000px) from `u32` to
/// `f32` for taffy's available-space measurement.
#[allow(
    clippy::cast_precision_loss,
    reason = "canvas dimensions are small (<10000px), far below f32's exact-integer range"
)]
fn canvas_f32(v: u32) -> f32 {
    v as f32
}

/// Widens a bench-loop index (bounded by `iters`, far below f64's
/// exact-integer range) to `f64` for the per-event cursor wobble.
#[allow(
    clippy::cast_precision_loss,
    reason = "i is bounded by the loop's iters count, far below f64's exact-integer range"
)]
fn idx_f64(i: usize) -> f64 {
    i as f64
}

fn env_u32(name: &str, fallback: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

/// One panel of the dashboard: header + a column of mixed widgets.
fn panel(title_key: &'static str, idx: usize) -> Box<dyn Widget> {
    let mut col = Column::new().push(Box::new(Text::new(title_key).with_size(18.0)));

    // Twenty rows. To exercise the Unit 2 redraw gate, only
    // widgets whose `needs_redraw` is genuinely opt-in show up
    // here — Toggle / Checkbox / Slider / DatePicker / etc. still
    // default to `true` and would peg the gate forever. Once
    // those are migrated this can grow back.
    for i in 0..20 {
        let row_idx = idx.saturating_mul(20).saturating_add(i);
        let label_key = leak_label(&format!("dash_lbl_{row_idx}"));
        match i % 3 {
            0 => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(Button::new(
                            leak_label(&format!("dash_btn_{row_idx}")),
                            leak_label(&format!("dash_btnlbl_{row_idx}")),
                            None,
                        ))),
                ));
            }
            1 => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(Badge::count(
                            u32::try_from(i).unwrap_or(u32::MAX).saturating_mul(3),
                        ))),
                ));
            }
            _ => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(Avatar::from_initials(&format!("U{i}"), 24.0))),
                ));
            }
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
        "dash_p0", "dash_p1", "dash_p2", "dash_p3", "dash_p4", "dash_p5", "dash_p6", "dash_p7",
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

#[allow(
    clippy::struct_field_names,
    reason = "the shared `_us` postfix states the unit (microseconds) on every timing field; \
              dropping it would read as unitless durations"
)]
#[derive(Default, Clone)]
struct Phase {
    handle_us: u128,
    layout_us: u128,
    draw_us: u128,
    render_us: u128,
    total_us: u128,
    raster_pixels: u64,
}

#[derive(Clone, Copy, Debug)]
struct ResourceSnapshot {
    rss_kib: Option<u64>,
    known_gpu_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct AllocationBreakdown {
    handle: crate::AllocationSnapshot,
    layout: crate::AllocationSnapshot,
    draw: crate::AllocationSnapshot,
    damage: crate::AllocationSnapshot,
    render: crate::AllocationSnapshot,
}

fn allocation_delta(
    after: crate::AllocationSnapshot,
    before: crate::AllocationSnapshot,
) -> crate::AllocationSnapshot {
    crate::AllocationSnapshot {
        calls: after.calls.saturating_sub(before.calls),
        bytes: after.bytes.saturating_sub(before.bytes),
        realloc_calls: after.realloc_calls.saturating_sub(before.realloc_calls),
        realloc_bytes: after.realloc_bytes.saturating_sub(before.realloc_bytes),
    }
}

fn add_allocations(total: &mut crate::AllocationSnapshot, sample: crate::AllocationSnapshot) {
    total.calls = total.calls.saturating_add(sample.calls);
    total.bytes = total.bytes.saturating_add(sample.bytes);
    total.realloc_calls = total.realloc_calls.saturating_add(sample.realloc_calls);
    total.realloc_bytes = total.realloc_bytes.saturating_add(sample.realloc_bytes);
}

struct BenchRun {
    histo: Vec<Phase>,
    total_redraws: usize,
    total_skipped: usize,
    allocations: Option<crate::AllocationSnapshot>,
    allocation_breakdown: Option<AllocationBreakdown>,
    resources_before: ResourceSnapshot,
    resources_after: ResourceSnapshot,
}

#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent benchmark modes are clearer as named flags than a combinatorial enum"
)]
struct BenchLoopOptions {
    scroll_mode: bool,
    frozen: bool,
    profile_allocations: bool,
    profile_allocation_breakdown: bool,
}

/// Everything one bench frame needs to lay out, dispatch events to, and
/// draw the dashboard tree — bundled into one struct so `full_draw` and
/// `dispatch` stay well under clippy's argument-count limit.
struct BenchState<'a> {
    root: &'a mut Box<dyn Widget>,
    taffy: &'a mut TaffyTree<()>,
    node_map: &'a HashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
    text_renderer: &'a mut TextRenderer,
    presence: &'a PresenceCtx,
    perms: &'a PermissionContext,
    theme: &'a Theme,
    scene: &'a mut Scene,
    renderer: &'a mut OffscreenRenderer,
    damage_regions: &'a mut Vec<vello::kurbo::Rect>,
    canvas_w: u32,
    canvas_h: u32,
}

#[allow(
    clippy::too_many_lines,
    reason = "the benchmark keeps layout, scene, damage, and render timing/allocation boundaries together"
)]
fn full_draw(
    state: &mut BenchState<'_>,
    phase: &mut Phase,
    mut allocation_breakdown: Option<&mut AllocationBreakdown>,
) {
    let allocation_before_layout = allocation_breakdown
        .as_ref()
        .map(|_| crate::allocation_profile_snapshot());
    let t_layout = Instant::now();
    let layout_dirty = state.root.is_layout_dirty();
    if layout_dirty {
        let node_map = state.node_map;
        let text_renderer = &mut *state.text_renderer;
        let _ = state.taffy.compute_layout_with_measure(
            state.root_node,
            Size {
                width: AvailableSpace::Definite(canvas_f32(state.canvas_w)),
                height: AvailableSpace::Definite(canvas_f32(state.canvas_h)),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, text_renderer) }
                } else {
                    Size::ZERO
                }
            },
        );
        state
            .root
            .apply_layout(state.taffy, state.root_node, Point::ZERO);
    }
    phase.layout_us = t_layout.elapsed().as_micros();
    if let (Some(before), Some(breakdown)) = (
        allocation_before_layout,
        allocation_breakdown.as_deref_mut(),
    ) {
        add_allocations(
            &mut breakdown.layout,
            allocation_delta(crate::allocation_profile_snapshot(), before),
        );
    }

    let allocation_before_draw = allocation_breakdown
        .as_ref()
        .map(|_| crate::allocation_profile_snapshot());
    let t_draw = Instant::now();
    state.scene.reset();
    // Wire the spatial-culling viewport just like the live App
    // does. Without this the bench's `draw_with_presence` walks
    // see `viewport == None` and skip the culling short-circuit,
    // masking the Item 1 win in the benchmark output.
    let vp = vello::kurbo::Rect::new(
        0.0,
        0.0,
        f64::from(state.canvas_w),
        f64::from(state.canvas_h),
    );
    let prev_vp = state.presence.push_viewport(vp);
    state.root.draw_with_presence(
        state.scene,
        Affine::IDENTITY,
        state.text_renderer,
        state.theme,
        state.presence,
        state.perms,
    );
    state.presence.pop_viewport(prev_vp);
    phase.draw_us = t_draw.elapsed().as_micros();
    if let (Some(before), Some(breakdown)) =
        (allocation_before_draw, allocation_breakdown.as_deref_mut())
    {
        add_allocations(
            &mut breakdown.draw,
            allocation_delta(crate::allocation_profile_snapshot(), before),
        );
    }

    let t_render = Instant::now();
    let allocation_before_damage = allocation_breakdown
        .as_ref()
        .map(|_| crate::allocation_profile_snapshot());
    // Layout can expose both old and new pixels, so it conservatively damages
    // the complete native target. Visual-only changes use the union of dirty
    // leaves plus a 16px safety margin for AA, focus rings, and small shadows.
    state.damage_regions.clear();
    if layout_dirty {
        state.damage_regions.push(vp);
    } else {
        state.root.collect_damage(state.damage_regions);
        for rect in &mut *state.damage_regions {
            *rect = rect.inflate(16.0, 16.0).intersect(vp);
        }
        state.damage_regions.retain(|rect| !rect.is_zero_area());
        if state.damage_regions.is_empty() {
            state.damage_regions.push(vp);
        }
    }
    if std::env::var_os("QUICK_UI_DASH_DAMAGE").is_some() {
        println!("damage regions: {:?}", state.damage_regions);
    }
    let allocation_before_render = allocation_breakdown
        .as_ref()
        .map(|_| crate::allocation_profile_snapshot());
    if let (Some(before), Some(after), Some(breakdown)) = (
        allocation_before_damage,
        allocation_before_render,
        allocation_breakdown.as_deref_mut(),
    ) {
        add_allocations(&mut breakdown.damage, allocation_delta(after, before));
    }
    phase.raster_pixels = state.renderer.render_damage_regions_gpu_wait(
        state.scene,
        state.theme.tokens.colors.surfaces.base,
        state.damage_regions,
    );
    phase.render_us = t_render.elapsed().as_micros();
    if let (Some(before), Some(breakdown)) = (allocation_before_render, allocation_breakdown) {
        add_allocations(
            &mut breakdown.render,
            allocation_delta(crate::allocation_profile_snapshot(), before),
        );
    }

    state.root.mark_drawn();
}

fn dispatch(state: &mut BenchState<'_>, doc: &mut AutoCommit, event: &WindowEvent) {
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    let modifiers = Modifiers::default();
    state.root.handle_event(
        event,
        1.0,
        &modifiers,
        doc,
        &automerge::ObjId::Root,
        &mut handled,
        &mut focus_target,
        state.root_node,
        state.taffy,
    );
}

fn make_cursor(x: f64, y: f64) -> WindowEvent {
    let device_id: DeviceId = unsafe { std::mem::zeroed() };
    WindowEvent::CursorMoved {
        device_id,
        position: PhysicalPosition::new(x, y),
    }
}

fn make_click_press() -> WindowEvent {
    let device_id: DeviceId = unsafe { std::mem::zeroed() };
    WindowEvent::MouseInput {
        device_id,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

fn make_click_release() -> WindowEvent {
    let device_id: DeviceId = unsafe { std::mem::zeroed() };
    WindowEvent::MouseInput {
        device_id,
        state: ElementState::Released,
        button: MouseButton::Left,
    }
}

fn make_wheel(dy: f32) -> WindowEvent {
    let device_id: DeviceId = unsafe { std::mem::zeroed() };
    WindowEvent::MouseWheel {
        device_id,
        delta: MouseScrollDelta::LineDelta(0.0, dy),
        phase: TouchPhase::Moved,
    }
}

/// Register every label key so `Text::new(key)` resolves through i18n
/// without panicking.
fn init_i18n_resources() {
    I18nManager::init("en-US");
    let mut ftl = String::new();
    ftl.push_str("dash_title = QuickDesktop Dashboard\n");
    for i in 0..8 {
        let _ = writeln!(ftl, "dash_p{i} = Panel {i}");
    }
    for row_idx in 0..160 {
        let _ = writeln!(ftl, "dash_lbl_{row_idx} = Metric {row_idx}");
        let _ = writeln!(ftl, "dash_btn_{row_idx} = btn_{row_idx}");
        let _ = writeln!(ftl, "dash_btnlbl_{row_idx} = Open");
    }
    I18nManager::add_resource("en-US", ftl);
}

/// Counts the total widget tree size for the report.
fn count_widgets(w: &dyn Widget) -> usize {
    1usize.saturating_add(
        w.children()
            .iter()
            .map(|c| count_widgets(*c))
            .sum::<usize>(),
    )
}

fn dirty_culprits(w: &dyn Widget, depth: usize, out: &mut Vec<(String, usize)>) {
    if w.needs_redraw() {
        out.push((w.name().to_string(), depth));
    }
    for c in w.children() {
        dirty_culprits(c, depth.saturating_add(1), out);
    }
}

/// A count out of a total, as a percentage. Both bounded by the bench's
/// iteration count, far below f64's exact-integer range.
#[allow(
    clippy::cast_precision_loss,
    reason = "count/total are bounded by the bench's iters, far below f64's exact-integer range"
)]
fn count_pct(count: usize, total: usize) -> f64 {
    (count as f64) / (total.max(1) as f64) * 100.0
}

/// Widens a microsecond duration (bounded by realistic per-frame times,
/// far below f64's exact-integer range) to a millisecond `f64`.
#[allow(
    clippy::cast_precision_loss,
    reason = "durations here are bounded by realistic per-frame microsecond counts, far below \
              f64's exact-integer range"
)]
fn us_to_ms(us: u128) -> f64 {
    (us as f64) / 1000.0
}

fn process_memory_kib() -> Option<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let value = |key: &str| {
        status.lines().find_map(|line| {
            line.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
    };
    Some((value("VmRSS:")?, value("VmHWM:")?))
}

fn resource_snapshot(renderer: &OffscreenRenderer) -> ResourceSnapshot {
    ResourceSnapshot {
        rss_kib: process_memory_kib().map(|(rss, _)| rss),
        known_gpu_bytes: renderer.memory_stats().known_gpu_bytes(),
    }
}

fn delta_label(before: u64, after: u64) -> String {
    if let Some(growth) = after.checked_sub(before) {
        format!("+{growth}")
    } else {
        format!("-{}", before.saturating_sub(after))
    }
}

/// Widens a histogram length (bounded by `iters`, far below f64's
/// exact-integer range) to `f64` for percentile-index math.
#[allow(
    clippy::cast_precision_loss,
    reason = "v.len() is bounded by iters, far below f64's exact-integer range"
)]
fn len_f64(n: usize) -> f64 {
    n as f64
}

fn pct(hist: &[Phase], pct_v: f64, f: impl Fn(&Phase) -> u128) -> u128 {
    let mut v: Vec<u128> = hist.iter().map(&f).collect();
    v.sort_unstable();
    // `pct_v` is always 95.0 or 99.0 from call sites and `v` is non-empty
    // (iters >= 1), so the rounded rank always lands in `0..v.len()`;
    // `unwrap_or(0)` below only matters if that invariant is ever broken.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "idx is a percentile rank of v.len(), always non-negative and in range"
    )]
    let idx = ((len_f64(v.len()) - 1.0) * pct_v / 100.0).round() as usize;
    v.get(idx).copied().unwrap_or(0)
}

fn mean(hist: &[Phase], f: impl Fn(&Phase) -> u128) -> u128 {
    if hist.is_empty() {
        return 0;
    }
    let sum: u128 = hist.iter().map(&f).sum();
    sum.checked_div(hist.len() as u128).unwrap_or(0)
}

fn frozen_cursor_pos(state: &BenchState<'_>) -> (f64, f64) {
    (
        f64::from(state.canvas_w) - 5.0,
        f64::from(state.canvas_h) - 5.0,
    )
}

/// Prime the cursor at the frozen position so the first event doesn't
/// trigger a hover-state change, then draw one frame to clear pending
/// dirty flags.
fn prime_frozen_cursor(state: &mut BenchState<'_>, doc: &mut AutoCommit) {
    let p = frozen_cursor_pos(state);
    dispatch(state, doc, &make_cursor(p.0, p.1));
    let mut warm = Phase::default();
    full_draw(state, &mut warm, None);
}

/// Two warm-up frames: first `compute_layout`, second post-cache.
fn warm_up(state: &mut BenchState<'_>) {
    for _ in 0..2 {
        let mut warm = Phase::default();
        full_draw(state, &mut warm, None);
    }
}

fn print_warmup_report(state: &BenchState<'_>) {
    println!(
        "after warm-up: root.needs_redraw()={} root.is_layout_dirty()={}",
        state.root.needs_redraw(),
        state.root.is_layout_dirty()
    );
    let mut culprits = Vec::new();
    dirty_culprits(state.root.as_ref(), 0, &mut culprits);
    println!("dirty culprits after warmup: {} widgets", culprits.len());
    for (name, d) in culprits.iter().take(8) {
        println!("  {name} (depth {d})");
    }
    let (warm_hits, warm_misses) = state.text_renderer.cache_stats();
    println!(
        "after warm-up: measure-cache hits={} misses={} hit-rate={:.1}%",
        warm_hits,
        warm_misses,
        count_pct(
            usize::try_from(warm_hits).unwrap_or(usize::MAX),
            usize::try_from(warm_hits.saturating_add(warm_misses)).unwrap_or(usize::MAX),
        ),
    );
}

/// One event-mix tick: routes to the scroll/frozen/mixed-cursor path per
/// `i`. `positions` is `[pa, pb, pc, pd, pe]` — the 5 hand-picked cursor
/// spots documented on [`run_bench_loop`].
fn dispatch_bench_event(
    state: &mut BenchState<'_>,
    doc: &mut AutoCommit,
    i: usize,
    scroll_mode: bool,
    frozen: bool,
    positions: [(f64, f64); 5],
) {
    let [pa, pb, pc, pd, pe] = positions;
    if scroll_mode {
        // Alternate scroll direction every ~50 events so we
        // exercise both clamp-eats-it (at the edges) and
        // dirty-signal-fires paths.
        let dy = if (i / 50).is_multiple_of(2) {
            -3.0
        } else {
            3.0
        };
        dispatch(state, doc, &make_wheel(dy));
    } else if frozen {
        // Identical no-op event — see comment above.
        let p = frozen_cursor_pos(state);
        dispatch(state, doc, &make_cursor(p.0, p.1));
    } else {
        let pick = i % 20;
        match pick {
            0..=13 => dispatch(
                state,
                doc,
                &make_cursor(pa.0 + idx_f64(i).sin() * 4.0, pa.1),
            ),
            14 | 15 => dispatch(state, doc, &make_cursor(pb.0, pb.1)),
            16 => dispatch(state, doc, &make_cursor(pc.0, pc.1)),
            17 => {
                let p = if i % 40 < 20 { pd } else { pe };
                dispatch(state, doc, &make_cursor(p.0, p.1));
                dispatch(state, doc, &make_click_press());
                dispatch(state, doc, &make_click_release());
            }
            _ => dispatch(
                state,
                doc,
                &make_cursor(pa.0 + idx_f64(i).cos() * 4.0, pa.1),
            ),
        }
    }
}

/// Drives `iters` events through the tree and times each one. Positions
/// exercise different parts of it: A empty panel area (no edge crossing),
/// B a button's hover edge, C a toggle's hover edge, D/E the header
/// `ToggleGroup`'s "Week"/"Month" segments (clicked alternately so each
/// click actually changes the selected segment).
fn run_bench_loop(
    state: &mut BenchState<'_>,
    doc: &mut AutoCommit,
    iters: usize,
    options: BenchLoopOptions,
) -> BenchRun {
    let positions = [
        (400.0, 300.0),
        (200.0, 200.0),
        (240.0, 200.0),
        (380.0, 50.0),
        (520.0, 50.0),
    ];

    let mut total_redraws = 0usize;
    let mut total_skipped = 0usize;
    // Materialize and touch the histogram before the resource baseline. This
    // keeps harness-owned pages out of the measured heap-growth delta.
    let mut histo = vec![Phase::default(); iters];
    let resources_before = resource_snapshot(state.renderer);
    if options.profile_allocations {
        // The timing histogram is harness storage, not framework work.
        crate::begin_allocation_profile();
    }
    let mut allocation_breakdown = options
        .profile_allocation_breakdown
        .then(AllocationBreakdown::default);

    for (i, sample) in (0..iters).zip(&mut histo) {
        let t_total = Instant::now();
        let mut phase = Phase::default();

        let allocation_before_handle = allocation_breakdown
            .as_ref()
            .map(|_| crate::allocation_profile_snapshot());
        let t_handle = Instant::now();
        dispatch_bench_event(
            state,
            doc,
            i,
            options.scroll_mode,
            options.frozen,
            positions,
        );
        phase.handle_us = t_handle.elapsed().as_micros();
        if let (Some(before), Some(breakdown)) =
            (allocation_before_handle, allocation_breakdown.as_mut())
        {
            add_allocations(
                &mut breakdown.handle,
                allocation_delta(crate::allocation_profile_snapshot(), before),
            );
        }

        // App-style gate: only draw if the tree signals dirty.
        if state.root.needs_redraw() {
            total_redraws = total_redraws.saturating_add(1);
            full_draw(state, &mut phase, allocation_breakdown.as_mut());
        } else {
            total_skipped = total_skipped.saturating_add(1);
        }
        phase.total_us = t_total.elapsed().as_micros();
        *sample = phase;
    }

    let allocations = options
        .profile_allocations
        .then(crate::end_allocation_profile);
    let resources_after = resource_snapshot(state.renderer);
    BenchRun {
        histo,
        total_redraws,
        total_skipped,
        allocations,
        allocation_breakdown,
        resources_before,
        resources_after,
    }
}

fn print_timing_table(histo: &[Phase]) {
    println!(
        "{:<20} {:>8} {:>8} {:>8}",
        "phase", "mean us", "p95 us", "p99 us"
    );
    println!(
        "{:<20} {:>8} {:>8} {:>8}",
        "handle_event",
        mean(histo, |p| p.handle_us),
        pct(histo, 95.0, |p| p.handle_us),
        pct(histo, 99.0, |p| p.handle_us)
    );
    println!(
        "{:<20} {:>8} {:>8} {:>8}",
        "layout (gated)",
        mean(histo, |p| p.layout_us),
        pct(histo, 95.0, |p| p.layout_us),
        pct(histo, 99.0, |p| p.layout_us)
    );
    println!(
        "{:<20} {:>8} {:>8} {:>8}",
        "draw_to_scene",
        mean(histo, |p| p.draw_us),
        pct(histo, 95.0, |p| p.draw_us),
        pct(histo, 99.0, |p| p.draw_us)
    );
    println!(
        "{:<20} {:>8} {:>8} {:>8}",
        "vello render",
        mean(histo, |p| p.render_us),
        pct(histo, 95.0, |p| p.render_us),
        pct(histo, 99.0, |p| p.render_us)
    );
    println!(
        "{:<20} {:>8} {:>8} {:>8}",
        "TOTAL/event",
        mean(histo, |p| p.total_us),
        pct(histo, 95.0, |p| p.total_us),
        pct(histo, 99.0, |p| p.total_us)
    );
}

fn native_pixels_per_redraw(histo: &[Phase], total_redraws: usize) -> u128 {
    if total_redraws == 0 {
        return 0;
    }
    histo
        .iter()
        .filter(|phase| phase.render_us > 0 || phase.draw_us > 0)
        .map(|phase| u128::from(phase.raster_pixels))
        .sum::<u128>()
        .checked_div(total_redraws as u128)
        .unwrap_or(0)
}

fn print_throughput_report(histo: &[Phase], total_redraws: usize) {
    let mean_total = mean(histo, |p| p.total_us).max(1);
    let mean_redraw_only: u128 = if total_redraws == 0 {
        0
    } else {
        let sum: u128 = histo
            .iter()
            .filter(|p| p.render_us > 0 || p.draw_us > 0)
            .map(|p| p.total_us)
            .sum();
        sum.checked_div(total_redraws as u128).unwrap_or(0)
    };
    println!();
    println!(
        "throughput (mean across mixed events):     {:>5} Hz   ({:.1} ms / event)",
        1_000_000_u128.checked_div(mean_total).unwrap_or(0),
        us_to_ms(mean_total),
    );
    if total_redraws > 0 {
        println!(
            "throughput (redrawing events only):        {:>5} Hz   ({:.1} ms / redraw)",
            1_000_000_u128.checked_div(mean_redraw_only).unwrap_or(0),
            us_to_ms(mean_redraw_only),
        );
        let raster_pixels = native_pixels_per_redraw(histo, total_redraws);
        println!("native pixels rasterized per redraw:      {raster_pixels}");
    }
}

fn print_final_report(
    state: &BenchState<'_>,
    histo: &[Phase],
    iters: usize,
    total_redraws: usize,
    total_skipped: usize,
    resources_before: ResourceSnapshot,
    resources_after: ResourceSnapshot,
) {
    println!();
    println!(
        "=== Dashboard bench ({iters} events, canvas {}×{}) ===",
        state.canvas_w, state.canvas_h
    );
    println!(
        "redrawn events:  {total_redraws}  ({:.1}%)",
        count_pct(total_redraws, iters)
    );
    println!("skipped events:  {total_skipped}  (no widget signalled dirty)");
    println!();
    print_timing_table(histo);
    print_throughput_report(histo, total_redraws);

    let (hits_end, misses_end) = state.text_renderer.cache_stats();
    println!(
        "measure-cache final: hits={} misses={} hit-rate={:.1}%",
        hits_end,
        misses_end,
        count_pct(
            usize::try_from(hits_end).unwrap_or(usize::MAX),
            usize::try_from(hits_end.saturating_add(misses_end)).unwrap_or(usize::MAX),
        ),
    );
    let memory = state.renderer.memory_stats();
    println!(
        "known GPU bytes: {} (output={} damage={} vello_pool={} vello_live={} images={}; policy_budget={})",
        memory.known_gpu_bytes(),
        memory.output_target_bytes,
        memory.damage_target_bytes,
        memory.vello.pooled_buffer_bytes,
        memory.vello.live_buffer_bytes,
        memory.vello.image_bytes,
        memory.vello.bump_budget_bytes,
    );
    println!(
        "CPU readback bytes: {} (must be 0 in timing-only runs)",
        memory.readback_bytes,
    );
    if let Some((rss, peak)) = process_memory_kib() {
        println!("process memory: rss={rss} KiB peak={peak} KiB");
    }
    println!(
        "known GPU growth after warm-up: {} bytes (before={} after={})",
        delta_label(
            resources_before.known_gpu_bytes,
            resources_after.known_gpu_bytes
        ),
        resources_before.known_gpu_bytes,
        resources_after.known_gpu_bytes,
    );
    if let (Some(before), Some(after)) = (resources_before.rss_kib, resources_after.rss_kib) {
        println!(
            "RSS growth after harness preallocation: {} KiB (before={} after={})",
            delta_label(before, after),
            before,
            after,
        );
    }
}

#[allow(
    clippy::exit,
    reason = "a benchmark budget is an executable acceptance gate for CI and target-hardware runs"
)]
fn enforce_frame_budget(histo: &[Phase], budget_us: u128) {
    let redrawing = histo
        .iter()
        .filter(|phase| phase.draw_us > 0 || phase.render_us > 0)
        .cloned()
        .collect::<Vec<_>>();
    if redrawing.is_empty() {
        eprintln!(
            "frame-budget: FAIL (no redrawing frames were measured; use animated or scroll mode)"
        );
        std::process::exit(2);
    }

    let mean_us = mean(&redrawing, |phase| phase.total_us);
    let p99_us = pct(&redrawing, 99.0, |phase| phase.total_us);
    let max_us = redrawing
        .iter()
        .map(|phase| phase.total_us)
        .max()
        .unwrap_or(0);
    let passed = p99_us <= budget_us;
    println!();
    println!(
        "frame-budget: {} budget={}us mean={}us p99={}us max={}us samples={}",
        if passed { "PASS" } else { "FAIL" },
        budget_us,
        mean_us,
        p99_us,
        max_us,
        redrawing.len(),
    );
    if !passed {
        std::process::exit(2);
    }
}

#[allow(
    clippy::exit,
    reason = "benchmark budgets are executable acceptance gates for CI and target-hardware runs"
)]
fn enforce_event_budget(histo: &[Phase], budget_us: u128) {
    let p99_us = pct(histo, 99.0, |phase| phase.total_us);
    let passed = p99_us <= budget_us;
    println!(
        "event-budget: {} budget={}us p99={}us samples={}",
        if passed { "PASS" } else { "FAIL" },
        budget_us,
        p99_us,
        histo.len(),
    );
    if !passed {
        std::process::exit(2);
    }
}

#[allow(
    clippy::exit,
    reason = "raster work is an executable partial-redraw regression gate"
)]
fn enforce_raster_pixel_budget(histo: &[Phase], total_redraws: usize, budget: u128) {
    if total_redraws == 0 {
        eprintln!("raster-pixel-budget: FAIL (no redrawing frames were measured)");
        std::process::exit(2);
    }
    let observed = native_pixels_per_redraw(histo, total_redraws);
    println!(
        "raster-pixel-budget: {} budget={}px/redraw observed={}px/redraw",
        if observed <= budget { "PASS" } else { "FAIL" },
        budget,
        observed,
    );
    if observed > budget {
        std::process::exit(2);
    }
}

#[allow(
    clippy::exit,
    reason = "benchmark budgets are executable acceptance gates for CI and target-hardware runs"
)]
fn enforce_resource_budgets(
    renderer: &OffscreenRenderer,
    before: ResourceSnapshot,
    after: ResourceSnapshot,
) {
    if let Ok(raw) = std::env::var("QUICK_UI_DASH_RSS_BUDGET_KIB")
        && let Ok(budget) = raw.parse::<u64>()
        && budget > 0
    {
        let rss = process_memory_kib().map_or(u64::MAX, |(rss, _)| rss);
        println!(
            "rss-budget: {} budget={}KiB observed={}KiB",
            if rss <= budget { "PASS" } else { "FAIL" },
            budget,
            rss,
        );
        if rss > budget {
            std::process::exit(2);
        }
    }
    if let Ok(raw) = std::env::var("QUICK_UI_DASH_GPU_BUDGET_BYTES")
        && let Ok(budget) = raw.parse::<u64>()
        && budget > 0
    {
        let observed = renderer.memory_stats().known_gpu_bytes();
        println!(
            "gpu-budget: {} budget={}B observed={}B",
            if observed <= budget { "PASS" } else { "FAIL" },
            budget,
            observed,
        );
        if observed > budget {
            std::process::exit(2);
        }
    }
    if let Ok(raw) = std::env::var("QUICK_UI_DASH_RSS_GROWTH_BUDGET_KIB")
        && let Ok(budget) = raw.parse::<u64>()
    {
        let observed = match (before.rss_kib, after.rss_kib) {
            (Some(start), Some(end)) => end.saturating_sub(start),
            _ => u64::MAX,
        };
        println!(
            "rss-growth-budget: {} budget={}KiB observed={}KiB",
            if observed <= budget { "PASS" } else { "FAIL" },
            budget,
            observed,
        );
        if observed > budget {
            std::process::exit(2);
        }
    }
    if let Ok(raw) = std::env::var("QUICK_UI_DASH_GPU_GROWTH_BUDGET_BYTES")
        && let Ok(budget) = raw.parse::<u64>()
    {
        let observed = after.known_gpu_bytes.saturating_sub(before.known_gpu_bytes);
        println!(
            "gpu-growth-budget: {} budget={}B observed={}B",
            if observed <= budget { "PASS" } else { "FAIL" },
            budget,
            observed,
        );
        if observed > budget {
            std::process::exit(2);
        }
    }
}

#[allow(
    clippy::exit,
    reason = "allocation budgets are executable acceptance gates for CI and target-hardware runs"
)]
fn report_and_enforce_allocations(
    snapshot: crate::AllocationSnapshot,
    breakdown: Option<AllocationBreakdown>,
    iters: usize,
    total_redraws: usize,
    budget_per_1000: Option<u64>,
    call_budget_per_redraw: Option<u64>,
    byte_budget_per_redraw: Option<u64>,
) {
    let denominator = u64::try_from(iters).unwrap_or(u64::MAX).max(1);
    let calls_per_1000 = snapshot
        .calls
        .saturating_mul(1000)
        .checked_div(denominator)
        .unwrap_or(0);
    let bytes_per_1000 = snapshot
        .bytes
        .saturating_mul(1000)
        .checked_div(denominator)
        .unwrap_or(0);
    println!(
        "allocations: calls={} bytes={} realloc_calls={} realloc_bytes={} calls_per_1000={} bytes_per_1000={}",
        snapshot.calls,
        snapshot.bytes,
        snapshot.realloc_calls,
        snapshot.realloc_bytes,
        calls_per_1000,
        bytes_per_1000,
    );

    let redraw_denominator = u64::try_from(total_redraws).unwrap_or(u64::MAX).max(1);
    if let Some(breakdown) = breakdown {
        let mut classified = crate::AllocationSnapshot::default();
        for phase in [
            breakdown.handle,
            breakdown.layout,
            breakdown.draw,
            breakdown.damage,
            breakdown.render,
        ] {
            add_allocations(&mut classified, phase);
        }
        let unclassified = allocation_delta(snapshot, classified);
        println!("allocation breakdown (calls / bytes; total over measured loop):");
        for (label, phase) in [
            ("handle_event", breakdown.handle),
            ("layout", breakdown.layout),
            ("draw_to_scene", breakdown.draw),
            ("damage_collect", breakdown.damage),
            ("vello_render", breakdown.render),
            ("unclassified", unclassified),
        ] {
            println!("  {label:<16} {:>10} / {:>12}", phase.calls, phase.bytes);
        }
    }
    let calls_per_redraw = snapshot.calls.checked_div(redraw_denominator).unwrap_or(0);
    let bytes_per_redraw = snapshot.bytes.checked_div(redraw_denominator).unwrap_or(0);
    if total_redraws > 0 {
        println!(
            "allocation redraw rate: calls_per_redraw={calls_per_redraw} bytes_per_redraw={bytes_per_redraw}"
        );
    }
    if let Some(budget) = budget_per_1000 {
        println!(
            "allocation-budget: {} budget={}calls/1000 observed={}calls/1000",
            if calls_per_1000 <= budget {
                "PASS"
            } else {
                "FAIL"
            },
            budget,
            calls_per_1000,
        );
        if calls_per_1000 > budget {
            std::process::exit(2);
        }
    }
    if let Some(budget) = call_budget_per_redraw {
        let passed = total_redraws > 0 && calls_per_redraw <= budget;
        println!(
            "allocation-call-redraw-budget: {} budget={}calls/redraw observed={}calls/redraw redraws={}",
            if passed { "PASS" } else { "FAIL" },
            budget,
            calls_per_redraw,
            total_redraws,
        );
        if !passed {
            std::process::exit(2);
        }
    }
    if let Some(budget) = byte_budget_per_redraw {
        let passed = total_redraws > 0 && bytes_per_redraw <= budget;
        println!(
            "allocation-byte-redraw-budget: {} budget={}B/redraw observed={}B/redraw redraws={}",
            if passed { "PASS" } else { "FAIL" },
            budget,
            bytes_per_redraw,
            total_redraws,
        );
        if !passed {
            std::process::exit(2);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "scenario setup intentionally remains linear so every acceptance setting is visible in one place"
)]
pub fn run(_args: &[String]) {
    init_i18n_resources();

    let iters: usize = std::env::var("QUICK_UI_DASH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    let static_mode = std::env::var("QUICK_UI_DASH_STATIC").is_ok();
    let include_animated = !static_mode;
    let canvas_w = env_u32("QUICK_UI_DASH_WIDTH", 1800);
    let canvas_h = env_u32("QUICK_UI_DASH_HEIGHT", 1200);
    println!(
        "mode: {}",
        if static_mode {
            "static (no spinners / indeterminate bars — gate-skipped events should dominate)"
        } else {
            "animated (header contains spinners + indeterminate progress)"
        }
    );

    println!("output target: {canvas_w}x{canvas_h} (native; 1:1 damage raster, no upscale)");
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
    let mut damage_regions = Vec::with_capacity(8);

    let tree_size = count_widgets(root.as_ref());
    println!("dashboard widget tree: {tree_size} widgets");

    let mut state = BenchState {
        root: &mut root,
        taffy: &mut taffy,
        node_map: &node_map,
        root_node,
        text_renderer: &mut text_renderer,
        presence: &presence,
        perms: &perms,
        theme: &theme,
        scene: &mut scene,
        renderer: &mut renderer,
        damage_regions: &mut damage_regions,
        canvas_w,
        canvas_h,
    };

    warm_up(&mut state);
    print_warmup_report(&state);

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
        prime_frozen_cursor(&mut state, &mut doc);
    }

    let allocation_budget = std::env::var("QUICK_UI_DASH_ALLOC_BUDGET_PER_1000")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok());
    let allocation_call_redraw_budget = std::env::var("QUICK_UI_DASH_ALLOC_CALL_BUDGET_PER_REDRAW")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok());
    let allocation_byte_redraw_budget = std::env::var("QUICK_UI_DASH_ALLOC_BYTE_BUDGET_PER_REDRAW")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok());
    let profile_allocation_breakdown = allocation_call_redraw_budget.is_some()
        || allocation_byte_redraw_budget.is_some()
        || std::env::var_os("QUICK_UI_DASH_ALLOC_BREAKDOWN").is_some_and(|raw| !raw.is_empty());
    let profile_allocations = allocation_budget.is_some()
        || profile_allocation_breakdown
        || std::env::var_os("QUICK_UI_DASH_ALLOC_PROFILE").is_some_and(|raw| !raw.is_empty());
    let run = run_bench_loop(
        &mut state,
        &mut doc,
        iters,
        BenchLoopOptions {
            scroll_mode,
            frozen,
            profile_allocations,
            profile_allocation_breakdown,
        },
    );

    print_final_report(
        &state,
        &run.histo,
        iters,
        run.total_redraws,
        run.total_skipped,
        run.resources_before,
        run.resources_after,
    );
    if let Some(snapshot) = run.allocations {
        report_and_enforce_allocations(
            snapshot,
            run.allocation_breakdown,
            iters,
            run.total_redraws,
            allocation_budget,
            allocation_call_redraw_budget,
            allocation_byte_redraw_budget,
        );
    }

    if let Ok(raw_budget) = std::env::var("QUICK_UI_DASH_FRAME_BUDGET_US")
        && let Ok(budget_us) = raw_budget.parse::<u128>()
        && budget_us > 0
    {
        enforce_frame_budget(&run.histo, budget_us);
    }
    if let Ok(raw_budget) = std::env::var("QUICK_UI_DASH_EVENT_BUDGET_US")
        && let Ok(budget_us) = raw_budget.parse::<u128>()
        && budget_us > 0
    {
        enforce_event_budget(&run.histo, budget_us);
    }
    if let Ok(raw_budget) = std::env::var("QUICK_UI_DASH_RASTER_PIXEL_BUDGET")
        && let Ok(budget) = raw_budget.parse::<u128>()
        && budget > 0
    {
        enforce_raster_pixel_budget(&run.histo, run.total_redraws, budget);
    }
    enforce_resource_budgets(state.renderer, run.resources_before, run.resources_after);
}
