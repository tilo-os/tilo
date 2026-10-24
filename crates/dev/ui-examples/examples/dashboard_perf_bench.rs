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
//! cargo run -p ui-examples --release --example ui-perf -- dashboard_perf_bench
//! ```
//! Override iterations: `TILO_UI_DASH_ITERS=2000`.
//! Override the render target: `TILO_UI_DASH_WIDTH=3840
//! TILO_UI_DASH_HEIGHT=2160`.
//! Set `TILO_UI_DASH_PEER=1` to add a collaborator whose pointer (dot and
//! name) moves on every event, drawn through the framework's presence layer;
//! `TILO_UI_DASH_PEER=full` repaints the window on every such frame instead,
//! the rule before #3936, for comparison.
//! Set `TILO_UI_DASH_VERIFY=1` to run a separate retained-damage pixel
//! parity preflight. It compares 40 (configurable: 20..=100) already-built
//! native scenes to a persistent full-render reference; it does not produce
//! performance measurements or accept benchmark budget gates.
//! Set `TILO_UI_DASH_SAMPLES=<path>` to write every redrawing frame's total
//! (microseconds, one per line) for `tools/perf/record.py emit`; the
//! scheduled performance lane records `render.ui.frame_us.4k` from it.
//! Native masked sparse fine shading is enabled by default when supported;
//! set `VELLO_SPARSE_FINE=0` to disable it. The raster-pixel metric remains
//! the actual fine-shaded native pixels; profile output separately accounts
//! for the coarse prefix work, so sparse mode is not described as eliminating it.
//! Set `VELLO_GPU_DISPATCH_TIMESTAMPS=1` together with
//! `VELLO_GPU_TIMESTAMPS=1` for an opt-in, profile-only per-dispatch GPU
//! timing breakdown. It rejects acceptance budgets.

use std::collections::HashMap;
use std::fmt::Write as _;

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::TextRole;
use tilo_ui::i18n::I18nManager;
use tilo_ui::offscreen::{OffscreenRenderTiming, OffscreenRenderer};
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::toggle_group::Segment;
use tilo_ui::widget::{
    Avatar, AvatarSize, Badge, Button, Column, ProgressBar, Row, ScrollAxis, Scrollable, Spinner,
    Text, ToggleGroup, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::dpi::PhysicalPosition;
use winit::event::{
    ElementState, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
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
    tilo_ui::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

/// Production Vello performance paths are enabled unless explicitly opted out.
fn env_enabled_by_default(name: &str) -> bool {
    !matches!(tilo_ui::env::var(name).as_deref(), Ok("0"))
}

#[allow(
    clippy::exit,
    reason = "an invalid verifier frame count is a command-line configuration error"
)]
fn verification_frame_count() -> usize {
    const DEFAULT: usize = 40;
    const MIN: usize = 20;
    const MAX: usize = 100;
    let Ok(raw) = tilo_ui::env::var("TILO_UI_DASH_VERIFY_FRAMES") else {
        return DEFAULT;
    };
    if let Ok(value @ MIN..=MAX) = raw.parse::<usize>() {
        value
    } else {
        eprintln!(
            "damage-verify: TILO_UI_DASH_VERIFY_FRAMES must be in {MIN}..={MAX}, got {raw:?}"
        );
        std::process::exit(2);
    }
}

/// Diagnostics can add work outside the normal acceptance workload, so reject
/// budget flags rather than reporting incomparable gate results.
#[allow(
    clippy::exit,
    reason = "mixing verifier readbacks with benchmark gates is a command-line configuration error"
)]
fn reject_budgets_in_diagnostic_mode(mode: &str, reject_profile_flags: bool) {
    const ACCEPTANCE_BUDGETS: &[&str] = &[
        "TILO_UI_DASH_FRAME_BUDGET_US",
        "TILO_UI_DASH_MEAN_FRAME_BUDGET_US",
        "TILO_UI_DASH_EVENT_BUDGET_US",
        "TILO_UI_DASH_RASTER_PIXEL_BUDGET",
        "TILO_UI_DASH_ALLOC_BUDGET_PER_1000",
        "TILO_UI_DASH_ALLOC_CALL_BUDGET_PER_REDRAW",
        "TILO_UI_DASH_ALLOC_BYTE_BUDGET_PER_REDRAW",
        "TILO_UI_DASH_RSS_BUDGET_KIB",
        "TILO_UI_DASH_GPU_BUDGET_BYTES",
        "TILO_UI_DASH_RSS_GROWTH_BUDGET_KIB",
        "TILO_UI_DASH_GPU_GROWTH_BUDGET_BYTES",
        // Samples of a diagnostic run are not comparable with the lane's.
        "TILO_UI_DASH_SAMPLES",
    ];
    // Verification intentionally suppresses every profile report, whereas
    // timestamp diagnostics compose with the frame/host profile reports.
    const VERIFICATION_ONLY_PROFILE_FLAGS: &[&str] = &[
        "TILO_UI_DASH_ALLOC_PROFILE",
        "TILO_UI_DASH_ALLOC_BREAKDOWN",
        "TILO_UI_DASH_FRAME_BREAKDOWN",
        "TILO_UI_DASH_RENDER_PROFILE",
    ];
    let names = if reject_profile_flags {
        ACCEPTANCE_BUDGETS
            .iter()
            .chain(VERIFICATION_ONLY_PROFILE_FLAGS)
            .copied()
            .collect::<Vec<_>>()
    } else {
        ACCEPTANCE_BUDGETS.to_vec()
    };
    let supplied = names
        .iter()
        .copied()
        .filter(|name| tilo_ui::env::var_os(name).is_some())
        .collect::<Vec<_>>();
    if !supplied.is_empty() {
        eprintln!(
            "{mode}: benchmark budget flags are incompatible with this diagnostic mode: {}",
            supplied.join(", "),
        );
        std::process::exit(2);
    }
}

/// One panel of the dashboard: header + a column of mixed widgets.
fn panel(title_key: &'static str, idx: usize) -> Box<dyn Widget> {
    let mut col = Column::new().push(Box::new(Text::new(title_key).with_role(TextRole::Title)));

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
                        .push(Box::new(
                            Avatar::person(format!("Teammate {i}"), format!("teammate-{i}"))
                                .size(AvatarSize::Small),
                        )),
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
        .push(Box::new(
            Text::new("dash_title").with_role(TextRole::LargeTitle),
        ))
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
        .push(Box::new(
            Avatar::person("Dana Brooks", "dana.brooks").size(AvatarSize::Regular),
        ))
        .push(Box::new(
            Avatar::person("Quinn Daley", "quinn.daley").size(AvatarSize::Regular),
        ))
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
    render_timings: Option<Vec<OffscreenRenderTiming>>,
    sparse_submission_accounting: tilo_ui::offscreen::SparseSubmissionAccounting,
    canonical_segment_order_attempt_accounting:
        tilo_ui::offscreen::CanonicalSegmentOrderAttemptAccounting,
    area_ppt8_attempt_accounting: tilo_ui::offscreen::AreaPpt8AttemptAccounting,
    clip_single_group_attempt_accounting: tilo_ui::offscreen::ClipSingleGroupAttemptAccounting,
    resources_before: ResourceSnapshot,
    resources_after: ResourceSnapshot,
}

/// Pixel-parity preflight for the retained damage renderer. This deliberately
/// owns a second, persistent target only in verification mode: creating it in
/// the normal benchmark would add a native-size target and readbacks to the
/// workload whose timings this example reports.
struct DamageVerifier {
    reference: OffscreenRenderer,
    requested_frames: usize,
    checked_frames: usize,
    differing_frames: usize,
    worst_differing_pixels: usize,
}

fn pixel_differs_beyond_aa(actual: &[u8], expected: &[u8]) -> bool {
    actual
        .iter()
        .zip(expected.iter())
        .any(|(a, b)| a.abs_diff(*b) > 1)
}

fn native_pixel_coordinates(pixel: usize, width: usize) -> (u32, u32) {
    (
        u32::try_from(pixel.checked_rem(width).unwrap_or(0)).unwrap_or(u32::MAX),
        u32::try_from(pixel.checked_div(width).unwrap_or(0)).unwrap_or(u32::MAX),
    )
}

impl DamageVerifier {
    fn new(width: u32, height: u32, requested_frames: usize) -> Self {
        // Keep the full-frame oracle independent from every retained optional
        // batching, sparse, direct, and selective CPU path, so an error cannot
        // agree with itself in both targets.
        let mut reference = OffscreenRenderer::new(width, height);
        reference.set_compute_pass_batching(false);
        reference.set_direct_path_dispatch_enabled(false);
        reference.set_defer_bump_readback_enabled(false);
        reference.set_sparse_fine_enabled(false);
        reference.set_cpu_metadata_enabled(false);
        reference.set_cpu_pathtag_enabled(false);
        reference.set_cpu_draw_prefix_enabled(false);
        Self {
            reference,
            requested_frames,
            checked_frames: 0,
            differing_frames: 0,
            worst_differing_pixels: 0,
        }
    }

    fn needs_frame(&self) -> bool {
        self.checked_frames < self.requested_frames
    }

    fn compare(&mut self, state: &mut BenchState<'_>) {
        // `full_draw` has already submitted and waited for this exact Scene's
        // retained-damage update. Read it back without submitting another
        // retained frame, then render that same Scene into the independent
        // full-size reference target until its bump budget has settled.
        let retained = state.renderer.readback_current();
        let full = self
            .reference
            .render_resilient(state.scene, state.theme.tokens.colors.surfaces.base);
        let differing = OffscreenRenderer::pixels_differing_beyond_aa(&retained, &full);
        self.checked_frames = self.checked_frames.saturating_add(1);
        self.worst_differing_pixels = self.worst_differing_pixels.max(differing);
        if differing != 0 {
            if self.differing_frames == 0 {
                Self::report_first_mismatch(state, &retained, &full);
            }
            self.differing_frames = self.differing_frames.saturating_add(1);
            eprintln!(
                "damage-verify: FAIL frame {}/{}: {} pixels differ by more than 1 channel value",
                self.checked_frames, self.requested_frames, differing,
            );
        }
    }

    /// Print enough detail to locate a retained-damage discrepancy without
    /// emitting a native-4K image or allocating diagnostics on passing frames.
    fn report_first_mismatch(state: &BenchState<'_>, retained: &[u8], full: &[u8]) {
        let width = usize::try_from(state.canvas_w).unwrap_or(usize::MAX).max(1);
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        let mut samples = Vec::with_capacity(8);
        for (pixel, (actual, expected)) in retained
            .chunks_exact(4)
            .zip(full.chunks_exact(4))
            .enumerate()
        {
            if pixel_differs_beyond_aa(actual, expected) {
                let (x, y) = native_pixel_coordinates(pixel, width);
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                if samples.len() < 8 {
                    samples.push(format!(
                        "({x},{y}) retained={actual:?} reference={expected:?}"
                    ));
                }
            }
        }
        eprintln!(
            "damage-verify: first mismatch bbox=({min_x},{min_y})..=({max_x},{max_y}) \\
             damage_regions={:?} samples={samples:?}",
            state.damage_regions,
        );
    }

    fn finish(&self) -> bool {
        let complete = self.checked_frames == self.requested_frames;
        let passed = complete && self.differing_frames == 0;
        println!(
            "damage-verify: {} checked={}/{} differing_frames={} worst_differing_pixels={}",
            if passed { "PASS" } else { "FAIL" },
            self.checked_frames,
            self.requested_frames,
            self.differing_frames,
            self.worst_differing_pixels,
        );
        passed
    }
}

#[cfg(test)]
mod damage_verifier_tests {
    use super::{
        OffscreenRenderTiming, Phase, allocator_traffic_bytes, ceil_bytes_per_redraw,
        complete_dispatch_unknown_totals, dispatch_timestamp_configuration_is_valid,
        gpu_timestamp_sample_label, has_complete_gpu_dispatch_timestamp,
        has_complete_gpu_timestamp, has_gpu_dispatch_timestamp_stage, mean_frame_budget_passes,
        native_pixel_coordinates, p99_values, parse_mean_frame_budget, pixel_differs_beyond_aa,
        redraw_frame_samples, redraw_frame_totals, sparse_coarse_profile_totals,
        traffic_byte_redraw_budget_passes, unaccounted_render_us,
    };

    #[test]
    fn damage_verification_counts_real_pixel_and_tile_differences() {
        // phase: reports_only_an_8_bit_difference_beyond_aa_at_its_native_coordinate
        {
            assert!(!pixel_differs_beyond_aa(
                &[10, 20, 30, 40],
                &[11, 19, 31, 39]
            ));
            assert!(pixel_differs_beyond_aa(
                &[10, 20, 30, 40],
                &[12, 20, 30, 40]
            ));
            assert_eq!(native_pixel_coordinates(13, 5), (3, 2));
        }
        // phase: sparse_path_tile_bounds_totals_distinguish_default_applied_and_fallback
        {
            let default = OffscreenRenderTiming::default();
            let applied = OffscreenRenderTiming {
                sparse_path_tile_bounds_submissions: 1,
                sparse_path_tile_bounds_tiles: 24,
                sparse_coarse_opaque_prune_submissions: 1,
                sparse_solid_fuse_submissions: 1,
                sparse_segment_row_mask_submissions: 1,
                canonical_segment_order_sparse_selected_attempts: 1,
                canonical_segment_order_full_selected_attempts: 1,
                area_ppt8_sparse_selected_attempts: 1,
                area_ppt8_full_selected_attempts: 1,
                clip_single_group_sparse_selected_attempts: 1,
                clip_single_group_full_selected_attempts: 1,
                sparse_fine_quad_skip_submissions: 1,
                sparse_fine_quad_total_quads: 64,
                sparse_fine_quad_active_quads: 16,
                sparse_fine_quad_skipped_quads: 48,
                ..OffscreenRenderTiming::default()
            };
            // A sparse fallback contributes no bound coverage because returned stats did not apply it.
            let fallback = OffscreenRenderTiming {
                sparse_submissions: 1,
                ..OffscreenRenderTiming::default()
            };
            assert_eq!(
                sparse_coarse_profile_totals([&default, &applied, &fallback]),
                super::SparseCoarseProfileTotals {
                    path_tile_bounds_submissions: 1,
                    path_tile_bounds_tiles: 24,
                    opaque_prune_selected_submissions: 1,
                    solid_fuse_selected_submissions: 1,
                    segment_row_mask_selected_submissions: 1,
                    canonical_segment_order_sparse_selected_attempts: 1,
                    canonical_segment_order_full_selected_attempts: 1,
                    area_ppt8_sparse_selected_attempts: 1,
                    area_ppt8_full_selected_attempts: 1,
                    clip_single_group_sparse_selected_attempts: 1,
                    clip_single_group_full_selected_attempts: 1,
                    fine_quad_skip_submissions: 1,
                    fine_quad_total: 64,
                    fine_quad_active: 16,
                    fine_quad_skipped: 48,
                    ..super::SparseCoarseProfileTotals::default()
                }
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn frame_budgets_use_exact_redraw_totals_and_benchmark_ranks() {
        // phase: timing_helpers_match_the_benchmark_p99_rank_and_saturate_unaccounted_time
        {
            let values = (0_u128..100).collect::<Vec<_>>();
            assert_eq!(p99_values(&values), 98);

            let phase = Phase {
                render_us: 100,
                ..Phase::default()
            };
            let timing = OffscreenRenderTiming {
                vello_submit_us: 20,
                copy_submit_us: 3,
                settle_wait_us: 70,
                full_attempts: 0,
                damage_setup_us: 5,
                damage_target_creations: 0,
                ..OffscreenRenderTiming::default()
            };
            assert_eq!(unaccounted_render_us(&phase, &timing), 2);
            assert_eq!(
                unaccounted_render_us(
                    &Phase {
                        render_us: 1,
                        ..Phase::default()
                    },
                    &timing,
                ),
                0,
            );

            let complete = OffscreenRenderTiming {
                gpu_timestamp_span_ns: 100,
                gpu_timestamp_last_pass_ns: 40,
                gpu_timestamp_other_pass_ns: 30,
                sparse_submissions: 2,
                sparse_fine_tiles: 12,
                sparse_written_pixels: 384,
                coarse_prefix_pixels: 768,
                sparse_coarse_adaptive_submissions: 2,
                sparse_coarse_adaptive_dense_submissions: 1,
                sparse_coarse_active_bins_submissions: 1,
                sparse_coarse_native_bins: 85,
                sparse_coarse_selected_native_bins: 7,
                sparse_path_tile_bounds_submissions: 1,
                sparse_path_tile_bounds_tiles: 48,
                sparse_coarse_opaque_prune_submissions: 1,
                sparse_solid_fuse_submissions: 1,
                sparse_segment_row_mask_submissions: 1,
                canonical_segment_order_sparse_selected_attempts: 1,
                canonical_segment_order_full_selected_attempts: 1,
                area_ppt8_sparse_selected_attempts: 1,
                area_ppt8_full_selected_attempts: 1,
                clip_single_group_sparse_selected_attempts: 1,
                clip_single_group_full_selected_attempts: 1,
                sparse_fine_quad_skip_submissions: 1,
                sparse_fine_quad_total_quads: 64,
                sparse_fine_quad_active_quads: 20,
                sparse_fine_quad_skipped_quads: 44,
                gpu_timestamp_expected_submissions: 1,
                gpu_timestamp_sampled_submissions: 1,
                ..timing
            };
            let partial = OffscreenRenderTiming {
                gpu_timestamp_expected_submissions: 1,
                gpu_timestamp_missing_submissions: 1,
                ..timing
            };
            let complete_dispatch = OffscreenRenderTiming {
                gpu_dispatch_timestamp_expected_submissions: 1,
                gpu_dispatch_timestamp_sampled_submissions: 1,
                gpu_dispatch_timestamp_present_stages: 1,
                gpu_dispatch_timestamp_stage_duration_ns: [40; 32],
                gpu_dispatch_timestamp_stage_dispatch_count: [2; 32],
                gpu_dispatch_timestamp_unknown_duration_ns: 7,
                gpu_dispatch_timestamp_unknown_dispatch_count: 2,
                ..timing
            };
            let missing_dispatch = OffscreenRenderTiming {
                gpu_dispatch_timestamp_expected_submissions: 1,
                gpu_dispatch_timestamp_missing_submissions: 1,
                gpu_dispatch_timestamp_unknown_duration_ns: 99,
                gpu_dispatch_timestamp_unknown_dispatch_count: 99,
                ..timing
            };
            let stale_dispatch = OffscreenRenderTiming {
                gpu_dispatch_timestamp_expected_submissions: 1,
                gpu_dispatch_timestamp_sampled_submissions: 1,
                gpu_dispatch_timestamp_stale_submissions: 1,
                ..timing
            };
            let complete_spans = [complete, partial]
                .into_iter()
                .filter(has_complete_gpu_timestamp)
                .map(|sample| sample.gpu_timestamp_span_ns)
                .collect::<Vec<_>>();
            assert_eq!(
                complete_spans,
                vec![100],
                "a missing timestamp frame must be omitted, not averaged as a zero span"
            );
            let complete_compute_parts = [complete, partial]
                .into_iter()
                .filter(has_complete_gpu_timestamp)
                .map(|sample| {
                    (
                        sample.gpu_timestamp_last_pass_ns,
                        sample.gpu_timestamp_other_pass_ns,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(complete_compute_parts, vec![(40, 30)]);
            let complete_sparse_accounting = [complete, partial]
                .into_iter()
                .filter(has_complete_gpu_timestamp)
                .map(|sample| {
                    (
                        sample.sparse_submissions,
                        sample.sparse_fine_tiles,
                        sample.sparse_written_pixels,
                        sample.coarse_prefix_pixels,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(complete_sparse_accounting, vec![(2, 12, 384, 768)]);
            assert_eq!(
                sparse_coarse_profile_totals([&complete, &partial]),
                super::SparseCoarseProfileTotals {
                    adaptive_submissions: 2,
                    adaptive_dense_submissions: 1,
                    active_bins_submissions: 1,
                    native_bins: 85,
                    selected_native_bins: 7,
                    path_tile_bounds_submissions: 1,
                    path_tile_bounds_tiles: 48,
                    opaque_prune_selected_submissions: 1,
                    solid_fuse_selected_submissions: 1,
                    segment_row_mask_selected_submissions: 1,
                    canonical_segment_order_sparse_selected_attempts: 1,
                    canonical_segment_order_full_selected_attempts: 1,
                    area_ppt8_sparse_selected_attempts: 1,
                    area_ppt8_full_selected_attempts: 1,
                    clip_single_group_sparse_selected_attempts: 1,
                    clip_single_group_full_selected_attempts: 1,
                    fine_quad_skip_submissions: 1,
                    fine_quad_total: 64,
                    fine_quad_active: 20,
                    fine_quad_skipped: 44,
                }
            );
            assert_eq!(gpu_timestamp_sample_label(&complete, false), "off");
            assert!(gpu_timestamp_sample_label(&complete, true).starts_with("span=100ns"));
            assert!(gpu_timestamp_sample_label(&partial, true).starts_with("incomplete"));
            assert!(dispatch_timestamp_configuration_is_valid(true, true));
            assert!(!dispatch_timestamp_configuration_is_valid(true, false));
            assert!(dispatch_timestamp_configuration_is_valid(false, false));
            assert!(has_complete_gpu_dispatch_timestamp(&complete_dispatch));
            assert!(!has_complete_gpu_dispatch_timestamp(&missing_dispatch));
            assert!(!has_complete_gpu_dispatch_timestamp(&stale_dispatch));
            assert!(has_gpu_dispatch_timestamp_stage(&complete_dispatch, 0));
            assert!(!has_gpu_dispatch_timestamp_stage(&complete_dispatch, 1));
            assert_eq!(
                [complete_dispatch, missing_dispatch, stale_dispatch]
                    .into_iter()
                    .filter(has_complete_gpu_dispatch_timestamp)
                    .count(),
                1
            );
            assert_eq!(vello::gpu_dispatch_timestamp_stage_name(0), Some("unknown"));
            assert_eq!(
                complete_dispatch_unknown_totals([complete_dispatch, missing_dispatch].iter()),
                (7, 2),
                "unknown stage totals must not include incomplete dispatch samples"
            );
        }
        // phase: allocation_traffic_budget_includes_reallocations_and_requires_redraws
        {
            let traffic = allocator_traffic_bytes(10, 5);
            assert_eq!(traffic, 15);
            assert_eq!(ceil_bytes_per_redraw(traffic, 2), 8);
            assert!(traffic_byte_redraw_budget_passes(traffic, 2, 8));
            assert!(!traffic_byte_redraw_budget_passes(traffic, 2, 7));
            assert!(!traffic_byte_redraw_budget_passes(traffic, 0, u64::MAX));
        }
        // phase: mean_frame_budget_uses_exact_redraw_totals_and_rejects_bad_configuration
        {
            assert_eq!(redraw_frame_totals(&[]), None);
            assert_eq!(mean_frame_budget_passes(&[], 8_000), None);

            let idle = Phase {
                total_us: 99_999,
                ..Phase::default()
            };
            assert_eq!(redraw_frame_totals(std::slice::from_ref(&idle)), None);
            assert_eq!(
                mean_frame_budget_passes(std::slice::from_ref(&idle), 8_000),
                None
            );
            let redraws = [
                idle,
                Phase {
                    draw_us: 1,
                    total_us: 8_000,
                    ..Phase::default()
                },
                Phase {
                    render_us: 1,
                    total_us: 8_000,
                    ..Phase::default()
                },
            ];
            assert_eq!(redraw_frame_totals(&redraws), Some((16_000, 2)));
            assert_eq!(mean_frame_budget_passes(&redraws, 8_000), Some(true));

            let fractional_excess = [
                Phase {
                    draw_us: 1,
                    total_us: 8_000,
                    ..Phase::default()
                },
                Phase {
                    render_us: 1,
                    total_us: 8_001,
                    ..Phase::default()
                },
            ];
            assert_eq!(
                mean_frame_budget_passes(&fractional_excess, 8_000),
                Some(false),
                "16,001us across two redraws must not pass an 8,000us mean budget"
            );

            let all_redraws = vec![
                Phase {
                    draw_us: 1,
                    total_us: 8_000,
                    ..Phase::default()
                };
                1_000
            ];
            assert_eq!(redraw_frame_totals(&all_redraws), Some((8_000_000, 1_000)));
            assert_eq!(mean_frame_budget_passes(&all_redraws, 8_000), Some(true));
            assert_eq!(parse_mean_frame_budget(None), Ok(None));
            assert_eq!(parse_mean_frame_budget(Some("8000")), Ok(Some(8_000)));
            assert!(parse_mean_frame_budget(Some("0")).is_err());
            assert!(parse_mean_frame_budget(Some("nope")).is_err());
        }
    }

    #[test]
    fn frame_samples_are_the_redraw_population_the_budgets_rank() {
        assert_eq!(redraw_frame_samples(&[]), "");
        let histo = [
            Phase {
                total_us: 99_999,
                ..Phase::default()
            },
            Phase {
                draw_us: 1,
                total_us: 7_000,
                ..Phase::default()
            },
            Phase {
                render_us: 1,
                total_us: 9_000,
                ..Phase::default()
            },
        ];
        // The skipped event is not a frame; both redraws are, in event order.
        assert_eq!(redraw_frame_samples(&histo), "7000\n9000\n");
        assert_eq!(
            redraw_frame_samples(&histo).lines().count(),
            redraw_frame_totals(&histo).map_or(0, |(_, redraws)| redraws)
        );
    }
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
    profile_render_timing: bool,
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
    theme: &'a Theme,
    scene: &'a mut Scene,
    renderer: &'a mut OffscreenRenderer,
    damage_regions: &'a mut Vec<vello::kurbo::Rect>,
    canvas_w: u32,
    canvas_h: u32,
    /// `TILO_UI_DASH_PEER=1`: a collaborator whose pointer moves on every
    /// event, painted over the widgets the way an app window paints it.
    peer: Option<&'a mut PeerBench>,
    /// The time the tree reads while it handles events, lays out and draws.
    clock: FrameClock,
}

/// Time between two benchmark events on the tree's clock: a 1 kHz pointer,
/// so 200 events span 0.2 s and the 30 Hz indicators step six times beside
/// the redraws the events themselves cause.
const EVENT_INTERVAL: tilo_ui::time::Duration = tilo_ui::time::Duration::from_millis(1);

/// Events of the unmeasured pass before the measured loop: the event mix
/// repeats every 40 events (`dispatch_bench_event`), so this is each of its
/// states once.
const WARM_EVENTS: usize = 40;

/// The clock the dashboard's widgets read: it starts when the tree is built
/// and moves a fixed interval per event, whatever the wall time. On the
/// wall clock a spinner steps as often as the node is slow: a loaded node
/// redraws more frames, each with more indicators changed, and the redraw
/// counters measure the node instead of the toolkit. Rendering and the
/// benchmark's own timings stay on the wall clock.
#[derive(Clone, Copy)]
struct FrameClock {
    at: tilo_ui::time::Instant,
    interval: tilo_ui::time::Duration,
}

impl FrameClock {
    fn new(interval: tilo_ui::time::Duration) -> Self {
        Self {
            at: tilo_ui::time::now(),
            interval,
        }
    }

    fn advance(&mut self, by: tilo_ui::time::Duration) {
        self.at = self.at.checked_add(by).unwrap_or(self.at);
    }

    /// Advance to the next event.
    fn tick(&mut self) {
        self.advance(self.interval);
    }

    /// Run `op` with the tree's clock reading this clock's time.
    fn pin<T>(self, op: impl FnOnce() -> T) -> T {
        tilo_clock::testing::with_monotonic_time(self.at, op)
    }
}

/// A remote collaborator's pointer, drawn through the framework's presence
/// layer: each frame damages only where the pointer and its name were and
/// are, never the whole window.
struct PeerBench {
    presence: PresenceCtx,
    layer: tilo_ui::presence::PresenceLayer,
    /// `TILO_UI_DASH_PEER=full`: the rule before #3936, under which any
    /// visible collaborator repainted the whole window, for comparison.
    full_repaint: bool,
}

impl PeerBench {
    fn from_env() -> Option<Self> {
        let mode = tilo_ui::env::var("TILO_UI_DASH_PEER").ok()?;
        let full_repaint = match mode.as_str() {
            "1" => false,
            "full" => true,
            _ => {
                eprintln!("TILO_UI_DASH_PEER must be 1 or full, got {mode:?}");
                std::process::exit(2);
            }
        };
        Some(Self {
            presence: PresenceCtx::default(),
            layer: tilo_ui::presence::PresenceLayer::new(),
            full_repaint,
        })
    }

    /// Glide the pointer along a slow Lissajous path across the canvas, a
    /// few dozen pixels per event, like a person's hand at ~30 Hz.
    fn move_pointer(&mut self, i: usize, canvas_w: u32, canvas_h: u32) {
        let (w, h) = (f64::from(canvas_w), f64::from(canvas_h));
        let t = idx_f64(i);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "pointer positions inside a canvas well under f32's exact range"
        )]
        let at = [
            (w / 2.0 + w / 3.0 * (t * 0.031).sin()) as f32,
            (h / 2.0 + h / 3.0 * (t * 0.023).cos()) as f32,
        ];
        self.presence
            .upsert_remote_cursor("peer-ada".into(), at, Some("Ada Lovelace"));
    }

    /// Paint the pointer into the presence layer and append it over the
    /// widgets. Returns its damage, `None` when it must repaint everything.
    fn draw(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: vello::kurbo::Rect,
    ) -> Option<Vec<vello::kurbo::Rect>> {
        let presence = &self.presence;
        self.layer.record(|layer| {
            tilo_ui::scene_parts::part_drawn(
                layer,
                tilo_ui::scene_parts::PartKey::new("bench.remote-presence", ()),
                |layer| {
                    tilo_ui::presence::draw_remote_presence(
                        layer,
                        Affine::IDENTITY,
                        viewport,
                        text_renderer,
                        theme,
                        presence,
                    )
                },
            );
        });
        scene.append(self.layer.scene(), None);
        self.layer.damage().filter(|_| !self.full_repaint)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the benchmark keeps layout, scene, damage, and render timing/allocation boundaries together"
)]
fn full_draw(
    state: &mut BenchState<'_>,
    phase: &mut Phase,
    mut allocation_breakdown: Option<&mut AllocationBreakdown>,
    render_timing: Option<&mut OffscreenRenderTiming>,
) {
    let allocation_before_layout = allocation_breakdown
        .as_ref()
        .map(|_| crate::allocation_profile_snapshot());
    let t_layout = tilo_ui::time::now();
    let clock = state.clock;
    let layout_dirty = state.root.is_layout_dirty();
    if layout_dirty {
        clock.pin(|| {
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
        });
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
    let t_draw = tilo_ui::time::now();
    state.scene.reset();
    // Wire the spatial-culling viewport just like the live App
    // does. Without this the bench's `draw` walks
    // see `viewport == None` and skip the culling short-circuit,
    // masking the Item 1 win in the benchmark output.
    let vp = vello::kurbo::Rect::new(
        0.0,
        0.0,
        f64::from(state.canvas_w),
        f64::from(state.canvas_h),
    );
    let peer_damage = clock.pin(|| {
        let prev_vp = state.presence.push_viewport(vp);
        state.root.draw(
            state.scene,
            Affine::IDENTITY,
            state.text_renderer,
            state.theme,
            state.presence,
        );
        state.presence.pop_viewport(prev_vp);
        state
            .peer
            .as_deref_mut()
            .map(|peer| peer.draw(state.scene, state.text_renderer, state.theme, vp))
    });
    phase.draw_us = t_draw.elapsed().as_micros();
    if let (Some(before), Some(breakdown)) =
        (allocation_before_draw, allocation_breakdown.as_deref_mut())
    {
        add_allocations(
            &mut breakdown.draw,
            allocation_delta(crate::allocation_profile_snapshot(), before),
        );
    }

    let t_render = tilo_ui::time::now();
    let allocation_before_damage = allocation_breakdown
        .as_ref()
        .map(|_| crate::allocation_profile_snapshot());
    // Layout can expose both old and new pixels, so it conservatively damages
    // the complete native target. Visual-only changes use the union of dirty
    // leaves plus a 16px safety margin for AA, focus rings, and small shadows.
    state.damage_regions.clear();
    if layout_dirty || matches!(peer_damage, Some(None)) {
        state.damage_regions.push(vp);
    } else {
        clock.pin(|| state.root.collect_damage(state.damage_regions));
        for rect in &mut *state.damage_regions {
            *rect = rect.inflate(16.0, 16.0).intersect(vp);
        }
        // The peer's marks, as the native runner submits them: whole pixels
        // plus one for rounding.
        for rect in peer_damage.into_iter().flatten().flatten() {
            state
                .damage_regions
                .push(rect.expand().inflate(1.0, 1.0).intersect(vp));
        }
        state.damage_regions.retain(|rect| !rect.is_zero_area());
        if state.damage_regions.is_empty() {
            state.damage_regions.push(vp);
        }
    }
    if tilo_ui::env::var_os("TILO_UI_DASH_DAMAGE").is_some() {
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
    if let Some(render_timing) = render_timing {
        let (pixels, timing) = state.renderer.render_damage_regions_gpu_wait_profiled(
            state.scene,
            state.theme.tokens.colors.surfaces.base,
            state.damage_regions,
        );
        // This is actual native fine-shaded pixel coverage, including dense
        // recovery; sparse mode does not turn coarse-prefix work into zero.
        phase.raster_pixels = pixels;
        *render_timing = timing;
    } else {
        phase.raster_pixels = state.renderer.render_damage_regions_gpu_wait(
            state.scene,
            state.theme.tokens.colors.surfaces.base,
            state.damage_regions,
        );
    }
    phase.render_us = t_render.elapsed().as_micros();
    if let (Some(before), Some(breakdown)) = (allocation_before_render, allocation_breakdown) {
        add_allocations(
            &mut breakdown.render,
            allocation_delta(crate::allocation_profile_snapshot(), before),
        );
    }

    clock.pin(|| state.root.mark_drawn());
    if let Some(peer) = state.peer.as_deref_mut() {
        peer.layer.presented();
    }
}

fn dispatch(state: &mut BenchState<'_>, doc: &mut AutoCommit, event: &WindowEvent) {
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    let modifiers = Modifiers::default();
    state.clock.pin(|| {
        state.root.handle_event(
            event,
            &mut tilo_ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: tilo_ui::widget::Edits::lend(doc),
                path: &automerge::ObjId::Root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: state.root_node,
                taffy: state.taffy,
            },
        );
    });
}

fn make_cursor(x: f64, y: f64) -> WindowEvent {
    let device_id = winit::event::DeviceId::dummy();
    WindowEvent::CursorMoved {
        device_id,
        position: PhysicalPosition::new(x, y),
    }
}

fn make_click_press() -> WindowEvent {
    let device_id = winit::event::DeviceId::dummy();
    WindowEvent::MouseInput {
        device_id,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

fn make_click_release() -> WindowEvent {
    let device_id = winit::event::DeviceId::dummy();
    WindowEvent::MouseInput {
        device_id,
        state: ElementState::Released,
        button: MouseButton::Left,
    }
}

fn make_wheel(dy: f32) -> WindowEvent {
    let device_id = winit::event::DeviceId::dummy();
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
    ftl.push_str("dash_title = Tilo Desktop Dashboard\n");
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
    full_draw(state, &mut warm, None, None);
}

/// Two warm-up frames: first `compute_layout`, second post-cache.
///
/// Each moves the indicators a step, as the wall clock did while a warm-up
/// frame rendered: the measured loop then starts with the paths a stepping
/// spinner takes (its damage, the retained raster's partial redraw) already
/// warm, not with their first use.
fn warm_up(state: &mut BenchState<'_>) {
    for _ in 0..2 {
        state.clock.advance(tilo_ui::animation::INDICATOR_STEP);
        let mut warm = Phase::default();
        full_draw(state, &mut warm, None, None);
    }
}

fn print_warmup_report(state: &BenchState<'_>) {
    println!(
        "after warm-up: root.needs_redraw()={} root.is_layout_dirty()={}",
        state.clock.pin(|| state.root.needs_redraw()),
        state.root.is_layout_dirty()
    );
    let mut culprits = Vec::new();
    state
        .clock
        .pin(|| dirty_culprits(state.root.as_ref(), 0, &mut culprits));
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
    println!(
        "after warm-up: CPU path-tag retained host pool={}B (excluded from known GPU bytes)",
        state.renderer.vello().cpu_pathtag_pool_retained_bytes(),
    );
    println!(
        "after warm-up: CPU metadata retained host pool={}B (excluded from known GPU bytes)",
        state.renderer.vello().cpu_metadata_pool_retained_bytes(),
    );
    println!(
        "after warm-up: CPU draw-prefix retained host pool={}B (excluded from known GPU bytes)",
        state.renderer.vello().cpu_draw_prefix_pool_retained_bytes(),
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
    // A collaborator's pointer moves on every event.
    let (canvas_w, canvas_h) = (state.canvas_w, state.canvas_h);
    if let Some(peer) = state.peer.as_deref_mut() {
        peer.move_pointer(i, canvas_w, canvas_h);
    }
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
    mut verifier: Option<&mut DamageVerifier>,
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
    let mut render_timings = options
        .profile_render_timing
        .then(|| vec![OffscreenRenderTiming::default(); iters]);
    state.renderer.reset_sparse_submission_accounting();
    state
        .renderer
        .reset_canonical_segment_order_attempt_accounting();
    state.renderer.reset_area_ppt8_attempt_accounting();
    state.renderer.reset_clip_single_group_attempt_accounting();
    let resources_before = resource_snapshot(state.renderer);
    if options.profile_allocations {
        // The timing histogram is harness storage, not framework work.
        crate::begin_allocation_profile();
    }
    let mut allocation_breakdown = options
        .profile_allocation_breakdown
        .then(AllocationBreakdown::default);

    for (i, sample) in (0..iters).zip(&mut histo) {
        let t_total = tilo_ui::time::now();
        let mut phase = Phase::default();

        let allocation_before_handle = allocation_breakdown
            .as_ref()
            .map(|_| crate::allocation_profile_snapshot());
        state.clock.tick();
        let t_handle = tilo_ui::time::now();
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

        // App-style gate: only draw if the tree signals dirty (or the peer
        // moved).
        let redrew = state.clock.pin(|| state.root.needs_redraw()) || state.peer.is_some();
        if redrew {
            total_redraws = total_redraws.saturating_add(1);
            let render_timing = render_timings
                .as_mut()
                .and_then(|timings| timings.get_mut(i));
            full_draw(
                state,
                &mut phase,
                allocation_breakdown.as_mut(),
                render_timing,
            );
        } else {
            total_skipped = total_skipped.saturating_add(1);
        }
        phase.total_us = t_total.elapsed().as_micros();
        if redrew
            && let Some(verifier) = verifier.as_deref_mut()
            && verifier.needs_frame()
        {
            // Keep this out of `phase`: it performs target readback plus a
            // full reference render, and verify mode suppresses all timing
            // and budget output below.
            verifier.compare(state);
        }
        *sample = phase;
    }

    let allocations = options
        .profile_allocations
        .then(crate::end_allocation_profile);
    let resources_after = resource_snapshot(state.renderer);
    let sparse_submission_accounting = state.renderer.sparse_submission_accounting();
    let canonical_segment_order_attempt_accounting =
        state.renderer.canonical_segment_order_attempt_accounting();
    let area_ppt8_attempt_accounting = state.renderer.area_ppt8_attempt_accounting();
    let clip_single_group_attempt_accounting =
        state.renderer.clip_single_group_attempt_accounting();
    BenchRun {
        histo,
        total_redraws,
        total_skipped,
        allocations,
        allocation_breakdown,
        render_timings,
        sparse_submission_accounting,
        canonical_segment_order_attempt_accounting,
        area_ppt8_attempt_accounting,
        clip_single_group_attempt_accounting,
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
        println!("native fine-shaded pixels per redraw:    {raster_pixels}");
    }
}

fn print_frame_breakdown_group(label: &str, samples: &[Phase]) {
    if samples.is_empty() {
        println!("{label:<18} {:>8}", "0 samples");
        return;
    }
    println!(
        "{label:<18} {:>8} {:>10} {:>10} {:>10} {:>10}",
        samples.len(),
        mean(samples, |phase| phase.total_us),
        pct(samples, 99.0, |phase| phase.total_us),
        mean(samples, |phase| phase.render_us),
        pct(samples, 99.0, |phase| phase.render_us),
    );
}

/// Reports existing frame samples after the timed loop. This does not alter
/// the workload, gate population, or resource snapshots; it only classifies
/// the `raster_pixels` that were already recorded for each redraw.
fn print_frame_breakdown(histo: &[Phase], full_pixels: u64) {
    let redrawn = histo
        .iter()
        .enumerate()
        .filter(|(_, phase)| phase.draw_us > 0 || phase.render_us > 0)
        .collect::<Vec<_>>();
    let full_or_recovery = redrawn
        .iter()
        .filter(|(_, phase)| phase.raster_pixels >= full_pixels)
        .map(|(_, phase)| (**phase).clone())
        .collect::<Vec<_>>();
    let partial = redrawn
        .iter()
        .filter(|(_, phase)| phase.raster_pixels > 0 && phase.raster_pixels < full_pixels)
        .map(|(_, phase)| (**phase).clone())
        .collect::<Vec<_>>();

    println!();
    println!("frame breakdown (post-timing; all gate samples remain unchanged):");
    println!(
        "{:<18} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "kind", "samples", "mean total", "p99 total", "mean render", "p99 render"
    );
    print_frame_breakdown_group("full/recovery", &full_or_recovery);
    print_frame_breakdown_group("partial", &partial);

    let mut slowest = redrawn;
    slowest.sort_unstable_by_key(|(_, phase)| std::cmp::Reverse(phase.total_us));
    println!("slowest redrawn samples (event index: total_us, raster_pixels):");
    for (index, phase) in slowest.into_iter().take(10) {
        println!("  {index}: {}, {}", phase.total_us, phase.raster_pixels);
    }
}

fn unaccounted_render_us(phase: &Phase, timing: &OffscreenRenderTiming) -> u128 {
    phase.render_us.saturating_sub(
        timing
            .vello_submit_us
            .saturating_add(timing.copy_submit_us)
            .saturating_add(timing.damage_setup_us)
            .saturating_add(timing.settle_wait_us),
    )
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the rank is the same bounded p99 index used by pct above"
)]
fn p99_values(values: &[u128]) -> u128 {
    let index = ((len_f64(values.len()) - 1.0) * 99.0 / 100.0).round() as usize;
    values.get(index).copied().unwrap_or(0)
}

fn has_complete_gpu_timestamp(timing: &OffscreenRenderTiming) -> bool {
    timing.gpu_timestamp_expected_submissions != 0
        && timing.gpu_timestamp_missing_submissions == 0
        && timing.gpu_timestamp_sampled_submissions == timing.gpu_timestamp_expected_submissions
}

fn has_complete_gpu_dispatch_timestamp(timing: &OffscreenRenderTiming) -> bool {
    timing.gpu_dispatch_timestamp_expected_submissions != 0
        && timing.gpu_dispatch_timestamp_missing_submissions == 0
        && timing.gpu_dispatch_timestamp_stale_submissions == 0
        && timing.gpu_dispatch_timestamp_sampled_submissions
            == timing.gpu_dispatch_timestamp_expected_submissions
}

fn has_gpu_dispatch_timestamp_stage(timing: &OffscreenRenderTiming, stage_index: usize) -> bool {
    let shift = u32::try_from(stage_index).unwrap_or(u32::MAX);
    let Some(stage_bit) = 1_u32.checked_shl(shift) else {
        return false;
    };
    timing.gpu_dispatch_timestamp_present_stages & stage_bit != 0
}

fn complete_dispatch_unknown_totals<'a>(
    timings: impl IntoIterator<Item = &'a OffscreenRenderTiming>,
) -> (u128, u128) {
    timings
        .into_iter()
        .filter(|timing| has_complete_gpu_dispatch_timestamp(timing))
        .fold((0, 0), |(duration, dispatches), timing| {
            (
                duration.saturating_add(timing.gpu_dispatch_timestamp_unknown_duration_ns),
                dispatches.saturating_add(u128::from(
                    timing.gpu_dispatch_timestamp_unknown_dispatch_count,
                )),
            )
        })
}

fn gpu_timestamp_sample_label(timing: &OffscreenRenderTiming, enabled: bool) -> String {
    if !enabled {
        return "off".to_owned();
    }
    if has_complete_gpu_timestamp(timing) {
        return format!(
            "span={}ns last-compute={}ns other-compute={}ns complete ({}/{}/{} received/expected/missing)",
            timing.gpu_timestamp_span_ns,
            timing.gpu_timestamp_last_pass_ns,
            timing.gpu_timestamp_other_pass_ns,
            timing.gpu_timestamp_sampled_submissions,
            timing.gpu_timestamp_expected_submissions,
            timing.gpu_timestamp_missing_submissions,
        );
    }
    format!(
        "incomplete ({}/{}/{} received/expected/missing)",
        timing.gpu_timestamp_sampled_submissions,
        timing.gpu_timestamp_expected_submissions,
        timing.gpu_timestamp_missing_submissions,
    )
}

fn dispatch_timestamp_configuration_is_valid(
    requested: bool,
    gpu_timestamps_enabled: bool,
) -> bool {
    !requested || gpu_timestamps_enabled
}

type RenderTimingSample<'a> = (usize, (&'a Phase, &'a OffscreenRenderTiming));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SparseCoarseProfileTotals {
    adaptive_submissions: u128,
    adaptive_dense_submissions: u128,
    active_bins_submissions: u128,
    native_bins: u128,
    selected_native_bins: u128,
    path_tile_bounds_submissions: u128,
    path_tile_bounds_tiles: u128,
    opaque_prune_selected_submissions: u128,
    solid_fuse_selected_submissions: u128,
    segment_row_mask_selected_submissions: u128,
    canonical_segment_order_sparse_selected_attempts: u128,
    canonical_segment_order_full_selected_attempts: u128,
    area_ppt8_sparse_selected_attempts: u128,
    area_ppt8_full_selected_attempts: u128,
    clip_single_group_sparse_selected_attempts: u128,
    clip_single_group_full_selected_attempts: u128,
    fine_quad_skip_submissions: u128,
    fine_quad_total: u128,
    fine_quad_active: u128,
    fine_quad_skipped: u128,
}

fn sparse_coarse_profile_totals<'a>(
    timings: impl IntoIterator<Item = &'a OffscreenRenderTiming>,
) -> SparseCoarseProfileTotals {
    timings.into_iter().fold(
        SparseCoarseProfileTotals::default(),
        |mut totals, timing| {
            totals.adaptive_submissions = totals
                .adaptive_submissions
                .saturating_add(u128::from(timing.sparse_coarse_adaptive_submissions));
            totals.adaptive_dense_submissions = totals
                .adaptive_dense_submissions
                .saturating_add(u128::from(timing.sparse_coarse_adaptive_dense_submissions));
            totals.active_bins_submissions = totals
                .active_bins_submissions
                .saturating_add(u128::from(timing.sparse_coarse_active_bins_submissions));
            totals.native_bins = totals
                .native_bins
                .saturating_add(u128::from(timing.sparse_coarse_native_bins));
            totals.selected_native_bins = totals
                .selected_native_bins
                .saturating_add(u128::from(timing.sparse_coarse_selected_native_bins));
            totals.path_tile_bounds_submissions = totals
                .path_tile_bounds_submissions
                .saturating_add(u128::from(timing.sparse_path_tile_bounds_submissions));
            totals.path_tile_bounds_tiles = totals
                .path_tile_bounds_tiles
                .saturating_add(u128::from(timing.sparse_path_tile_bounds_tiles));
            totals.opaque_prune_selected_submissions = totals
                .opaque_prune_selected_submissions
                .saturating_add(u128::from(timing.sparse_coarse_opaque_prune_submissions));
            totals.solid_fuse_selected_submissions = totals
                .solid_fuse_selected_submissions
                .saturating_add(u128::from(timing.sparse_solid_fuse_submissions));
            totals.segment_row_mask_selected_submissions = totals
                .segment_row_mask_selected_submissions
                .saturating_add(u128::from(timing.sparse_segment_row_mask_submissions));
            totals.canonical_segment_order_sparse_selected_attempts = totals
                .canonical_segment_order_sparse_selected_attempts
                .saturating_add(u128::from(
                    timing.canonical_segment_order_sparse_selected_attempts,
                ));
            totals.canonical_segment_order_full_selected_attempts = totals
                .canonical_segment_order_full_selected_attempts
                .saturating_add(u128::from(
                    timing.canonical_segment_order_full_selected_attempts,
                ));
            totals.area_ppt8_sparse_selected_attempts = totals
                .area_ppt8_sparse_selected_attempts
                .saturating_add(u128::from(timing.area_ppt8_sparse_selected_attempts));
            totals.area_ppt8_full_selected_attempts = totals
                .area_ppt8_full_selected_attempts
                .saturating_add(u128::from(timing.area_ppt8_full_selected_attempts));
            totals.clip_single_group_sparse_selected_attempts = totals
                .clip_single_group_sparse_selected_attempts
                .saturating_add(u128::from(
                    timing.clip_single_group_sparse_selected_attempts,
                ));
            totals.clip_single_group_full_selected_attempts = totals
                .clip_single_group_full_selected_attempts
                .saturating_add(u128::from(timing.clip_single_group_full_selected_attempts));
            totals.fine_quad_skip_submissions = totals
                .fine_quad_skip_submissions
                .saturating_add(u128::from(timing.sparse_fine_quad_skip_submissions));
            totals.fine_quad_total = totals
                .fine_quad_total
                .saturating_add(u128::from(timing.sparse_fine_quad_total_quads));
            totals.fine_quad_active = totals
                .fine_quad_active
                .saturating_add(u128::from(timing.sparse_fine_quad_active_quads));
            totals.fine_quad_skipped = totals
                .fine_quad_skipped
                .saturating_add(u128::from(timing.sparse_fine_quad_skipped_quads));
            totals
        },
    )
}

fn print_sparse_path_accounting(totals: &SparseCoarseProfileTotals) {
    println!(
        "  sparse path accounting (all sparse attempts): adaptive_submissions={} adaptive_dense_submissions={} active_bin_submissions={} native_bins={} selected_native_bins={} path_tile_bounds_applied_submissions={} path_tile_bounds_native_bbox_tiles={} opaque_prune_shader_selected_submissions={} solid_fuse_shader_selected_submissions={} segment_row_mask_shader_selected={} canonical_segment_order_shader_selected_sparse_attempts={} canonical_segment_order_shader_selected_full_attempts={} area_ppt8_shader_selected_sparse_attempts={} area_ppt8_shader_selected_full_attempts={} clip_single_group_shader_selected_sparse_attempts={} clip_single_group_shader_selected_full_attempts={} fine_quad_skip_applied_submissions={} fine_quad_total_invocation_groups={} fine_quad_active_invocation_groups={} fine_quad_skipped_invocation_groups={} (opaque prune, solid fuse, segment row mask, canonical segment order, Area PPT8, and clip single group are shader selections only; bbox/quad counts are coverage and invocation-group diagnostics, not tiles or commands pruned, fused, row-skipped, allocated tiles, GPU memory, copy savings, completion, or speed)",
        totals.adaptive_submissions,
        totals.adaptive_dense_submissions,
        totals.active_bins_submissions,
        totals.native_bins,
        totals.selected_native_bins,
        totals.path_tile_bounds_submissions,
        totals.path_tile_bounds_tiles,
        totals.opaque_prune_selected_submissions,
        totals.solid_fuse_selected_submissions,
        totals.segment_row_mask_selected_submissions,
        totals.canonical_segment_order_sparse_selected_attempts,
        totals.canonical_segment_order_full_selected_attempts,
        totals.area_ppt8_sparse_selected_attempts,
        totals.area_ppt8_full_selected_attempts,
        totals.clip_single_group_sparse_selected_attempts,
        totals.clip_single_group_full_selected_attempts,
        totals.fine_quad_skip_submissions,
        totals.fine_quad_total,
        totals.fine_quad_active,
        totals.fine_quad_skipped,
    );
}

fn report_timing_values(label: &str, values: &mut [u128], unit: &str) {
    if values.is_empty() {
        println!("  {label:<14} no samples");
        return;
    }
    values.sort_unstable();
    println!(
        "  {label:<14} mean={}{} p99={}{}",
        values
            .iter()
            .sum::<u128>()
            .checked_div(u128::try_from(values.len()).unwrap_or(1))
            .unwrap_or(0),
        unit,
        p99_values(values),
        unit,
    );
}

fn print_gpu_timestamp_profile(
    samples: &[RenderTimingSample<'_>],
    gpu_timestamp_buffer_bytes: u64,
) {
    let complete_samples = samples
        .iter()
        .copied()
        .filter(|(_, (_, timing))| has_complete_gpu_timestamp(timing))
        .collect::<Vec<_>>();
    let values = |f: fn(&OffscreenRenderTiming) -> u128| {
        complete_samples
            .iter()
            .map(|(_, (_, timing))| f(timing))
            .collect::<Vec<_>>()
    };
    let expected = samples.iter().fold(0_u128, |total, (_, (_, timing))| {
        total.saturating_add(u128::from(timing.gpu_timestamp_expected_submissions))
    });
    let received = samples.iter().fold(0_u128, |total, (_, (_, timing))| {
        total.saturating_add(u128::from(timing.gpu_timestamp_sampled_submissions))
    });
    let missing = samples.iter().fold(0_u128, |total, (_, (_, timing))| {
        total.saturating_add(u128::from(timing.gpu_timestamp_missing_submissions))
    });
    println!(
        "  GPU timestamps are Vello compute-pass device intervals, not host encode/resolve/map/settle time; queue scheduling or device preemption can still affect the measured span."
    );
    println!(
        "  gpu samples: complete={}/{} omitted={}; submissions expected={expected} received={received} missing={missing}; diagnostic buffers={gpu_timestamp_buffer_bytes}B",
        complete_samples.len(),
        samples.len(),
        samples.len().saturating_sub(complete_samples.len()),
    );
    let mut span = values(|timing| timing.gpu_timestamp_span_ns);
    report_timing_values("gpu-span", &mut span, "ns");
    let mut pass = values(|timing| timing.gpu_timestamp_pass_ns);
    report_timing_values("gpu-pass-time", &mut pass, "ns");
    let mut last_compute = values(|timing| timing.gpu_timestamp_last_pass_ns);
    report_timing_values("last-compute", &mut last_compute, "ns");
    let mut other_compute = values(|timing| timing.gpu_timestamp_other_pass_ns);
    report_timing_values("other-compute", &mut other_compute, "ns");
    let mut pass_count = values(|timing| u128::from(timing.gpu_timestamp_pass_count));
    report_timing_values("gpu-pass-count", &mut pass_count, "count");
    println!(
        "  last-compute is the final Vello compute pass in the current full-render pipeline; this diagnostic mapping is not a general pipeline guarantee."
    );
}

fn print_gpu_dispatch_timestamp_profile(
    samples: &[RenderTimingSample<'_>],
    status: vello::GpuDispatchTimestampStatus,
    dispatch_timestamp_buffer_bytes: u64,
) {
    let complete_samples = samples
        .iter()
        .copied()
        .filter(|(_, (_, timing))| has_complete_gpu_dispatch_timestamp(timing))
        .collect::<Vec<_>>();
    let total = |f: fn(&OffscreenRenderTiming) -> u128| {
        samples.iter().fold(0_u128, |total, (_, (_, timing))| {
            total.saturating_add(f(timing))
        })
    };
    let expected = total(|timing| u128::from(timing.gpu_dispatch_timestamp_expected_submissions));
    let received = total(|timing| u128::from(timing.gpu_dispatch_timestamp_sampled_submissions));
    let missing = total(|timing| u128::from(timing.gpu_dispatch_timestamp_missing_submissions));
    let stale = total(|timing| u128::from(timing.gpu_dispatch_timestamp_stale_submissions));
    println!(
        "  gpu dispatch timestamps: status={status:?}; complete={}/{} omitted={}; submissions expected={expected} received={received} missing={missing} stale={stale}; diagnostic buffers={dispatch_timestamp_buffer_bytes}B",
        complete_samples.len(),
        samples.len(),
        samples.len().saturating_sub(complete_samples.len()),
    );
    for stage_index in 0..32 {
        let stage_samples = complete_samples
            .iter()
            .filter(|(_, (_, timing))| has_gpu_dispatch_timestamp_stage(timing, stage_index))
            .map(|(_, (_, timing))| *timing)
            .collect::<Vec<_>>();
        if stage_samples.is_empty() {
            continue;
        }
        let mut duration = stage_samples
            .iter()
            .map(|timing| {
                timing
                    .gpu_dispatch_timestamp_stage_duration_ns
                    .get(stage_index)
                    .copied()
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();
        let mut dispatches = stage_samples
            .iter()
            .map(|timing| {
                u128::from(
                    timing
                        .gpu_dispatch_timestamp_stage_dispatch_count
                        .get(stage_index)
                        .copied()
                        .unwrap_or(0),
                )
            })
            .collect::<Vec<_>>();
        let stage_id = u16::try_from(stage_index.saturating_add(1)).unwrap_or(u16::MAX);
        let stage_name = vello::gpu_dispatch_timestamp_stage_name(stage_id).unwrap_or("unknown");
        let total_dispatches = dispatches
            .iter()
            .copied()
            .fold(0_u128, u128::saturating_add);
        println!(
            "  dispatch-stage-{stage_name} (id={stage_id}): complete_samples={} total_dispatches={total_dispatches}",
            stage_samples.len(),
        );
        report_timing_values("duration", &mut duration, "ns");
        report_timing_values("dispatches", &mut dispatches, "count");
    }
    let (unknown_duration, unknown_dispatches) =
        complete_dispatch_unknown_totals(complete_samples.iter().map(|(_, (_, timing))| *timing));
    if unknown_duration != 0 || unknown_dispatches != 0 {
        println!(
            "  dispatch-stage-unknown: total_duration={unknown_duration}ns total_dispatches={unknown_dispatches}"
        );
    }
}

/// Sparse fine measurements include every render attempt. They describe the
/// selected fine coverage alongside the coarse prefix that still ran.
fn print_sparse_fine_profile(samples: &[RenderTimingSample<'_>]) {
    let total = |f: fn(&OffscreenRenderTiming) -> u128| {
        samples.iter().fold(0_u128, |total, (_, (_, timing))| {
            total.saturating_add(f(timing))
        })
    };
    let values = |f: fn(&OffscreenRenderTiming) -> u128| {
        samples
            .iter()
            .map(|(_, (_, timing))| f(timing))
            .collect::<Vec<_>>()
    };
    let sparse_submissions = total(|timing| u128::from(timing.sparse_submissions));
    let sparse_fine_tiles = total(|timing| u128::from(timing.sparse_fine_tiles));
    let sparse_written_pixels = total(|timing| u128::from(timing.sparse_written_pixels));
    let coarse_prefix_pixels = total(|timing| u128::from(timing.coarse_prefix_pixels));
    let adaptive = sparse_coarse_profile_totals(samples.iter().map(|(_, (_, timing))| *timing));
    println!(
        "  sparse fine accounting (all render attempts): submissions={sparse_submissions} fine_tiles={sparse_fine_tiles} written_pixels={sparse_written_pixels} coarse_prefix_pixels={coarse_prefix_pixels}"
    );
    print_sparse_path_accounting(&adaptive);
    let mut sparse_submission_values = values(|timing| u128::from(timing.sparse_submissions));
    report_timing_values("sparse-submits", &mut sparse_submission_values, "count");
    let mut sparse_tile_values = values(|timing| u128::from(timing.sparse_fine_tiles));
    report_timing_values("sparse-fine-tiles", &mut sparse_tile_values, "count");
    let mut sparse_written_values = values(|timing| u128::from(timing.sparse_written_pixels));
    report_timing_values("sparse-written", &mut sparse_written_values, "px");
    let mut coarse_prefix_values = values(|timing| u128::from(timing.coarse_prefix_pixels));
    report_timing_values("coarse-prefix", &mut coarse_prefix_values, "px");
    let mut adaptive_values =
        values(|timing| u128::from(timing.sparse_coarse_adaptive_submissions));
    report_timing_values("adaptive-coarse", &mut adaptive_values, "count");
    let mut adaptive_dense_values =
        values(|timing| u128::from(timing.sparse_coarse_adaptive_dense_submissions));
    report_timing_values("adaptive-dense-coarse", &mut adaptive_dense_values, "count");
    let mut native_bin_values = values(|timing| u128::from(timing.sparse_coarse_native_bins));
    report_timing_values("native-coarse-bins", &mut native_bin_values, "count");
    let mut selected_bin_values =
        values(|timing| u128::from(timing.sparse_coarse_selected_native_bins));
    report_timing_values("selected-coarse-bins", &mut selected_bin_values, "count");
    let mut path_tile_bounds_values =
        values(|timing| u128::from(timing.sparse_path_tile_bounds_submissions));
    report_timing_values(
        "path-tile-bounds-applied",
        &mut path_tile_bounds_values,
        "count",
    );
    let mut path_tile_area_values =
        values(|timing| u128::from(timing.sparse_path_tile_bounds_tiles));
    report_timing_values(
        "path-tile-bounds-native-bbox",
        &mut path_tile_area_values,
        "tiles",
    );
    let mut opaque_prune_values =
        values(|timing| u128::from(timing.sparse_coarse_opaque_prune_submissions));
    report_timing_values("opaque-prune-selected", &mut opaque_prune_values, "count");
    let mut solid_fuse_values = values(|timing| u128::from(timing.sparse_solid_fuse_submissions));
    report_timing_values("solid-fuse-selected", &mut solid_fuse_values, "count");
    let mut segment_row_mask_values =
        values(|timing| u128::from(timing.sparse_segment_row_mask_submissions));
    report_timing_values(
        "segment-row-mask-selected",
        &mut segment_row_mask_values,
        "count",
    );
    let mut fine_quad_skip_values =
        values(|timing| u128::from(timing.sparse_fine_quad_skip_submissions));
    report_timing_values(
        "fine-quad-skip-applied",
        &mut fine_quad_skip_values,
        "count",
    );
    let mut fine_quad_total_values =
        values(|timing| u128::from(timing.sparse_fine_quad_total_quads));
    report_timing_values("fine-quad-total", &mut fine_quad_total_values, "groups");
    let mut fine_quad_active_values =
        values(|timing| u128::from(timing.sparse_fine_quad_active_quads));
    report_timing_values("fine-quad-active", &mut fine_quad_active_values, "groups");
    let mut fine_quad_skipped_values =
        values(|timing| u128::from(timing.sparse_fine_quad_skipped_quads));
    report_timing_values("fine-quad-skipped", &mut fine_quad_skipped_values, "groups");
}

fn print_render_timing_profile(
    histo: &[Phase],
    timings: &[OffscreenRenderTiming],
    gpu_timestamp_buffer_bytes: u64,
    gpu_dispatch_timestamp_status: vello::GpuDispatchTimestampStatus,
    gpu_dispatch_timestamp_buffer_bytes: u64,
) {
    let gpu_timestamp_enabled =
        std::env::var("VELLO_GPU_TIMESTAMPS").is_ok_and(|value| value == "1");
    let samples = histo
        .iter()
        .zip(timings)
        .enumerate()
        .filter(|(_, (phase, _))| phase.draw_us > 0 || phase.render_us > 0)
        .collect::<Vec<_>>();
    let values = |f: fn(&Phase, &OffscreenRenderTiming) -> u128| {
        samples
            .iter()
            .map(|(_, (phase, timing))| f(phase, timing))
            .collect::<Vec<_>>()
    };
    println!();
    println!(
        "render timing profile (opt-in host boundaries; dirty-copy encoding joins Vello's render submission only when a copy is required):"
    );
    let mut vello = values(|_, timing| timing.vello_submit_us);
    report_timing_values("vello-submit*", &mut vello, "us");
    let mut copy = values(|_, timing| timing.copy_submit_us);
    report_timing_values("copy-encode", &mut copy, "us");
    let mut damage_setup = values(|_, timing| timing.damage_setup_us);
    report_timing_values("damage-setup", &mut damage_setup, "us");
    let mut scratch_creations = values(|_, timing| u128::from(timing.damage_target_creations));
    report_timing_values("scratch-create", &mut scratch_creations, "count");
    let scratch_creation_total = samples.iter().fold(0_u128, |total, (_, (_, timing))| {
        total.saturating_add(u128::from(timing.damage_target_creations))
    });
    let scratch_creation_frames = samples
        .iter()
        .filter(|(_, (_, timing))| timing.damage_target_creations != 0)
        .count();
    println!(
        "  scratch creation churn: total={scratch_creation_total} frames_with_creations={scratch_creation_frames}/{}",
        samples.len()
    );
    print_sparse_fine_profile(&samples);
    if gpu_timestamp_enabled {
        print_gpu_timestamp_profile(&samples, gpu_timestamp_buffer_bytes);
    }
    if std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS").is_ok_and(|value| value == "1") {
        print_gpu_dispatch_timestamp_profile(
            &samples,
            gpu_dispatch_timestamp_status,
            gpu_dispatch_timestamp_buffer_bytes,
        );
    }
    let mut settle = values(|_, timing| timing.settle_wait_us);
    report_timing_values("settle-wait", &mut settle, "us");
    let mut unaccounted = values(unaccounted_render_us);
    report_timing_values("unaccounted", &mut unaccounted, "us");

    let mut slowest = samples;
    slowest.sort_unstable_by_key(|(_, (phase, _))| std::cmp::Reverse(phase.render_us));
    println!(
        "slowest render samples (event: render/vello/copy/setup/creates/settle/unaccounted us; sparse submits/fine tiles/written/coarse-prefix; complete GPU timestamps show span/last-compute/other-compute; full attempts, fine-shaded pixels):"
    );
    for (index, (phase, timing)) in slowest.into_iter().take(10) {
        let gpu_sample = gpu_timestamp_sample_label(timing, gpu_timestamp_enabled);
        println!(
            "  {index}: {}/{}/{}/{}/{}/{}/{}; sparse={}/{}/{}/{}; gpu={gpu_sample}; {}, {}",
            phase.render_us,
            timing.vello_submit_us,
            timing.copy_submit_us,
            timing.damage_setup_us,
            timing.damage_target_creations,
            timing.settle_wait_us,
            unaccounted_render_us(phase, timing),
            timing.sparse_submissions,
            timing.sparse_fine_tiles,
            timing.sparse_written_pixels,
            timing.coarse_prefix_pixels,
            timing.full_attempts,
            phase.raster_pixels,
        );
    }
    println!(
        "  * Vello submit excludes caller dirty-copy command encoding; the two values sum to the combined submit interval."
    );
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
        "known GPU bytes: {} (output={} damage={} vello_pool={} vello_live={} images={} sparse_upload={}; policy_budget={})",
        memory.known_gpu_bytes(),
        memory.output_target_bytes,
        memory.damage_target_bytes,
        memory.vello.pooled_buffer_bytes,
        memory.vello.live_buffer_bytes,
        memory.vello.image_bytes,
        memory.vello.sparse_upload_bytes,
        memory.vello.bump_budget_bytes,
    );
    println!(
        "CPU readback bytes: {} (must be 0 in timing-only runs)",
        memory.readback_bytes,
    );
    println!(
        "CPU path-tag retained host pool: {}B (excluded from known GPU bytes)",
        state.renderer.vello().cpu_pathtag_pool_retained_bytes(),
    );
    println!(
        "CPU metadata retained host pool: {}B (excluded from known GPU bytes)",
        state.renderer.vello().cpu_metadata_pool_retained_bytes(),
    );
    println!(
        "CPU draw-prefix retained host pool: {}B (excluded from known GPU bytes)",
        state.renderer.vello().cpu_draw_prefix_pool_retained_bytes(),
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

/// Total and count for exactly the redraw population used by frame budgets.
fn redraw_frame_totals(histo: &[Phase]) -> Option<(u128, usize)> {
    let (total_us, redraws) = histo
        .iter()
        .filter(|phase| phase.draw_us > 0 || phase.render_us > 0)
        .fold((0_u128, 0_usize), |(total, count), phase| {
            (
                total.saturating_add(phase.total_us),
                count.saturating_add(1),
            )
        });
    (redraws > 0).then_some((total_us, redraws))
}

/// The redraw population the frame budgets rank, one total in microseconds
/// per line: the samples file `tools/perf/record.py emit` reads.
fn redraw_frame_samples(histo: &[Phase]) -> String {
    histo
        .iter()
        .filter(|phase| phase.draw_us > 0 || phase.render_us > 0)
        .fold(String::new(), |mut samples, phase| {
            let _ = writeln!(samples, "{}", phase.total_us);
            samples
        })
}

/// `TILO_UI_DASH_SAMPLES=<path>`: write every redrawing frame's total, so the
/// scheduled performance lane records the distribution rather than the
/// printed summary.
#[allow(
    clippy::exit,
    reason = "a run that cannot hand its samples to the performance lane must not pass as recorded"
)]
fn write_frame_samples(histo: &[Phase], path: &std::path::Path) {
    let samples = redraw_frame_samples(histo);
    if samples.is_empty() {
        eprintln!(
            "frame-samples: FAIL (no redrawing frames were measured; use animated or scroll mode)"
        );
        std::process::exit(2);
    }
    if let Err(error) = std::fs::write(path, &samples) {
        eprintln!(
            "frame-samples: FAIL (cannot write {}: {error})",
            path.display()
        );
        std::process::exit(2);
    }
    println!(
        "frame-samples: {} redraw totals (us) written to {}",
        samples.lines().count(),
        path.display()
    );
}

/// Whether the exact redraw total is within the requested arithmetic mean.
/// `None` deliberately represents an invalid zero-redraw acceptance sample.
fn mean_frame_budget_passes(histo: &[Phase], budget_us: u128) -> Option<bool> {
    let (total_us, redraws) = redraw_frame_totals(histo)?;
    let redraws = u128::try_from(redraws).unwrap_or(u128::MAX);
    Some(total_us <= budget_us.saturating_mul(redraws))
}

fn parse_mean_frame_budget(raw: Option<&str>) -> Result<Option<u128>, &'static str> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let budget = raw
        .parse::<u128>()
        .ok()
        .filter(|budget| *budget > 0)
        .ok_or("must be a positive integer microsecond budget")?;
    Ok(Some(budget))
}

#[allow(
    clippy::exit,
    reason = "a benchmark budget is an executable acceptance gate for CI and target-hardware runs"
)]
fn enforce_mean_frame_budget(histo: &[Phase], budget_us: u128) {
    let Some((total_us, redraws)) = redraw_frame_totals(histo) else {
        eprintln!(
            "mean-frame-budget: FAIL (no redrawing frames were measured; use animated or scroll mode)"
        );
        std::process::exit(2);
    };
    let sample_count = u128::try_from(redraws).unwrap_or(u128::MAX);
    let mean_us = total_us.checked_div(sample_count).unwrap_or(0);
    // Compare totals, rather than the truncated integer mean, so a fractional
    // microsecond excess cannot pass the acceptance gate.
    let passed = mean_frame_budget_passes(histo, budget_us).unwrap_or(false);
    println!(
        "mean-frame-budget: {} budget={}us mean={}us samples={}",
        if passed { "PASS" } else { "FAIL" },
        budget_us,
        mean_us,
        redraws,
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
    if let Ok(raw) = tilo_ui::env::var("TILO_UI_DASH_RSS_BUDGET_KIB")
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
    if let Ok(raw) = tilo_ui::env::var("TILO_UI_DASH_GPU_BUDGET_BYTES")
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
    if let Ok(raw) = tilo_ui::env::var("TILO_UI_DASH_RSS_GROWTH_BUDGET_KIB")
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
    if let Ok(raw) = tilo_ui::env::var("TILO_UI_DASH_GPU_GROWTH_BUDGET_BYTES")
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
    let traffic_bytes = allocator_traffic_bytes(snapshot.bytes, snapshot.realloc_bytes);
    let traffic_bytes_per_redraw = ceil_bytes_per_redraw(traffic_bytes, total_redraws);
    if total_redraws > 0 {
        println!(
            "allocation redraw rate: calls_per_redraw={calls_per_redraw} allocator_traffic_bytes_per_redraw={traffic_bytes_per_redraw} (new_alloc_bytes={} realloc_bytes={})",
            snapshot.bytes, snapshot.realloc_bytes,
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
        let passed = traffic_byte_redraw_budget_passes(traffic_bytes, total_redraws, budget);
        println!(
            "allocation-traffic-byte-redraw-budget: {} budget={}B/redraw observed_ceiling={}B/redraw total_traffic={}B redraws={}",
            if passed { "PASS" } else { "FAIL" },
            budget,
            traffic_bytes_per_redraw,
            traffic_bytes,
            total_redraws,
        );
        if !passed {
            std::process::exit(2);
        }
    }
}

fn allocator_traffic_bytes(new_allocation_bytes: u64, reallocation_bytes: u64) -> u128 {
    u128::from(new_allocation_bytes).saturating_add(u128::from(reallocation_bytes))
}

fn ceil_bytes_per_redraw(total_bytes: u128, total_redraws: usize) -> u128 {
    let redraws = u128::try_from(total_redraws).unwrap_or(u128::MAX);
    if redraws == 0 {
        return 0;
    }
    total_bytes
        .saturating_add(redraws.saturating_sub(1))
        .checked_div(redraws)
        .unwrap_or(0)
}

fn traffic_byte_redraw_budget_passes(
    total_traffic_bytes: u128,
    total_redraws: usize,
    budget_per_redraw: u64,
) -> bool {
    let redraws = u128::try_from(total_redraws).unwrap_or(u128::MAX);
    redraws > 0 && total_traffic_bytes <= u128::from(budget_per_redraw).saturating_mul(redraws)
}

#[allow(
    clippy::exit,
    clippy::too_many_lines,
    reason = "scenario setup intentionally remains linear so every acceptance setting is visible in one place; a failed verifier must return a nonzero benchmark status"
)]
pub fn run(_args: &[String]) {
    init_i18n_resources();

    let verify_damage = tilo_ui::env::var("TILO_UI_DASH_VERIFY").is_ok_and(|value| value == "1");
    let gpu_timestamp_profile =
        std::env::var("VELLO_GPU_TIMESTAMPS").is_ok_and(|value| value == "1");
    let gpu_dispatch_timestamp_profile =
        std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS").is_ok_and(|value| value == "1");
    let sparse_fine_configured = env_enabled_by_default("VELLO_SPARSE_FINE");
    let compute_batch_configured = env_enabled_by_default("VELLO_COMPUTE_BATCH");
    let sparse_coarse_configured =
        std::env::var("VELLO_SPARSE_COARSE").is_ok_and(|value| value == "1");
    let sparse_coarse_auto_configured =
        std::env::var("VELLO_SPARSE_COARSE_AUTO").is_ok_and(|value| value == "1");
    let sparse_tile_bounds_requested =
        std::env::var("VELLO_SPARSE_TILE_BOUNDS").is_ok_and(|value| value == "1");
    let sparse_fine_quad_skip_requested =
        std::env::var("VELLO_SPARSE_FINE_QUAD_SKIP").is_ok_and(|value| value == "1");
    let sparse_opaque_prune_requested =
        std::env::var("VELLO_SPARSE_OPAQUE_PRUNE").is_ok_and(|value| value == "1");
    let sparse_solid_fuse_requested =
        std::env::var("VELLO_SPARSE_SOLID_FUSE").is_ok_and(|value| value == "1");
    let sparse_segment_row_mask_requested =
        std::env::var("VELLO_SPARSE_SEGMENT_ROW_MASK").is_ok_and(|value| value == "1");
    let canonical_segment_order_requested =
        std::env::var("VELLO_CANONICAL_SEGMENT_ORDER").is_ok_and(|value| value == "1");
    let area_ppt8_requested = std::env::var("VELLO_AREA_PPT8").is_ok_and(|value| value == "1");
    let clip_single_group_requested =
        std::env::var("VELLO_CLIP_SINGLE_GROUP").is_ok_and(|value| value == "1");
    let cpu_pathtag_configured = env_enabled_by_default("VELLO_CPU_PATHTAG");
    let cpu_pathtag_reuse_configured = env_enabled_by_default("VELLO_CPU_PATHTAG_REUSE");
    let cpu_metadata_configured = env_enabled_by_default("VELLO_CPU_METADATA");
    let cpu_draw_prefix_configured = env_enabled_by_default("VELLO_CPU_DRAW_PREFIX");
    let sparse_upload_reuse_configured = env_enabled_by_default("VELLO_SPARSE_UPLOAD_REUSE");
    let direct_path_dispatch_configured = env_enabled_by_default("VELLO_DIRECT_PATH_DISPATCH");
    let defer_bump_readback_configured =
        std::env::var("VELLO_DEFER_BUMP_READBACK").is_ok_and(|value| value == "1");
    if !dispatch_timestamp_configuration_is_valid(
        gpu_dispatch_timestamp_profile,
        gpu_timestamp_profile,
    ) {
        eprintln!("VELLO_GPU_DISPATCH_TIMESTAMPS=1 requires VELLO_GPU_TIMESTAMPS=1");
        std::process::exit(2);
    }
    if verify_damage {
        reject_budgets_in_diagnostic_mode("damage-verify", true);
        if gpu_timestamp_profile {
            eprintln!(
                "damage-verify: VELLO_GPU_TIMESTAMPS=1 is incompatible with image verification mode"
            );
            std::process::exit(2);
        }
    }
    if gpu_timestamp_profile {
        reject_budgets_in_diagnostic_mode("gpu-timestamp-profile", false);
    }
    let profile_render_timing = gpu_timestamp_profile
        || tilo_ui::env::var_os("TILO_UI_DASH_RENDER_PROFILE").is_some_and(|raw| !raw.is_empty());

    let iters: usize = tilo_ui::env::var("TILO_UI_DASH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    let static_mode = tilo_ui::env::var("TILO_UI_DASH_STATIC").is_ok();
    let include_animated = !static_mode;
    let canvas_w = env_u32("TILO_UI_DASH_WIDTH", 1800);
    let canvas_h = env_u32("TILO_UI_DASH_HEIGHT", 1200);
    println!(
        "mode: {}",
        if static_mode {
            "static (no spinners / indeterminate bars — gate-skipped events should dominate)"
        } else {
            "animated (header contains spinners + indeterminate progress)"
        }
    );
    if verify_damage {
        println!(
            "damage-verify mode: retained/full pixel parity preflight only; \
             reference explicitly disables batching, sparse, direct, and selective CPU paths; timing/budget reports are suppressed"
        );
    } else if gpu_timestamp_profile {
        println!(
            "GPU timestamp profile enabled (profile-only query buffers and resolve/readback overhead; acceptance budgets are rejected)"
        );
    }

    println!("output target: {canvas_w}x{canvas_h} (native; 1:1 damage raster, no upscale)");
    let mut renderer = OffscreenRenderer::new(canvas_w, canvas_h);
    match renderer.adapter_info() {
        Some(adapter) => println!(
            "UI renderer adapter: name={:?} backend={:?} device_type={:?} driver={:?}",
            adapter.name, adapter.backend, adapter.device_type, adapter.driver
        ),
        None => println!(
            "UI renderer adapter: unavailable (selected shared render context is inaccessible)"
        ),
    }
    let gpu_dispatch_timestamp_status = renderer.vello_mut().gpu_dispatch_timestamp_status();
    println!(
        "sparse fine: configured={} supported={} (raster metric is actual fine-shaded native pixels; profile retains coarse-prefix accounting)",
        sparse_fine_configured,
        renderer.vello().sparse_fine_supported(),
    );
    println!(
        "compute-pass batching: configured={compute_batch_configured} active={} (default shared GPU batching; VELLO_COMPUTE_BATCH=0 disables it)",
        renderer.vello().compute_pass_batching_enabled(),
    );
    println!(
        "sparse coarse: forced={sparse_coarse_configured} auto={sparse_coarse_auto_configured} supported={} adaptive_active={} active_bin_available={} (AUTO requires no forced coarse flag and selects active native bins only when coverage saves at least half and 32 bins; otherwise coarse remains dense)",
        renderer.vello().sparse_coarse_supported(),
        renderer.vello().sparse_coarse_adaptive_enabled(),
        renderer.vello().sparse_coarse_active_bins_available(),
    );
    println!(
        "sparse path tile bounds: env_requested={sparse_tile_bounds_requested} renderer_requested={} available={} (reported bbox tile area is only actual applied coverage; it is not allocated per-path tiles or physical memory saved)",
        renderer.vello().sparse_tile_bounds_requested(),
        renderer.vello().sparse_tile_bounds_available(),
    );
    println!(
        "sparse fine quad skip: env_requested={sparse_fine_quad_skip_requested} renderer_requested={} available={} (reported groups are actual Area fine invocations, not GPU memory or copy savings)",
        renderer.vello().sparse_fine_quad_skip_requested(),
        renderer.vello().sparse_fine_quad_skip_available(),
    );
    println!(
        "sparse opaque prune: env_requested={sparse_opaque_prune_requested} renderer_requested={} available={} (reported selections mean only that the coarse shader was selected; they do not count tiles or commands pruned, GPU memory, or copy savings)",
        renderer.vello().sparse_opaque_prune_requested(),
        renderer.vello().sparse_opaque_prune_available(),
    );
    println!(
        "sparse solid fuse: env_requested={sparse_solid_fuse_requested} renderer_requested={} available={} (reported selections mean only that the matched producer/consumer shader path was selected; they do not count fused commands, GPU memory, or copy savings)",
        renderer.vello().sparse_solid_fuse_requested(),
        renderer.vello().sparse_solid_fuse_available(),
    );
    println!(
        "sparse segment row mask: env_requested={sparse_segment_row_mask_requested} renderer_requested={} available={} (reported selections mean only that the paired sparse shader path was selected; they do not count skipped rows, GPU memory, or copy savings)",
        renderer.vello().sparse_segment_row_mask_requested(),
        renderer.vello().sparse_segment_row_mask_available(),
    );
    println!(
        "canonical segment order: env_requested={canonical_segment_order_requested} renderer_requested={} available={} (selected attempts report only an actual recorded pass, not completion or speed)",
        renderer.vello().canonical_segment_order_requested(),
        renderer.vello().canonical_segment_order_available(),
    );
    println!(
        "area PPT8: env_requested={area_ppt8_requested} renderer_requested={} available={} (selected attempts report only an actual recorded Area shader pass, not completion or speed)",
        renderer.vello().area_ppt8_requested(),
        renderer.vello().area_ppt8_available(),
    );
    println!(
        "clip single group: env_requested={clip_single_group_requested} renderer_requested={} available={} (selected attempts report only an actual recorded clip shader pass, not completion or speed)",
        renderer.vello().clip_single_group_requested(),
        renderer.vello().clip_single_group_available(),
    );
    println!(
        "CPU path tags: configured={cpu_pathtag_configured} active={} (only path-tag reduce/scan use CPU; flatten and later stages remain GPU, with CPU tag-monoid upload retained)",
        renderer.vello().cpu_pathtag_enabled(),
    );
    println!(
        "CPU path-tag reuse: configured={cpu_pathtag_reuse_configured} active={} retained_host_pool={}B (excluded from known GPU bytes)",
        renderer.vello().cpu_pathtag_reuse_enabled(),
        renderer.vello().cpu_pathtag_pool_retained_bytes(),
    );
    println!(
        "CPU metadata: configured={cpu_metadata_configured} active={} retained_host_pool={}B (bbox-clear and draw-reduce only; flatten and later stages remain GPU)",
        renderer.vello().cpu_metadata_enabled(),
        renderer.vello().cpu_metadata_pool_retained_bytes(),
    );
    println!(
        "CPU draw prefix: configured={cpu_draw_prefix_configured} active={} retained_host_pool={}B (bounded draw-prefix only; GPU draw leaf consumes the same GPU monoid table)",
        renderer.vello().cpu_draw_prefix_enabled(),
        renderer.vello().cpu_draw_prefix_pool_retained_bytes(),
    );
    println!(
        "sparse upload reuse: configured={} active={} persistent_gpu_bytes={} (before the first sparse upload)",
        sparse_upload_reuse_configured,
        renderer.vello().sparse_upload_reuse_enabled(),
        renderer.memory_stats().vello.sparse_upload_bytes,
    );
    println!(
        "deferred bump readback: configured={defer_bump_readback_configured} active={} (moves the same synchronous bump download after fine; it does not eliminate measurement, recovery, or waits)",
        renderer.vello().defer_bump_readback_enabled(),
    );
    println!(
        "direct path dispatch: configured={direct_path_dispatch_configured} active={} (replaces GPU indirect path-count/path-tiling setup; it does not remove CPU readback)",
        renderer.vello().direct_path_dispatch_enabled(),
    );
    println!(
        "geometric damage capacity: configured={} active={} (default on; TILO_UI_DAMAGE_GEOMETRIC=0 restores fixed-width capacity)",
        env_enabled_by_default("TILO_UI_DAMAGE_GEOMETRIC"),
        renderer.damage_geometric_capacity_enabled(),
    );
    println!(
        "gpu dispatch timestamps: requested={gpu_dispatch_timestamp_profile} status={gpu_dispatch_timestamp_status:?} (requires VELLO_GPU_TIMESTAMPS=1; diagnostic-only)",
    );
    let mut verifier =
        verify_damage.then(|| DamageVerifier::new(canvas_w, canvas_h, verification_frame_count()));
    let theme = Theme::dark();
    let mut text_renderer = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut doc = AutoCommit::new();

    // Damage verification compares every frame it asks for, so there each
    // event moves the indicators one step and redraws.
    let clock = FrameClock::new(if verify_damage {
        tilo_ui::animation::INDICATOR_STEP
    } else {
        EVENT_INTERVAL
    });
    let mut root = clock.pin(|| build_tree(include_animated));
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = clock.pin(|| root.build_layout(&mut taffy, &theme, &mut node_map));
    let mut scene = Scene::new();
    let mut damage_regions = Vec::with_capacity(8);

    let tree_size = count_widgets(root.as_ref());
    println!("dashboard widget tree: {tree_size} widgets");
    let mut peer = PeerBench::from_env();
    if let Some(peer) = peer.as_ref() {
        println!(
            "peer: a collaborator's pointer moves on every event ({})",
            if peer.full_repaint {
                "full repaint while visible, the rule before #3936"
            } else {
                "presence-layer damage"
            }
        );
    }

    let mut state = BenchState {
        root: &mut root,
        taffy: &mut taffy,
        node_map: &node_map,
        root_node,
        text_renderer: &mut text_renderer,
        presence: &presence,
        theme: &theme,
        scene: &mut scene,
        renderer: &mut renderer,
        damage_regions: &mut damage_regions,
        canvas_w,
        canvas_h,
        peer: peer.as_mut(),
        clock,
    };

    warm_up(&mut state);
    print_warmup_report(&state);

    // FROZEN — identical no-op CursorMoved every event. Proves
    // the Unit 1+2 gate skips when nothing in the tree mutates.
    // SCROLL — fire a wheel event every iteration; exercises the
    // translation-invariant cache + Scrollable's scroll-dirty
    // signal, which must keep scrolling smooth.
    let frozen = tilo_ui::env::var("TILO_UI_DASH_FROZEN").is_ok();
    let scroll_mode = tilo_ui::env::var("TILO_UI_DASH_SCROLL").is_ok();
    if frozen {
        prime_frozen_cursor(&mut state, &mut doc);
    }

    let allocation_budget = tilo_ui::env::var("TILO_UI_DASH_ALLOC_BUDGET_PER_1000")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok());
    let allocation_call_redraw_budget =
        tilo_ui::env::var("TILO_UI_DASH_ALLOC_CALL_BUDGET_PER_REDRAW")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok());
    let allocation_byte_redraw_budget =
        tilo_ui::env::var("TILO_UI_DASH_ALLOC_BYTE_BUDGET_PER_REDRAW")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok());
    let profile_allocation_breakdown = allocation_call_redraw_budget.is_some()
        || allocation_byte_redraw_budget.is_some()
        || tilo_ui::env::var_os("TILO_UI_DASH_ALLOC_BREAKDOWN").is_some_and(|raw| !raw.is_empty());
    let profile_allocations = allocation_budget.is_some()
        || profile_allocation_breakdown
        || tilo_ui::env::var_os("TILO_UI_DASH_ALLOC_PROFILE").is_some_and(|raw| !raw.is_empty());
    let frame_breakdown =
        tilo_ui::env::var_os("TILO_UI_DASH_FRAME_BREAKDOWN").is_some_and(|raw| !raw.is_empty());
    // One pass of the event mix, unmeasured: every state it reaches (each
    // segment chosen, the thumb on its way, a bar wrapping around) has been
    // drawn and rendered once, so the measured loop counts what each redraw
    // allocates, not a buffer's one-time growth to the largest of them. On
    // the wall clock whether that growth landed in the measured loop
    // depended on which states the node's pace happened to reach there.
    let _ = run_bench_loop(
        &mut state,
        &mut doc,
        WARM_EVENTS.min(iters),
        BenchLoopOptions {
            scroll_mode,
            frozen,
            profile_allocations: false,
            profile_allocation_breakdown: false,
            profile_render_timing: false,
        },
        None,
    );
    let run = run_bench_loop(
        &mut state,
        &mut doc,
        iters,
        BenchLoopOptions {
            scroll_mode,
            frozen,
            profile_allocations,
            profile_allocation_breakdown,
            profile_render_timing,
        },
        verifier.as_mut(),
    );

    if let Some(verifier) = verifier.as_ref() {
        println!(
            "damage-verify mode completed; no benchmark timings or acceptance budgets are reported"
        );
        if !verifier.finish() {
            std::process::exit(2);
        }
        return;
    }

    print_final_report(
        &state,
        &run.histo,
        iters,
        run.total_redraws,
        run.total_skipped,
        run.resources_before,
        run.resources_after,
    );
    println!(
        "sparse submission accounting (measured workload): total={} active={} path_tile_bounds_applied={} path_tile_bounds_native_bbox_tiles={} opaque_prune_shader_selected={} solid_fuse_shader_selected={} segment_row_mask_shader_selected={} canonical_segment_order_shader_selected_sparse_attempts={} area_ppt8_shader_selected={} clip_single_group_shader_selected={} fine_quad_skip_applied={} fine_quad_total_invocation_groups={} fine_quad_active_invocation_groups={} fine_quad_skipped_invocation_groups={} (opaque prune, solid fuse, segment row mask, canonical segment order, Area PPT8, and clip single group are shader selections only; bbox tile area and fine quad groups are diagnostics, not tiles or commands pruned, fused, row-skipped, allocated tiles, GPU memory, copy savings, completion, or speed)",
        run.sparse_submission_accounting.sparse_submissions,
        run.sparse_submission_accounting.active_sparse_submissions,
        run.sparse_submission_accounting
            .path_tile_bounds_submissions,
        run.sparse_submission_accounting.path_tile_bounds_tiles,
        run.sparse_submission_accounting
            .coarse_opaque_prune_selected_submissions,
        run.sparse_submission_accounting
            .solid_fuse_selected_submissions,
        run.sparse_submission_accounting
            .segment_row_mask_selected_submissions,
        run.sparse_submission_accounting
            .canonical_segment_order_selected_sparse_attempts,
        run.sparse_submission_accounting
            .area_ppt8_selected_sparse_attempts,
        run.sparse_submission_accounting
            .clip_single_group_shader_selected,
        run.sparse_submission_accounting.fine_quad_skip_submissions,
        run.sparse_submission_accounting.fine_quad_total_quads,
        run.sparse_submission_accounting.fine_quad_active_quads,
        run.sparse_submission_accounting.fine_quad_skipped_quads,
    );
    println!(
        "canonical segment order shader-selected attempts: full={} sparse={} total={}",
        run.canonical_segment_order_attempt_accounting
            .full_selected_attempts,
        run.canonical_segment_order_attempt_accounting
            .sparse_selected_attempts,
        run.canonical_segment_order_attempt_accounting
            .full_selected_attempts
            .saturating_add(
                run.canonical_segment_order_attempt_accounting
                    .sparse_selected_attempts,
            ),
    );
    println!(
        "area PPT8 shader-selected attempts: full={} sparse={} total={}",
        run.area_ppt8_attempt_accounting.full_selected_attempts,
        run.area_ppt8_attempt_accounting.sparse_selected_attempts,
        run.area_ppt8_attempt_accounting
            .full_selected_attempts
            .saturating_add(run.area_ppt8_attempt_accounting.sparse_selected_attempts),
    );
    println!(
        "clip single group shader-selected attempts: full={} sparse={} total={}",
        run.clip_single_group_attempt_accounting
            .full_selected_attempts,
        run.clip_single_group_attempt_accounting
            .sparse_selected_attempts,
        run.clip_single_group_attempt_accounting
            .full_selected_attempts
            .saturating_add(
                run.clip_single_group_attempt_accounting
                    .sparse_selected_attempts
            ),
    );
    if frame_breakdown {
        let full_pixels = u64::from(canvas_w).saturating_mul(u64::from(canvas_h));
        print_frame_breakdown(&run.histo, full_pixels);
    }
    if let Some(timings) = run.render_timings.as_deref() {
        print_render_timing_profile(
            &run.histo,
            timings,
            state.renderer.vello().gpu_timestamp_buffer_bytes(),
            gpu_dispatch_timestamp_status,
            state.renderer.vello().gpu_dispatch_timestamp_buffer_bytes(),
        );
    }
    if let Some(path) = tilo_ui::env::var_os("TILO_UI_DASH_SAMPLES").filter(|raw| !raw.is_empty()) {
        write_frame_samples(&run.histo, std::path::Path::new(&path));
    }
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

    if let Ok(raw_budget) = tilo_ui::env::var("TILO_UI_DASH_FRAME_BUDGET_US")
        && let Ok(budget_us) = raw_budget.parse::<u128>()
        && budget_us > 0
    {
        enforce_frame_budget(&run.histo, budget_us);
    }
    let mean_frame_budget = match tilo_ui::env::var_os("TILO_UI_DASH_MEAN_FRAME_BUDGET_US") {
        None => Ok(None),
        Some(raw) => match raw.to_str() {
            Some(raw) => parse_mean_frame_budget(Some(raw)),
            None => Err("must be valid Unicode"),
        },
    };
    match mean_frame_budget {
        Ok(Some(budget_us)) => enforce_mean_frame_budget(&run.histo, budget_us),
        Ok(None) => {}
        Err(reason) => {
            eprintln!("TILO_UI_DASH_MEAN_FRAME_BUDGET_US: {reason}");
            std::process::exit(2);
        }
    }
    if let Ok(raw_budget) = tilo_ui::env::var("TILO_UI_DASH_EVENT_BUDGET_US")
        && let Ok(budget_us) = raw_budget.parse::<u128>()
        && budget_us > 0
    {
        enforce_event_budget(&run.histo, budget_us);
    }
    if let Ok(raw_budget) = tilo_ui::env::var("TILO_UI_DASH_RASTER_PIXEL_BUDGET")
        && let Ok(budget) = raw_budget.parse::<u128>()
        && budget > 0
    {
        enforce_raster_pixel_budget(&run.histo, run.total_redraws, budget);
    }
    enforce_resource_budgets(state.renderer, run.resources_before, run.resources_after);
}
