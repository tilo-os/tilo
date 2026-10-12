//! Native-resolution shell redraw benchmark.
//!
//! This is intentionally separate from `draw_bench`: it drives real input through
//! [`FrameBuilder`], lays out and paints the resulting scene, then uses the DRM bridge's
//! production single-output path. Every one of the 1,000 samples includes Vello completion,
//! texture readback, and the RGBA-to-XRGB swizzle into one reused dumb-buffer-shaped allocation.
//! An initial production paint establishes the already-visible desktop needed to resolve named
//! input targets; it is session setup, not one of the 1,000 action-triggered redraw samples.
//!
//! It does *not* own a DRM/KMS device, so it cannot time a physical page flip or display scanout.
//! That unavailable presentation latency is reported separately and is not folded into either
//! gate. The default numbers are consequently a valid DRM render/readback measurement, not a
//! claim about end-to-end display latency.
//!
//! Run with:
//!
//! ```text
//! cargo run -p shell --example redraw_bench --release
//! ```

#[path = "support/populated_shell_fixture.rs"]
mod populated_shell_fixture;

use std::array;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use automerge::{AutoCommit, ROOT};
use populated_shell_fixture::PopulatedShellFixture;
use shell::compositor::backend::drm::paint::{
    BridgePhaseTiming, SceneDamage, VelloDrmBridge, XrgbCpuWriteMode, XrgbHistoryCommit,
    XrgbTargetHistory,
};
use shell::compositor::frame::FrameBuilder;
use ui::taffy::NodeId;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

const WIDTH: u32 = 3_840;
const HEIGHT: u32 = 2_160;
const SAMPLE_COUNT: usize = 1_000;
const MEAN_LIMIT_US: u128 = 25_000;
const P99_LIMIT_US: u128 = 50_000;
const ACTION_TARGETS: [&str; 2] = ["topbar_tray_pill", "topbar_overview_button"];
const PROFILE_ENV: &str = "TILO_REDRAW_BENCH_PROFILE";
const PROFILE_OUTPUT_ENV: &str = "TILO_REDRAW_BENCH_PROFILE_OUTPUT";
const SAMPLES_ENV: &str = "TILO_REDRAW_BENCH_SAMPLES";
const SCENE_REUSE_ENV: &str = "TILO_REDRAW_BENCH_SCENE_REUSE";
const GEOMETRY_REUSE_ENV: &str = "TILO_REDRAW_BENCH_GEOMETRY_REUSE";
const XRGB_DAMAGE_ENV: &str = "TILO_DRM_XRGB_DAMAGE";
const VELLO_GPU_TIMESTAMPS_ENV: &str = "VELLO_GPU_TIMESTAMPS";
const VELLO_GPU_DISPATCH_TIMESTAMPS_ENV: &str = "VELLO_GPU_DISPATCH_TIMESTAMPS";
const MAX_SCENE_REUSE_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCENE_REUSE_SNAPSHOTS: usize = 4;
const MAX_SCENE_REUSE_RETAINED_BYTES: usize =
    MAX_SCENE_REUSE_PAYLOAD_BYTES * MAX_SCENE_REUSE_SNAPSHOTS;
const MAX_SCENE_REUSE_DAMAGE_RECTS: usize = 64;
const MAX_ACTION_CORRELATION_RECORDS: usize = SAMPLE_COUNT;
const GPU_COARSE_STAGE_IDS: [u16; 3] = [17, 24, 28];
const GPU_FINE_STAGE_IDS: [u16; 4] = [20, 21, 22, 23];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimingSummary {
    count: usize,
    total_us: u128,
    mean_us: u128,
    p99_us: u128,
    min_us: u128,
    max_us: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SampleError {
    Empty,
    Missing { index: usize },
    Zero { index: usize },
    TotalOverflow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BenchmarkPhaseTotals {
    input_us: u128,
    registry_us: u128,
    layout_us: u128,
    scene_us: u128,
    submit_us: u128,
    completed_readback_map_us: u128,
    settle_us: u128,
    cpu_copy_swizzle_us: u128,
    readback_bytes: u128,
    cpu_output_bytes: u128,
}

/// Actual complete versus bounded CPU destination writes in the measured action loop.
///
/// This is bookkeeping over validated whole/rectangle extents, not a per-pixel counter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CpuXrgbWriteTotals {
    full: u64,
    partial: u64,
    bytes: u128,
}

impl CpuXrgbWriteTotals {
    fn record_full(&mut self, bytes: usize) -> Result<(), String> {
        self.full = self.full.saturating_add(1);
        self.bytes = self.bytes.saturating_add(
            u128::try_from(bytes).map_err(|_| "XRGB byte count overflow".to_owned())?,
        );
        Ok(())
    }

    fn record_history_write(&mut self, mode: XrgbCpuWriteMode, bytes: u64) {
        match mode {
            XrgbCpuWriteMode::Full => self.full = self.full.saturating_add(1),
            XrgbCpuWriteMode::Partial => self.partial = self.partial.saturating_add(1),
        }
        self.bytes = self.bytes.saturating_add(u128::from(bytes));
    }

    fn total(self) -> u64 {
        self.full.saturating_add(self.partial)
    }
}

/// Final measured-workload diagnostics, grouped so the benchmark handoff remains auditable
/// without an ever-growing `finish_run` argument list.
struct FinishDiagnostics<'a> {
    render_attempts: RenderAttemptTotals,
    phase_totals: BenchmarkPhaseTotals,
    gpu_dispatch_timestamp_profile: Option<&'a GpuDispatchTimestampProfile>,
    action_correlations: Option<&'a ActionCorrelationProfile>,
    profile_output_path: Option<&'a Path>,
    samples_output_path: Option<&'a Path>,
    scene_reuse: Option<&'a SceneReuseTelemetry>,
    geometry_reuse: Option<&'a GeometryReuseTelemetry>,
    xrgb_writes: CpuXrgbWriteTotals,
}

impl BenchmarkPhaseTotals {
    fn add_sample(&mut self, sample: BenchmarkPhaseTiming) {
        self.input_us = self.input_us.saturating_add(sample.input_us);
        self.registry_us = self.registry_us.saturating_add(sample.registry_us);
        self.layout_us = self.layout_us.saturating_add(sample.layout_us);
        self.scene_us = self.scene_us.saturating_add(sample.scene_us);
        self.submit_us = self.submit_us.saturating_add(sample.bridge.submit_us);
        self.completed_readback_map_us = self
            .completed_readback_map_us
            .saturating_add(sample.bridge.completed_readback_map_us);
        self.settle_us = self.settle_us.saturating_add(sample.bridge.settle_us);
        self.cpu_copy_swizzle_us = self
            .cpu_copy_swizzle_us
            .saturating_add(sample.bridge.cpu_copy_swizzle_us);
        self.readback_bytes = self
            .readback_bytes
            .saturating_add(u128::from(sample.bridge.readback_bytes));
        self.cpu_output_bytes = self
            .cpu_output_bytes
            .saturating_add(u128::from(sample.bridge.cpu_output_bytes));
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BenchmarkPhaseTiming {
    input_us: u128,
    registry_us: u128,
    layout_us: u128,
    scene_us: u128,
    bridge: BridgePhaseTiming,
}

/// One profile-only action record, joined after the same elapsed wall sample was captured.
///
/// CPU bridge phases span every bounded retry. GPU timestamp totals apply only to the exactly
/// matched final submission, so recovery actions never imply complete per-attempt GPU coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActionCorrelationRecord {
    index: usize,
    target: &'static str,
    wall_us: u128,
    phase: BenchmarkPhaseTiming,
    attempts: u64,
    sparse_attempts: u64,
    full_attempts: u64,
    recovery_attempts: u64,
    sparse_stats: Option<vello::SparseRenderStats>,
    timestamp: ActionGpuTimestampObservation,
}

/// Exact state of the existing bounded dispatch-timestamp slot for one action.
///
/// Stage values are populated only for an exact submission-id match. They are deliberately
/// absent, rather than zero-filled, for missing, stale, or invalid samples. `unknown_stage_ns`
/// is a subset of `total_timed_stage_ns`; `other_stage_ns` excludes known coarse/fine and
/// unknown stage IDs so the four duration classes remain auditable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActionGpuTimestampObservation {
    status: vello::GpuDispatchTimestampStatus,
    expected_submission_id: Option<u64>,
    observed_submission_id: Option<u64>,
    exact_match: bool,
    sample_missing: bool,
    stale_sample: bool,
    coarse_ns: Option<u128>,
    fine_ns: Option<u128>,
    total_timed_stage_ns: Option<u128>,
    other_stage_ns: Option<u128>,
    unknown_stage_ns: Option<u128>,
    coarse_dispatches: Option<u128>,
    fine_dispatches: Option<u128>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DispatchStageBreakdown {
    total_timed_stage_ns: u128,
    coarse_ns: u128,
    fine_ns: u128,
    other_stage_ns: u128,
    unknown_stage_ns: u128,
    coarse_dispatches: u128,
    fine_dispatches: u128,
}

impl ActionGpuTimestampObservation {
    fn from_sample(
        expected_submission_id: Option<u64>,
        status: vello::GpuDispatchTimestampStatus,
        sample: Option<&vello::GpuDispatchTimestampSample>,
    ) -> Self {
        let observed_submission_id = sample.map(|sample| sample.submission_id);
        let exact_match = expected_submission_id
            .zip(observed_submission_id)
            .is_some_and(|(expected, observed)| expected == observed);
        let sample_missing = expected_submission_id.is_some() && observed_submission_id.is_none();
        let stale_sample =
            expected_submission_id.is_some() && observed_submission_id.is_some() && !exact_match;
        let breakdown = sample.filter(|_| exact_match).map(dispatch_stage_breakdown);
        Self {
            status,
            expected_submission_id,
            observed_submission_id,
            exact_match,
            sample_missing,
            stale_sample,
            coarse_ns: breakdown.map(|breakdown| breakdown.coarse_ns),
            fine_ns: breakdown.map(|breakdown| breakdown.fine_ns),
            total_timed_stage_ns: breakdown.map(|breakdown| breakdown.total_timed_stage_ns),
            other_stage_ns: breakdown.map(|breakdown| breakdown.other_stage_ns),
            unknown_stage_ns: breakdown.map(|breakdown| breakdown.unknown_stage_ns),
            coarse_dispatches: breakdown.map(|breakdown| breakdown.coarse_dispatches),
            fine_dispatches: breakdown.map(|breakdown| breakdown.fine_dispatches),
        }
    }
}

/// Bounded all-action correlation storage, allocated only for explicit profile runs.
#[derive(Debug)]
struct ActionCorrelationProfile {
    records: Vec<ActionCorrelationRecord>,
}

impl ActionCorrelationProfile {
    fn new() -> Self {
        Self {
            records: Vec::with_capacity(MAX_ACTION_CORRELATION_RECORDS),
        }
    }

    fn record(&mut self, record: &ActionCorrelationRecord) -> Result<(), String> {
        if self.records.len() >= MAX_ACTION_CORRELATION_RECORDS {
            return Err("diagnostic action-correlation record capacity exceeded".to_owned());
        }
        self.records.push(*record);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RenderAttemptTotals {
    total: u64,
    sparse: u64,
    full: u64,
    recovery: u64,
    adaptive_sparse: u64,
    active_sparse: u64,
    dense_fallback_sparse: u64,
    coarse_native_bins: u128,
    coarse_selected_native_bins: u128,
    path_tile_bounds_sparse: u64,
    path_tile_bounds_tiles: u128,
    coarse_opaque_prune_selected_sparse: u64,
    solid_fuse_selected_sparse: u64,
    segment_row_mask_selected_sparse: u64,
    canonical_segment_order_selected_full: u64,
    canonical_segment_order_selected_sparse: u64,
    area_ppt8_selected_full: u64,
    area_ppt8_selected_sparse: u64,
    clip_single_group_selected_full: u64,
    clip_single_group_selected_sparse: u64,
    fine_quad_skip_sparse: u64,
    fine_quad_total: u128,
    fine_quad_active: u128,
    fine_quad_skipped: u128,
}

impl RenderAttemptTotals {
    fn record_canonical_segment_order_attempts(&mut self, full: u64, sparse: u64) {
        self.canonical_segment_order_selected_full = self
            .canonical_segment_order_selected_full
            .saturating_add(full);
        self.canonical_segment_order_selected_sparse = self
            .canonical_segment_order_selected_sparse
            .saturating_add(sparse);
    }

    fn record_area_ppt8_attempts(&mut self, full: u64, sparse: u64) {
        self.area_ppt8_selected_full = self.area_ppt8_selected_full.saturating_add(full);
        self.area_ppt8_selected_sparse = self.area_ppt8_selected_sparse.saturating_add(sparse);
    }

    fn record_clip_single_group_attempts(&mut self, full: u64, sparse: u64) {
        self.clip_single_group_selected_full =
            self.clip_single_group_selected_full.saturating_add(full);
        self.clip_single_group_selected_sparse = self
            .clip_single_group_selected_sparse
            .saturating_add(sparse);
    }

    /// Account only for the sparse submission that returned these stats. A dense retry after a
    /// sparse overflow remains a dense recovery and must never be reported as adaptive sparse.
    fn record_sparse_stats(&mut self, stats: vello::SparseRenderStats) {
        if stats.coarse_active_bins {
            self.active_sparse = self.active_sparse.saturating_add(1);
        }
        if stats.coarse_adaptive {
            self.adaptive_sparse = self.adaptive_sparse.saturating_add(1);
            if !stats.coarse_active_bins {
                self.dense_fallback_sparse = self.dense_fallback_sparse.saturating_add(1);
            }
        }
        self.coarse_native_bins = self
            .coarse_native_bins
            .saturating_add(u128::from(stats.coarse_native_bins));
        self.coarse_selected_native_bins = self
            .coarse_selected_native_bins
            .saturating_add(u128::from(stats.coarse_selected_native_bins));
        if stats.path_tile_bounds_applied {
            self.path_tile_bounds_sparse = self.path_tile_bounds_sparse.saturating_add(1);
            self.path_tile_bounds_tiles = self
                .path_tile_bounds_tiles
                .saturating_add(u128::from(stats.path_tile_bounds_tiles));
        }
        if stats.coarse_opaque_prune_selected {
            self.coarse_opaque_prune_selected_sparse =
                self.coarse_opaque_prune_selected_sparse.saturating_add(1);
        }
        if stats.solid_fuse_selected {
            self.solid_fuse_selected_sparse = self.solid_fuse_selected_sparse.saturating_add(1);
        }
        if stats.segment_row_mask_selected {
            self.segment_row_mask_selected_sparse =
                self.segment_row_mask_selected_sparse.saturating_add(1);
        }
        if stats.fine_quad_skip_applied {
            self.fine_quad_skip_sparse = self.fine_quad_skip_sparse.saturating_add(1);
            self.fine_quad_total = self
                .fine_quad_total
                .saturating_add(u128::from(stats.fine_quad_total_quads));
            self.fine_quad_active = self
                .fine_quad_active
                .saturating_add(u128::from(stats.fine_quad_active_quads));
            self.fine_quad_skipped = self.fine_quad_skipped.saturating_add(u128::from(
                stats
                    .fine_quad_total_quads
                    .saturating_sub(stats.fine_quad_active_quads),
            ));
        }
    }
}

/// The actual render route for one complete bridge paint. This deliberately records only
/// returned facts; it cannot prove the unmeasured atlas, configuration, or geometry guards that
/// a future cache would also require.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SceneReuseRenderMode {
    Full,
    SparseNonActiveCoarse,
    SparseActiveCoarse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SceneReuseBounds {
    applied: bool,
    native_tile_area: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SceneReuseDamage<'a> {
    Full,
    Rectangles(&'a [vello::FineDamageRect]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StoredSceneReuseDamage {
    Full,
    Rectangles(Vec<vello::FineDamageRect>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SceneReuseObservation<'a> {
    damage: SceneReuseDamage<'a>,
    mode: SceneReuseRenderMode,
    bounds: Option<SceneReuseBounds>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredSceneReuseObservation {
    damage: StoredSceneReuseDamage,
    mode: SceneReuseRenderMode,
    bounds: Option<SceneReuseBounds>,
}

impl<'a> SceneReuseObservation<'a> {
    fn from_paint(
        damage_regions: Option<&'a [vello::FineDamageRect]>,
        total_attempts: u64,
        sparse_attempts: u64,
        full_attempts: u64,
        sparse_stats: Option<vello::SparseRenderStats>,
    ) -> Result<Self, SceneReuseObservationRejection> {
        // A completed retry can have rendered or copied intermediate state. It is intentionally
        // not a cacheability claim, even when the final packed upload bytes repeat.
        if total_attempts > 1 || (sparse_attempts > 0 && full_attempts > 0) {
            return Err(SceneReuseObservationRejection::Recovery);
        }

        let damage = match damage_regions {
            None => SceneReuseDamage::Full,
            Some([]) => return Err(SceneReuseObservationRejection::Empty),
            Some(rectangles) if rectangles.len() > MAX_SCENE_REUSE_DAMAGE_RECTS => {
                return Err(SceneReuseObservationRejection::Oversized);
            }
            Some(rectangles) if rectangles.iter().all(valid_scene_reuse_damage_rect) => {
                SceneReuseDamage::Rectangles(rectangles)
            }
            Some(_) => return Err(SceneReuseObservationRejection::InvalidMetadata),
        };

        match (total_attempts, sparse_attempts, full_attempts, sparse_stats) {
            (1, 0, 1, None) => Ok(Self {
                damage,
                mode: SceneReuseRenderMode::Full,
                bounds: None,
            }),
            (1, 1, 0, Some(stats)) => Ok(Self {
                damage,
                mode: if stats.coarse_active_bins {
                    SceneReuseRenderMode::SparseActiveCoarse
                } else {
                    SceneReuseRenderMode::SparseNonActiveCoarse
                },
                bounds: Some(SceneReuseBounds {
                    applied: stats.path_tile_bounds_applied,
                    native_tile_area: stats.path_tile_bounds_tiles,
                }),
            }),
            _ => Err(SceneReuseObservationRejection::InvalidMetadata),
        }
    }

    fn into_stored(self) -> StoredSceneReuseObservation {
        StoredSceneReuseObservation {
            damage: match self.damage {
                SceneReuseDamage::Full => StoredSceneReuseDamage::Full,
                SceneReuseDamage::Rectangles(rectangles) => {
                    StoredSceneReuseDamage::Rectangles(rectangles.to_vec())
                }
            },
            mode: self.mode,
            bounds: self.bounds,
        }
    }
}

fn valid_scene_reuse_damage_rect(rect: &vello::FineDamageRect) -> bool {
    rect.width > 0
        && rect.height > 0
        && rect
            .x
            .checked_add(rect.width)
            .is_some_and(|right| right <= WIDTH)
        && rect
            .y
            .checked_add(rect.height)
            .is_some_and(|bottom| bottom <= HEIGHT)
}

impl StoredSceneReuseObservation {
    fn damage_matches(&self, observation: SceneReuseObservation<'_>) -> bool {
        match (&self.damage, observation.damage) {
            (StoredSceneReuseDamage::Full, SceneReuseDamage::Full) => true,
            (StoredSceneReuseDamage::Rectangles(previous), SceneReuseDamage::Rectangles(next)) => {
                previous.as_slice() == next
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SceneReuseObservationRejection {
    Empty,
    Oversized,
    InvalidMetadata,
    Recovery,
}

#[derive(Debug)]
struct SceneReuseSnapshot {
    // Exact byte storage; allocator bookkeeping is deliberately excluded from the reported
    // payload budget, which counts only the resolved-upload bytes a cache would retain.
    payload: Box<[u8]>,
    observation: StoredSceneReuseObservation,
    last_seen_observation: u64,
}

#[derive(Debug)]
struct GeometryReuseSnapshot {
    descriptor: vello::GeometryInputDescriptor,
    spans: [Box<[u8]>; 4],
    mode: SceneReuseRenderMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GeometryReuseRejection {
    MissingOrMalformed,
    Empty,
    Oversized,
    CpuRoute,
    RecoveryOrInvalidRoute,
    PendingBumpReadback,
}

/// Bounded profile-only exact comparison of resolver-expanded geometry inputs.
/// It measures input opportunity only; no entry represents retained GPU geometry.
#[derive(Debug, Default)]
struct GeometryReuseTelemetry {
    observations: u64,
    accepted: u64,
    hits: u64,
    rank_hits: [u64; MAX_SCENE_REUSE_SNAPSHOTS],
    copied_bytes: u128,
    retained_bytes: usize,
    peak_retained_bytes: usize,
    geometry_bytes: u128,
    geometry_min_bytes: Option<usize>,
    geometry_max_bytes: usize,
    descriptor_layout_differences: u64,
    descriptor_viewport_differences: u64,
    descriptor_capacity_differences: u64,
    descriptor_equal: u64,
    mode_differences: u64,
    missing_or_malformed: u64,
    empty: u64,
    oversized: u64,
    cpu_route: u64,
    recovery_or_invalid_route: u64,
    pending_bump_readback: u64,
    snapshots: Vec<GeometryReuseSnapshot>,
}

impl GeometryReuseTelemetry {
    fn observe(
        &mut self,
        input: Result<vello::GeometryInput<'_>, vello::GeometryInputError>,
        mode: Result<SceneReuseRenderMode, GeometryReuseRejection>,
    ) {
        self.observations = self.observations.saturating_add(1);
        let input = match input {
            Ok(input) => input,
            Err(vello::GeometryInputError::EmptyGeometry) => {
                self.reject(GeometryReuseRejection::Empty);
                return;
            }
            Err(vello::GeometryInputError::CpuRoute) => {
                self.reject(GeometryReuseRejection::CpuRoute);
                return;
            }
            Err(_) => {
                self.reject(GeometryReuseRejection::MissingOrMalformed);
                return;
            }
        };
        let spans = [
            input.path_tags,
            input.path_data,
            input.transforms,
            input.styles,
        ];
        let bytes = self.record_geometry_bytes(spans);
        if input.descriptor.cpu_route {
            self.reject(GeometryReuseRejection::CpuRoute);
            return;
        }
        if input.descriptor.bump_readback_pending {
            self.reject(GeometryReuseRejection::PendingBumpReadback);
            return;
        }
        let mode = match mode {
            Ok(mode) => mode,
            Err(reason) => {
                self.reject(reason);
                return;
            }
        };
        self.observe_admitted(input.descriptor, spans, bytes, mode);
    }

    fn record_geometry_bytes(&mut self, spans: [&[u8]; 4]) -> usize {
        let bytes = spans
            .iter()
            .fold(0_usize, |total, span| total.saturating_add(span.len()));
        self.geometry_bytes = self
            .geometry_bytes
            .saturating_add(u128::try_from(bytes).unwrap_or(u128::MAX));
        self.geometry_min_bytes = Some(
            self.geometry_min_bytes
                .map_or(bytes, |value| value.min(bytes)),
        );
        self.geometry_max_bytes = self.geometry_max_bytes.max(bytes);
        bytes
    }

    fn observe_admitted(
        &mut self,
        descriptor: vello::GeometryInputDescriptor,
        spans: [&[u8]; 4],
        bytes: usize,
        mode: SceneReuseRenderMode,
    ) {
        if bytes == 0 {
            self.reject(GeometryReuseRejection::Empty);
            return;
        }
        if bytes > MAX_SCENE_REUSE_PAYLOAD_BYTES {
            self.reject(GeometryReuseRejection::Oversized);
            return;
        }
        self.accepted = self.accepted.saturating_add(1);
        if let Some(index) = self.snapshots.iter().position(|snapshot| {
            snapshot
                .spans
                .iter()
                .zip(spans)
                .all(|(stored, current)| stored.as_ref() == current)
        }) {
            self.record_hit(index, descriptor, mode);
            return;
        }
        self.insert_snapshot(descriptor, spans, bytes, mode);
    }

    fn record_hit(
        &mut self,
        index: usize,
        descriptor: vello::GeometryInputDescriptor,
        mode: SceneReuseRenderMode,
    ) {
        let mru_rank = self.snapshots.len().saturating_sub(index);
        let snapshot = self.snapshots.remove(index);
        self.hits = self.hits.saturating_add(1);
        if let Some(rank) = self.rank_hits.get_mut(mru_rank.saturating_sub(1)) {
            *rank = rank.saturating_add(1);
        }
        self.descriptor_layout_differences =
            self.descriptor_layout_differences
                .saturating_add(u64::from(!geometry_layout_matches(
                    snapshot.descriptor,
                    descriptor,
                )));
        self.descriptor_viewport_differences =
            self.descriptor_viewport_differences
                .saturating_add(u64::from(!geometry_viewport_matches(
                    snapshot.descriptor,
                    descriptor,
                )));
        self.descriptor_capacity_differences =
            self.descriptor_capacity_differences
                .saturating_add(u64::from(!geometry_capacity_matches(
                    snapshot.descriptor,
                    descriptor,
                )));
        self.descriptor_equal =
            self.descriptor_equal
                .saturating_add(u64::from(geometry_descriptor_matches(
                    snapshot.descriptor,
                    descriptor,
                )));
        self.mode_differences = self
            .mode_differences
            .saturating_add(u64::from(snapshot.mode != mode));
        self.snapshots.push(GeometryReuseSnapshot {
            descriptor,
            mode,
            ..snapshot
        });
    }

    fn insert_snapshot(
        &mut self,
        descriptor: vello::GeometryInputDescriptor,
        spans: [&[u8]; 4],
        bytes: usize,
        mode: SceneReuseRenderMode,
    ) {
        while !self.snapshots.is_empty()
            && (self.snapshots.len() >= MAX_SCENE_REUSE_SNAPSHOTS
                || self.retained_bytes.saturating_add(bytes) > MAX_SCENE_REUSE_RETAINED_BYTES)
        {
            let evicted = self.snapshots.remove(0);
            self.retained_bytes = self
                .retained_bytes
                .saturating_sub(geometry_snapshot_bytes(&evicted));
        }
        self.copied_bytes = self
            .copied_bytes
            .saturating_add(u128::try_from(bytes).unwrap_or(u128::MAX));
        self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        self.peak_retained_bytes = self.peak_retained_bytes.max(self.retained_bytes);
        self.snapshots.push(GeometryReuseSnapshot {
            descriptor,
            spans: spans.map(Box::<[u8]>::from),
            mode,
        });
    }

    fn reject(&mut self, reason: GeometryReuseRejection) {
        match reason {
            GeometryReuseRejection::MissingOrMalformed => {
                self.missing_or_malformed = self.missing_or_malformed.saturating_add(1);
            }
            GeometryReuseRejection::Empty => self.empty = self.empty.saturating_add(1),
            GeometryReuseRejection::Oversized => self.oversized = self.oversized.saturating_add(1),
            GeometryReuseRejection::CpuRoute => self.cpu_route = self.cpu_route.saturating_add(1),
            GeometryReuseRejection::RecoveryOrInvalidRoute => {
                self.recovery_or_invalid_route = self.recovery_or_invalid_route.saturating_add(1);
            }
            GeometryReuseRejection::PendingBumpReadback => {
                self.pending_bump_readback = self.pending_bump_readback.saturating_add(1);
            }
        }
        self.snapshots.clear();
        self.retained_bytes = 0;
    }
}

fn geometry_snapshot_bytes(snapshot: &GeometryReuseSnapshot) -> usize {
    snapshot
        .spans
        .iter()
        .fold(0_usize, |total, span| total.saturating_add(span.len()))
}

fn geometry_layout_matches(
    left: vello::GeometryInputDescriptor,
    right: vello::GeometryInputDescriptor,
) -> bool {
    (
        left.n_draw_objects,
        left.n_paths,
        left.n_clips,
        left.bin_data_start,
        left.path_tag_base,
        left.path_data_base,
        left.draw_tag_base,
        left.draw_data_base,
        left.transform_base,
        left.style_base,
    ) == (
        right.n_draw_objects,
        right.n_paths,
        right.n_clips,
        right.bin_data_start,
        right.path_tag_base,
        right.path_data_base,
        right.draw_tag_base,
        right.draw_data_base,
        right.transform_base,
        right.style_base,
    )
}

fn geometry_viewport_matches(
    left: vello::GeometryInputDescriptor,
    right: vello::GeometryInputDescriptor,
) -> bool {
    (
        left.target_width,
        left.target_height,
        left.width_in_tiles,
        left.height_in_tiles,
    ) == (
        right.target_width,
        right.target_height,
        right.width_in_tiles,
        right.height_in_tiles,
    )
}

fn geometry_capacity_matches(
    left: vello::GeometryInputDescriptor,
    right: vello::GeometryInputDescriptor,
) -> bool {
    (
        left.lines_size,
        left.binning_size,
        left.tiles_size,
        left.seg_counts_size,
        left.segments_size,
        left.blend_size,
        left.ptcl_size,
    ) == (
        right.lines_size,
        right.binning_size,
        right.tiles_size,
        right.seg_counts_size,
        right.segments_size,
        right.blend_size,
        right.ptcl_size,
    )
}

fn geometry_descriptor_matches(
    left: vello::GeometryInputDescriptor,
    right: vello::GeometryInputDescriptor,
) -> bool {
    left.cpu_route == right.cpu_route
        && geometry_layout_matches(left, right)
        && geometry_viewport_matches(left, right)
        && geometry_capacity_matches(left, right)
}

/// Bounded diagnostic-only exact upload-byte history. A hit is a necessary condition for future
/// scene reuse, not evidence that skipping Vello encoding, coarse work, or geometry is safe.
#[derive(Debug, Default)]
struct SceneReuseTelemetry {
    observations: u64,
    admitted: u64,
    hits: u64,
    rank_hits: [u64; MAX_SCENE_REUSE_SNAPSHOTS],
    hit_any_metadata_changed: u64,
    hit_damage_changed: u64,
    hit_mode_changed: u64,
    hit_bounds_changed: u64,
    reuse_gap_total: u128,
    reuse_gap_max: u64,
    empty_observations: u64,
    oversized_observations: u64,
    invalid_metadata_observations: u64,
    recovery_observations: u64,
    observed_payload_bytes: u128,
    observed_payload_min_bytes: Option<usize>,
    observed_payload_max_bytes: usize,
    admitted_payload_bytes: u128,
    admitted_payload_min_bytes: Option<usize>,
    admitted_payload_max_bytes: usize,
    retained_payload_bytes: usize,
    peak_payload_bytes: usize,
    snapshot_copy_bytes: u128,
    snapshots: Vec<SceneReuseSnapshot>,
}

impl SceneReuseTelemetry {
    /// Observe a completed ordinary paint. The borrowed packed scene is compared in place and is
    /// copied only when it misses the bounded exact-byte history. Rejection counters record one
    /// mutually exclusive primary reason: empty/oversized payload checks intentionally precede
    /// recovery and metadata checks because no payload snapshot can be admitted either way.
    fn observe(
        &mut self,
        payload: &[u8],
        observation: Result<SceneReuseObservation<'_>, SceneReuseObservationRejection>,
    ) {
        self.observations = self.observations.saturating_add(1);
        self.record_observed_payload_size(payload.len());
        if payload.is_empty() {
            self.reject(SceneReuseObservationRejection::Empty);
            return;
        }
        if payload.len() > MAX_SCENE_REUSE_PAYLOAD_BYTES {
            self.reject(SceneReuseObservationRejection::Oversized);
            return;
        }

        let observation = match observation {
            Ok(observation) => observation,
            Err(rejection) => {
                self.reject(rejection);
                return;
            }
        };

        self.admitted = self.admitted.saturating_add(1);
        self.record_admitted_payload_size(payload.len());
        if let Some(index) = self
            .snapshots
            .iter()
            .rposition(|snapshot| snapshot.payload.as_ref() == payload)
        {
            let rank = self.snapshots.len().saturating_sub(index);
            let rank_index = rank.saturating_sub(1);
            if let Some(rank_hits) = self.rank_hits.get_mut(rank_index) {
                *rank_hits = rank_hits.saturating_add(1);
            }
            let mut snapshot = self.snapshots.remove(index);
            self.hits = self.hits.saturating_add(1);
            self.reuse_gap_total = self.reuse_gap_total.saturating_add(u128::from(
                self.observations
                    .saturating_sub(snapshot.last_seen_observation),
            ));
            self.reuse_gap_max = self.reuse_gap_max.max(
                self.observations
                    .saturating_sub(snapshot.last_seen_observation),
            );
            let damage_changed = !snapshot.observation.damage_matches(observation);
            let mode_changed = snapshot.observation.mode != observation.mode;
            let bounds_changed = snapshot.observation.bounds != observation.bounds;
            if damage_changed || mode_changed || bounds_changed {
                self.hit_any_metadata_changed = self.hit_any_metadata_changed.saturating_add(1);
            }
            if damage_changed {
                self.hit_damage_changed = self.hit_damage_changed.saturating_add(1);
            }
            if mode_changed {
                self.hit_mode_changed = self.hit_mode_changed.saturating_add(1);
            }
            if bounds_changed {
                self.hit_bounds_changed = self.hit_bounds_changed.saturating_add(1);
            }
            snapshot.observation = observation.into_stored();
            snapshot.last_seen_observation = self.observations;
            self.snapshots.push(snapshot);
            return;
        }

        self.make_room_for(payload.len());
        let payload_bytes = payload.len();
        self.retained_payload_bytes = self.retained_payload_bytes.saturating_add(payload_bytes);
        self.peak_payload_bytes = self.peak_payload_bytes.max(self.retained_payload_bytes);
        self.snapshot_copy_bytes = self
            .snapshot_copy_bytes
            .saturating_add(u128::try_from(payload_bytes).unwrap_or(u128::MAX));
        self.snapshots.push(SceneReuseSnapshot {
            payload: Box::<[u8]>::from(payload),
            observation: observation.into_stored(),
            last_seen_observation: self.observations,
        });
    }

    fn make_room_for(&mut self, payload_bytes: usize) {
        while !self.snapshots.is_empty()
            && (self.snapshots.len() >= MAX_SCENE_REUSE_SNAPSHOTS
                || self.retained_payload_bytes.saturating_add(payload_bytes)
                    > MAX_SCENE_REUSE_RETAINED_BYTES)
        {
            let evicted = self.snapshots.remove(0);
            self.retained_payload_bytes = self
                .retained_payload_bytes
                .saturating_sub(evicted.payload.len());
        }
    }

    fn clear_history(&mut self) {
        self.snapshots.clear();
        self.retained_payload_bytes = 0;
    }

    fn reject(&mut self, rejection: SceneReuseObservationRejection) {
        match rejection {
            SceneReuseObservationRejection::Empty => {
                self.empty_observations = self.empty_observations.saturating_add(1);
            }
            SceneReuseObservationRejection::Oversized => {
                self.oversized_observations = self.oversized_observations.saturating_add(1);
            }
            SceneReuseObservationRejection::InvalidMetadata => {
                self.invalid_metadata_observations =
                    self.invalid_metadata_observations.saturating_add(1);
            }
            SceneReuseObservationRejection::Recovery => {
                self.recovery_observations = self.recovery_observations.saturating_add(1);
            }
        }
        self.clear_history();
    }

    fn record_observed_payload_size(&mut self, bytes: usize) {
        self.observed_payload_bytes = self
            .observed_payload_bytes
            .saturating_add(u128::try_from(bytes).unwrap_or(u128::MAX));
        self.observed_payload_min_bytes = Some(
            self.observed_payload_min_bytes
                .map_or(bytes, |minimum| minimum.min(bytes)),
        );
        self.observed_payload_max_bytes = self.observed_payload_max_bytes.max(bytes);
    }

    fn record_admitted_payload_size(&mut self, bytes: usize) {
        self.admitted_payload_bytes = self
            .admitted_payload_bytes
            .saturating_add(u128::try_from(bytes).unwrap_or(u128::MAX));
        self.admitted_payload_min_bytes = Some(
            self.admitted_payload_min_bytes
                .map_or(bytes, |minimum| minimum.min(bytes)),
        );
        self.admitted_payload_max_bytes = self.admitted_payload_max_bytes.max(bytes);
    }

    fn misses(&self) -> u64 {
        self.admitted.saturating_sub(self.hits)
    }

    fn hypothetical_hits(&self, capacity: usize) -> u64 {
        self.rank_hits
            .iter()
            .take(capacity.min(MAX_SCENE_REUSE_SNAPSHOTS))
            .copied()
            .fold(0_u64, u64::saturating_add)
    }
}

const GPU_DISPATCH_STAGE_COUNT: usize = 32;

#[derive(Debug, Default)]
struct GpuDispatchStageTotals {
    duration_ns: Vec<u128>,
    dispatches: Vec<u128>,
}

#[derive(Debug)]
struct GpuDispatchTimingTotals {
    expected_submissions: u64,
    matched_submissions: u64,
    missing_submissions: u64,
    stale_submissions: u64,
    unsupported_submissions: u64,
    disabled_submissions: u64,
    pending_submissions: u64,
    failed_submissions: u64,
    invalid_submissions: u64,
    overflowed_submissions: u64,
    no_submission_id: u64,
    recovery_submissions: u64,
    stages: [GpuDispatchStageTotals; GPU_DISPATCH_STAGE_COUNT],
    unknown: GpuDispatchStageTotals,
}

#[derive(Debug)]
struct GpuDispatchTimestampProfile {
    totals: GpuDispatchTimingTotals,
    buffer_bytes: u64,
}

impl Default for GpuDispatchTimingTotals {
    fn default() -> Self {
        Self {
            expected_submissions: 0,
            matched_submissions: 0,
            missing_submissions: 0,
            stale_submissions: 0,
            unsupported_submissions: 0,
            disabled_submissions: 0,
            pending_submissions: 0,
            failed_submissions: 0,
            invalid_submissions: 0,
            overflowed_submissions: 0,
            no_submission_id: 0,
            recovery_submissions: 0,
            stages: array::from_fn(|_| GpuDispatchStageTotals::default()),
            unknown: GpuDispatchStageTotals::default(),
        }
    }
}

impl GpuDispatchTimingTotals {
    /// Record exactly one final submission from a completed action paint. Earlier bounded
    /// retries are deliberately counted as recoveries rather than pretending their samples
    /// belong to the completed action.
    fn record_completed_paint(
        &mut self,
        expected_submission_id: Option<u64>,
        status: vello::GpuDispatchTimestampStatus,
        sample: Option<&vello::GpuDispatchTimestampSample>,
        recovery_submissions: u64,
    ) {
        self.recovery_submissions = self
            .recovery_submissions
            .saturating_add(recovery_submissions);
        match status {
            vello::GpuDispatchTimestampStatus::Disabled => {
                self.disabled_submissions = self.disabled_submissions.saturating_add(1);
                return;
            }
            vello::GpuDispatchTimestampStatus::Unsupported => {
                self.unsupported_submissions = self.unsupported_submissions.saturating_add(1);
                return;
            }
            vello::GpuDispatchTimestampStatus::Pending { .. } => {
                self.pending_submissions = self.pending_submissions.saturating_add(1);
            }
            vello::GpuDispatchTimestampStatus::MapFailed { .. } => {
                self.failed_submissions = self.failed_submissions.saturating_add(1);
            }
            vello::GpuDispatchTimestampStatus::Invalid { .. } => {
                self.invalid_submissions = self.invalid_submissions.saturating_add(1);
            }
            vello::GpuDispatchTimestampStatus::Overflowed { .. } => {
                self.overflowed_submissions = self.overflowed_submissions.saturating_add(1);
            }
            vello::GpuDispatchTimestampStatus::Idle
            | vello::GpuDispatchTimestampStatus::Ready { .. } => {}
        }

        let Some(expected_submission_id) = expected_submission_id else {
            self.no_submission_id = self.no_submission_id.saturating_add(1);
            return;
        };
        self.expected_submissions = self.expected_submissions.saturating_add(1);
        let Some(sample) = sample else {
            self.missing_submissions = self.missing_submissions.saturating_add(1);
            return;
        };
        if sample.submission_id != expected_submission_id {
            self.stale_submissions = self.stale_submissions.saturating_add(1);
            self.missing_submissions = self.missing_submissions.saturating_add(1);
            return;
        }

        self.matched_submissions = self.matched_submissions.saturating_add(1);
        let stage_count = usize::try_from(sample.stage_count)
            .unwrap_or(sample.stages.len())
            .min(sample.stages.len());
        for stage in sample.stages.iter().take(stage_count) {
            if stage.dispatch_count == 0 {
                continue;
            }
            let stage_index = usize::from(stage.stage_id).checked_sub(1);
            if let Some(stage_totals) = stage_index.and_then(|index| self.stages.get_mut(index)) {
                stage_totals.duration_ns.push(stage.duration_ns);
                stage_totals
                    .dispatches
                    .push(u128::from(stage.dispatch_count));
            } else {
                self.unknown.duration_ns.push(stage.duration_ns);
                self.unknown
                    .dispatches
                    .push(u128::from(stage.dispatch_count));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiagnosticValueSummary {
    count: usize,
    mean: u128,
    p99: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GateOutcome {
    Pass,
    Fail,
    DisabledDiagnostic,
}

/// Summarize one required entry per action. `None` and zero-duration entries are invalid rather
/// than silently dropped, so a reported result is necessarily an all-sample result.
fn summarize_samples(samples: &[Option<u128>]) -> Result<TimingSummary, SampleError> {
    if samples.is_empty() {
        return Err(SampleError::Empty);
    }

    let mut sorted = Vec::with_capacity(samples.len());
    let mut total_us = 0_u128;
    for (index, sample) in samples.iter().enumerate() {
        let us = sample.ok_or(SampleError::Missing { index })?;
        if us == 0 {
            return Err(SampleError::Zero { index });
        }
        total_us = total_us.checked_add(us).ok_or(SampleError::TotalOverflow)?;
        sorted.push(us);
    }
    sorted.sort_unstable();
    let count_as_u128 = u128::try_from(samples.len()).map_err(|_| SampleError::TotalOverflow)?;

    // Nearest-rank p99: for 1,000 samples this is entry 990 in ascending-latency order.
    let rank = samples
        .len()
        .checked_mul(99)
        .and_then(|value| value.checked_add(99))
        .ok_or(SampleError::TotalOverflow)?
        / 100;
    let p99_index = rank.checked_sub(1).ok_or(SampleError::TotalOverflow)?;
    let p99_us = *sorted.get(p99_index).ok_or(SampleError::TotalOverflow)?;
    let min_us = *sorted.first().ok_or(SampleError::Empty)?;
    let max_us = *sorted.last().ok_or(SampleError::Empty)?;

    Ok(TimingSummary {
        count: samples.len(),
        total_us,
        mean_us: total_us
            .checked_div(count_as_u128)
            .ok_or(SampleError::TotalOverflow)?,
        p99_us,
        min_us,
        max_us,
    })
}

fn meets_gates(summary: TimingSummary) -> bool {
    // Compare the sum, not the rounded display mean: 8,000.9us must not pass an 8,000us gate.
    let Ok(count) = u128::try_from(summary.count) else {
        return false;
    };
    summary.total_us <= MEAN_LIMIT_US.saturating_mul(count) && summary.p99_us <= P99_LIMIT_US
}

fn gate_outcome(summary: TimingSummary, diagnostic: bool) -> GateOutcome {
    if diagnostic {
        GateOutcome::DisabledDiagnostic
    } else if meets_gates(summary) {
        GateOutcome::Pass
    } else {
        GateOutcome::Fail
    }
}

fn profiling_requested() -> bool {
    std::env::var(PROFILE_ENV).is_ok_and(|value| value == "1")
}

fn scene_reuse_requested() -> bool {
    scene_reuse_requested_from_value(std::env::var(SCENE_REUSE_ENV).ok().as_deref())
}

fn geometry_reuse_requested() -> bool {
    geometry_reuse_requested_from_value(std::env::var(GEOMETRY_REUSE_ENV).ok().as_deref())
}

fn scene_reuse_requested_from_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn geometry_reuse_requested_from_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| value == "1")
}

/// Resolved-upload comparisons retain CPU bytes and are intentionally diagnostic-only. The
/// profile switch also disables the acceptance gate for the entire run, before observations are
/// made after each measured sample has been recorded.
fn scene_reuse_configuration_is_valid(diagnostic: bool, requested: bool) -> bool {
    diagnostic || !requested
}

fn geometry_reuse_configuration_is_valid(diagnostic: bool, requested: bool) -> bool {
    diagnostic || !requested
}

fn scene_reuse_telemetry_enabled(diagnostic: bool, requested: bool) -> bool {
    diagnostic && requested
}

fn geometry_reuse_telemetry_enabled(diagnostic: bool, requested: bool) -> bool {
    diagnostic && requested
}

/// Joined records are profile-only CPU diagnostics; ordinary benchmark runs retain no records.
fn action_correlation_enabled(diagnostic: bool) -> bool {
    diagnostic
}

/// Latch the optional all-action report destination only for diagnostic runs. Ordinary benchmark
/// runs neither inspect nor allocate a path for this profile-only artifact.
fn profile_output_path_from_value(
    diagnostic: bool,
    value: Option<OsString>,
) -> Result<Option<PathBuf>, String> {
    if !diagnostic {
        return Ok(None);
    }
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(format!(
            "{PROFILE_OUTPUT_ENV} must name a non-empty output path"
        ));
    }
    Ok(Some(PathBuf::from(value)))
}

fn profile_output_path(diagnostic: bool) -> Result<Option<PathBuf>, String> {
    if !diagnostic {
        return Ok(None);
    }
    profile_output_path_from_value(true, std::env::var_os(PROFILE_OUTPUT_ENV))
}

/// The acceptance samples' destination for the scheduled performance lane. A diagnostic run times
/// extra work, so its samples are never handed over as acceptance samples.
fn samples_output_path_from_value(
    diagnostic: bool,
    value: Option<OsString>,
) -> Result<Option<PathBuf>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(format!("{SAMPLES_ENV} must name a non-empty output path"));
    }
    if diagnostic {
        return Err(format!(
            "{SAMPLES_ENV} is incompatible with {PROFILE_ENV}=1; diagnostic timings are not acceptance samples"
        ));
    }
    Ok(Some(PathBuf::from(value)))
}

/// One validated sample per line, in microseconds and action order: the samples file
/// `tools/perf/record.py emit` reads. Only a set `summarize_samples` accepts is written, so a
/// recorded distribution is always a complete one.
fn sample_lines(samples: &[Option<u128>]) -> Result<String, SampleError> {
    summarize_samples(samples)?;
    let mut lines = String::with_capacity(samples.len().saturating_mul(8));
    for us in samples.iter().flatten() {
        lines.push_str(&us.to_string());
        lines.push('\n');
    }
    Ok(lines)
}

fn xrgb_damage_requested() -> bool {
    xrgb_damage_requested_from_value(std::env::var(XRGB_DAMAGE_ENV).ok().as_deref())
}

fn xrgb_damage_requested_from_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn gpu_timestamp_flags_requested() -> (bool, bool) {
    (
        std::env::var(VELLO_GPU_TIMESTAMPS_ENV).is_ok_and(|value| value == "1"),
        std::env::var(VELLO_GPU_DISPATCH_TIMESTAMPS_ENV).is_ok_and(|value| value == "1"),
    )
}

/// Timestamp instrumentation changes the benchmark's GPU work, so a run with either Vello
/// timestamp flag must use the explicit diagnostic mode and can never report an acceptance pass.
fn timestamp_profile_configuration_is_valid(
    diagnostic: bool,
    gpu_timestamps_requested: bool,
    dispatch_timestamps_requested: bool,
) -> bool {
    diagnostic || (!gpu_timestamps_requested && !dispatch_timestamps_requested)
}

/// The direct-path workgroup override is an A/B diagnostic. Only the renderer's effective
/// initialization-time value is authoritative: invalid environment values intentionally resolve
/// back to the compatible default of 128.
fn direct_path_workgroup_configuration_is_valid(
    diagnostic: bool,
    effective_workgroups: u32,
) -> bool {
    diagnostic || effective_workgroups == 128
}

fn diagnostic_mean(total: u128, count: u128) -> Result<u128, String> {
    total
        .checked_div(count)
        .ok_or_else(|| "diagnostic sample count was zero".to_owned())
}

/// Summarize optional GPU stage values without treating a zero-duration hardware timestamp as a
/// failed benchmark sample. The caller has already excluded missing or mismatched submissions.
fn summarize_diagnostic_values(values: &mut [u128]) -> Option<DiagnosticValueSummary> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let total = values.iter().copied().fold(0_u128, u128::saturating_add);
    let count = values.len();
    let count_as_u128 = u128::try_from(count).ok()?;
    let rank = count.checked_mul(99)?.checked_add(99)? / 100;
    let p99 = *values.get(rank.checked_sub(1)?)?;
    Some(DiagnosticValueSummary {
        count,
        mean: total.checked_div(count_as_u128)?,
        p99,
    })
}

fn dispatch_stage_breakdown(sample: &vello::GpuDispatchTimestampSample) -> DispatchStageBreakdown {
    let stage_count = usize::try_from(sample.stage_count)
        .unwrap_or(sample.stages.len())
        .min(sample.stages.len());
    let mut breakdown = DispatchStageBreakdown::default();
    for stage in sample.stages.iter().take(stage_count) {
        if stage.dispatch_count == 0 {
            continue;
        }
        breakdown.total_timed_stage_ns = breakdown
            .total_timed_stage_ns
            .saturating_add(stage.duration_ns);
        if stage.stage_id == 0 || vello::gpu_dispatch_timestamp_stage_name(stage.stage_id).is_none()
        {
            breakdown.unknown_stage_ns =
                breakdown.unknown_stage_ns.saturating_add(stage.duration_ns);
        } else if GPU_COARSE_STAGE_IDS.contains(&stage.stage_id) {
            breakdown.coarse_ns = breakdown.coarse_ns.saturating_add(stage.duration_ns);
            breakdown.coarse_dispatches = breakdown
                .coarse_dispatches
                .saturating_add(u128::from(stage.dispatch_count));
        } else if GPU_FINE_STAGE_IDS.contains(&stage.stage_id) {
            breakdown.fine_ns = breakdown.fine_ns.saturating_add(stage.duration_ns);
            breakdown.fine_dispatches = breakdown
                .fine_dispatches
                .saturating_add(u128::from(stage.dispatch_count));
        } else {
            breakdown.other_stage_ns = breakdown.other_stage_ns.saturating_add(stage.duration_ns);
        }
    }
    breakdown
}

fn gpu_dispatch_timestamp_status_name(status: vello::GpuDispatchTimestampStatus) -> &'static str {
    match status {
        vello::GpuDispatchTimestampStatus::Disabled => "disabled",
        vello::GpuDispatchTimestampStatus::Unsupported => "unsupported",
        vello::GpuDispatchTimestampStatus::Idle => "idle",
        vello::GpuDispatchTimestampStatus::Pending { .. } => "pending",
        vello::GpuDispatchTimestampStatus::Ready { .. } => "ready",
        vello::GpuDispatchTimestampStatus::MapFailed { .. } => "map_failed",
        vello::GpuDispatchTimestampStatus::Invalid { .. } => "invalid",
        vello::GpuDispatchTimestampStatus::Overflowed { .. } => "overflowed",
    }
}

fn gpu_dispatch_timestamp_status_submission_id(
    status: vello::GpuDispatchTimestampStatus,
) -> Option<u64> {
    match status {
        vello::GpuDispatchTimestampStatus::Pending { submission_id }
        | vello::GpuDispatchTimestampStatus::Ready { submission_id, .. }
        | vello::GpuDispatchTimestampStatus::MapFailed { submission_id }
        | vello::GpuDispatchTimestampStatus::Invalid { submission_id }
        | vello::GpuDispatchTimestampStatus::Overflowed { submission_id, .. } => {
            Some(submission_id)
        }
        vello::GpuDispatchTimestampStatus::Disabled
        | vello::GpuDispatchTimestampStatus::Unsupported
        | vello::GpuDispatchTimestampStatus::Idle => None,
    }
}

/// Read the diagnostic slot only after the bridge has synchronously completed the action's
/// texture readback. The expected submission id is captured before consuming the sample so a
/// delayed sample is recorded as stale/missing, never assigned to the wrong action.
fn record_gpu_dispatch_timestamps(
    bridge: &mut VelloDrmBridge,
    totals: &mut GpuDispatchTimingTotals,
    recovery_submissions: u64,
) -> ActionGpuTimestampObservation {
    let expected_submission_id = bridge.gpu_dispatch_timestamp_latest_submission_id();
    let sample = bridge.take_gpu_dispatch_timestamp_sample();
    let status = dispatch_timestamp_status_after_take(sample.as_ref(), || {
        bridge.gpu_dispatch_timestamp_status()
    });
    let observation =
        ActionGpuTimestampObservation::from_sample(expected_submission_id, status, sample.as_ref());
    totals.record_completed_paint(
        expected_submission_id,
        status,
        sample.as_ref(),
        recovery_submissions,
    );
    observation
}

/// `take_gpu_dispatch_timestamp_sample` polls and may decode a ready sample into an invalid
/// state. A decoded sample is therefore authoritative for this completed paint; inspect the
/// renderer status only when there was no sample to decode.
fn dispatch_timestamp_status_after_take(
    sample: Option<&vello::GpuDispatchTimestampSample>,
    status_if_missing: impl FnOnce() -> vello::GpuDispatchTimestampStatus,
) -> vello::GpuDispatchTimestampStatus {
    match sample {
        Some(sample) => vello::GpuDispatchTimestampStatus::Ready {
            submission_id: sample.submission_id,
            stage_count: sample.stage_count,
        },
        None => status_if_missing(),
    }
}

/// Consume any setup-frame sample before the 1,000 measured actions. It is intentionally not
/// aggregated: the setup paint establishes input targets but is not a redraw benchmark sample.
fn discard_initial_gpu_dispatch_timestamp(bridge: &mut VelloDrmBridge) {
    let _ = bridge.gpu_dispatch_timestamp_latest_submission_id();
    let _ = bridge.gpu_dispatch_timestamp_status();
    let _ = bridge.take_gpu_dispatch_timestamp_sample();
}

fn print_gpu_dispatch_timestamp_profile(totals: &GpuDispatchTimingTotals, buffer_bytes: u64) {
    println!(
        "diagnostic GPU dispatch timestamps: matched={}/{} missing={} stale={} unsupported={} disabled={} pending={} map_failed={} invalid={} overflowed={} no_submission_id={} recovery_submissions={}; diagnostic_buffers={}B",
        totals.matched_submissions,
        totals.expected_submissions,
        totals.missing_submissions,
        totals.stale_submissions,
        totals.unsupported_submissions,
        totals.disabled_submissions,
        totals.pending_submissions,
        totals.failed_submissions,
        totals.invalid_submissions,
        totals.overflowed_submissions,
        totals.no_submission_id,
        totals.recovery_submissions,
        buffer_bytes,
    );
    for (stage_index, totals) in totals.stages.iter().enumerate() {
        let mut durations = totals.duration_ns.clone();
        let Some(duration) = summarize_diagnostic_values(&mut durations) else {
            continue;
        };
        let mut dispatches = totals.dispatches.clone();
        let dispatches = summarize_diagnostic_values(&mut dispatches);
        let stage_id = u16::try_from(stage_index.saturating_add(1)).unwrap_or(u16::MAX);
        let stage_name = vello::gpu_dispatch_timestamp_stage_name(stage_id).unwrap_or("unknown");
        println!(
            "  dispatch-stage-{stage_name} (id={stage_id}): samples={} duration_mean={}ns duration_p99={}ns dispatch_mean={} dispatch_p99={}",
            duration.count,
            duration.mean,
            duration.p99,
            dispatches.map_or(0, |summary| summary.mean),
            dispatches.map_or(0, |summary| summary.p99),
        );
    }
    let mut unknown_durations = totals.unknown.duration_ns.clone();
    if let Some(duration) = summarize_diagnostic_values(&mut unknown_durations) {
        let mut unknown_dispatches = totals.unknown.dispatches.clone();
        let dispatches = summarize_diagnostic_values(&mut unknown_dispatches);
        println!(
            "  dispatch-stage-unknown (id=0): samples={} duration_mean={}ns duration_p99={}ns dispatch_mean={} dispatch_p99={}",
            duration.count,
            duration.mean,
            duration.p99,
            dispatches.map_or(0, |summary| summary.mean),
            dispatches.map_or(0, |summary| summary.p99),
        );
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "each profile-only action line keeps the wall sample, CPU phases, returned sparse facts, and exact timestamp-match state together"
)]
fn write_action_correlation_profile(
    writer: &mut impl Write,
    profile: &ActionCorrelationProfile,
) -> io::Result<()> {
    writeln!(
        writer,
        "diagnostic action correlation: records={} capacity={} (wall_us is the same captured value appended to samples; CPU bridge phases span all attempts; GPU stage breakdowns cover only an exactly matched final submission)",
        profile.records.len(),
        MAX_ACTION_CORRELATION_RECORDS,
    )?;
    for record in &profile.records {
        let sparse = record.sparse_stats.unwrap_or_default();
        let timestamp = record.timestamp;
        writeln!(
            writer,
            "  action-correlation index={} target={} wall_us={} input_us={} registry_us={} layout_us={} scene_us={} submit_us={} gpu_completion_map_us={} settle_us={} cpu_copy_swizzle_us={} readback_bytes={} cpu_output_bytes={} attempts={} sparse_attempts={} full_attempts={} recovery_attempts={} sparse_present={} sparse_fine_tiles={} sparse_fine_pixels={} sparse_written_pixels={} sparse_coarse_pixels={} sparse_coarse_active_bins={} sparse_coarse_adaptive={} sparse_coarse_native_bins={} sparse_coarse_selected_native_bins={} sparse_path_tile_bounds_applied={} sparse_path_tile_bounds_tiles={} sparse_fine_quad_skip_applied={} sparse_fine_quad_total_quads={} sparse_fine_quad_active_quads={} sparse_opaque_prune_selected={} sparse_solid_fuse_selected={} sparse_segment_row_mask_selected={} sparse_canonical_segment_order_selected={} timestamp_status={} timestamp_status_submission_id={:?} timestamp_expected_submission_id={:?} timestamp_observed_submission_id={:?} timestamp_exact_match={} timestamp_missing={} timestamp_stale={} gpu_total_timed_stage_ns={:?} gpu_coarse_ns={:?} gpu_fine_ns={:?} gpu_other_stage_ns={:?} gpu_unknown_stage_ns={:?} gpu_coarse_dispatches={:?} gpu_fine_dispatches={:?}",
            record.index,
            record.target,
            record.wall_us,
            record.phase.input_us,
            record.phase.registry_us,
            record.phase.layout_us,
            record.phase.scene_us,
            record.phase.bridge.submit_us,
            record.phase.bridge.completed_readback_map_us,
            record.phase.bridge.settle_us,
            record.phase.bridge.cpu_copy_swizzle_us,
            record.phase.bridge.readback_bytes,
            record.phase.bridge.cpu_output_bytes,
            record.attempts,
            record.sparse_attempts,
            record.full_attempts,
            record.recovery_attempts,
            record.sparse_stats.is_some(),
            sparse.fine_tiles,
            sparse.fine_pixels,
            sparse.written_pixels,
            sparse.coarse_pixels,
            sparse.coarse_active_bins,
            sparse.coarse_adaptive,
            sparse.coarse_native_bins,
            sparse.coarse_selected_native_bins,
            sparse.path_tile_bounds_applied,
            sparse.path_tile_bounds_tiles,
            sparse.fine_quad_skip_applied,
            sparse.fine_quad_total_quads,
            sparse.fine_quad_active_quads,
            sparse.coarse_opaque_prune_selected,
            sparse.solid_fuse_selected,
            sparse.segment_row_mask_selected,
            sparse.canonical_segment_order_shader_selected,
            gpu_dispatch_timestamp_status_name(timestamp.status),
            gpu_dispatch_timestamp_status_submission_id(timestamp.status),
            timestamp.expected_submission_id,
            timestamp.observed_submission_id,
            timestamp.exact_match,
            timestamp.sample_missing,
            timestamp.stale_sample,
            timestamp.total_timed_stage_ns,
            timestamp.coarse_ns,
            timestamp.fine_ns,
            timestamp.other_stage_ns,
            timestamp.unknown_stage_ns,
            timestamp.coarse_dispatches,
            timestamp.fine_dispatches,
        )?;
    }
    Ok(())
}

fn open_profile_output(path: &Path) -> io::Result<BufWriter<File>> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(BufWriter::new)
}

fn report_action_correlation_profile(
    profile: &ActionCorrelationProfile,
    output_path: Option<&Path>,
) -> Result<(), String> {
    if let Some(path) = output_path {
        let mut writer = open_profile_output(path).map_err(|error| {
            format!(
                "failed to create {PROFILE_OUTPUT_ENV} {}: {error}",
                path.display()
            )
        })?;
        write_action_correlation_profile(&mut writer, profile).map_err(|error| {
            format!(
                "failed to write {PROFILE_OUTPUT_ENV} {}: {error}",
                path.display()
            )
        })?;
        writer.flush().map_err(|error| {
            format!(
                "failed to flush {PROFILE_OUTPUT_ENV} {}: {error}",
                path.display()
            )
        })?;
        println!(
            "diagnostic action correlation: records={} output_file={} (all action-correlation rows were written after the loop; stdout rows suppressed)",
            profile.records.len(),
            path.display(),
        );
    } else {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        write_action_correlation_profile(&mut stdout, profile).map_err(|error| {
            format!("failed to write action-correlation stdout report: {error}")
        })?;
        stdout.flush().map_err(|error| {
            format!("failed to flush action-correlation stdout report: {error}")
        })?;
    }
    Ok(())
}

fn print_scene_reuse_profile(telemetry: &SceneReuseTelemetry) {
    let [rank1, rank2, rank3, rank4] = telemetry.rank_hits;
    let cap1_hits = telemetry.hypothetical_hits(1);
    let cap2_hits = telemetry.hypothetical_hits(2);
    let cap4_hits = telemetry.hypothetical_hits(4);
    let reuse_gap_mean = telemetry
        .reuse_gap_total
        .checked_div(u128::from(telemetry.hits))
        .unwrap_or(0);
    println!(
        "diagnostic packed-scene exact reuse: observations={} admitted={} misses={} hits={} rank1={} rank2={} rank3={} rank4={} reuse_gaps(matches={} mean_actions={} max_actions={})",
        telemetry.observations,
        telemetry.admitted,
        telemetry.misses(),
        telemetry.hits,
        rank1,
        rank2,
        rank3,
        rank4,
        telemetry.hits,
        reuse_gap_mean,
        telemetry.reuse_gap_max,
    );
    println!(
        "  hypothetical LRU capacities from this one four-entry history (rank classification only, not independent simulations): capacity1_hits={} capacity1_misses={} capacity2_hits={} capacity2_misses={} capacity4_hits={} capacity4_misses={}",
        cap1_hits,
        telemetry.admitted.saturating_sub(cap1_hits),
        cap2_hits,
        telemetry.admitted.saturating_sub(cap2_hits),
        cap4_hits,
        telemetry.admitted.saturating_sub(cap4_hits),
    );
    println!(
        "  observed_payload_bytes(total={} min={} max={}) admitted_payload_bytes(total={} min={} max={}) retained_cpu_payload_bytes={} peak_cpu_payload_bytes={} actual_single_history_snapshot_copy_bytes={} entries={} (payload bytes only; allocator metadata excluded)",
        telemetry.observed_payload_bytes,
        telemetry.observed_payload_min_bytes.unwrap_or(0),
        telemetry.observed_payload_max_bytes,
        telemetry.admitted_payload_bytes,
        telemetry.admitted_payload_min_bytes.unwrap_or(0),
        telemetry.admitted_payload_max_bytes,
        telemetry.retained_payload_bytes,
        telemetry.peak_payload_bytes,
        telemetry.snapshot_copy_bytes,
        telemetry.snapshots.len(),
    );
    println!(
        "  matching-payload metadata comparisons against the most-recent same-payload entry: any_changed={} ordered_damage={} actual_mode={} returned_bounds_applied_or_area={} (not hypothetical full-cache-key hits; a payload-only LRU can collapse metadata states); history_invalidations(primary reasons, mutually exclusive): empty={} oversized={} invalid_metadata={} recovery={}",
        telemetry.hit_any_metadata_changed,
        telemetry.hit_damage_changed,
        telemetry.hit_mode_changed,
        telemetry.hit_bounds_changed,
        telemetry.empty_observations,
        telemetry.oversized_observations,
        telemetry.invalid_metadata_observations,
        telemetry.recovery_observations,
    );
    println!(
        "  exact resolved-upload byte hits are necessary conditions only, not proven safe coarse/geometry cache hits; atlas, configuration, and exact bounds-coordinate guards are unmeasured."
    );
}

fn print_geometry_reuse_profile(telemetry: &GeometryReuseTelemetry) {
    let [rank1, rank2, rank3, rank4] = telemetry.rank_hits;
    println!(
        "diagnostic geometry-input exact reuse: observations={} accepted={} misses={} hits={} rank1={} rank2={} rank3={} rank4={} entries={}",
        telemetry.observations,
        telemetry.accepted,
        telemetry.accepted.saturating_sub(telemetry.hits),
        telemetry.hits,
        rank1,
        rank2,
        rank3,
        rank4,
        telemetry.snapshots.len(),
    );
    println!(
        "  extracted_geometry_span_bytes(total={} min={} max={}) copied_cpu_payload_bytes={} retained_cpu_payload_bytes={} peak_cpu_payload_bytes={} (payload only; descriptor and allocator overhead excluded); matching-span descriptors: equal={} layout_different={} viewport_different={} capacities_different={} full_or_sparse_mode_different={}",
        telemetry.geometry_bytes,
        telemetry.geometry_min_bytes.unwrap_or(0),
        telemetry.geometry_max_bytes,
        telemetry.copied_bytes,
        telemetry.retained_bytes,
        telemetry.peak_retained_bytes,
        telemetry.descriptor_equal,
        telemetry.descriptor_layout_differences,
        telemetry.descriptor_viewport_differences,
        telemetry.descriptor_capacity_differences,
        telemetry.mode_differences,
    );
    println!(
        "  history_invalidations(primary mutually-exclusive): missing_or_malformed={} empty={} oversized={} cpu_route={} recovery_or_invalid_route={} pending_bump_readback={}; exact geometry inputs are opportunity evidence only, not safe GPU-cache entries, line-count provenance, or saved dispatches.",
        telemetry.missing_or_malformed,
        telemetry.empty,
        telemetry.oversized,
        telemetry.cpu_route,
        telemetry.recovery_or_invalid_route,
        telemetry.pending_bump_readback,
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "the benchmark keeps setup, all required samples, and final gate reporting in one auditable linear execution path"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the shell fixture owns long-lived permits by design and lives for the whole run"
)]
fn run() -> Result<(), String> {
    let diagnostic = profiling_requested();
    let profile_output_path = profile_output_path(diagnostic)?;
    let samples_output_path =
        samples_output_path_from_value(diagnostic, std::env::var_os(SAMPLES_ENV))?;
    let scene_reuse_requested = scene_reuse_requested();
    let geometry_reuse_requested = geometry_reuse_requested();
    let xrgb_damage_requested = xrgb_damage_requested();
    if !scene_reuse_configuration_is_valid(diagnostic, scene_reuse_requested) {
        return Err(format!(
            "{PROFILE_ENV}=1 is required when {SCENE_REUSE_ENV}=1; resolved-upload reuse telemetry is diagnostic-only"
        ));
    }
    if !geometry_reuse_configuration_is_valid(diagnostic, geometry_reuse_requested) {
        return Err(format!(
            "{PROFILE_ENV}=1 is required when {GEOMETRY_REUSE_ENV}=1; geometry-input reuse telemetry is diagnostic-only"
        ));
    }
    let (gpu_timestamps_requested, dispatch_timestamps_requested) = gpu_timestamp_flags_requested();
    if !timestamp_profile_configuration_is_valid(
        diagnostic,
        gpu_timestamps_requested,
        dispatch_timestamps_requested,
    ) {
        return Err(format!(
            "{PROFILE_ENV}=1 is required when {VELLO_GPU_TIMESTAMPS_ENV}=1 or {VELLO_GPU_DISPATCH_TIMESTAMPS_ENV}=1; timestamp-instrumented runs are diagnostic-only"
        ));
    }
    let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())?;
    let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).map_err(|error| error.to_string())?;
    let xrgb_damage_available = xrgb_damage_requested && bridge.xrgb_history_available();
    let sparse_tile_bounds_requested =
        std::env::var("VELLO_SPARSE_TILE_BOUNDS").is_ok_and(|value| value == "1");
    bridge.set_phase_timing_enabled(diagnostic);
    let direct_path_workgroups = bridge.direct_path_workgroups();
    if !direct_path_workgroup_configuration_is_valid(diagnostic, direct_path_workgroups) {
        return Err(format!(
            "{PROFILE_ENV}=1 is required when VELLO_DIRECT_PATH_WORKGROUPS selects effective workgroups={direct_path_workgroups}; non-default dispatch grids are diagnostic-only"
        ));
    }
    let adapter = bridge.adapter_info().map_err(|error| error.to_string())?;
    println!(
        "DRM bridge adapter: name={:?} backend={} device_type={} driver={:?}",
        adapter.name, adapter.backend, adapter.device_type, adapter.driver
    );
    println!(
        "sparse path tile bounds: env_requested={sparse_tile_bounds_requested} renderer_requested={} available={} (profile counts only submissions where returned stats report applied bounds; tile area is not an allocation or memory-saving claim)",
        bridge.sparse_tile_bounds_requested(),
        bridge.sparse_tile_bounds_available(),
    );
    let sparse_fine_quad_skip_requested =
        std::env::var("VELLO_SPARSE_FINE_QUAD_SKIP").is_ok_and(|value| value == "1");
    println!(
        "sparse fine quad skip: env_requested={sparse_fine_quad_skip_requested} renderer_requested={} available={} (counts are actual Area fine invocation groups, not GPU memory or copy savings)",
        bridge.sparse_fine_quad_skip_requested(),
        bridge.sparse_fine_quad_skip_available(),
    );
    let sparse_opaque_prune_requested =
        std::env::var("VELLO_SPARSE_OPAQUE_PRUNE").is_ok_and(|value| value == "1");
    println!(
        "sparse opaque prune: env_requested={sparse_opaque_prune_requested} renderer_requested={} available={} (counts report only returned coarse shader selection, not tiles or commands pruned, GPU memory, or copy savings)",
        bridge.sparse_opaque_prune_requested(),
        bridge.sparse_opaque_prune_available(),
    );
    let sparse_solid_fuse_requested =
        std::env::var("VELLO_SPARSE_SOLID_FUSE").is_ok_and(|value| value == "1");
    println!(
        "sparse solid fuse: env_requested={sparse_solid_fuse_requested} renderer_requested={} available={} (counts report only returned matched producer/consumer shader selection)",
        bridge.sparse_solid_fuse_requested(),
        bridge.sparse_solid_fuse_available(),
    );
    let sparse_segment_row_mask_requested =
        std::env::var("VELLO_SPARSE_SEGMENT_ROW_MASK").is_ok_and(|value| value == "1");
    println!(
        "sparse segment row mask: env_requested={sparse_segment_row_mask_requested} renderer_requested={} available={} (counts report only returned paired shader selection, not rows skipped or resource savings)",
        bridge.sparse_segment_row_mask_requested(),
        bridge.sparse_segment_row_mask_available(),
    );
    let canonical_segment_order_requested =
        std::env::var("VELLO_CANONICAL_SEGMENT_ORDER").is_ok_and(|value| value == "1");
    println!(
        "canonical segment order: env_requested={canonical_segment_order_requested} renderer_requested={} available={} (selected attempts report only an actual recorded pass, not completion or speed)",
        bridge.canonical_segment_order_requested(),
        bridge.canonical_segment_order_available(),
    );
    let area_ppt8_requested = std::env::var("VELLO_AREA_PPT8").is_ok_and(|value| value == "1");
    println!(
        "area PPT8: env_requested={area_ppt8_requested} renderer_requested={} available={} (selected attempts report only an actual recorded Area shader pass, not completion or speed)",
        bridge.area_ppt8_requested(),
        bridge.area_ppt8_available(),
    );
    let clip_single_group_requested =
        std::env::var("VELLO_CLIP_SINGLE_GROUP").is_ok_and(|value| value == "1");
    println!(
        "clip single group: env_requested={clip_single_group_requested} renderer_requested={} available={} (selected attempts report only an actual recorded clip shader pass, not completion or speed)",
        bridge.clip_single_group_requested(),
        bridge.clip_single_group_available(),
    );
    println!(
        "CPU XRGB damage history: requested={xrgb_damage_requested} available={xrgb_damage_available} targets=1 (opt-in candidate; actual measured writes are reported after the action loop)"
    );
    if diagnostic {
        println!(
            "diagnostic: {PROFILE_ENV}=1; phase timing is enabled and acceptance gates are disabled"
        );
        println!(
            "diagnostic GPU dispatch timestamps: VELLO_GPU_DISPATCH_TIMESTAMPS=1 also requires VELLO_GPU_TIMESTAMPS=1; direct_path_workgroups={direct_path_workgroups} (default/invalid is 128)",
        );
    }
    let width = usize::try_from(WIDTH).map_err(|_| "width does not fit usize".to_owned())?;
    let height = usize::try_from(HEIGHT).map_err(|_| "height does not fit usize".to_owned())?;
    let destination_pitch = width
        .checked_mul(4)
        .ok_or_else(|| "destination pitch overflow".to_owned())?;
    let destination_len = destination_pitch
        .checked_mul(height)
        .ok_or_else(|| "destination allocation overflow".to_owned())?;
    let mut destination = vec![0_u8; destination_len];
    let mut document = AutoCommit::new();
    let modifiers = Modifiers::default();
    let mut focus: Option<NodeId> = None;
    let mut samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut render_attempts = RenderAttemptTotals::default();
    let mut phase_totals = BenchmarkPhaseTotals::default();
    let mut gpu_dispatch_timestamps = diagnostic.then(GpuDispatchTimingTotals::default);
    let mut action_correlations =
        action_correlation_enabled(diagnostic).then(ActionCorrelationProfile::new);
    let mut scene_reuse = scene_reuse_telemetry_enabled(diagnostic, scene_reuse_requested)
        .then(SceneReuseTelemetry::default);
    let mut geometry_reuse = geometry_reuse_telemetry_enabled(diagnostic, geometry_reuse_requested)
        .then(GeometryReuseTelemetry::default);
    let mut xrgb_history = xrgb_damage_available.then(|| XrgbTargetHistory::new(1));
    let mut xrgb_writes = CpuXrgbWriteTotals::default();
    paint_initial_desktop(&mut frame, &mut bridge, &mut destination, destination_pitch)?;
    if diagnostic {
        discard_initial_gpu_dispatch_timestamp(&mut bridge);
    }
    for sample_index in 0..SAMPLE_COUNT {
        let start = Instant::now();
        let target_index = sample_index
            .checked_rem(ACTION_TARGETS.len())
            .ok_or_else(|| "benchmark action target list is empty".to_owned())?;
        let target = *ACTION_TARGETS
            .get(target_index)
            .ok_or_else(|| "benchmark action target list is empty".to_owned())?;
        let registry_started = diagnostic.then(Instant::now);
        let rect = frame
            .widget_registry()
            .get(target)
            .copied()
            .filter(|rect| rect.area() > 0.0)
            .ok_or_else(|| {
                format!("required input target {target:?} is absent or has zero area")
            })?;
        let registry_us = registry_started.map_or(0, |started| started.elapsed().as_micros());
        let input_started = diagnostic.then(Instant::now);
        dispatch_click(
            &mut frame,
            target,
            rect.center(),
            modifiers,
            &mut document,
            &mut focus,
        )?;
        let input_us = input_started.map_or(0, |started| started.elapsed().as_micros());

        // This is the native compositor's per-redraw sequence: input mutates state, then layout
        // and paint produce the scene. `paint_settled` is deliberately not used: it can perform a
        // capture-only second layout/draw that production redraws do not unconditionally take.
        let layout_started = diagnostic.then(Instant::now);
        frame.relayout();
        let layout_us = layout_started.map_or(0, |started| started.elapsed().as_micros());
        let scene_started = diagnostic.then(Instant::now);
        let frame_paint = frame.paint_with_damage();
        let scene_us = scene_started.map_or(0, |started| started.elapsed().as_micros());
        bridge.set_backdrop(frame_paint.backdrop.clone());
        let damage = SceneDamage {
            revision: frame_paint.revision,
            previous_revision: frame_paint.previous_revision,
            regions: frame_paint.damage_regions.as_deref(),
        };
        if let Some(history) = xrgb_history.as_mut() {
            let transaction = history.prepare(
                0,
                bridge.xrgb_history_identity(destination_pitch, 1),
                damage.revision,
                damage.previous_revision,
                damage.regions,
                &[],
            );
            let write = bridge
                .paint_damage_into_xrgb8888_with_history(
                    &frame_paint.scene,
                    damage,
                    &transaction,
                    &mut destination,
                    destination_pitch,
                )
                .map_err(|error| format!("sample {sample_index}: {error}"))?;
            transaction.commit(if write.damage_admitted {
                XrgbHistoryCommit::DamageAccepted
            } else {
                XrgbHistoryCommit::FullBoundary
            });
            xrgb_writes.record_history_write(write.mode, write.cpu_output_bytes);
        } else {
            bridge
                .paint_damage_into_xrgb8888(
                    &frame_paint.scene,
                    damage,
                    &mut destination,
                    destination_pitch,
                )
                .map_err(|error| format!("sample {sample_index}: {error}"))?;
            xrgb_writes.record_full(destination_len)?;
        }
        let phase_timing = if diagnostic {
            let bridge_timing = bridge.last_phase_timing().ok_or_else(|| {
                format!("sample {sample_index}: diagnostic bridge timing was not recorded")
            })?;
            let timing = BenchmarkPhaseTiming {
                input_us,
                registry_us,
                layout_us,
                scene_us,
                bridge: bridge_timing,
            };
            phase_totals.add_sample(timing);
            Some(timing)
        } else {
            None
        };
        let attempts = u64::try_from(bridge.last_render_attempts())
            .map_err(|_| "render attempt count does not fit u64".to_owned())?;
        let sparse_attempts = u64::try_from(bridge.last_sparse_render_attempts())
            .map_err(|_| "sparse render attempt count does not fit u64".to_owned())?;
        let full_attempts = u64::try_from(bridge.last_full_render_attempts())
            .map_err(|_| "full render attempt count does not fit u64".to_owned())?;
        render_attempts.total = render_attempts.total.saturating_add(attempts);
        render_attempts.sparse = render_attempts.sparse.saturating_add(sparse_attempts);
        render_attempts.full = render_attempts.full.saturating_add(full_attempts);
        render_attempts.record_canonical_segment_order_attempts(
            u64::try_from(bridge.last_canonical_segment_order_full_attempts()).map_err(|_| {
                "canonical segment-order full attempt count does not fit u64".to_owned()
            })?,
            u64::try_from(bridge.last_canonical_segment_order_sparse_attempts()).map_err(|_| {
                "canonical segment-order sparse attempt count does not fit u64".to_owned()
            })?,
        );
        render_attempts.record_area_ppt8_attempts(
            u64::try_from(bridge.last_area_ppt8_full_attempts())
                .map_err(|_| "Area PPT8 full attempt count does not fit u64".to_owned())?,
            u64::try_from(bridge.last_area_ppt8_sparse_attempts())
                .map_err(|_| "Area PPT8 sparse attempt count does not fit u64".to_owned())?,
        );
        render_attempts.record_clip_single_group_attempts(
            u64::try_from(bridge.last_clip_single_group_full_attempts())
                .map_err(|_| "clip single-group full attempt count does not fit u64".to_owned())?,
            u64::try_from(bridge.last_clip_single_group_sparse_attempts()).map_err(|_| {
                "clip single-group sparse attempt count does not fit u64".to_owned()
            })?,
        );
        let sample_recovery_attempts = attempts.saturating_sub(1);
        render_attempts.recovery = render_attempts
            .recovery
            .saturating_add(sample_recovery_attempts);
        let sparse_stats = bridge.last_sparse_render_stats();
        if let Some(stats) = sparse_stats {
            render_attempts.record_sparse_stats(stats);
        }
        let timestamp_observation = gpu_dispatch_timestamps.as_mut().map(|totals| {
            record_gpu_dispatch_timestamps(&mut bridge, totals, sample_recovery_attempts)
        });
        let elapsed_us = start.elapsed().as_micros();
        samples.push(Some(elapsed_us));
        if let Some(correlations) = action_correlations.as_mut() {
            let Some(phase) = phase_timing else {
                return Err(
                    "diagnostic action correlation was enabled without phase timing".to_owned(),
                );
            };
            let Some(timestamp) = timestamp_observation else {
                return Err(
                    "diagnostic action correlation was enabled without timestamp observation"
                        .to_owned(),
                );
            };
            let record = ActionCorrelationRecord {
                index: sample_index,
                target,
                wall_us: elapsed_us,
                phase,
                attempts,
                sparse_attempts,
                full_attempts,
                recovery_attempts: sample_recovery_attempts,
                sparse_stats,
                timestamp,
            };
            correlations.record(&record)?;
        }
        if let Some(telemetry) = scene_reuse.as_mut() {
            telemetry.observe(
                bridge.diagnostic_packed_scene(),
                SceneReuseObservation::from_paint(
                    frame_paint.damage_regions.as_deref(),
                    attempts,
                    sparse_attempts,
                    full_attempts,
                    bridge.last_sparse_render_stats(),
                ),
            );
        }
        if let Some(telemetry) = geometry_reuse.as_mut() {
            let mode = SceneReuseObservation::from_paint(
                frame_paint.damage_regions.as_deref(),
                attempts,
                sparse_attempts,
                full_attempts,
                bridge.last_sparse_render_stats(),
            )
            .map(|observation| observation.mode)
            .map_err(|_| GeometryReuseRejection::RecoveryOrInvalidRoute);
            telemetry.observe(bridge.diagnostic_geometry_input(), mode);
        }
    }
    let gpu_dispatch_timestamp_profile =
        gpu_dispatch_timestamps.map(|totals| GpuDispatchTimestampProfile {
            totals,
            buffer_bytes: bridge.gpu_dispatch_timestamp_buffer_bytes(),
        });
    finish_run(
        &samples,
        diagnostic,
        &FinishDiagnostics {
            render_attempts,
            phase_totals,
            gpu_dispatch_timestamp_profile: gpu_dispatch_timestamp_profile.as_ref(),
            action_correlations: action_correlations.as_ref(),
            profile_output_path: profile_output_path.as_deref(),
            samples_output_path: samples_output_path.as_deref(),
            scene_reuse: scene_reuse.as_ref(),
            geometry_reuse: geometry_reuse.as_ref(),
            xrgb_writes,
        },
    )
}

/// Establish the displayed desktop and its live input-target registry before timed redraws.
fn paint_initial_desktop(
    frame: &mut FrameBuilder,
    bridge: &mut VelloDrmBridge,
    destination: &mut [u8],
    destination_pitch: usize,
) -> Result<(), String> {
    let initial = frame.paint_with_damage();
    bridge.set_backdrop(initial.backdrop.clone());
    bridge
        .paint_damage_into_xrgb8888(
            &initial.scene,
            SceneDamage {
                revision: initial.revision,
                previous_revision: initial.previous_revision,
                regions: initial.damage_regions.as_deref(),
            },
            destination,
            destination_pitch,
        )
        .map_err(|error| format!("initial desktop paint: {error}"))
}

#[allow(
    clippy::too_many_lines,
    reason = "the final report keeps every measured-workload total and its acceptance decision in one auditable output sequence"
)]
fn finish_run(
    samples: &[Option<u128>],
    diagnostic: bool,
    diagnostics: &FinishDiagnostics<'_>,
) -> Result<(), String> {
    let FinishDiagnostics {
        render_attempts,
        phase_totals,
        gpu_dispatch_timestamp_profile,
        action_correlations,
        profile_output_path,
        samples_output_path,
        scene_reuse,
        geometry_reuse,
        xrgb_writes,
    } = diagnostics;
    let summary =
        summarize_samples(samples).map_err(|error| format!("invalid sample set: {error:?}"))?;
    // Written before the gate decides: the lane records the distribution of a run whether or not
    // this renderer meets the hardware gate below.
    if let Some(path) = samples_output_path {
        let lines =
            sample_lines(samples).map_err(|error| format!("invalid sample set: {error:?}"))?;
        std::fs::write(path, lines)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        println!(
            "samples: {} action redraw totals (us) written to {}",
            summary.count,
            path.display()
        );
    }
    println!(
        "DRM render/readback at {WIDTH}x{HEIGHT}: samples={} mean={}us p99={}us min={}us max={}us",
        summary.count, summary.mean_us, summary.p99_us, summary.min_us, summary.max_us
    );
    println!(
        "DRM timing total: total_us={} samples={}",
        summary.total_us, summary.count
    );
    let gate = gate_outcome(summary, diagnostic);
    println!("gates: mean<={MEAN_LIMIT_US}us p99<={P99_LIMIT_US}us ({gate:?})");
    println!(
        "Vello render attempts: total={} sparse={} full={} recovery={}",
        render_attempts.total,
        render_attempts.sparse,
        render_attempts.full,
        render_attempts.recovery,
    );
    println!(
        "CPU XRGB writes: full={} partial={} total={} bytes={}",
        xrgb_writes.full,
        xrgb_writes.partial,
        xrgb_writes.total(),
        xrgb_writes.bytes,
    );
    println!(
        "sparse path tile bounds actual submissions: total={} active={} applied={} native_bbox_tiles={} (counted only from returned applied stats; bbox tile area is not an allocation or memory-saving claim)",
        render_attempts.sparse,
        render_attempts.active_sparse,
        render_attempts.path_tile_bounds_sparse,
        render_attempts.path_tile_bounds_tiles,
    );
    println!(
        "sparse fine quad skip actual submissions: applied={} total_invocation_groups={} active_invocation_groups={} skipped_invocation_groups={} (counted only from returned applied stats; not GPU memory or copy savings)",
        render_attempts.fine_quad_skip_sparse,
        render_attempts.fine_quad_total,
        render_attempts.fine_quad_active,
        render_attempts.fine_quad_skipped,
    );
    println!(
        "sparse opaque prune shader-selected submissions: {} (counted only from returned stats; not tiles or commands pruned, GPU memory, or copy savings)",
        render_attempts.coarse_opaque_prune_selected_sparse,
    );
    println!(
        "sparse solid fuse shader-selected submissions: {}",
        render_attempts.solid_fuse_selected_sparse,
    );
    println!(
        "sparse segment row mask shader-selected submissions: {}",
        render_attempts.segment_row_mask_selected_sparse,
    );
    println!(
        "canonical segment order shader-selected attempts: full={} sparse={} total={}",
        render_attempts.canonical_segment_order_selected_full,
        render_attempts.canonical_segment_order_selected_sparse,
        render_attempts
            .canonical_segment_order_selected_full
            .saturating_add(render_attempts.canonical_segment_order_selected_sparse),
    );
    println!(
        "area PPT8 shader-selected attempts: full={} sparse={} total={}",
        render_attempts.area_ppt8_selected_full,
        render_attempts.area_ppt8_selected_sparse,
        render_attempts
            .area_ppt8_selected_full
            .saturating_add(render_attempts.area_ppt8_selected_sparse),
    );
    println!(
        "clip single group shader-selected attempts: full={} sparse={} total={}",
        render_attempts.clip_single_group_selected_full,
        render_attempts.clip_single_group_selected_sparse,
        render_attempts
            .clip_single_group_selected_full
            .saturating_add(render_attempts.clip_single_group_selected_sparse),
    );
    println!(
        "presentation: unavailable in this harness (no DRM/KMS device, dumb-buffer page flip, or display scanout timed)"
    );

    if diagnostic {
        println!(
            "diagnostic sparse coarse: adaptive_sparse_submissions={} active_sparse_submissions={} adaptive_dense_fallback_sparse_submissions={} native_bins={} selected_native_bins={}",
            render_attempts.adaptive_sparse,
            render_attempts.active_sparse,
            render_attempts.dense_fallback_sparse,
            render_attempts.coarse_native_bins,
            render_attempts.coarse_selected_native_bins,
        );
        let count =
            u128::try_from(summary.count).map_err(|_| "sample count overflow".to_owned())?;
        println!(
            "diagnostic phase means: input={}us registry={}us layout={}us scene={}us submit={}us gpu_completion_map={}us settle={}us cpu_copy_swizzle={}us readback_bytes/sample={} cpu_output_bytes/sample={}",
            diagnostic_mean(phase_totals.input_us, count)?,
            diagnostic_mean(phase_totals.registry_us, count)?,
            diagnostic_mean(phase_totals.layout_us, count)?,
            diagnostic_mean(phase_totals.scene_us, count)?,
            diagnostic_mean(phase_totals.submit_us, count)?,
            diagnostic_mean(phase_totals.completed_readback_map_us, count)?,
            diagnostic_mean(phase_totals.settle_us, count)?,
            diagnostic_mean(phase_totals.cpu_copy_swizzle_us, count)?,
            diagnostic_mean(phase_totals.readback_bytes, count)?,
            diagnostic_mean(phase_totals.cpu_output_bytes, count)?,
        );
        if let Some(profile) = gpu_dispatch_timestamp_profile {
            print_gpu_dispatch_timestamp_profile(&profile.totals, profile.buffer_bytes);
        }
        if let Some(profile) = action_correlations {
            report_action_correlation_profile(profile, *profile_output_path)?;
        }
        if let Some(telemetry) = scene_reuse {
            print_scene_reuse_profile(telemetry);
        }
        if let Some(telemetry) = geometry_reuse {
            print_geometry_reuse_profile(telemetry);
        }
    }
    match gate {
        GateOutcome::Pass => Ok(()),
        GateOutcome::Fail => Err(format!(
            "redraw gate failed (mean<={MEAN_LIMIT_US}us p99<={P99_LIMIT_US}us): total={}us / {} samples, p99={}us",
            summary.total_us, summary.count, summary.p99_us,
        )),
        GateOutcome::DisabledDiagnostic => {
            Err("diagnostic run completed: acceptance gates are intentionally disabled".to_owned())
        }
    }
}

fn dispatch_click(
    frame: &mut FrameBuilder,
    target: &str,
    center: vello::kurbo::Point,
    modifiers: Modifiers,
    document: &mut AutoCommit,
    focus: &mut Option<NodeId>,
) -> Result<(), String> {
    let events = [
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(center.x, center.y),
        },
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    ];
    // Pointer motion can be consumed merely by hover handling, so only a button event qualifies
    // the action as delivered. The motion still precedes it exactly as it does on the native wire.
    let [cursor, press, release] = events;
    let _ = frame.dispatch_event(
        &cursor,
        &modifiers,
        &mut ui::widget::Edits::lend(document),
        &ROOT,
        focus,
    );
    let pressed = frame.dispatch_event(
        &press,
        &modifiers,
        &mut ui::widget::Edits::lend(document),
        &ROOT,
        focus,
    );
    let released = frame.dispatch_event(
        &release,
        &modifiers,
        &mut ui::widget::Edits::lend(document),
        &ROOT,
        focus,
    );
    if pressed || released {
        Ok(())
    } else {
        Err(format!(
            "required input target {target:?} did not handle its click"
        ))
    }
}

fn main() -> Result<(), String> {
    run()
}

#[cfg(test)]
mod tests {
    use super::{
        ACTION_TARGETS, ActionCorrelationProfile, ActionCorrelationRecord,
        ActionGpuTimestampObservation, BenchmarkPhaseTiming, BenchmarkPhaseTotals,
        BridgePhaseTiming, CpuXrgbWriteTotals, GateOutcome, GeometryReuseRejection,
        GeometryReuseTelemetry, GpuDispatchTimingTotals, HEIGHT, MAX_ACTION_CORRELATION_RECORDS,
        MAX_SCENE_REUSE_PAYLOAD_BYTES, MAX_SCENE_REUSE_RETAINED_BYTES, MAX_SCENE_REUSE_SNAPSHOTS,
        MEAN_LIMIT_US, P99_LIMIT_US, PopulatedShellFixture, RenderAttemptTotals, SampleError,
        SceneReuseObservation, SceneReuseRenderMode, SceneReuseTelemetry, WIDTH,
        action_correlation_enabled, direct_path_workgroup_configuration_is_valid, dispatch_click,
        dispatch_stage_breakdown, dispatch_timestamp_status_after_take, gate_outcome,
        geometry_reuse_configuration_is_valid, geometry_reuse_requested_from_value,
        geometry_reuse_telemetry_enabled, gpu_dispatch_timestamp_status_name, meets_gates,
        open_profile_output, profile_output_path_from_value, sample_lines,
        samples_output_path_from_value, scene_reuse_configuration_is_valid,
        scene_reuse_requested_from_value, scene_reuse_telemetry_enabled,
        summarize_diagnostic_values, summarize_samples, timestamp_profile_configuration_is_valid,
        write_action_correlation_profile, xrgb_damage_requested_from_value,
    };
    use automerge::AutoCommit;
    use serde::Deserialize;
    use shell::compositor::backend::drm::paint::{
        BridgeError, SceneDamage, SharedCoarseGeometryDiagnostic, VelloDrmBridge, XrgbCpuWriteMode,
        XrgbHistoryCommit, XrgbTargetHistory,
    };
    use std::ffi::OsString;
    use std::io::{self, Write};
    use ui::taffy::NodeId;
    use vello_encoding::DrawTag;
    use winit::event::Modifiers;

    fn geometry_descriptor() -> vello::GeometryInputDescriptor {
        vello::GeometryInputDescriptor {
            n_draw_objects: 1,
            n_paths: 1,
            n_clips: 0,
            bin_data_start: 0,
            path_tag_base: 0,
            path_data_base: 1,
            draw_tag_base: 2,
            draw_data_base: 3,
            transform_base: 4,
            style_base: 5,
            target_width: 16,
            target_height: 16,
            width_in_tiles: 1,
            height_in_tiles: 1,
            lines_size: 1,
            binning_size: 1,
            tiles_size: 1,
            seg_counts_size: 1,
            segments_size: 1,
            blend_size: 1,
            ptcl_size: 1,
            cpu_route: false,
            bump_readback_pending: false,
        }
    }

    fn geometry_input_with_descriptor(
        descriptor: vello::GeometryInputDescriptor,
        spans: [&[u8]; 4],
    ) -> vello::GeometryInput<'_> {
        vello::GeometryInput {
            descriptor,
            path_tags: spans[0],
            path_data: spans[1],
            transforms: spans[2],
            styles: spans[3],
        }
    }

    fn geometry_input(bytes: &[u8]) -> vello::GeometryInput<'_> {
        geometry_input_with_descriptor(
            geometry_descriptor(),
            [&bytes[..1], &bytes[1..2], &bytes[2..3], &bytes[3..]],
        )
    }

    fn geometry_input_one_span(bytes: &[u8]) -> vello::GeometryInput<'_> {
        geometry_input_with_descriptor(geometry_descriptor(), [bytes, &[], &[], &[]])
    }

    fn geometry_rejection_total(telemetry: &GeometryReuseTelemetry) -> u64 {
        telemetry
            .missing_or_malformed
            .saturating_add(telemetry.empty)
            .saturating_add(telemetry.oversized)
            .saturating_add(telemetry.cpu_route)
            .saturating_add(telemetry.recovery_or_invalid_route)
            .saturating_add(telemetry.pending_bump_readback)
    }

    fn assert_geometry_observation_accounting(telemetry: &GeometryReuseTelemetry) {
        assert_eq!(
            telemetry.observations,
            telemetry
                .accepted
                .saturating_add(geometry_rejection_total(telemetry))
        );
    }

    fn geometry_descriptor_change(field: usize) -> (vello::GeometryInputDescriptor, usize) {
        let mut changed = geometry_descriptor();
        let category = match field {
            0 => {
                changed.n_draw_objects = 2;
                0
            }
            1 => {
                changed.n_paths = 2;
                0
            }
            2 => {
                changed.n_clips = 1;
                0
            }
            3 => {
                changed.bin_data_start = 1;
                0
            }
            4 => {
                changed.path_tag_base = 1;
                0
            }
            5 => {
                changed.path_data_base = 2;
                0
            }
            6 => {
                changed.draw_tag_base = 3;
                0
            }
            7 => {
                changed.draw_data_base = 4;
                0
            }
            8 => {
                changed.transform_base = 5;
                0
            }
            9 => {
                changed.style_base = 6;
                0
            }
            10 => {
                changed.target_width = 17;
                1
            }
            11 => {
                changed.target_height = 17;
                1
            }
            12 => {
                changed.width_in_tiles = 2;
                1
            }
            13 => {
                changed.height_in_tiles = 2;
                1
            }
            14 => {
                changed.lines_size = 2;
                2
            }
            15 => {
                changed.binning_size = 2;
                2
            }
            16 => {
                changed.tiles_size = 2;
                2
            }
            17 => {
                changed.seg_counts_size = 2;
                2
            }
            18 => {
                changed.segments_size = 2;
                2
            }
            19 => {
                changed.blend_size = 2;
                2
            }
            20 => {
                changed.ptcl_size = 2;
                2
            }
            _ => unreachable!(),
        };
        (changed, category)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn geometry_reuse_cache_hits_ranks_and_evicts_only_exact_spans() {
        // Phase `geometry_reuse_a_b_a_promotes_rank_two_without_copying`.
        {
            let mut telemetry = GeometryReuseTelemetry::default();
            telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            telemetry.observe(Ok(geometry_input(b"bbbb")), Ok(SceneReuseRenderMode::Full));
            let a_pointer = telemetry.snapshots[0].spans[0].as_ptr();
            telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            assert_eq!(telemetry.hits, 2);
            assert_eq!(telemetry.rank_hits[0], 1);
            assert_eq!(telemetry.rank_hits[1], 1);
            assert_eq!(telemetry.copied_bytes, 8);
            assert_eq!(telemetry.snapshots[1].spans[0].as_ptr(), a_pointer);
            assert_eq!(telemetry.snapshots[0].spans[0].as_ref(), b"b");
            assert_eq!(telemetry.snapshots[1].spans[0].as_ref(), b"a");
            assert_geometry_observation_accounting(&telemetry);
        }
        // Phase `geometry_reuse_lru_rank_and_eviction_sequence`.
        {
            let mut telemetry = GeometryReuseTelemetry::default();
            for bytes in [
                b"aaaa", b"bbbb", b"cccc", b"dddd", b"aaaa", b"eeee", b"bbbb", b"aaaa",
            ] {
                telemetry.observe(Ok(geometry_input(bytes)), Ok(SceneReuseRenderMode::Full));
            }
            assert_eq!(telemetry.accepted, 8);
            assert_eq!(telemetry.hits, 2);
            assert_eq!(telemetry.rank_hits[3], 1);
            assert_eq!(telemetry.rank_hits[2], 1);
            assert_eq!(telemetry.snapshots.len(), 4);
            assert_eq!(telemetry.copied_bytes, 24);
            assert_eq!(telemetry.retained_bytes, 16);
            assert_eq!(telemetry.peak_retained_bytes, 16);
            assert_eq!(telemetry.snapshots[0].spans[0].as_ref(), b"d");
            assert_eq!(telemetry.snapshots[1].spans[0].as_ref(), b"e");
            assert_eq!(telemetry.snapshots[2].spans[0].as_ref(), b"b");
            assert_eq!(telemetry.snapshots[3].spans[0].as_ref(), b"a");
        }
        // Phase `geometry_reuse_requires_exact_span_boundaries_and_bytes`.
        {
            let bytes = b"abcdefgh";
            let base = [&bytes[..2], &bytes[2..4], &bytes[4..6], &bytes[6..]];
            let mut boundaries = GeometryReuseTelemetry::default();
            boundaries.observe(
                Ok(geometry_input_with_descriptor(geometry_descriptor(), base)),
                Ok(SceneReuseRenderMode::Full),
            );
            boundaries.observe(
                Ok(geometry_input_with_descriptor(
                    geometry_descriptor(),
                    [&bytes[..1], &bytes[1..3], &bytes[3..5], &bytes[5..]],
                )),
                Ok(SceneReuseRenderMode::Full),
            );
            assert_eq!(boundaries.hits, 0);
            for index in [0, 2, 4, 6] {
                let mut changed = *bytes;
                changed[index] ^= 1;
                let mut telemetry = GeometryReuseTelemetry::default();
                telemetry.observe(
                    Ok(geometry_input_with_descriptor(geometry_descriptor(), base)),
                    Ok(SceneReuseRenderMode::Full),
                );
                telemetry.observe(
                    Ok(geometry_input_with_descriptor(
                        geometry_descriptor(),
                        [&changed[..2], &changed[2..4], &changed[4..6], &changed[6..]],
                    )),
                    Ok(SceneReuseRenderMode::Full),
                );
                assert_eq!(telemetry.hits, 0);
                assert_eq!(telemetry.accepted, 2);
                assert_eq!(telemetry.copied_bytes, 16);
            }
            let longer = b"abcdefghi";
            let mut different_length = GeometryReuseTelemetry::default();
            different_length.observe(
                Ok(geometry_input_with_descriptor(geometry_descriptor(), base)),
                Ok(SceneReuseRenderMode::Full),
            );
            different_length.observe(
                Ok(geometry_input_with_descriptor(
                    geometry_descriptor(),
                    [&longer[..2], &longer[2..4], &longer[4..6], &longer[6..]],
                )),
                Ok(SceneReuseRenderMode::Full),
            );
            assert_eq!(different_length.hits, 0);
            assert_eq!(different_length.accepted, 2);
            assert_eq!(different_length.copied_bytes, 17);
        }
        // Phase `geometry_reuse_payload_caps_evict_before_copying_and_reject_oversized`.
        {
            let payloads = (0_u8..5)
                .map(|value| vec![value; MAX_SCENE_REUSE_PAYLOAD_BYTES])
                .collect::<Vec<_>>();
            let mut telemetry = GeometryReuseTelemetry::default();
            for payload in &payloads {
                telemetry.observe(
                    Ok(geometry_input_one_span(payload)),
                    Ok(SceneReuseRenderMode::Full),
                );
            }
            assert_eq!(telemetry.snapshots.len(), MAX_SCENE_REUSE_SNAPSHOTS);
            assert_eq!(telemetry.retained_bytes, MAX_SCENE_REUSE_RETAINED_BYTES);
            assert_eq!(
                telemetry.peak_retained_bytes,
                MAX_SCENE_REUSE_RETAINED_BYTES
            );
            assert_eq!(
                telemetry.copied_bytes,
                u128::try_from(MAX_SCENE_REUSE_PAYLOAD_BYTES * 5).unwrap()
            );
            let oversized = vec![0_u8; MAX_SCENE_REUSE_PAYLOAD_BYTES + 1];
            telemetry.observe(
                Ok(geometry_input_one_span(&oversized)),
                Ok(SceneReuseRenderMode::Full),
            );
            assert!(telemetry.snapshots.is_empty());
            assert_eq!(telemetry.retained_bytes, 0);
            assert_eq!(telemetry.oversized, 1);
            assert_eq!(
                telemetry.peak_retained_bytes,
                MAX_SCENE_REUSE_RETAINED_BYTES
            );
            assert_eq!(
                telemetry.copied_bytes,
                u128::try_from(MAX_SCENE_REUSE_PAYLOAD_BYTES * 5).unwrap()
            );
            assert_geometry_observation_accounting(&telemetry);
        }
        // Phase `geometry_reuse_refreshes_every_descriptor_category`.
        {
            for field in 0..21 {
                let (changed, category) = geometry_descriptor_change(field);
                let mut telemetry = GeometryReuseTelemetry::default();
                telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
                telemetry.observe(
                    Ok(geometry_input_with_descriptor(
                        changed,
                        [b"a", b"a", b"a", b"a"],
                    )),
                    Ok(SceneReuseRenderMode::Full),
                );
                telemetry.observe(
                    Ok(geometry_input_with_descriptor(
                        changed,
                        [b"a", b"a", b"a", b"a"],
                    )),
                    Ok(SceneReuseRenderMode::Full),
                );
                assert_eq!(telemetry.hits, 2);
                assert_eq!(telemetry.descriptor_equal, 1);
                assert_eq!(telemetry.copied_bytes, 4);
                let differences = [
                    telemetry.descriptor_layout_differences,
                    telemetry.descriptor_viewport_differences,
                    telemetry.descriptor_capacity_differences,
                ];
                assert_eq!(differences[category], 1);
                assert_eq!(differences.iter().copied().sum::<u64>(), 1);
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn geometry_reuse_accounts_changes_errors_and_admission() {
        // Phase `geometry_reuse_counts_mode_changes_without_copying`.
        {
            let mut telemetry = GeometryReuseTelemetry::default();
            telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            telemetry.observe(
                Ok(geometry_input(b"aaaa")),
                Ok(SceneReuseRenderMode::SparseActiveCoarse),
            );
            telemetry.observe(
                Ok(geometry_input(b"aaaa")),
                Ok(SceneReuseRenderMode::SparseNonActiveCoarse),
            );
            assert_eq!(telemetry.hits, 2);
            assert_eq!(telemetry.descriptor_equal, 2);
            assert_eq!(telemetry.mode_differences, 2);
            assert_eq!(telemetry.copied_bytes, 4);
        }
        // Phase `geometry_reuse_counts_combined_descriptor_differences`.
        {
            let mut changed = geometry_descriptor();
            changed.n_clips = 1;
            changed.target_width = 17;
            changed.ptcl_size = 2;
            let mut telemetry = GeometryReuseTelemetry::default();
            telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            telemetry.observe(
                Ok(geometry_input_with_descriptor(
                    changed,
                    [b"a", b"a", b"a", b"a"],
                )),
                Ok(SceneReuseRenderMode::Full),
            );
            assert_eq!(telemetry.descriptor_layout_differences, 1);
            assert_eq!(telemetry.descriptor_viewport_differences, 1);
            assert_eq!(telemetry.descriptor_capacity_differences, 1);
            assert_eq!(telemetry.descriptor_equal, 0);
        }
        // Phase `geometry_reuse_extraction_errors_clear_history_without_bytes`.
        {
            let errors = [
                vello::GeometryInputError::MissingOrAmbiguousUpload,
                vello::GeometryInputError::MalformedConfig,
                vello::GeometryInputError::MalformedSpans,
                vello::GeometryInputError::EmptyGeometry,
                vello::GeometryInputError::CpuRoute,
            ];
            for error in errors {
                let mut telemetry = GeometryReuseTelemetry::default();
                telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
                telemetry.observe(
                    Err::<vello::GeometryInput<'static>, _>(error),
                    Ok(SceneReuseRenderMode::Full),
                );
                assert!(telemetry.snapshots.is_empty());
                assert_eq!(telemetry.retained_bytes, 0);
                assert_eq!(telemetry.copied_bytes, 4);
                assert_eq!(telemetry.geometry_bytes, 4);
                assert_eq!(telemetry.accepted, 1);
                assert_eq!(geometry_rejection_total(&telemetry), 1);
                match error {
                    vello::GeometryInputError::EmptyGeometry => assert_eq!(telemetry.empty, 1),
                    vello::GeometryInputError::CpuRoute => assert_eq!(telemetry.cpu_route, 1),
                    _ => assert_eq!(telemetry.missing_or_malformed, 1),
                }
                assert_geometry_observation_accounting(&telemetry);
            }
        }
        // Phase `geometry_reuse_admission_rejections_account_bytes_and_precedence`.
        {
            let empty = geometry_input_with_descriptor(geometry_descriptor(), [&[], &[], &[], &[]]);
            let mut empty_telemetry = GeometryReuseTelemetry::default();
            empty_telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            empty_telemetry.observe(Ok(empty), Ok(SceneReuseRenderMode::Full));
            assert_eq!(empty_telemetry.empty, 1);
            assert_eq!(empty_telemetry.geometry_bytes, 4);
            assert_eq!(empty_telemetry.geometry_min_bytes, Some(0));

            let mut pending_descriptor = geometry_descriptor();
            pending_descriptor.bump_readback_pending = true;
            let mut pending_telemetry = GeometryReuseTelemetry::default();
            pending_telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            pending_telemetry.observe(
                Ok(geometry_input_with_descriptor(
                    pending_descriptor,
                    [b"b", b"b", b"b", b"b"],
                )),
                Err(GeometryReuseRejection::RecoveryOrInvalidRoute),
            );
            assert_eq!(pending_telemetry.pending_bump_readback, 1);
            assert_eq!(pending_telemetry.recovery_or_invalid_route, 0);
            assert_eq!(pending_telemetry.geometry_bytes, 8);

            let mut cpu_descriptor = geometry_descriptor();
            cpu_descriptor.cpu_route = true;
            let mut cpu_telemetry = GeometryReuseTelemetry::default();
            cpu_telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            cpu_telemetry.observe(
                Ok(geometry_input_with_descriptor(
                    cpu_descriptor,
                    [b"b", b"b", b"b", b"b"],
                )),
                Ok(SceneReuseRenderMode::Full),
            );
            assert_eq!(cpu_telemetry.cpu_route, 1);
            assert_eq!(cpu_telemetry.geometry_bytes, 8);

            let oversized = vec![0_u8; MAX_SCENE_REUSE_PAYLOAD_BYTES + 1];
            let mut oversized_telemetry = GeometryReuseTelemetry::default();
            oversized_telemetry
                .observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            oversized_telemetry.observe(
                Ok(geometry_input_one_span(&oversized)),
                Ok(SceneReuseRenderMode::Full),
            );
            assert_eq!(oversized_telemetry.oversized, 1);
            assert_eq!(
                oversized_telemetry.geometry_bytes,
                u128::try_from(MAX_SCENE_REUSE_PAYLOAD_BYTES + 5).unwrap()
            );

            let mut recovery_telemetry = GeometryReuseTelemetry::default();
            recovery_telemetry.observe(Ok(geometry_input(b"aaaa")), Ok(SceneReuseRenderMode::Full));
            recovery_telemetry.observe(
                Ok(geometry_input(b"bbbb")),
                Err(GeometryReuseRejection::RecoveryOrInvalidRoute),
            );
            assert_eq!(recovery_telemetry.recovery_or_invalid_route, 1);
            for telemetry in [
                empty_telemetry,
                pending_telemetry,
                cpu_telemetry,
                oversized_telemetry,
                recovery_telemetry,
            ] {
                assert!(telemetry.snapshots.is_empty());
                assert_eq!(telemetry.retained_bytes, 0);
                assert_eq!(telemetry.copied_bytes, 4);
                assert_eq!(geometry_rejection_total(&telemetry), 1);
                assert_geometry_observation_accounting(&telemetry);
            }
        }
        // Phase `geometry_reuse_option_requires_profile_and_exact_one`.
        {
            for (value, expected) in [
                (None, false),
                (Some(""), false),
                (Some("0"), false),
                (Some("true"), false),
                (Some("01"), false),
                (Some("1"), true),
            ] {
                assert_eq!(geometry_reuse_requested_from_value(value), expected);
            }
            for diagnostic in [false, true] {
                for requested in [false, true] {
                    assert_eq!(
                        geometry_reuse_configuration_is_valid(diagnostic, requested),
                        diagnostic || !requested
                    );
                    assert_eq!(
                        geometry_reuse_telemetry_enabled(diagnostic, requested),
                        diagnostic && requested
                    );
                }
            }
        }
    }

    #[test]
    fn acceptance_gates_aggregate_every_sample_at_exact_boundaries() {
        // Phase `aggregation_uses_all_samples_and_nearest_rank_p99`.
        {
            let mut samples = vec![Some(1_000); 989];
            samples.push(Some(50_000));
            samples.extend(std::iter::repeat_n(Some(80_000), 10));

            let summary = summarize_samples(&samples).expect("valid samples");
            assert_eq!(summary.count, 1_000);
            assert_eq!(summary.total_us, 1_839_000);
            assert_eq!(summary.mean_us, 1_839);
            assert_eq!(summary.p99_us, 50_000);
            assert_eq!(summary.min_us, 1_000);
            assert_eq!(summary.max_us, 80_000);
        }
        // Phase `empty_zero_and_missing_samples_are_rejected`.
        {
            assert_eq!(summarize_samples(&[]), Err(SampleError::Empty));
            assert_eq!(
                summarize_samples(&[Some(1), Some(0)]),
                Err(SampleError::Zero { index: 1 })
            );
            assert_eq!(
                summarize_samples(&[Some(1), None]),
                Err(SampleError::Missing { index: 1 })
            );
        }
        // Phase `gates_check_exact_mean_and_p99_boundaries`.
        {
            let mut passing = vec![Some(24_721); 989];
            passing.extend(std::iter::repeat_n(Some(P99_LIMIT_US), 11));
            let passing = summarize_samples(&passing).expect("valid samples");
            let count = u128::try_from(passing.count).expect("usize must fit u128");
            assert!(passing.total_us <= MEAN_LIMIT_US * count);
            assert_eq!(passing.p99_us, P99_LIMIT_US);
            assert!(meets_gates(passing));

            let mut mean_fail = vec![Some(24_722); 989];
            mean_fail.extend(std::iter::repeat_n(Some(P99_LIMIT_US), 11));
            assert!(!meets_gates(
                summarize_samples(&mean_fail).expect("valid samples")
            ));

            let mut p99_fail = vec![Some(24_721); 989];
            p99_fail.extend(std::iter::repeat_n(Some(P99_LIMIT_US + 1), 11));
            assert!(!meets_gates(
                summarize_samples(&p99_fail).expect("valid samples")
            ));
        }
        // Phase `diagnostic_runs_cannot_pass_acceptance`.
        {
            let summary = summarize_samples(&vec![Some(1_000); 1_000]).expect("valid samples");
            assert!(meets_gates(summary));
            assert_eq!(gate_outcome(summary, false), GateOutcome::Pass);
            assert_eq!(
                gate_outcome(summary, true),
                GateOutcome::DisabledDiagnostic,
                "diagnostic timestamps perturb timing and must never qualify a release gate"
            );
        }
    }

    #[test]
    fn diagnostic_flags_are_exact_opt_in_and_xrgb_writes_are_accounted() {
        // Phase `timestamp_flags_require_explicit_diagnostic_mode`.
        {
            assert!(timestamp_profile_configuration_is_valid(
                false, false, false
            ));
            assert!(timestamp_profile_configuration_is_valid(true, false, false));
            assert!(!timestamp_profile_configuration_is_valid(
                false, true, false
            ));
            assert!(!timestamp_profile_configuration_is_valid(
                false, false, true
            ));
            assert!(!timestamp_profile_configuration_is_valid(false, true, true));
            assert!(timestamp_profile_configuration_is_valid(true, true, true));
        }
        // Phase `direct_path_workgroup_override_requires_explicit_diagnostic_mode`.
        {
            assert!(direct_path_workgroup_configuration_is_valid(false, 128));
            assert!(!direct_path_workgroup_configuration_is_valid(false, 1));
            assert!(!direct_path_workgroup_configuration_is_valid(false, 8));
            assert!(direct_path_workgroup_configuration_is_valid(true, 1));
        }
        // Phase `xrgb_damage_flag_is_exact_opt_in`.
        {
            assert!(!xrgb_damage_requested_from_value(None));
            assert!(!xrgb_damage_requested_from_value(Some("0")));
            assert!(xrgb_damage_requested_from_value(Some("1")));
        }
        // Phase `xrgb_write_accounting_reports_actual_modes_and_bytes`.
        {
            let mut totals = CpuXrgbWriteTotals::default();
            totals.record_full(128).expect("small test byte count");
            totals.record_history_write(XrgbCpuWriteMode::Partial, 16);
            totals.record_history_write(XrgbCpuWriteMode::Full, 32);
            assert_eq!(totals.full, 2);
            assert_eq!(totals.partial, 1);
            assert_eq!(totals.total(), 3);
            assert_eq!(totals.bytes, 176);
        }
        // Phase `scene_reuse_requires_profile_and_defaults_to_no_telemetry`.
        {
            assert!(!scene_reuse_requested_from_value(None));
            assert!(!scene_reuse_requested_from_value(Some("0")));
            assert!(!scene_reuse_requested_from_value(Some("true")));
            assert!(scene_reuse_requested_from_value(Some("1")));
            assert!(scene_reuse_configuration_is_valid(false, false));
            assert!(!scene_reuse_configuration_is_valid(false, true));
            assert!(scene_reuse_configuration_is_valid(true, true));
            assert!(!scene_reuse_telemetry_enabled(false, false));
            assert!(!scene_reuse_telemetry_enabled(true, false));
            assert!(scene_reuse_telemetry_enabled(true, true));
        }
        // Phase `profile_output_path_is_latched_only_for_diagnostic_mode`.
        {
            let requested = Some(OsString::from("correlation.txt"));
            assert_eq!(
                profile_output_path_from_value(false, requested.clone())
                    .expect("ordinary mode ignores the diagnostic output path"),
                None
            );
            assert_eq!(
                profile_output_path_from_value(true, requested)
                    .expect("diagnostic output path is accepted")
                    .as_deref(),
                Some(std::path::Path::new("correlation.txt"))
            );
            assert!(profile_output_path_from_value(true, Some(OsString::new())).is_err());
        }
    }

    #[test]
    fn samples_output_is_acceptance_only_and_writes_complete_sets() {
        assert_eq!(samples_output_path_from_value(false, None), Ok(None));
        assert_eq!(samples_output_path_from_value(true, None), Ok(None));
        assert_eq!(
            samples_output_path_from_value(false, Some(OsString::from("samples.txt")))
                .expect("an acceptance run writes samples")
                .as_deref(),
            Some(std::path::Path::new("samples.txt"))
        );
        assert!(
            samples_output_path_from_value(true, Some(OsString::from("samples.txt"))).is_err(),
            "diagnostic timings must not reach the performance lane"
        );
        assert!(samples_output_path_from_value(false, Some(OsString::new())).is_err());
        assert_eq!(
            sample_lines(&[Some(9_000), Some(7_000), Some(12_000)]),
            Ok("9000\n7000\n12000\n".to_owned())
        );
        assert_eq!(
            sample_lines(&[Some(9_000), None]),
            Err(SampleError::Missing { index: 1 })
        );
        assert_eq!(sample_lines(&[]), Err(SampleError::Empty));
    }

    fn full_scene_reuse_observation(
        damage: Option<&[vello::FineDamageRect]>,
    ) -> Result<SceneReuseObservation<'_>, super::SceneReuseObservationRejection> {
        SceneReuseObservation::from_paint(damage, 1, 0, 1, None)
    }

    fn sparse_scene_reuse_observation(
        damage: &[vello::FineDamageRect],
        active: bool,
        bounds_applied: bool,
        bounds_area: u32,
    ) -> Result<SceneReuseObservation<'_>, super::SceneReuseObservationRejection> {
        SceneReuseObservation::from_paint(
            Some(damage),
            1,
            1,
            0,
            Some(vello::SparseRenderStats {
                coarse_active_bins: active,
                path_tile_bounds_applied: bounds_applied,
                path_tile_bounds_tiles: bounds_area,
                canonical_segment_order_shader_selected: false,
                ..vello::SparseRenderStats::default()
            }),
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn scene_reuse_cache_hits_only_exact_snapshots_and_stays_bounded() {
        // Phase `scene_reuse_exact_a_b_a_is_a_rank_two_hit_without_a_payload_copy`.
        {
            let mut telemetry = SceneReuseTelemetry::default();
            telemetry.observe(b"A", full_scene_reuse_observation(None));
            telemetry.observe(b"B", full_scene_reuse_observation(None));
            telemetry.observe(b"A", full_scene_reuse_observation(None));

            assert_eq!(telemetry.observations, 3);
            assert_eq!(telemetry.admitted, 3);
            assert_eq!(telemetry.misses(), 2);
            assert_eq!(telemetry.hits, 1);
            assert_eq!(telemetry.rank_hits, [0, 1, 0, 0]);
            assert_eq!(telemetry.hypothetical_hits(1), 0);
            assert_eq!(telemetry.hypothetical_hits(2), 1);
            assert_eq!(telemetry.hypothetical_hits(4), 1);
            assert_eq!(telemetry.snapshot_copy_bytes, 2);
            assert_eq!(telemetry.observed_payload_bytes, 3);
            assert_eq!(telemetry.observed_payload_min_bytes, Some(1));
            assert_eq!(telemetry.observed_payload_max_bytes, 1);
            assert_eq!(telemetry.admitted_payload_bytes, 3);
            assert_eq!(telemetry.admitted_payload_min_bytes, Some(1));
            assert_eq!(telemetry.admitted_payload_max_bytes, 1);
            assert_eq!(telemetry.reuse_gap_total, 2);
            assert_eq!(telemetry.reuse_gap_max, 2);
        }
        // Phase `scene_reuse_retains_four_snapshots_and_evicts_the_fifth`.
        {
            let mut telemetry = SceneReuseTelemetry::default();
            for payload in [b"A".as_slice(), b"B", b"C", b"D", b"E"] {
                telemetry.observe(payload, full_scene_reuse_observation(None));
            }

            assert_eq!(telemetry.snapshots.len(), MAX_SCENE_REUSE_SNAPSHOTS);
            assert!(
                telemetry
                    .snapshots
                    .iter()
                    .all(|snapshot| snapshot.payload.as_ref() != b"A".as_slice())
            );
            telemetry.observe(b"A", full_scene_reuse_observation(None));
            assert_eq!(telemetry.hits, 0);
            assert_eq!(telemetry.misses(), 6);
            assert_eq!(telemetry.snapshots.len(), MAX_SCENE_REUSE_SNAPSHOTS);
        }
        // Phase `scene_reuse_requires_exact_bytes_even_at_equal_length`.
        {
            let mut telemetry = SceneReuseTelemetry::default();
            telemetry.observe(&[7, 8, 9], full_scene_reuse_observation(None));
            telemetry.observe(&[7, 8, 10], full_scene_reuse_observation(None));

            assert_eq!(telemetry.hits, 0);
            assert_eq!(telemetry.misses(), 2);
            assert_eq!(telemetry.snapshot_copy_bytes, 6);
        }
        // Phase `scene_reuse_hit_promotes_recency_before_future_eviction`.
        {
            let mut telemetry = SceneReuseTelemetry::default();
            for payload in [b"A".as_slice(), b"B", b"C"] {
                telemetry.observe(payload, full_scene_reuse_observation(None));
            }
            telemetry.observe(b"A", full_scene_reuse_observation(None));
            telemetry.observe(b"D", full_scene_reuse_observation(None));
            telemetry.observe(b"E", full_scene_reuse_observation(None));
            telemetry.observe(b"A", full_scene_reuse_observation(None));

            assert_eq!(telemetry.hits, 2);
            assert_eq!(telemetry.rank_hits, [0, 0, 2, 0]);
            assert!(
                telemetry
                    .snapshots
                    .iter()
                    .any(|snapshot| snapshot.payload.as_ref() == b"A".as_slice())
            );
            assert!(
                telemetry
                    .snapshots
                    .iter()
                    .all(|snapshot| snapshot.payload.as_ref() != b"B".as_slice())
            );
        }
        // Phase `scene_reuse_metadata_changes_count_only_for_matching_payloads`.
        {
            let damage = [vello::FineDamageRect {
                x: 24,
                y: 48,
                width: 80,
                height: 96,
            }];
            let mut telemetry = SceneReuseTelemetry::default();
            telemetry.observe(b"same", full_scene_reuse_observation(None));
            telemetry.observe(
                b"same",
                sparse_scene_reuse_observation(&damage, true, true, 30),
            );
            telemetry.observe(
                b"different",
                sparse_scene_reuse_observation(&damage, false, false, 0),
            );

            assert_eq!(telemetry.hits, 1);
            assert_eq!(telemetry.hit_any_metadata_changed, 1);
            assert_eq!(telemetry.hit_damage_changed, 1);
            assert_eq!(telemetry.hit_mode_changed, 1);
            assert_eq!(telemetry.hit_bounds_changed, 1);
        }
        // Phase `scene_reuse_empty_oversized_invalid_and_recovery_observations_clear_history`.
        {
            let mut telemetry = SceneReuseTelemetry::default();
            telemetry.observe(b"seed", full_scene_reuse_observation(None));
            telemetry.observe(b"", full_scene_reuse_observation(None));
            assert_eq!(telemetry.empty_observations, 1);
            assert!(telemetry.snapshots.is_empty());

            telemetry.observe(b"seed", full_scene_reuse_observation(None));
            let oversized_payload = vec![0_u8; MAX_SCENE_REUSE_PAYLOAD_BYTES + 1];
            telemetry.observe(&oversized_payload, full_scene_reuse_observation(None));
            assert_eq!(telemetry.oversized_observations, 1);
            assert!(telemetry.snapshots.is_empty());

            telemetry.observe(b"seed", full_scene_reuse_observation(None));
            let oversized_damage = vec![
                vello::FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                };
                super::MAX_SCENE_REUSE_DAMAGE_RECTS + 1
            ];
            telemetry.observe(
                b"oversized-metadata",
                full_scene_reuse_observation(Some(&oversized_damage)),
            );
            assert_eq!(telemetry.oversized_observations, 2);
            assert!(telemetry.snapshots.is_empty());

            telemetry.observe(b"seed", full_scene_reuse_observation(None));
            let invalid_damage = [vello::FineDamageRect {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            }];
            telemetry.observe(
                b"invalid",
                full_scene_reuse_observation(Some(&invalid_damage)),
            );
            assert_eq!(telemetry.invalid_metadata_observations, 1);
            assert!(telemetry.snapshots.is_empty());

            telemetry.observe(b"seed", full_scene_reuse_observation(None));
            telemetry.observe(
                b"recovery",
                SceneReuseObservation::from_paint(
                    None,
                    2,
                    1,
                    1,
                    Some(vello::SparseRenderStats::default()),
                ),
            );
            assert_eq!(telemetry.recovery_observations, 1);
            assert!(telemetry.snapshots.is_empty());
        }
        // Phase `scene_reuse_payload_and_entry_caps_are_bounded_before_snapshot_copy`.
        {
            let mut telemetry = SceneReuseTelemetry::default();
            for value in 0_u8..5 {
                let payload = vec![value; MAX_SCENE_REUSE_PAYLOAD_BYTES];
                telemetry.observe(&payload, full_scene_reuse_observation(None));
                assert!(telemetry.snapshots.len() <= MAX_SCENE_REUSE_SNAPSHOTS);
                assert!(telemetry.retained_payload_bytes <= MAX_SCENE_REUSE_RETAINED_BYTES);
            }

            assert_eq!(telemetry.snapshots.len(), MAX_SCENE_REUSE_SNAPSHOTS);
            assert_eq!(
                telemetry.retained_payload_bytes,
                MAX_SCENE_REUSE_RETAINED_BYTES
            );
            assert_eq!(telemetry.peak_payload_bytes, MAX_SCENE_REUSE_RETAINED_BYTES);
            assert_eq!(
                telemetry.snapshot_copy_bytes,
                u128::try_from(MAX_SCENE_REUSE_PAYLOAD_BYTES)
                    .expect("payload cap fits u128")
                    .saturating_mul(5)
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn diagnostic_aggregation_keeps_every_phase_and_exact_timestamps() {
        // Phase `diagnostic_phase_aggregation_keeps_every_phase_and_byte`.
        {
            let mut totals = BenchmarkPhaseTotals::default();
            totals.add_sample(BenchmarkPhaseTiming {
                input_us: 1,
                registry_us: 2,
                layout_us: 3,
                scene_us: 4,
                bridge: BridgePhaseTiming {
                    submit_us: 5,
                    completed_readback_map_us: 6,
                    settle_us: 7,
                    cpu_copy_swizzle_us: 8,
                    readback_bytes: 9,
                    cpu_output_bytes: 10,
                    backdrop_us: 0,
                },
            });
            totals.add_sample(BenchmarkPhaseTiming {
                input_us: 10,
                registry_us: 20,
                layout_us: 30,
                scene_us: 40,
                bridge: BridgePhaseTiming {
                    submit_us: 50,
                    completed_readback_map_us: 60,
                    settle_us: 70,
                    cpu_copy_swizzle_us: 80,
                    readback_bytes: 90,
                    cpu_output_bytes: 100,
                    backdrop_us: 0,
                },
            });
            assert_eq!(totals.input_us, 11);
            assert_eq!(totals.registry_us, 22);
            assert_eq!(totals.layout_us, 33);
            assert_eq!(totals.scene_us, 44);
            assert_eq!(totals.submit_us, 55);
            assert_eq!(totals.completed_readback_map_us, 66);
            assert_eq!(totals.settle_us, 77);
            assert_eq!(totals.cpu_copy_swizzle_us, 88);
            assert_eq!(totals.readback_bytes, 99);
            assert_eq!(totals.cpu_output_bytes, 110);
        }
        // Phase `sparse_coarse_aggregation_uses_returned_submission_stats`.
        {
            let mut totals = RenderAttemptTotals {
                full: 1,
                recovery: 1,
                ..RenderAttemptTotals::default()
            };
            assert_eq!(totals.canonical_segment_order_selected_full, 0);
            assert_eq!(totals.canonical_segment_order_selected_sparse, 0);
            assert_eq!(totals.area_ppt8_selected_full, 0);
            assert_eq!(totals.area_ppt8_selected_sparse, 0);
            assert_eq!(totals.clip_single_group_selected_full, 0);
            assert_eq!(totals.clip_single_group_selected_sparse, 0);
            totals.record_canonical_segment_order_attempts(2, 1);
            totals.record_area_ppt8_attempts(3, 2);
            totals.record_clip_single_group_attempts(4, 3);
            assert_eq!(totals.solid_fuse_selected_sparse, 0);
            assert_eq!(totals.segment_row_mask_selected_sparse, 0);
            totals.record_sparse_stats(vello::SparseRenderStats {
                coarse_adaptive: true,
                coarse_active_bins: true,
                coarse_native_bins: 64,
                coarse_selected_native_bins: 12,
                path_tile_bounds_applied: true,
                path_tile_bounds_tiles: 36,
                coarse_opaque_prune_selected: true,
                solid_fuse_selected: true,
                segment_row_mask_selected: true,
                canonical_segment_order_shader_selected: false,
                fine_quad_skip_applied: true,
                fine_quad_total_quads: 64,
                fine_quad_active_quads: 20,
                ..vello::SparseRenderStats::default()
            });
            totals.record_sparse_stats(vello::SparseRenderStats {
                coarse_adaptive: true,
                coarse_active_bins: false,
                coarse_native_bins: 64,
                coarse_selected_native_bins: 40,
                path_tile_bounds_applied: false,
                path_tile_bounds_tiles: 99,
                coarse_opaque_prune_selected: false,
                solid_fuse_selected: false,
                segment_row_mask_selected: false,
                canonical_segment_order_shader_selected: false,
                fine_quad_skip_applied: false,
                fine_quad_total_quads: 64,
                fine_quad_active_quads: 1,
                ..vello::SparseRenderStats::default()
            });
            // A forced legacy active-bin submission remains actual active work, but it is not an
            // AUTO adaptive decision.
            totals.record_sparse_stats(vello::SparseRenderStats {
                coarse_adaptive: false,
                coarse_active_bins: true,
                coarse_native_bins: 64,
                coarse_selected_native_bins: 64,
                path_tile_bounds_applied: true,
                path_tile_bounds_tiles: 12,
                coarse_opaque_prune_selected: true,
                solid_fuse_selected: false,
                segment_row_mask_selected: false,
                canonical_segment_order_shader_selected: false,
                fine_quad_skip_applied: true,
                fine_quad_total_quads: 64,
                fine_quad_active_quads: 64,
                ..vello::SparseRenderStats::default()
            });

            assert_eq!(totals.adaptive_sparse, 2);
            assert_eq!(totals.active_sparse, 2);
            assert_eq!(totals.dense_fallback_sparse, 1);
            assert_eq!(totals.coarse_native_bins, 192);
            assert_eq!(totals.coarse_selected_native_bins, 116);
            assert_eq!(totals.path_tile_bounds_sparse, 2);
            assert_eq!(totals.path_tile_bounds_tiles, 48);
            assert_eq!(totals.area_ppt8_selected_full, 3);
            assert_eq!(totals.area_ppt8_selected_sparse, 2);
            assert_eq!(totals.clip_single_group_selected_full, 4);
            assert_eq!(totals.clip_single_group_selected_sparse, 3);
            assert_eq!(totals.coarse_opaque_prune_selected_sparse, 2);
            assert_eq!(totals.solid_fuse_selected_sparse, 1);
            assert_eq!(totals.segment_row_mask_selected_sparse, 1);
            assert_eq!(totals.fine_quad_skip_sparse, 2);
            assert_eq!(totals.fine_quad_total, 128);
            assert_eq!(totals.fine_quad_active, 84);
            assert_eq!(totals.fine_quad_skipped, 44);
            assert_eq!(totals.full, 1);
            assert_eq!(totals.recovery, 1);
            assert_eq!(totals.canonical_segment_order_selected_full, 2);
            assert_eq!(totals.canonical_segment_order_selected_sparse, 1);
        }
        // Phase `dispatch_timestamp_aggregation_requires_exact_submission_match`.
        {
            let mut totals = GpuDispatchTimingTotals::default();
            let sample = dispatch_sample(41, &[(7, 3, 10), (20, 1, 30)]);
            let status = dispatch_timestamp_status_after_take(Some(&sample), || {
                panic!("a decoded sample must not be pre-classified as pending")
            });
            totals.record_completed_paint(Some(41), status, Some(&sample), 0);

            assert_eq!(totals.expected_submissions, 1);
            assert_eq!(totals.matched_submissions, 1);
            assert_eq!(totals.missing_submissions, 0);
            assert_eq!(totals.stale_submissions, 0);
            assert_eq!(totals.pending_submissions, 0);
            let flatten = totals.stages.get(6).expect("flatten stage is in range");
            let fine = totals.stages.get(19).expect("fine stage is in range");
            assert_eq!(flatten.duration_ns, vec![10]);
            assert_eq!(fine.dispatches, vec![1]);
            let mut fine_durations = fine.duration_ns.clone();
            let summary = summarize_diagnostic_values(&mut fine_durations)
                .expect("matched GPU stage has a duration");
            assert_eq!(summary.mean, 30);
            assert_eq!(summary.p99, 30);
        }
        // Phase `dispatch_timestamp_mismatch_and_unavailable_states_are_never_zero_time`.
        {
            let mut totals = GpuDispatchTimingTotals::default();
            let mismatched = dispatch_sample(8, &[(7, 1, 100)]);
            totals.record_completed_paint(
                Some(9),
                vello::GpuDispatchTimestampStatus::Ready {
                    submission_id: 9,
                    stage_count: 1,
                },
                Some(&mismatched),
                2,
            );
            totals.record_completed_paint(
                None,
                vello::GpuDispatchTimestampStatus::Unsupported,
                None,
                0,
            );

            assert_eq!(totals.expected_submissions, 1);
            assert_eq!(totals.matched_submissions, 0);
            assert_eq!(totals.missing_submissions, 1);
            assert_eq!(totals.stale_submissions, 1);
            assert_eq!(totals.unsupported_submissions, 1);
            assert_eq!(totals.recovery_submissions, 2);
            assert!(
                totals
                    .stages
                    .iter()
                    .all(|stage| stage.duration_ns.is_empty())
            );
        }
        // Phase `dispatch_timestamp_missing_sample_reads_post_take_invalid_status`.
        {
            let mut totals = GpuDispatchTimingTotals::default();
            let status = dispatch_timestamp_status_after_take(None, || {
                vello::GpuDispatchTimestampStatus::Invalid { submission_id: 12 }
            });
            totals.record_completed_paint(Some(12), status, None, 0);

            assert_eq!(totals.expected_submissions, 1);
            assert_eq!(totals.matched_submissions, 0);
            assert_eq!(totals.missing_submissions, 1);
            assert_eq!(totals.invalid_submissions, 1);
            assert_eq!(totals.pending_submissions, 0);
            assert!(
                totals
                    .stages
                    .iter()
                    .all(|stage| stage.duration_ns.is_empty())
            );
        }
    }

    fn dispatch_sample(
        submission_id: u64,
        stages: &[(u16, u32, u128)],
    ) -> vello::GpuDispatchTimestampSample {
        let mut sample_stages = [vello::GpuDispatchStageSample::default(); 32];
        for (index, (stage_id, dispatch_count, duration_ns)) in stages.iter().copied().enumerate() {
            let slot = sample_stages
                .get_mut(index)
                .expect("test stage count fits the fixed Vello sample array");
            *slot = vello::GpuDispatchStageSample {
                stage_id,
                dispatch_count,
                duration_ns,
            };
        }
        vello::GpuDispatchTimestampSample {
            submission_id,
            stage_count: u32::try_from(stages.len()).expect("test stage count fits u32"),
            stages: sample_stages,
        }
    }

    fn correlation_record(
        index: usize,
        timestamp: &ActionGpuTimestampObservation,
    ) -> ActionCorrelationRecord {
        ActionCorrelationRecord {
            index,
            target: ACTION_TARGETS[0],
            wall_us: 77,
            phase: BenchmarkPhaseTiming {
                input_us: 1,
                registry_us: 2,
                layout_us: 3,
                scene_us: 4,
                bridge: BridgePhaseTiming {
                    submit_us: 5,
                    completed_readback_map_us: 6,
                    settle_us: 7,
                    cpu_copy_swizzle_us: 8,
                    readback_bytes: 9,
                    cpu_output_bytes: 10,
                    backdrop_us: 0,
                },
            },
            attempts: 1,
            sparse_attempts: 1,
            full_attempts: 0,
            recovery_attempts: 0,
            sparse_stats: Some(vello::SparseRenderStats {
                fine_tiles: 11,
                fine_pixels: 12,
                written_pixels: 13,
                coarse_pixels: 14,
                canonical_segment_order_shader_selected: false,
                ..vello::SparseRenderStats::default()
            }),
            timestamp: *timestamp,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn action_correlation_joins_exact_samples_and_writes_complete_records() {
        // Phase `action_correlation_joins_wall_phase_and_exact_gpu_sample`.
        {
            let sample = dispatch_sample(
                41,
                &[
                    (17, 3, 10),
                    (20, 4, 30),
                    (7, 1, 5),
                    (0, 2, 7),
                    (u16::MAX, 1, 11),
                ],
            );
            let status = dispatch_timestamp_status_after_take(Some(&sample), || {
                panic!("decoded timestamp sample must be authoritative")
            });
            let timestamp =
                ActionGpuTimestampObservation::from_sample(Some(41), status, Some(&sample));
            let mut profile = ActionCorrelationProfile::new();
            let record = correlation_record(7, &timestamp);
            profile.record(&record).expect("one bounded profile record");

            let record = profile.records.first().expect("stored joined record");
            assert_eq!(record.index, 7);
            assert_eq!(record.target, ACTION_TARGETS[0]);
            assert_eq!(record.wall_us, 77);
            assert_eq!(record.phase.bridge.completed_readback_map_us, 6);
            assert_eq!(record.sparse_stats.map(|stats| stats.fine_tiles), Some(11));
            assert!(record.timestamp.exact_match);
            assert!(!record.timestamp.sample_missing);
            assert!(!record.timestamp.stale_sample);
            assert_eq!(record.timestamp.expected_submission_id, Some(41));
            assert_eq!(record.timestamp.observed_submission_id, Some(41));
            assert_eq!(record.timestamp.coarse_ns, Some(10));
            assert_eq!(record.timestamp.fine_ns, Some(30));
            assert_eq!(record.timestamp.total_timed_stage_ns, Some(63));
            assert_eq!(record.timestamp.other_stage_ns, Some(5));
            assert_eq!(record.timestamp.unknown_stage_ns, Some(18));
            assert_eq!(record.timestamp.coarse_dispatches, Some(3));
            assert_eq!(record.timestamp.fine_dispatches, Some(4));
            let breakdown = dispatch_stage_breakdown(&sample);
            assert_eq!(
                breakdown.total_timed_stage_ns, 63,
                "the exact matched sample retains coarse, fine, other, zero-id unknown, and out-of-catalog unknown stage time"
            );
            assert_eq!(
                breakdown.total_timed_stage_ns,
                breakdown
                    .coarse_ns
                    .saturating_add(breakdown.fine_ns)
                    .saturating_add(breakdown.other_stage_ns)
                    .saturating_add(breakdown.unknown_stage_ns),
                "the unknown class is a subset of the total, not an omitted duration"
            );
        }
        // Phase `action_correlation_marks_stale_missing_and_recovery_without_backfill`.
        {
            let stale_sample = dispatch_sample(8, &[(17, 1, 100), (20, 1, 200)]);
            let stale = ActionGpuTimestampObservation::from_sample(
                Some(9),
                vello::GpuDispatchTimestampStatus::Ready {
                    submission_id: 9,
                    stage_count: 2,
                },
                Some(&stale_sample),
            );
            assert!(!stale.exact_match);
            assert!(stale.stale_sample);
            assert!(!stale.sample_missing);
            assert_eq!(stale.coarse_ns, None);
            assert_eq!(stale.fine_ns, None);
            assert_eq!(stale.total_timed_stage_ns, None);
            assert_eq!(stale.other_stage_ns, None);
            assert_eq!(stale.unknown_stage_ns, None);
            assert_eq!(stale.coarse_dispatches, None);
            assert_eq!(stale.fine_dispatches, None);

            let missing = ActionGpuTimestampObservation::from_sample(
                Some(12),
                vello::GpuDispatchTimestampStatus::Pending { submission_id: 12 },
                None,
            );
            assert!(!missing.exact_match);
            assert!(missing.sample_missing);
            assert!(!missing.stale_sample);
            assert_eq!(
                gpu_dispatch_timestamp_status_name(missing.status),
                "pending",
                "a pending final slot remains explicit rather than becoming a zero-time GPU result"
            );

            let mut recovered = correlation_record(8, &stale);
            recovered.attempts = 2;
            recovered.sparse_attempts = 1;
            recovered.full_attempts = 1;
            recovered.recovery_attempts = 1;
            let mut profile = ActionCorrelationProfile::new();
            profile.record(&recovered).expect("recovery action record");
            let record = profile.records.first().expect("stored recovery record");
            assert_eq!(record.recovery_attempts, 1);
            assert_eq!(record.timestamp.coarse_ns, None);
            assert_eq!(record.timestamp.fine_ns, None);
            assert_eq!(record.timestamp.total_timed_stage_ns, None);
        }
        // Phase `action_correlation_keeps_disabled_unsupported_invalid_and_idle_states_explicit`.
        {
            let disabled = ActionGpuTimestampObservation::from_sample(
                None,
                vello::GpuDispatchTimestampStatus::Disabled,
                None,
            );
            let unsupported = ActionGpuTimestampObservation::from_sample(
                None,
                vello::GpuDispatchTimestampStatus::Unsupported,
                None,
            );
            let invalid = ActionGpuTimestampObservation::from_sample(
                Some(12),
                vello::GpuDispatchTimestampStatus::Invalid { submission_id: 12 },
                None,
            );
            let no_expected_id = ActionGpuTimestampObservation::from_sample(
                None,
                vello::GpuDispatchTimestampStatus::Idle,
                None,
            );

            for observation in [disabled, unsupported, invalid, no_expected_id] {
                assert!(!observation.exact_match);
                assert_eq!(observation.coarse_ns, None);
                assert_eq!(observation.fine_ns, None);
                assert_eq!(observation.total_timed_stage_ns, None);
                assert_eq!(observation.other_stage_ns, None);
                assert_eq!(observation.unknown_stage_ns, None);
                assert_eq!(observation.coarse_dispatches, None);
                assert_eq!(observation.fine_dispatches, None);
            }
            assert!(!disabled.sample_missing);
            assert!(!unsupported.sample_missing);
            assert!(invalid.sample_missing);
            assert!(!no_expected_id.sample_missing);
            assert_eq!(
                gpu_dispatch_timestamp_status_name(invalid.status),
                "invalid"
            );
            assert_eq!(
                gpu_dispatch_timestamp_status_name(no_expected_id.status),
                "idle"
            );
        }
        // Phase `action_correlation_is_profile_gated_and_bounded`.
        {
            assert!(!action_correlation_enabled(false));
            assert!(action_correlation_enabled(true));

            let timestamp = ActionGpuTimestampObservation::from_sample(
                None,
                vello::GpuDispatchTimestampStatus::Disabled,
                None,
            );
            let mut profile = ActionCorrelationProfile::new();
            for index in 0..MAX_ACTION_CORRELATION_RECORDS {
                let record = correlation_record(index, &timestamp);
                profile
                    .record(&record)
                    .expect("fixed all-action correlation capacity");
            }
            assert_eq!(profile.records.len(), MAX_ACTION_CORRELATION_RECORDS);
            let beyond_capacity = correlation_record(MAX_ACTION_CORRELATION_RECORDS, &timestamp);
            assert!(
                profile.record(&beyond_capacity).is_err(),
                "the diagnostic must not retain more than one record per configured action"
            );
        }
        // Phase `action_correlation_writer_reports_complete_records_and_propagates_errors`.
        {
            let timestamp = ActionGpuTimestampObservation::from_sample(
                None,
                vello::GpuDispatchTimestampStatus::Disabled,
                None,
            );
            let mut profile = ActionCorrelationProfile::new();
            let record = correlation_record(3, &timestamp);
            profile.record(&record).expect("bounded report record");
            let mut output = Vec::new();
            write_action_correlation_profile(&mut output, &profile)
                .expect("in-memory diagnostic report write");
            let output = String::from_utf8(output).expect("report is UTF-8 diagnostics");
            assert!(output.contains("records=1"));
            assert!(output.contains("action-correlation index=3"));
            assert!(output.contains("gpu_total_timed_stage_ns=None"));
            assert!(output.contains("gpu_unknown_stage_ns=None"));

            struct FailingWriter;

            impl Write for FailingWriter {
                fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
                    Err(io::Error::other("injected report write failure"))
                }

                fn flush(&mut self) -> io::Result<()> {
                    Ok(())
                }
            }

            assert!(write_action_correlation_profile(&mut FailingWriter, &profile).is_err());
        }
        // Phase `profile_output_file_is_created_new_and_never_overwritten`.
        {
            let stamp =
                tilo_clock::try_unix_now_ns().expect("the test clock is after the Unix epoch");
            let path = std::env::temp_dir().join(format!(
                "tilo-redraw-bench-profile-output-{}-{stamp}",
                std::process::id()
            ));
            {
                let mut writer = open_profile_output(&path).expect("first output path creation");
                writer
                    .write_all(b"existing report")
                    .expect("write first output contents");
                writer.flush().expect("flush first output contents");
            }
            let second = open_profile_output(&path);
            assert!(
                second.is_err_and(|error| error.kind() == io::ErrorKind::AlreadyExists),
                "diagnostic output must refuse to overwrite an existing artifact"
            );
            std::fs::remove_file(&path).expect("remove test-only diagnostic artifact");
        }
    }

    #[test]
    fn benchmark_actions_alternate_quick_settings_and_produce_bounded_damage() {
        // Phase `benchmark_actions_alternate_quick_settings_open_and_outside_dismissal`.
        {
            let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())
                .expect("explicit populated shell fixture");
            let _initial_scene = frame.paint();
            let mut document = AutoCommit::new();
            let modifiers = Modifiers::default();
            let mut focus: Option<NodeId> = None;

            // This mirrors the benchmark action path exactly but deliberately stops before the GPU
            // bridge: it establishes which shell state each sampled redraw actually represents.
            for sample_index in 0..12 {
                let target = *ACTION_TARGETS
                    .get(sample_index % ACTION_TARGETS.len())
                    .expect("benchmark action target list is non-empty");
                let rect = frame
                    .widget_registry()
                    .get(target)
                    .copied()
                    .filter(|rect| rect.area() > 0.0)
                    .expect("benchmark target must be painted");
                dispatch_click(
                    &mut frame,
                    target,
                    rect.center(),
                    modifiers,
                    &mut document,
                    &mut focus,
                )
                .expect("benchmark click must be handled");
                frame.relayout();
                let _scene = frame.paint();

                let overlays = frame.chrome().overlays;
                eprintln!(
                    "redraw_bench action {sample_index} target={target}: quick_settings={} overview={}",
                    overlays.quick_settings, overlays.overview
                );
                assert_eq!(overlays.quick_settings, sample_index.is_multiple_of(2));
                assert!(
                    !overlays.overview,
                    "the overview target dismisses open quick settings before TopBarChrome receives it"
                );
            }
        }
        // Phase `benchmark_actions_produce_bounded_damage`.
        {
            let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())
                .expect("explicit populated shell fixture");
            let _initial = frame.paint_with_damage();
            let mut document = AutoCommit::new();
            let modifiers = Modifiers::default();
            let mut focus: Option<NodeId> = None;
            let mut sparse_frames = 0_usize;
            let mut covered_pixels = 0_u128;
            let full_pixels = u128::from(WIDTH) * u128::from(HEIGHT);

            for sample_index in 0..12 {
                let target = ACTION_TARGETS[sample_index % ACTION_TARGETS.len()];
                let rect = frame
                    .widget_registry()
                    .get(target)
                    .copied()
                    .filter(|rect| rect.area() > 0.0)
                    .expect("benchmark target must be painted");
                dispatch_click(
                    &mut frame,
                    target,
                    rect.center(),
                    modifiers,
                    &mut document,
                    &mut focus,
                )
                .expect("benchmark click must be handled");
                frame.relayout();
                let painted = frame.paint_with_damage();
                if let Some(regions) = painted.damage_regions.as_deref() {
                    sparse_frames = sparse_frames.saturating_add(1);
                    for region in regions {
                        covered_pixels = covered_pixels.saturating_add(
                            u128::from(region.width).saturating_mul(u128::from(region.height)),
                        );
                    }
                }
            }
            eprintln!(
                "redraw_bench retained classification: sparse_frames={sparse_frames}/12 covered_pixels={covered_pixels} full_pixels/frame={full_pixels}"
            );
            assert!(
                sparse_frames > 0,
                "the production root should expose bounded changes"
            );
            assert!(
                covered_pixels
                    < full_pixels.saturating_mul(
                        u128::try_from(sparse_frames).expect("twelve frame count fits u128"),
                    ),
                "each classified sparse frame must cover less than the native output"
            );
        }
    }

    #[test]
    fn native_4k_retained_pixels_match_dense_oracle_for_benchmark_actions() {
        let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())
            .expect("explicit populated shell fixture");
        let width = usize::try_from(WIDTH).expect("native width fits usize");
        let height = usize::try_from(HEIGHT).expect("native height fits usize");
        let pitch = width.checked_mul(4).expect("native pitch fits usize");
        let len = pitch.checked_mul(height).expect("native output fits usize");
        let mut retained = VelloDrmBridge::new(WIDTH, HEIGHT).expect("retained bridge");
        let mut dense = VelloDrmBridge::new(WIDTH, HEIGHT).expect("dense oracle bridge");
        // An exact comparison needs a repeatable oracle. Unordered, a tile's
        // segments sum in GPU scheduling order, so two renders of one scene can
        // differ by one level on a multi-threaded device
        // (`native_4k_fixed_quick_settings_pose_225_segment_order_is_causal`).
        for bridge in [&mut retained, &mut dense] {
            bridge
                .enable_canonical_segment_order()
                .expect("canonical segment order");
            assert!(bridge.canonical_segment_order_available());
        }
        retained.set_phase_timing_enabled(true);
        let mut retained_pixels = vec![0_u8; len];
        let mut dense_pixels = vec![0_u8; len];
        let mut document = AutoCommit::new();
        let modifiers = Modifiers::default();
        let mut focus: Option<NodeId> = None;
        let mut sparse_attempts = 0_u64;
        let mut full_attempts = 0_u64;
        let mut readback_bytes = 0_u128;
        let opaque_prune_requested = retained.sparse_opaque_prune_requested();
        if opaque_prune_requested {
            assert!(
                retained.sparse_opaque_prune_available(),
                "a requested sparse opaque prune renderer must expose its selected shader path"
            );
        }
        let solid_fuse_requested = retained.sparse_solid_fuse_requested();
        let sparse_solid_fuse_preempted =
            retained.sparse_fine_quad_skip_requested() || opaque_prune_requested;
        if solid_fuse_requested && !sparse_solid_fuse_preempted {
            assert!(
                retained.sparse_solid_fuse_available(),
                "a requested sparse solid-fuse renderer must expose its selected shader path"
            );
        }
        let segment_row_mask_requested = retained.sparse_segment_row_mask_requested();
        let segment_row_mask_available = retained.sparse_segment_row_mask_available();
        let canonical_segment_order_requested = retained.canonical_segment_order_requested();
        let canonical_segment_order_available = retained.canonical_segment_order_available();
        let area_ppt8_requested = retained.area_ppt8_requested();
        let area_ppt8_available = retained.area_ppt8_available();
        let area_ppt8_preempted = retained.sparse_fine_quad_skip_requested()
            || opaque_prune_requested
            || solid_fuse_requested
            || segment_row_mask_requested
            || canonical_segment_order_requested;
        let mut opaque_prune_selected_submissions = 0_u64;
        let mut solid_fuse_selected_submissions = 0_u64;
        let mut segment_row_mask_selected_submissions = 0_u64;
        let mut canonical_segment_order_selected_full_attempts = 0_u64;
        let mut canonical_segment_order_selected_sparse_attempts = 0_u64;
        let mut area_ppt8_selected_full_attempts = 0_u64;
        let mut area_ppt8_selected_sparse_attempts = 0_u64;
        let mut active_bounded_sparse_submissions = 0_u64;

        for sample_index in 0..=12 {
            if sample_index != 0 {
                let target = ACTION_TARGETS[(sample_index - 1) % ACTION_TARGETS.len()];
                let rect = frame
                    .widget_registry()
                    .get(target)
                    .copied()
                    .filter(|rect| rect.area() > 0.0)
                    .expect("benchmark target must be painted");
                dispatch_click(
                    &mut frame,
                    target,
                    rect.center(),
                    modifiers,
                    &mut document,
                    &mut focus,
                )
                .expect("benchmark click must be handled");
                frame.relayout();
            }
            // Both bridges consume this one candidate scene. The dense bridge is therefore a
            // same-time pixel oracle rather than a second animation/layout sample.
            let candidate = frame.paint_with_damage();
            retained
                .paint_damage_into_xrgb8888(
                    &candidate.scene,
                    SceneDamage {
                        revision: candidate.revision,
                        previous_revision: candidate.previous_revision,
                        regions: candidate.damage_regions.as_deref(),
                    },
                    &mut retained_pixels,
                    pitch,
                )
                .expect("retained native paint");
            canonical_segment_order_selected_full_attempts =
                canonical_segment_order_selected_full_attempts.saturating_add(
                    u64::try_from(retained.last_canonical_segment_order_full_attempts())
                        .expect("canonical full attempt count fits u64"),
                );
            canonical_segment_order_selected_sparse_attempts =
                canonical_segment_order_selected_sparse_attempts.saturating_add(
                    u64::try_from(retained.last_canonical_segment_order_sparse_attempts())
                        .expect("canonical sparse attempt count fits u64"),
                );
            area_ppt8_selected_full_attempts = area_ppt8_selected_full_attempts.saturating_add(
                u64::try_from(retained.last_area_ppt8_full_attempts())
                    .expect("Area PPT8 full attempt count fits u64"),
            );
            area_ppt8_selected_sparse_attempts = area_ppt8_selected_sparse_attempts.saturating_add(
                u64::try_from(retained.last_area_ppt8_sparse_attempts())
                    .expect("Area PPT8 sparse attempt count fits u64"),
            );
            if let Some(stats) = retained.last_sparse_render_stats() {
                if stats.coarse_opaque_prune_selected {
                    opaque_prune_selected_submissions =
                        opaque_prune_selected_submissions.saturating_add(1);
                }
                if stats.solid_fuse_selected {
                    solid_fuse_selected_submissions =
                        solid_fuse_selected_submissions.saturating_add(1);
                }
                if stats.coarse_active_bins && stats.path_tile_bounds_applied {
                    active_bounded_sparse_submissions =
                        active_bounded_sparse_submissions.saturating_add(1);
                }
                if stats.segment_row_mask_selected {
                    assert!(
                        stats.coarse_active_bins && stats.path_tile_bounds_applied,
                        "row-mask selection must report the required active-bounds path"
                    );
                    segment_row_mask_selected_submissions =
                        segment_row_mask_selected_submissions.saturating_add(1);
                }
            }
            dense
                .paint_into_xrgb8888(&candidate.scene, &mut dense_pixels, pitch)
                .expect("dense native oracle paint");
            assert_eq!(
                dense.last_sparse_render_attempts(),
                0,
                "dense oracle must not attempt sparse rendering"
            );
            assert!(
                dense.last_sparse_render_stats().is_none(),
                "dense oracle must not report sparse shader selection"
            );
            let original_mismatch = native_pixel_mismatch(&retained_pixels, &dense_pixels, WIDTH);
            if original_mismatch.mismatches != 0 {
                let original_oracle_packed_scene = dense.diagnostic_packed_scene().to_vec();
                report_native_oracle_failure(
                    NativeOracleBridges {
                        retained: &mut retained,
                        dense: &mut dense,
                    },
                    NativeOracleFailure {
                        scene: &candidate.scene,
                        revision: candidate.revision,
                        previous_revision: candidate.previous_revision,
                        damage: candidate.damage_regions.as_deref(),
                        original_retained_pixels: &retained_pixels,
                        dense_pixels: &dense_pixels,
                        original_oracle_packed_scene,
                        len,
                        pitch,
                    },
                    &original_mismatch,
                    sample_index,
                );
            }
            assert_native_pixel_mismatch_is_zero(&original_mismatch, sample_index);
            sparse_attempts = sparse_attempts.saturating_add(
                u64::try_from(retained.last_sparse_render_attempts())
                    .expect("sparse attempt count fits u64"),
            );
            full_attempts = full_attempts.saturating_add(
                u64::try_from(retained.last_full_render_attempts())
                    .expect("full attempt count fits u64"),
            );
            readback_bytes = readback_bytes.saturating_add(u128::from(
                retained
                    .last_phase_timing()
                    .expect("profile timing enabled")
                    .readback_bytes,
            ));
        }
        eprintln!(
            "redraw_bench native 4K oracle: sparse_attempts={sparse_attempts} full_attempts={full_attempts} opaque_prune_shader_selected_submissions={opaque_prune_selected_submissions} solid_fuse_shader_selected_submissions={solid_fuse_selected_submissions} segment_row_mask_shader_selected_submissions={segment_row_mask_selected_submissions} canonical_segment_order_shader_selected_full_attempts={canonical_segment_order_selected_full_attempts} canonical_segment_order_shader_selected_sparse_attempts={canonical_segment_order_selected_sparse_attempts} area_ppt8_shader_selected_full_attempts={area_ppt8_selected_full_attempts} area_ppt8_shader_selected_sparse_attempts={area_ppt8_selected_sparse_attempts} readback_payload_bytes={readback_bytes}"
        );
        assert!(
            sparse_attempts > 0,
            "benchmark candidates must exercise sparse paint"
        );
        if opaque_prune_requested {
            assert!(
                opaque_prune_selected_submissions > 0,
                "a requested available opaque-prune shader must be selected by at least one benchmark sparse submission"
            );
        } else {
            assert_eq!(
                opaque_prune_selected_submissions, 0,
                "opaque prune must not be reported without the renderer opt-in"
            );
        }
        if solid_fuse_requested && !sparse_solid_fuse_preempted {
            assert!(
                solid_fuse_selected_submissions > 0,
                "a requested available solid-fuse shader must be selected by at least one benchmark sparse submission"
            );
        } else {
            assert_eq!(
                solid_fuse_selected_submissions, 0,
                "solid fuse must be unselected without its sole opt-in or when a higher-precedence sparse mode is requested"
            );
        }
        if segment_row_mask_requested
            && segment_row_mask_available
            && active_bounded_sparse_submissions > 0
        {
            assert!(
                segment_row_mask_selected_submissions > 0,
                "an available requested row-mask shader must be selected by an active-bounds sparse benchmark submission"
            );
        } else if !segment_row_mask_requested || !segment_row_mask_available {
            assert_eq!(
                segment_row_mask_selected_submissions, 0,
                "row mask must not be reported without an available renderer opt-in"
            );
        }
        assert!(
            canonical_segment_order_requested
                && canonical_segment_order_available
                && canonical_segment_order_selected_full_attempts > 0,
            "the dense oracle must render in canonical segment order"
        );
        if area_ppt8_requested && area_ppt8_available && !area_ppt8_preempted {
            assert!(
                area_ppt8_selected_full_attempts.saturating_add(area_ppt8_selected_sparse_attempts)
                    > 0,
                "an available requested Area PPT8 shader must be selected by at least one benchmark submission"
            );
        } else {
            assert_eq!(
                area_ppt8_selected_full_attempts, 0,
                "Area PPT8 full selection must not be reported without an available renderer opt-in"
            );
            assert_eq!(
                area_ppt8_selected_sparse_attempts, 0,
                "Area PPT8 sparse selection must not be reported without an available renderer opt-in"
            );
        }
    }

    #[test]
    fn native_4k_benchmark_actions_use_one_and_two_target_xrgb_history() {
        let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())
            .expect("explicit populated shell fixture");
        let width = usize::try_from(WIDTH).expect("native width fits usize");
        let height = usize::try_from(HEIGHT).expect("native height fits usize");
        let pitch = width.checked_mul(4).expect("native pitch fits usize");
        let len = pitch.checked_mul(height).expect("native output fits usize");
        let mut one_bridge = VelloDrmBridge::new(WIDTH, HEIGHT).expect("one-target bridge");
        let mut two_bridge = VelloDrmBridge::new(WIDTH, HEIGHT).expect("two-target bridge");
        let one_identity = one_bridge
            .xrgb_history_identity(pitch, 1)
            .expect("one-target identity");
        let two_identity = two_bridge
            .xrgb_history_identity(pitch, 2)
            .expect("two-target identity");
        let mut one_history = XrgbTargetHistory::new(1);
        let mut two_history = XrgbTargetHistory::new(2);
        let mut one_destination = vec![0_u8; len];
        let mut two_destinations = [vec![0xA7; len], vec![0xB8; len]];
        let mut document = AutoCommit::new();
        let modifiers = Modifiers::default();
        let mut focus: Option<NodeId> = None;
        let mut one_partial = 0_u64;
        let mut two_partial = 0_u64;

        for sample_index in 0..=12 {
            if sample_index != 0 {
                let target = ACTION_TARGETS[(sample_index - 1) % ACTION_TARGETS.len()];
                let rect = frame
                    .widget_registry()
                    .get(target)
                    .copied()
                    .filter(|rect| rect.area() > 0.0)
                    .expect("benchmark target must be painted");
                dispatch_click(
                    &mut frame,
                    target,
                    rect.center(),
                    modifiers,
                    &mut document,
                    &mut focus,
                )
                .expect("benchmark click must be handled");
                frame.relayout();
            }
            let frame_paint = frame.paint_with_damage();
            let damage = SceneDamage {
                revision: frame_paint.revision,
                previous_revision: frame_paint.previous_revision,
                regions: frame_paint.damage_regions.as_deref(),
            };

            let transaction = one_history.prepare(
                0,
                Some(one_identity),
                damage.revision,
                damage.previous_revision,
                damage.regions,
                &[],
            );
            let one_write = one_bridge
                .paint_damage_into_xrgb8888_with_history(
                    &frame_paint.scene,
                    damage,
                    &transaction,
                    &mut one_destination,
                    pitch,
                )
                .expect("one-target history paint");
            transaction.commit(if one_write.damage_admitted {
                XrgbHistoryCommit::DamageAccepted
            } else {
                XrgbHistoryCommit::FullBoundary
            });
            if one_write.mode == XrgbCpuWriteMode::Partial {
                one_partial = one_partial.saturating_add(1);
            }

            let target_index = sample_index % two_destinations.len();
            let transaction = two_history.prepare(
                target_index,
                Some(two_identity),
                damage.revision,
                damage.previous_revision,
                damage.regions,
                &[],
            );
            let two_write = two_bridge
                .paint_damage_into_xrgb8888_with_history(
                    &frame_paint.scene,
                    damage,
                    &transaction,
                    two_destinations
                        .get_mut(target_index)
                        .expect("two-target slot exists"),
                    pitch,
                )
                .expect("two-target history paint");
            transaction.commit(if two_write.damage_admitted {
                XrgbHistoryCommit::DamageAccepted
            } else {
                XrgbHistoryCommit::FullBoundary
            });
            if two_write.mode == XrgbCpuWriteMode::Partial {
                two_partial = two_partial.saturating_add(1);
            }
        }

        assert!(
            one_partial > 0,
            "the native action sequence must exercise a real one-target partial CPU write"
        );
        assert!(
            two_partial > 0,
            "the native action sequence must exercise a real two-target partial CPU write"
        );
    }

    #[test]
    #[ignore = "explicit native-4K shared-coarse fine diagnostic"]
    fn native_4k_shared_coarse_damage_pixels_are_exact() {
        let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())
            .expect("explicit populated shell fixture");
        let width = usize::try_from(WIDTH).expect("native width fits usize");
        let height = usize::try_from(HEIGHT).expect("native height fits usize");
        let pitch = width.checked_mul(4).expect("native pitch fits usize");
        let len = pitch.checked_mul(height).expect("native output fits usize");
        let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).expect("shared-coarse bridge");
        let mut full_pixels = vec![0_u8; len];
        let mut sparse_pixels = vec![0_u8; len];
        let mut document = AutoCommit::new();
        let modifiers = Modifiers::default();
        let mut focus: Option<NodeId> = None;
        let full_damage = [vello::FineDamageRect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        }];

        for sample_index in 0..=12 {
            if sample_index != 0 {
                let target = ACTION_TARGETS[(sample_index - 1) % ACTION_TARGETS.len()];
                let rect = frame
                    .widget_registry()
                    .get(target)
                    .copied()
                    .filter(|rect| rect.area() > 0.0)
                    .expect("benchmark target must be painted");
                dispatch_click(
                    &mut frame,
                    target,
                    rect.center(),
                    modifiers,
                    &mut document,
                    &mut focus,
                )
                .expect("benchmark click must be handled");
                frame.relayout();
            }
            let candidate = frame.paint_with_damage();
            let damage = candidate.damage_regions.as_deref().unwrap_or(&full_damage);
            let (stats, mismatch) = render_shared_coarse_diagnostic(
                &mut bridge,
                &candidate.scene,
                damage,
                &mut full_pixels,
                &mut sparse_pixels,
                pitch,
            )
            .unwrap_or_else(|error| {
                panic!("shared-coarse diagnostic candidate {sample_index} failed: {error}")
            });
            eprintln!(
                "redraw_bench shared-coarse candidate={sample_index}: stats={stats:?} damage={damage:?}"
            );
            assert_native_pixel_mismatch_is_zero(&mismatch, sample_index);
        }
    }

    #[test]
    #[ignore = "explicit native-4K dense repeatability diagnostic"]
    fn native_4k_dense_oracle_is_repeatable_within_and_across_contexts() {
        let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())
            .expect("explicit populated shell fixture");
        let mut repeatability = DenseRepeatabilityHarness::new();
        let mut document = AutoCommit::new();
        let modifiers = Modifiers::default();
        let mut focus: Option<NodeId> = None;

        for sample_index in 0..=12 {
            if sample_index != 0 {
                let target = ACTION_TARGETS[(sample_index - 1) % ACTION_TARGETS.len()];
                let rect = frame
                    .widget_registry()
                    .get(target)
                    .copied()
                    .filter(|rect| rect.area() > 0.0)
                    .expect("benchmark target must be painted");
                dispatch_click(
                    &mut frame,
                    target,
                    rect.center(),
                    modifiers,
                    &mut document,
                    &mut focus,
                )
                .expect("benchmark click must be handled");
                frame.relayout();
            }
            // Build exactly one scene, then do not mutate the FrameBuilder until every dense
            // repetition from both independent renderer contexts has completed.
            let candidate = frame.paint_with_damage();
            repeatability
                .assert_scene_exact(&candidate.scene, &format!("candidate={sample_index}"));
        }
    }

    #[test]
    #[ignore = "explicit native-4K fixed-pose dense repeatability diagnostic"]
    fn native_4k_fixed_quick_settings_pose_225_dense_is_repeatable() {
        let candidate = fixed_quick_settings_pose_candidate(225);
        let mut repeatability = DenseRepeatabilityHarness::new();
        repeatability.assert_scene_exact(&candidate.scene, "fixed_quick_settings_pose=225");
    }

    #[test]
    #[ignore = "explicit native-4K fixed-pose shared-coarse geometry diagnostic"]
    fn native_4k_fixed_quick_settings_pose_225_shared_geometry_is_repeatable() {
        const REPETITIONS: usize = 8;
        let candidate = fixed_quick_settings_pose_candidate(225);
        let width = usize::try_from(WIDTH).expect("native width fits usize");
        let pitch = width.checked_mul(4).expect("native pitch fits usize");
        let len = pitch
            .checked_mul(usize::try_from(HEIGHT).expect("native height fits usize"))
            .expect("native output fits usize");
        let full_damage = [vello::FineDamageRect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        }];
        let damage = candidate.damage_regions.as_deref().unwrap_or(&full_damage);
        let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).expect("geometry bridge");
        let mut full = vec![0_u8; len];
        let mut sparse = vec![0_u8; len];
        let mut baseline_full = vec![0_u8; len];
        let mut baseline_geometry: Option<FixedPoseGeometryBaseline> = None;

        for repeat in 0..REPETITIONS {
            let capture = bridge
                .shared_coarse_fine_geometry_diagnostic_into_xrgb8888(
                    &candidate.scene,
                    damage,
                    &mut full,
                    &mut sparse,
                    pitch,
                )
                .expect("fixed-pose geometry render")
                .expect("shared-coarse Area geometry diagnostic must be available");
            let packed_scene = bridge.diagnostic_packed_scene().to_vec();
            let parsed = fixed_pose_geometry_baseline(&capture, packed_scene)
                .unwrap_or_else(|error| panic!("fixed-pose geometry parse incomplete: {error}"));
            if repeat == 0 {
                eprintln!(
                    "redraw_bench fixed-pose geometry first capture: config_len={} packed_scene_len={} ptcl_len={} segments_len={} tile1435_commands={} tile1436_commands={}",
                    capture.config.len(),
                    parsed.packed_scene.len(),
                    capture.ptcl.len(),
                    capture.segments.len(),
                    parsed.tiles[0].commands.len(),
                    parsed.tiles[1].commands.len(),
                );
            }
            let pair_mismatch = native_pixel_mismatch_in_damage(&full, &sparse, WIDTH, damage);
            if pair_mismatch.mismatches != 0 {
                report_fixed_pose_geometry_mismatch(
                    "sharedCoarseFull/sharedCoarseSparse",
                    &pair_mismatch,
                    baseline_geometry.as_ref(),
                    &capture,
                );
            }
            if let Some(baseline) = baseline_geometry.as_ref() {
                report_fixed_pose_geometry_comparison(baseline, &parsed, repeat);
            }
            assert_fixed_quick_settings_pose_exact(
                "shared_geometry_pair",
                225,
                repeat,
                &pair_mismatch,
            );
            if repeat == 0 {
                baseline_full.copy_from_slice(&full);
                baseline_geometry = Some(parsed);
                continue;
            }
            let full_mismatch = native_pixel_mismatch(&full, &baseline_full, WIDTH);
            if full_mismatch.mismatches != 0 {
                report_fixed_pose_geometry_mismatch(
                    "sharedCoarseFull/sharedCoarseFullBaseline",
                    &full_mismatch,
                    baseline_geometry.as_ref(),
                    &capture,
                );
            }
            assert_fixed_quick_settings_pose_exact(
                "shared_geometry_full_baseline",
                225,
                repeat,
                &full_mismatch,
            );
        }
    }

    #[derive(Deserialize)]
    struct SegmentOrderReplayFixture {
        native_extent: [u32; 2],
        pose_permille: u16,
        tile: u32,
        logical_command: u32,
        command_tag: u32,
        size_and_rule: u32,
        backdrop: u32,
        segment_stride_bytes: u32,
        meaningful_u32_fields: u32,
        count: usize,
        left: Vec<[u32; 5]>,
        right: Vec<[u32; 5]>,
    }

    #[test]
    #[ignore = "explicit native-4K causal fixed-order Area replay diagnostic"]
    fn native_4k_fixed_quick_settings_pose_225_segment_order_is_causal() {
        let fixture: SegmentOrderReplayFixture =
            serde_json::from_str(include_str!("fixtures/captured-segment-orders.json"))
                .expect("saved segment-order fixture");
        assert_eq!(fixture.native_extent, [WIDTH, HEIGHT]);
        assert_eq!(fixture.pose_permille, 225);
        assert_eq!(fixture.command_tag, 1);
        assert_eq!(fixture.size_and_rule, 98);
        assert_eq!(fixture.backdrop, 0);
        assert_eq!(fixture.segment_stride_bytes, 24);
        assert_eq!(fixture.meaningful_u32_fields, 5);
        assert_eq!(fixture.count, 49);
        assert_eq!(fixture.left.len(), 49, "saved left segment count");
        assert_eq!(fixture.right.len(), 49, "saved right segment count");
        let expected: [[u32; 5]; 49] =
            fixture
                .left
                .clone()
                .try_into()
                .unwrap_or_else(|values: Vec<[u32; 5]>| {
                    panic!("saved left segment count must be 49, got {}", values.len())
                });
        let right_order: [[u32; 5]; 49] =
            fixture
                .right
                .clone()
                .try_into()
                .unwrap_or_else(|values: Vec<[u32; 5]>| {
                    panic!("saved right segment count must be 49, got {}", values.len())
                });
        let candidate = fixed_quick_settings_pose_candidate(225);
        let width = usize::try_from(WIDTH).expect("native width fits usize");
        let pitch = width.checked_mul(4).expect("native pitch fits usize");
        let len = pitch
            .checked_mul(usize::try_from(HEIGHT).expect("native height fits usize"))
            .expect("native output fits usize");
        let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).expect("causal replay bridge");
        let adapter = bridge.adapter_info().expect("causal replay adapter");
        eprintln!(
            "redraw_bench segment-order replay: native_extent={}x{} pose=225 tile={} logical_command={} adapter={} backend={} device_type={} driver={}",
            WIDTH,
            HEIGHT,
            fixture.tile,
            fixture.logical_command,
            adapter.name,
            adapter.backend,
            adapter.device_type,
            adapter.driver,
        );
        let mut left = vec![0_u8; len];
        let mut right_pixels = vec![0_u8; len];
        let control = vello::SegmentOrderReplayInput {
            tile: fixture.tile,
            logical_command: fixture.logical_command,
            size_and_rule: fixture.size_and_rule,
            backdrop: fixture.backdrop,
            expected,
            left: expected,
            right: expected,
        };
        let control_replay = bridge
            .segment_order_replay_into_xrgb8888(
                &candidate.scene,
                &control,
                &mut left,
                &mut right_pixels,
                pitch,
            )
            .expect("AA control replay")
            .expect("Area replay must be available");
        assert_replay_witness("AA-left", &control_replay.left_witness, &expected);
        assert_replay_witness("AA-right", &control_replay.right_witness, &expected);
        assert_eq!(
            control_replay.left_witness[1],
            control_replay.right_witness[1]
        );
        eprintln!(
            "redraw_bench segment-order replay case=AA attempt={} segment_start={}",
            bridge.last_render_attempts(),
            control_replay.left_witness[1],
        );
        assert_replay_images_exact("AA control", &left, &right_pixels, WIDTH, None);

        let ab = vello::SegmentOrderReplayInput {
            tile: fixture.tile,
            logical_command: fixture.logical_command,
            size_and_rule: fixture.size_and_rule,
            backdrop: fixture.backdrop,
            expected,
            left: expected,
            right: right_order,
        };
        let ab_replay = bridge
            .segment_order_replay_into_xrgb8888(
                &candidate.scene,
                &ab,
                &mut left,
                &mut right_pixels,
                pitch,
            )
            .expect("AB replay")
            .expect("Area replay must be available");
        assert_replay_witness("AB-left", &ab_replay.left_witness, &expected);
        assert_replay_witness("AB-right", &ab_replay.right_witness, &right_order);
        assert_eq!(ab_replay.left_witness[1], ab_replay.right_witness[1]);
        eprintln!(
            "redraw_bench segment-order replay case=AB attempt={} segment_start={}",
            bridge.last_render_attempts(),
            ab_replay.left_witness[1],
        );
        assert_replay_images_exact(
            "AB outside target tile",
            &left,
            &right_pixels,
            WIDTH,
            Some((
                fixture.tile % (WIDTH / 16) * 16,
                fixture.tile / (WIDTH / 16) * 16,
            )),
        );
        assert_eq!(
            native_xrgb_byte(&left, WIDTH, 3770, 95, 0),
            49,
            "saved left byte"
        );
        assert_eq!(
            native_xrgb_byte(&right_pixels, WIDTH, 3770, 95, 0),
            48,
            "saved right byte"
        );

        let ba = vello::SegmentOrderReplayInput {
            tile: fixture.tile,
            logical_command: fixture.logical_command,
            size_and_rule: fixture.size_and_rule,
            backdrop: fixture.backdrop,
            expected,
            left: right_order,
            right: expected,
        };
        let ba_replay = bridge
            .segment_order_replay_into_xrgb8888(
                &candidate.scene,
                &ba,
                &mut left,
                &mut right_pixels,
                pitch,
            )
            .expect("BA replay")
            .expect("Area replay must be available");
        assert_replay_witness("BA-left", &ba_replay.left_witness, &right_order);
        assert_replay_witness("BA-right", &ba_replay.right_witness, &expected);
        assert_eq!(ba_replay.left_witness[1], ba_replay.right_witness[1]);
        eprintln!(
            "redraw_bench segment-order replay case=BA attempt={} segment_start={}",
            bridge.last_render_attempts(),
            ba_replay.left_witness[1],
        );
        assert_replay_images_exact(
            "BA outside target tile",
            &left,
            &right_pixels,
            WIDTH,
            Some((
                fixture.tile % (WIDTH / 16) * 16,
                fixture.tile / (WIDTH / 16) * 16,
            )),
        );
        assert_eq!(
            native_xrgb_byte(&left, WIDTH, 3770, 95, 0),
            48,
            "saved right byte"
        );
        assert_eq!(
            native_xrgb_byte(&right_pixels, WIDTH, 3770, 95, 0),
            49,
            "saved left byte"
        );
    }

    #[test]
    #[ignore = "explicit native-4K representative shared-coarse fine-command distribution diagnostic"]
    fn native_4k_shared_coarse_fine_command_distribution_is_representative() {
        const FIXED_POSES_PERMILLE: [u16; 4] = [0, 225, 500, 1000];

        let width = usize::try_from(WIDTH).expect("native width fits usize");
        let height = usize::try_from(HEIGHT).expect("native height fits usize");
        let pitch = width.checked_mul(4).expect("native pitch fits usize");
        let len = pitch.checked_mul(height).expect("native output fits usize");
        let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).expect("geometry bridge");
        let mut full = vec![0_u8; len];
        let mut sparse = vec![0_u8; len];
        let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())
            .expect("explicit populated shell fixture");
        let mut document = AutoCommit::new();
        let modifiers = Modifiers::default();
        let mut focus: Option<NodeId> = None;

        let initial = frame.paint_with_damage();
        capture_representative_fine_command_distribution(
            &mut bridge,
            "framework_initial",
            &initial.scene,
            initial.damage_regions.as_deref(),
            &mut full,
            &mut sparse,
            pitch,
        );

        for action_index in 0..6 {
            let target = ACTION_TARGETS[action_index % ACTION_TARGETS.len()];
            let rect = frame
                .widget_registry()
                .get(target)
                .copied()
                .filter(|rect| rect.area() > 0.0)
                .expect("benchmark target must be painted");
            dispatch_click(
                &mut frame,
                target,
                rect.center(),
                modifiers,
                &mut document,
                &mut focus,
            )
            .expect("benchmark click must be handled");
            frame.relayout();
            let candidate = frame.paint_with_damage();
            let label = format!(
                "framework_click_{}_{}",
                action_index.saturating_add(1),
                target
            );
            capture_representative_fine_command_distribution(
                &mut bridge,
                &label,
                &candidate.scene,
                candidate.damage_regions.as_deref(),
                &mut full,
                &mut sparse,
                pitch,
            );
        }

        for pose in FIXED_POSES_PERMILLE {
            let candidate = fixed_quick_settings_pose_candidate(pose);
            let label = format!("fixed_quick_settings_pose_{pose}");
            capture_representative_fine_command_distribution(
                &mut bridge,
                &label,
                &candidate.scene,
                candidate.damage_regions.as_deref(),
                &mut full,
                &mut sparse,
                pitch,
            );
        }
    }

    #[test]
    #[ignore = "explicit native-4K fixed quick-settings pose diagnostic"]
    fn native_4k_fixed_quick_settings_poses_match_shared_coarse_and_dense() {
        const CAPTURE_POSES_PERMILLE: [u16; 41] = [
            0, 25, 50, 75, 100, 125, 150, 175, 200, 225, 250, 275, 300, 325, 350, 375, 400, 425,
            450, 475, 500, 525, 550, 575, 600, 625, 650, 675, 700, 725, 750, 775, 800, 825, 850,
            875, 900, 925, 950, 975, 1000,
        ];

        let mut oracle = FixedQuickSettingsPoseOracle::new();
        for pose in CAPTURE_POSES_PERMILLE {
            oracle.assert_pose_exact(pose);
        }
        assert!(
            oracle.compact_damage_poses > 0,
            "fixed quick-settings sweep never exercised compact fine damage; compact_damage_poses={}",
            oracle.compact_damage_poses,
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn ptcl_parsing_and_fine_tile_union_follow_the_shader_layout() {
        // Phase `native_ptcl_parser_uses_shader_word_widths_and_excludes_segment_padding`.
        {
            let config = native_words_to_bytes(&[240, 135, 3840, 2160]);
            let mut ptcl = vec![0_u32; PTCL_INITIAL_WORDS];
            ptcl[1..6].copy_from_slice(&[1, 2, 0, 7, 0]);
            let mut segments = native_words_to_bytes(&[10, 11, 12, 13, 14, 0xfeed_beef]);
            let parsed =
                parse_native_fine_tile(&config, &native_words_to_bytes(&ptcl), &segments, 0)
                    .expect("valid native PTCL");
            assert_eq!(
                parsed.commands,
                vec![ParsedFineCommand::Fill {
                    size_and_rule: 2,
                    backdrop: 7,
                    segments: vec![[10, 11, 12, 13, 14]],
                }]
            );
            segments[20..24].copy_from_slice(&0x1234_5678_u32.to_le_bytes());
            assert_eq!(
                parse_native_fine_tile(&config, &native_words_to_bytes(&ptcl), &segments, 0)
                    .expect("padding remains ignored"),
                parsed
            );
        }
        // Phase `native_ptcl_parser_rejects_jump_cycles_bounds_and_truncated_segments`.
        {
            let config = native_words_to_bytes(&[240, 135, 3840, 2160]);
            let mut cycle = vec![0_u32; PTCL_CONTINUATION_WORDS + 1];
            cycle[1..3].copy_from_slice(&[12, 1]);
            assert!(
                parse_native_fine_tile(&config, &native_words_to_bytes(&cycle), &[], 0)
                    .expect_err("cycle must be explicit")
                    .contains("cycle")
            );

            let mut out_of_range = vec![0_u32; PTCL_INITIAL_WORDS];
            out_of_range[1..3].copy_from_slice(&[12, 64]);
            assert!(
                parse_native_fine_tile(&config, &native_words_to_bytes(&out_of_range), &[], 0,)
                    .expect_err("jump bound must be explicit")
                    .contains("invalid block")
            );

            let mut truncated = vec![0_u32; PTCL_INITIAL_WORDS];
            truncated[1..6].copy_from_slice(&[1, 4, 0, 0, 0]);
            assert!(
                parse_native_fine_tile(
                    &config,
                    &native_words_to_bytes(&truncated),
                    &native_words_to_bytes(&[1, 2, 3, 4, 5, 0]),
                    0,
                )
                .expect_err("segment range must be explicit")
                .contains("exceeds")
            );
            assert!(
                parse_native_fine_tile(&config, &[0, 0, 0], &[], 0)
                    .expect_err("PTCL alignment must be explicit")
                    .contains("aligned")
            );
            let mut adjacent_end = vec![0_u32; PTCL_INITIAL_WORDS * 2];
            adjacent_end[1..PTCL_INITIAL_WORDS].fill(3);
            assert!(
                parse_native_fine_tile(&config, &native_words_to_bytes(&adjacent_end), &[], 0)
                    .expect_err("initial PTCL block must not fall through")
                    .contains("crosses its allocation")
            );
        }
        // Phase `native_fine_tile_union_clips_boundaries_and_deduplicates_damage`.
        {
            let full = native_fine_tile_union(None, 32, 32);
            assert_eq!(full.mode, FineDiagnosticDamageMode::Full);
            assert_eq!(full.tiles, vec![0, 1, 2, 3]);

            let explicitly_empty = native_fine_tile_union(Some(&[]), 32, 32);
            assert_eq!(explicitly_empty.mode, FineDiagnosticDamageMode::Provided);
            assert!(explicitly_empty.tiles.is_empty());

            let exact_tile_edge = native_fine_tile_union(
                Some(&[vello::FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 16,
                }]),
                32,
                32,
            );
            assert_eq!(exact_tile_edge.tiles, vec![0]);

            let boundary = native_fine_tile_union(
                Some(&[vello::FineDamageRect {
                    x: 15,
                    y: 15,
                    width: 2,
                    height: 2,
                }]),
                32,
                32,
            );
            assert_eq!(boundary.mode, FineDiagnosticDamageMode::Provided);
            assert_eq!(boundary.tiles, vec![0, 1, 2, 3]);

            let final_partial_tile = native_fine_tile_union(
                Some(&[vello::FineDamageRect {
                    x: 16,
                    y: 16,
                    width: 1,
                    height: 1,
                }]),
                17,
                17,
            );
            assert_eq!(final_partial_tile.tiles, vec![3]);

            let overlap = native_fine_tile_union(
                Some(&[
                    vello::FineDamageRect {
                        x: 0,
                        y: 0,
                        width: 16,
                        height: 16,
                    },
                    vello::FineDamageRect {
                        x: 8,
                        y: 8,
                        width: 16,
                        height: 16,
                    },
                ]),
                32,
                32,
            );
            assert_eq!(overlap.tiles, vec![0, 1, 2, 3]);

            let empty = native_fine_tile_union(
                Some(&[
                    vello::FineDamageRect {
                        x: 16,
                        y: 0,
                        width: 0,
                        height: 8,
                    },
                    vello::FineDamageRect {
                        x: 33,
                        y: 32,
                        width: 8,
                        height: 8,
                    },
                ]),
                32,
                32,
            );
            assert_eq!(empty.mode, FineDiagnosticDamageMode::Provided);
            assert!(empty.tiles.is_empty());

            let saturated = native_fine_tile_union(
                Some(&[vello::FineDamageRect {
                    x: 31,
                    y: 31,
                    width: u32::MAX,
                    height: u32::MAX,
                }]),
                32,
                32,
            );
            assert_eq!(saturated.tiles, vec![3]);
        }
        // Phase `fine_command_distribution_counts_blur_fixture_with_shader_widths`.
        {
            let config = native_words_to_bytes(&[240, 135, 3840, 2160]);
            let mut ptcl = vec![0_u32; PTCL_INITIAL_WORDS];
            ptcl[1..5].copy_from_slice(&[CMD_FILL, 4, 0, 7]);
            ptcl[5..8].copy_from_slice(&[CMD_BLUR_RECT, 77, 11]);
            ptcl[8..10].copy_from_slice(&[5, 9]);
            ptcl[10..12].copy_from_slice(&[9, 123]);
            let segments = native_words_to_bytes(&[1, 2, 3, 4, 5, 0, 6, 7, 8, 9, 10, 0]);
            let parsed =
                parse_native_fine_tile(&config, &native_words_to_bytes(&ptcl), &segments, 0)
                    .expect("valid blur fixture");
            let mut distribution = FineCommandDistribution::default();
            distribution.record_tile(&parsed);

            let mut jumped_ptcl = vec![0_u32; PTCL_INITIAL_WORDS + PTCL_CONTINUATION_WORDS];
            let continuation_start =
                u32::try_from(PTCL_INITIAL_WORDS).expect("continuation offset fits u32");
            jumped_ptcl[1..3].copy_from_slice(&[12, continuation_start]);
            jumped_ptcl[PTCL_INITIAL_WORDS..PTCL_INITIAL_WORDS + 3].copy_from_slice(&[
                CMD_BLUR_RECT,
                77,
                12,
            ]);
            jumped_ptcl[PTCL_INITIAL_WORDS + 3..PTCL_INITIAL_WORDS + 6].copy_from_slice(&[
                CMD_BLUR_RECT,
                99,
                13,
            ]);
            let jumped =
                parse_native_fine_tile(&config, &native_words_to_bytes(&jumped_ptcl), &[], 0)
                    .expect("valid jump continuation fixture");
            assert_eq!(
                opaque_prune_prediction(&jumped.commands, true),
                OpaquePrunePrediction {
                    retained_commands: 2,
                    removed_commands: 0,
                    useful_prunes: 0,
                },
                "the dynamic JUMP is parsed before dry-run command analysis"
            );
            distribution.record_tile(&jumped);

            assert_eq!(distribution.total_commands, 6);
            assert_eq!(distribution.fill_commands, 1);
            assert_eq!(distribution.fill_segments, 2);
            assert_eq!(distribution.blur_commands, 3);
            assert_eq!(distribution.tiles_with_blur, 2);
            assert_eq!(distribution.max_blur_per_tile, 2);
            assert_eq!(
                distribution.blur_info_offsets,
                std::collections::BTreeSet::from([77, 99])
            );
            assert_eq!(distribution.executed_tag_histogram.get(&CMD_FILL), Some(&1));
            assert_eq!(
                distribution.executed_tag_histogram.get(&CMD_BLUR_RECT),
                Some(&3)
            );
            assert_eq!(distribution.executed_tag_histogram.get(&5), Some(&1));
            assert_eq!(distribution.executed_tag_histogram.get(&9), Some(&1));
            assert!(!distribution.executed_tag_histogram.contains_key(&0));
            assert!(!distribution.executed_tag_histogram.contains_key(&12));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn ptcl_telemetry_models_clips_prunes_successors_and_row_masks() {
        // Phase `clip_telemetry_tracks_nested_depth_empty_groups_and_end_clip_bits`.
        {
            let commands = vec![
                other_command(CMD_BEGIN_CLIP, &[]),
                other_command(CMD_BEGIN_CLIP, &[]),
                other_command(CMD_END_CLIP, &[8, 1.0_f32.to_bits()]),
                other_command(CMD_SOLID, &[]),
                other_command(CMD_COLOR, &[u32::MAX]),
                other_command(CMD_END_CLIP, &[0, 0.5_f32.to_bits()]),
            ];
            let telemetry = clip_tile_telemetry(&commands);
            assert_eq!(telemetry.max_depth, 2);
            assert_eq!(telemetry.empty_groups, 1);
            assert_eq!(
                telemetry
                    .end_clip_mode_alpha_bits
                    .get(&(8, 1.0_f32.to_bits())),
                Some(&1)
            );
            assert_eq!(
                telemetry
                    .end_clip_mode_alpha_bits
                    .get(&(0, 0.5_f32.to_bits())),
                Some(&1)
            );
        }
        // Phase `opaque_prune_prediction_respects_clip_anchors_alpha_and_depth`.
        {
            let nested = vec![
                other_command(CMD_SOLID, &[]),
                other_command(CMD_COLOR, &[u32::MAX]),
                other_command(CMD_BEGIN_CLIP, &[]),
                fill_command(0, 0, &[]),
                other_command(CMD_SOLID, &[]),
                other_command(CMD_COLOR, &[u32::MAX]),
                // A later destination-out composition must retain the active clip anchor/history.
                other_command(CMD_END_CLIP, &[8, 1.0_f32.to_bits()]),
                other_command(CMD_SOLID, &[]),
                other_command(CMD_COLOR, &[0xfeff_ffff]),
            ];
            let root = opaque_prune_prediction(&nested, false);
            let clip_local = opaque_prune_prediction(&nested, true);
            assert_eq!(root.removed_commands, 0);
            assert_eq!(root.useful_prunes, 0);
            assert_eq!(root.retained_commands, 9);
            assert_eq!(clip_local.removed_commands, 1);
            assert_eq!(clip_local.useful_prunes, 1);
            assert_eq!(clip_local.retained_commands, 8);

            let fill_before_color = vec![
                other_command(CMD_SOLID, &[]),
                fill_command(0, 0, &[]),
                other_command(CMD_COLOR, &[u32::MAX]),
            ];
            assert_eq!(
                opaque_prune_prediction(&fill_before_color, true),
                OpaquePrunePrediction {
                    retained_commands: 3,
                    removed_commands: 0,
                    useful_prunes: 0,
                }
            );

            let mut too_deep = vec![other_command(CMD_BEGIN_CLIP, &[]); 9];
            too_deep.push(other_command(CMD_SOLID, &[]));
            too_deep.push(other_command(CMD_COLOR, &[u32::MAX]));
            too_deep.extend(
                std::iter::repeat_with(|| other_command(CMD_END_CLIP, &[8, 1.0_f32.to_bits()]))
                    .take(9),
            );
            assert_eq!(opaque_prune_prediction(&too_deep, true).useful_prunes, 0);
        }
        // Phase `solid_successor_telemetry_distinguishes_fusable_tags`.
        {
            let tile = parsed_tile(vec![
                other_command(CMD_SOLID, &[]),
                other_command(CMD_COLOR, &[u32::MAX]),
                other_command(CMD_SOLID, &[]),
                other_command(6, &[1, 2]),
                other_command(CMD_SOLID, &[]),
                fill_command(0, 0, &[]),
                other_command(CMD_SOLID, &[]),
                other_command(CMD_END_CLIP, &[8, 1.0_f32.to_bits()]),
                other_command(CMD_SOLID, &[]),
                other_command(CMD_BLUR_RECT, &[77, 0]),
                other_command(CMD_SOLID, &[]),
            ]);
            let mut distribution = FineCommandDistribution::default();
            distribution.record_tile(&tile);
            assert_eq!(distribution.solid_successor_tags.get(&CMD_COLOR), Some(&1));
            assert_eq!(distribution.solid_successor_tags.get(&6), Some(&1));
            assert_eq!(
                distribution.solid_successor_tags.get(&CMD_END_CLIP),
                Some(&1)
            );
            assert_eq!(
                distribution.solid_successor_tags.get(&CMD_BLUR_RECT),
                Some(&1)
            );
            assert_eq!(distribution.solid_noneligible_successors, 2);
        }
        // Phase `segment_row_mask_cpu_model_covers_boundaries_and_conservative_fallbacks`.
        {
            let sentinel = Y_EDGE_SENTINEL.to_bits();
            assert_eq!(
                segment_row_mask_potential([
                    0.0_f32.to_bits(),
                    0.0_f32.to_bits(),
                    16.0_f32.to_bits(),
                    16.0_f32.to_bits(),
                    sentinel,
                ]),
                u16::MAX
            );
            assert_eq!(
                segment_row_mask_potential([
                    0.0_f32.to_bits(),
                    8.0_f32.to_bits(),
                    16.0_f32.to_bits(),
                    8.0_f32.to_bits(),
                    sentinel,
                ]),
                1_u16 << 7
            );
            assert_eq!(
                segment_row_mask_potential([
                    (-0.0_f32).to_bits(),
                    8.0_f32.to_bits(),
                    0.0_f32.to_bits(),
                    8.0_f32.to_bits(),
                    0.0_f32.to_bits(),
                ]),
                1_u16 << 7,
                "signed zero must not create a y-edge tail when dx is exactly zero"
            );
            assert!(
                !cpu_f32_row_dy_contribution(8.0, 8.0, 7)
                    && !cpu_f32_y_edge_contribution(-0.0, 0.0, 0.0, 7),
                "horizontal and signed-zero segments must have zero independent CPU contributions"
            );
            assert!(cpu_f32_y_edge_contribution(0.0, 1.0, 8.0, 8));
            assert_eq!(
                segment_row_mask_potential([
                    f32::NAN.to_bits(),
                    1.0_f32.to_bits(),
                    2.0_f32.to_bits(),
                    3.0_f32.to_bits(),
                    sentinel,
                ]),
                u16::MAX
            );
            assert_eq!(
                segment_row_mask_potential([
                    17.0_f32.to_bits(),
                    1.0_f32.to_bits(),
                    2.0_f32.to_bits(),
                    3.0_f32.to_bits(),
                    sentinel,
                ]),
                u16::MAX
            );

            let mut state = 0x1a2b_3c4d_u32;
            for _ in 0..64 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let x0 = f32::from(u16::try_from(state >> 16).expect("upper word fits u16"))
                    / f32::from(u16::MAX)
                    * 16.0;
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let y0 = f32::from(u16::try_from(state >> 16).expect("upper word fits u16"))
                    / f32::from(u16::MAX)
                    * 16.0;
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let x1 = f32::from(u16::try_from(state >> 16).expect("upper word fits u16"))
                    / f32::from(u16::MAX)
                    * 16.0;
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let y1 = f32::from(u16::try_from(state >> 16).expect("upper word fits u16"))
                    / f32::from(u16::MAX)
                    * 16.0;
                let mask = segment_row_mask_potential([
                    x0.to_bits(),
                    y0.to_bits(),
                    x1.to_bits(),
                    y1.to_bits(),
                    sentinel,
                ]);
                assert_ne!(mask, 0, "finite deterministic fixture must retain a row");
            }
        }
    }

    #[test]
    fn parsed_fine_tile_comparison_reports_every_kind_of_difference() {
        // Phase `parsed_fine_tile_comparison_reports_order_only_fill_changes`.
        {
            let left = parsed_tile(vec![fill_command(
                4,
                7,
                &[[1, 2, 3, 4, 5], [6, 7, 8, 9, 10]],
            )]);
            let right = parsed_tile(vec![fill_command(
                4,
                7,
                &[[6, 7, 8, 9, 10], [1, 2, 3, 4, 5]],
            )]);
            assert_eq!(
                compare_parsed_fine_tiles(&left, &right),
                TileGeometryComparison {
                    all_command_metadata_equal: true,
                    all_fill_multisets_equal: true,
                    any_order_changed: true,
                }
            );
        }
        // Phase `parsed_fine_tile_comparison_detects_changed_meaningful_segment`.
        {
            let left = parsed_tile(vec![fill_command(2, 7, &[[1, 2, 3, 4, 5]])]);
            let right = parsed_tile(vec![fill_command(2, 7, &[[1, 2, 3, 4, 6]])]);
            let comparison = compare_parsed_fine_tiles(&left, &right);
            assert!(comparison.all_command_metadata_equal);
            assert!(!comparison.all_fill_multisets_equal);
            assert!(!comparison.any_order_changed);
        }
        // Phase `parsed_fine_tile_comparison_detects_fill_metadata_and_other_args`.
        {
            let base = parsed_tile(vec![
                fill_command(2, 7, &[[1, 2, 3, 4, 5]]),
                ParsedFineCommand::Other {
                    tag: 5,
                    args: vec![9],
                },
            ]);
            for changed in [
                parsed_tile(vec![
                    fill_command(3, 7, &[[1, 2, 3, 4, 5]]),
                    ParsedFineCommand::Other {
                        tag: 5,
                        args: vec![9],
                    },
                ]),
                parsed_tile(vec![
                    fill_command(2, 8, &[[1, 2, 3, 4, 5]]),
                    ParsedFineCommand::Other {
                        tag: 5,
                        args: vec![9],
                    },
                ]),
                parsed_tile(vec![
                    fill_command(2, 7, &[[1, 2, 3, 4, 5]]),
                    ParsedFineCommand::Other {
                        tag: 5,
                        args: vec![10],
                    },
                ]),
            ] {
                let comparison = compare_parsed_fine_tiles(&base, &changed);
                assert!(!comparison.all_command_metadata_equal);
                assert!(comparison.all_fill_multisets_equal);
                assert!(!comparison.any_order_changed);
            }
        }
        // Phase `parsed_fine_tile_comparison_detects_command_count_and_type_mismatches`.
        {
            let fill = fill_command(2, 7, &[[1, 2, 3, 4, 5]]);
            let base = parsed_tile(vec![fill.clone()]);
            for changed in [
                parsed_tile(vec![]),
                parsed_tile(vec![ParsedFineCommand::Other {
                    tag: 5,
                    args: vec![9],
                }]),
            ] {
                let comparison = compare_parsed_fine_tiles(&base, &changed);
                assert!(!comparison.all_command_metadata_equal);
                assert!(!comparison.all_fill_multisets_equal);
                assert!(!comparison.any_order_changed);
            }
        }
    }

    fn parsed_tile(commands: Vec<ParsedFineCommand>) -> ParsedFineTile {
        ParsedFineTile { commands }
    }

    fn fill_command(size_and_rule: u32, backdrop: u32, segments: &[[u32; 5]]) -> ParsedFineCommand {
        ParsedFineCommand::Fill {
            size_and_rule,
            backdrop,
            segments: segments.to_vec(),
        }
    }

    fn other_command(tag: u32, args: &[u32]) -> ParsedFineCommand {
        ParsedFineCommand::Other {
            tag,
            args: args.to_vec(),
        }
    }

    fn native_words_to_bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[derive(Debug)]
    struct NativePixelMismatch {
        mismatches: usize,
        max_delta: u8,
        first: Option<(usize, usize, usize, u8, u8)>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ParsedFineTile {
        commands: Vec<ParsedFineCommand>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum ParsedFineCommand {
        Fill {
            size_and_rule: u32,
            backdrop: u32,
            segments: Vec<[u32; 5]>,
        },
        Other {
            tag: u32,
            args: Vec<u32>,
        },
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FixedPoseGeometryBaseline {
        config: Vec<u8>,
        packed_scene: Vec<u8>,
        tiles: [ParsedFineTile; 2],
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct TileGeometryComparison {
        all_command_metadata_equal: bool,
        all_fill_multisets_equal: bool,
        any_order_changed: bool,
    }

    const PTCL_INITIAL_WORDS: usize = 64;
    const PTCL_CONTINUATION_WORDS: usize = 256;
    const SEGMENT_STRIDE_BYTES: usize = 24;
    const MAX_PTCL_COMMANDS: usize = 65_536;
    const MAX_CAPTURED_SEGMENTS: usize = 65_536;
    const NATIVE_FINE_TILE_SIZE: u32 = 16;
    const CMD_FILL: u32 = 1;
    const CMD_SOLID: u32 = 3;
    const CMD_COLOR: u32 = 5;
    const CMD_BEGIN_CLIP: u32 = 10;
    const CMD_END_CLIP: u32 = 11;
    const CMD_BLUR_RECT: u32 = 13;
    const FINE_TILE_ROWS: u32 = 16;
    const FINE_TILE_ROWS_I32: i32 = 16;
    const FINE_TILE_ROWS_F32: f32 = 16.0;
    const Y_EDGE_SENTINEL: f32 = 1_000_000_000.0;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FineDiagnosticDamageMode {
        Full,
        Provided,
    }

    impl FineDiagnosticDamageMode {
        const fn label(self) -> &'static str {
            match self {
                Self::Full => "full",
                Self::Provided => "provided",
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct NativeFineTileSelection {
        mode: FineDiagnosticDamageMode,
        tiles: Vec<usize>,
    }

    #[derive(Default)]
    struct FineCommandDistribution {
        executed_tag_histogram: std::collections::BTreeMap<u32, u64>,
        fill_commands: u64,
        fill_segments: u64,
        blur_commands: u64,
        tiles_with_blur: u64,
        max_blur_per_tile: u64,
        blur_info_offsets: std::collections::BTreeSet<u32>,
        total_commands: u64,
        max_clip_depth: u64,
        tiles_clip_depth_gt4: u64,
        tiles_clip_depth_gt8: u64,
        end_clip_mode_alpha_bits: std::collections::BTreeMap<(u32, u32), u64>,
        empty_clip_groups: u64,
        root_opaque_prune: OpaquePrunePrediction,
        clip_local_opaque_prune: OpaquePrunePrediction,
        segment_references: u64,
        total_segment_row_visits: u64,
        cpu_model_potential_clear_rows: u64,
        solid_successor_tags: std::collections::BTreeMap<u32, u64>,
        solid_noneligible_successors: u64,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct OpaquePrunePrediction {
        retained_commands: u64,
        removed_commands: u64,
        useful_prunes: u64,
    }

    #[derive(Default)]
    struct ClipTileTelemetry {
        max_depth: u64,
        empty_groups: u64,
        end_clip_mode_alpha_bits: std::collections::BTreeMap<(u32, u32), u64>,
    }

    impl FineCommandDistribution {
        fn record_tile(&mut self, tile: &ParsedFineTile) {
            let mut tile_blur_commands = 0_u64;
            for command in &tile.commands {
                self.total_commands = self.total_commands.saturating_add(1);
                match command {
                    ParsedFineCommand::Fill { segments, .. } => {
                        let count = self.executed_tag_histogram.entry(CMD_FILL).or_default();
                        *count = count.saturating_add(1);
                        self.fill_commands = self.fill_commands.saturating_add(1);
                        self.fill_segments = self.fill_segments.saturating_add(
                            u64::try_from(segments.len()).expect("captured segment count fits u64"),
                        );
                        for segment in segments {
                            let potential_rows = segment_row_mask_potential(*segment);
                            self.segment_references = self.segment_references.saturating_add(1);
                            self.total_segment_row_visits = self
                                .total_segment_row_visits
                                .saturating_add(u64::from(FINE_TILE_ROWS));
                            self.cpu_model_potential_clear_rows = self
                                .cpu_model_potential_clear_rows
                                .saturating_add(u64::from(
                                    FINE_TILE_ROWS.saturating_sub(potential_rows.count_ones()),
                                ));
                        }
                    }
                    ParsedFineCommand::Other { tag, args } => {
                        let count = self.executed_tag_histogram.entry(*tag).or_default();
                        *count = count.saturating_add(1);
                        if *tag == CMD_BLUR_RECT {
                            self.blur_commands = self.blur_commands.saturating_add(1);
                            tile_blur_commands = tile_blur_commands.saturating_add(1);
                            self.blur_info_offsets.insert(
                                *args
                                    .first()
                                    .expect("CMD_BLUR_RECT must contain its info offset"),
                            );
                        }
                    }
                }
            }
            if tile_blur_commands != 0 {
                self.tiles_with_blur = self.tiles_with_blur.saturating_add(1);
                self.max_blur_per_tile = self.max_blur_per_tile.max(tile_blur_commands);
            }
            let clip = clip_tile_telemetry(&tile.commands);
            self.max_clip_depth = self.max_clip_depth.max(clip.max_depth);
            if clip.max_depth > 4 {
                self.tiles_clip_depth_gt4 = self.tiles_clip_depth_gt4.saturating_add(1);
            }
            if clip.max_depth > 8 {
                self.tiles_clip_depth_gt8 = self.tiles_clip_depth_gt8.saturating_add(1);
            }
            self.empty_clip_groups = self.empty_clip_groups.saturating_add(clip.empty_groups);
            for (key, count) in clip.end_clip_mode_alpha_bits {
                let total = self.end_clip_mode_alpha_bits.entry(key).or_default();
                *total = total.saturating_add(count);
            }
            self.root_opaque_prune
                .accumulate(opaque_prune_prediction(&tile.commands, false));
            self.clip_local_opaque_prune
                .accumulate(opaque_prune_prediction(&tile.commands, true));
            for index in 0..tile.commands.len() {
                if command_tag(&tile.commands[index]) != CMD_SOLID {
                    continue;
                }
                let successor = tile.commands.get(index.saturating_add(1)).map(command_tag);
                if let Some(tag @ (CMD_COLOR | 6 | 7 | 8 | 9 | CMD_END_CLIP | CMD_BLUR_RECT)) =
                    successor
                {
                    let count = self.solid_successor_tags.entry(tag).or_default();
                    *count = count.saturating_add(1);
                } else {
                    self.solid_noneligible_successors =
                        self.solid_noneligible_successors.saturating_add(1);
                }
            }
        }

        fn histogram_key_value(&self) -> String {
            self.executed_tag_histogram
                .iter()
                .map(|(tag, count)| format!("{tag}:{count}"))
                .collect::<Vec<_>>()
                .join(",")
        }

        fn bounded_blur_info_offsets(&self) -> String {
            const MAX_REPORTED_OFFSETS: usize = 32;
            let mut offsets = self
                .blur_info_offsets
                .iter()
                .take(MAX_REPORTED_OFFSETS)
                .map(u32::to_string)
                .collect::<Vec<_>>();
            if self.blur_info_offsets.len() > MAX_REPORTED_OFFSETS {
                offsets.push(format!(
                    "...+{}",
                    self.blur_info_offsets
                        .len()
                        .saturating_sub(MAX_REPORTED_OFFSETS)
                ));
            }
            format!("[{}]", offsets.join(","))
        }

        fn end_clip_mode_alpha_bits_key_value(&self) -> String {
            self.end_clip_mode_alpha_bits
                .iter()
                .map(|((mode, alpha), count)| format!("{mode}:{alpha}:{count}"))
                .collect::<Vec<_>>()
                .join(",")
        }

        fn solid_successor_key_value(&self) -> String {
            self.solid_successor_tags
                .iter()
                .map(|(tag, count)| format!("{tag}:{count}"))
                .collect::<Vec<_>>()
                .join(",")
        }
    }

    impl OpaquePrunePrediction {
        fn accumulate(&mut self, other: Self) {
            self.retained_commands = self
                .retained_commands
                .saturating_add(other.retained_commands);
            self.removed_commands = self.removed_commands.saturating_add(other.removed_commands);
            self.useful_prunes = self.useful_prunes.saturating_add(other.useful_prunes);
        }
    }

    fn clip_tile_telemetry(commands: &[ParsedFineCommand]) -> ClipTileTelemetry {
        #[derive(Default)]
        struct ClipFrame {
            has_content: bool,
        }

        let mut telemetry = ClipTileTelemetry::default();
        let mut clips: Vec<ClipFrame> = Vec::new();
        for command in commands {
            match command {
                ParsedFineCommand::Other { tag, args: _ } if *tag == CMD_BEGIN_CLIP => {
                    if let Some(parent) = clips.last_mut() {
                        parent.has_content = true;
                    }
                    clips.push(ClipFrame::default());
                    telemetry.max_depth = telemetry
                        .max_depth
                        .max(u64::try_from(clips.len()).expect("clip depth fits u64"));
                }
                ParsedFineCommand::Other { tag, args } if *tag == CMD_END_CLIP => {
                    if let [blend, alpha_bits, ..] = args.as_slice() {
                        let count = telemetry
                            .end_clip_mode_alpha_bits
                            .entry((*blend, *alpha_bits))
                            .or_default();
                        *count = count.saturating_add(1);
                    }
                    if clips.pop().is_some_and(|frame| !frame.has_content) {
                        telemetry.empty_groups = telemetry.empty_groups.saturating_add(1);
                    }
                }
                _ => {
                    if let Some(frame) = clips.last_mut() {
                        frame.has_content = true;
                    }
                }
            }
        }
        telemetry
    }

    fn opaque_prune_prediction(
        commands: &[ParsedFineCommand],
        permit_clip_local: bool,
    ) -> OpaquePrunePrediction {
        let mut output = Vec::with_capacity(commands.len());
        let mut active_clip_anchors = Vec::new();
        let mut previous_is_solid = false;
        let mut prediction = OpaquePrunePrediction::default();
        for command in commands {
            let tag = command_tag(command);
            if tag == CMD_BEGIN_CLIP {
                output.push(());
                active_clip_anchors.push(output.len());
                previous_is_solid = false;
                continue;
            }
            if tag == CMD_END_CLIP {
                output.push(());
                active_clip_anchors.pop();
                previous_is_solid = false;
                continue;
            }
            if tag == CMD_SOLID {
                output.push(());
                previous_is_solid = true;
                continue;
            }

            let opaque_color = matches!(command, ParsedFineCommand::Other { tag, args }
                if *tag == CMD_COLOR
                    && args.first().is_some_and(|color| *color >> 24 == u32::from(u8::MAX)));
            if opaque_color && previous_is_solid {
                output.pop();
                let reset = if active_clip_anchors.is_empty() {
                    Some(0)
                } else if permit_clip_local && active_clip_anchors.len() <= 8 {
                    active_clip_anchors.last().copied()
                } else {
                    None
                };
                if let Some(anchor) = reset {
                    let removed = output.len().saturating_sub(anchor);
                    prediction.removed_commands = prediction
                        .removed_commands
                        .saturating_add(u64::try_from(removed).expect("removed commands fit u64"));
                    if removed != 0 {
                        prediction.useful_prunes = prediction.useful_prunes.saturating_add(1);
                    }
                    output.truncate(anchor);
                }
                // The candidate solid was deliberately popped before reset calculation; re-add
                // the complete pair after preserving the root or innermost-clip anchor.
                output.push(());
                output.push(());
            } else {
                output.push(());
            }
            previous_is_solid = false;
        }
        prediction.retained_commands =
            u64::try_from(output.len()).expect("predicted command count fits u64");
        prediction
    }

    fn command_tag(command: &ParsedFineCommand) -> u32 {
        match command {
            ParsedFineCommand::Fill { .. } => CMD_FILL,
            ParsedFineCommand::Other { tag, .. } => *tag,
        }
    }

    fn segment_row_mask_potential(segment: [u32; 5]) -> u16 {
        let x0 = f32::from_bits(segment[0]);
        let y0 = f32::from_bits(segment[1]);
        let x1 = f32::from_bits(segment[2]);
        let y1 = f32::from_bits(segment[3]);
        let y_edge = f32::from_bits(segment[4]);
        let y_edge_is_sentinel = y_edge.to_bits() == Y_EDGE_SENTINEL.to_bits();
        let coordinate_invalid = [x0, y0, x1, y1]
            .into_iter()
            .any(|value| !value.is_finite() || !(0.0..=FINE_TILE_ROWS_F32).contains(&value));
        if coordinate_invalid
            || (!y_edge_is_sentinel
                && (!y_edge.is_finite() || !(0.0..=FINE_TILE_ROWS_F32).contains(&y_edge)))
        {
            return u16::MAX;
        }

        let min_y = y0.min(y1);
        let max_y = y0.max(y1);
        let mut mask = row_range_mask(
            floor_native_row(min_y).saturating_sub(1),
            ceil_native_row(max_y),
        );
        if x0 != x1 && !y_edge_is_sentinel {
            mask |= row_range_mask(
                floor_native_row(y_edge).saturating_sub(1),
                FINE_TILE_ROWS_I32,
            );
        }
        for row in 0..FINE_TILE_ROWS {
            if mask & (1_u16 << row) == 0 {
                assert!(
                    !cpu_f32_row_dy_contribution(y0, y1, row)
                        && !cpu_f32_y_edge_contribution(x0, x1, y_edge, row),
                    "CPU row-mask model omitted row {row} with a nonzero contribution"
                );
            }
        }
        mask
    }

    fn row_range_mask(start: i32, end: i32) -> u16 {
        let start = start.clamp(0, FINE_TILE_ROWS_I32);
        let end = end.clamp(0, FINE_TILE_ROWS_I32);
        (start..end).fold(0_u16, |mask, row| {
            mask | (1_u16 << u32::try_from(row).expect("native row is non-negative"))
        })
    }

    fn floor_native_row(value: f32) -> i32 {
        for next_row in 1_u8..=16 {
            if value < f32::from(next_row) {
                return i32::from(next_row).saturating_sub(1);
            }
        }
        FINE_TILE_ROWS_I32
    }

    fn ceil_native_row(value: f32) -> i32 {
        for row in 0_u8..=16 {
            if value <= f32::from(row) {
                return i32::from(row);
            }
        }
        FINE_TILE_ROWS_I32
    }

    fn cpu_f32_row_dy_contribution(y0: f32, y1: f32, row: u32) -> bool {
        let row = f32::from(u16::try_from(row).expect("native row fits u16"));
        let y = y0 - row;
        let delta_y = y1 - y0;
        let clipped_y0 = y.clamp(0.0, 1.0);
        let clipped_y1 = (y + delta_y).clamp(0.0, 1.0);
        clipped_y0 - clipped_y1 != 0.0
    }

    fn cpu_f32_y_edge_contribution(x0: f32, x1: f32, y_edge: f32, row: u32) -> bool {
        let row = f32::from(u16::try_from(row).expect("native row fits u16"));
        let delta_x = x1 - x0;
        let sign = if delta_x > 0.0 {
            1.0
        } else if delta_x < 0.0 {
            -1.0
        } else {
            0.0
        };
        sign * (row - y_edge + 1.0).clamp(0.0, 1.0) != 0.0
    }

    fn native_fine_tile_union(
        damage: Option<&[vello::FineDamageRect]>,
        width: u32,
        height: u32,
    ) -> NativeFineTileSelection {
        let tiles_x = width.div_ceil(NATIVE_FINE_TILE_SIZE);
        let tiles_y = height.div_ceil(NATIVE_FINE_TILE_SIZE);
        let tile_count = usize::try_from(tiles_x.saturating_mul(tiles_y))
            .expect("native fine tile count fits usize");
        if damage.is_none() {
            return NativeFineTileSelection {
                mode: FineDiagnosticDamageMode::Full,
                tiles: (0..tile_count).collect(),
            };
        }

        let mut selected = vec![false; tile_count];
        for rect in damage.expect("checked above") {
            let x0 = rect.x.min(width);
            let y0 = rect.y.min(height);
            let x1 = rect.x.saturating_add(rect.width).min(width);
            let y1 = rect.y.saturating_add(rect.height).min(height);
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            let tile_x0 = x0 / NATIVE_FINE_TILE_SIZE;
            let tile_x1 = x1.saturating_sub(1) / NATIVE_FINE_TILE_SIZE;
            let tile_y0 = y0 / NATIVE_FINE_TILE_SIZE;
            let tile_y1 = y1.saturating_sub(1) / NATIVE_FINE_TILE_SIZE;
            for tile_y in tile_y0..=tile_y1 {
                for tile_x in tile_x0..=tile_x1 {
                    let tile = tile_y.saturating_mul(tiles_x).saturating_add(tile_x);
                    selected[usize::try_from(tile).expect("validated native tile index")] = true;
                }
            }
        }
        NativeFineTileSelection {
            mode: FineDiagnosticDamageMode::Provided,
            tiles: selected
                .iter()
                .enumerate()
                .filter_map(|(tile, selected)| selected.then_some(tile))
                .collect(),
        }
    }

    fn capture_representative_fine_command_distribution(
        bridge: &mut VelloDrmBridge,
        label: &str,
        scene: &vello::Scene,
        damage: Option<&[vello::FineDamageRect]>,
        full_pixels: &mut [u8],
        sparse_pixels: &mut [u8],
        pitch: usize,
    ) {
        let selection = native_fine_tile_union(damage, WIDTH, HEIGHT);
        let scene_draw_tags = scene.encoding().draw_tags.len();
        let scene_blur_draw_tags = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|tag| **tag == DrawTag::BLUR_RECT)
            .count();
        let full_damage = [vello::FineDamageRect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        }];
        let requested_damage = damage.unwrap_or(&full_damage);
        if selection.tiles.is_empty() {
            eprintln!(
                "redraw_bench fine_command_distribution representative=true timing_trajectory=not_same_1000_benchmark label={label} damage_mode={} scene_draw_tags={scene_draw_tags} scene_blur_draw_tags={scene_blur_draw_tags} selected_tiles=0 capture_fine_tiles=0 capture_config_bytes=0 capture_ptcl_bytes=0 capture_segment_bytes=0 capture_bytes=0 total_commands=0 fill_commands=0 fill_segments=0 blur_commands=0 tiles_with_blur=0 max_blur_per_tile=0 unique_blur_info_offsets=0 blur_info_offsets=[] max_clip_depth=0 tiles_clip_depth_gt4=0 tiles_clip_depth_gt8=0 empty_clip_groups=0 end_clip_mode_alpha_bits= root_prune_prediction_retained_commands=0 root_prune_prediction_removed_commands=0 root_prune_prediction_useful_prunes=0 clip_local_prune_prediction_retained_commands=0 clip_local_prune_prediction_removed_commands=0 clip_local_prune_prediction_useful_prunes=0 segment_references=0 total_segment_row_visits=0 cpu_model_potential_clear_rows=0 rowmask_model=cpu_not_gpu_proof solid_successor_tags= solid_noneligible_successors=0 executed_command_tag_histogram= capture=skipped_empty_damage",
                selection.mode.label(),
            );
            return;
        }

        let capture = bridge
            .shared_coarse_fine_geometry_diagnostic_into_xrgb8888(
                scene,
                requested_damage,
                full_pixels,
                sparse_pixels,
                pitch,
            )
            .unwrap_or_else(|error| {
                panic!("{label}: shared-coarse geometry capture failed: {error}")
            })
            .unwrap_or_else(|| panic!("{label}: shared-coarse Area geometry capture unavailable"));
        let expected_tiles =
            u32::try_from(selection.tiles.len()).expect("native tile count fits u32");
        assert_eq!(
            capture.stats.fine_tiles, expected_tiles,
            "{label}: selected native tile union must match SparseRenderStats::fine_tiles"
        );

        let mut distribution = FineCommandDistribution::default();
        for &tile in &selection.tiles {
            let parsed =
                parse_native_fine_tile(&capture.config, &capture.ptcl, &capture.segments, tile)
                    .unwrap_or_else(|error| {
                        panic!("{label}: native fine tile {tile} parse failed: {error}")
                    });
            distribution.record_tile(&parsed);
        }
        let capture_bytes = capture
            .config
            .len()
            .saturating_add(capture.ptcl.len())
            .saturating_add(capture.segments.len());
        eprintln!(
            "redraw_bench fine_command_distribution representative=true timing_trajectory=not_same_1000_benchmark label={label} damage_mode={} scene_draw_tags={scene_draw_tags} scene_blur_draw_tags={scene_blur_draw_tags} selected_tiles={} capture_fine_tiles={} capture_config_bytes={} capture_ptcl_bytes={} capture_segment_bytes={} capture_bytes={capture_bytes} total_commands={} fill_commands={} fill_segments={} blur_commands={} tiles_with_blur={} max_blur_per_tile={} unique_blur_info_offsets={} blur_info_offsets={} max_clip_depth={} tiles_clip_depth_gt4={} tiles_clip_depth_gt8={} empty_clip_groups={} end_clip_mode_alpha_bits={} root_prune_prediction_retained_commands={} root_prune_prediction_removed_commands={} root_prune_prediction_useful_prunes={} clip_local_prune_prediction_retained_commands={} clip_local_prune_prediction_removed_commands={} clip_local_prune_prediction_useful_prunes={} segment_references={} total_segment_row_visits={} cpu_model_potential_clear_rows={} rowmask_model=cpu_not_gpu_proof solid_successor_tags={} solid_noneligible_successors={} executed_command_tag_histogram={}",
            selection.mode.label(),
            selection.tiles.len(),
            capture.stats.fine_tiles,
            capture.config.len(),
            capture.ptcl.len(),
            capture.segments.len(),
            distribution.total_commands,
            distribution.fill_commands,
            distribution.fill_segments,
            distribution.blur_commands,
            distribution.tiles_with_blur,
            distribution.max_blur_per_tile,
            distribution.blur_info_offsets.len(),
            distribution.bounded_blur_info_offsets(),
            distribution.max_clip_depth,
            distribution.tiles_clip_depth_gt4,
            distribution.tiles_clip_depth_gt8,
            distribution.empty_clip_groups,
            distribution.end_clip_mode_alpha_bits_key_value(),
            distribution.root_opaque_prune.retained_commands,
            distribution.root_opaque_prune.removed_commands,
            distribution.root_opaque_prune.useful_prunes,
            distribution.clip_local_opaque_prune.retained_commands,
            distribution.clip_local_opaque_prune.removed_commands,
            distribution.clip_local_opaque_prune.useful_prunes,
            distribution.segment_references,
            distribution.total_segment_row_visits,
            distribution.cpu_model_potential_clear_rows,
            distribution.solid_successor_key_value(),
            distribution.solid_noneligible_successors,
            distribution.histogram_key_value(),
        );
        let mismatch =
            native_pixel_mismatch_in_damage(full_pixels, sparse_pixels, WIDTH, requested_damage);
        if mismatch.mismatches != 0 {
            report_native_pixel_mismatch(label, &mismatch);
            panic!(
                "{label}: shared-coarse full/sparse requested-pixel mismatch: mismatches={} max_delta={} first={:?}",
                mismatch.mismatches, mismatch.max_delta, mismatch.first
            );
        }
    }

    fn native_u32(bytes: &[u8], word: usize, label: &str) -> Result<u32, String> {
        let byte = word
            .checked_mul(4)
            .ok_or_else(|| format!("{label} word offset overflowed"))?;
        let end = byte
            .checked_add(4)
            .ok_or_else(|| format!("{label} word range overflowed"))?;
        let word = bytes
            .get(byte..end)
            .ok_or_else(|| format!("{label} word {word} is out of range"))?;
        Ok(u32::from_le_bytes(word.try_into().expect("four-byte word")))
    }

    fn native_word_count(bytes: &[u8], label: &str) -> Result<usize, String> {
        if bytes.len() % 4 != 0 {
            return Err(format!(
                "{label} byte length {} is not u32 aligned",
                bytes.len()
            ));
        }
        Ok(bytes.len() / 4)
    }

    fn parse_native_fine_tile(
        config: &[u8],
        ptcl: &[u8],
        segments: &[u8],
        tile: usize,
    ) -> Result<ParsedFineTile, String> {
        native_word_count(config, "config")?;
        let native_config = (
            native_u32(config, 0, "config")?,
            native_u32(config, 1, "config")?,
            native_u32(config, 2, "config")?,
            native_u32(config, 3, "config")?,
        );
        if native_config != (240, 135, 3840, 2160) {
            return Err(format!(
                "expected native config 240x135 tiles / 3840x2160 pixels, got {native_config:?}"
            ));
        }
        let word_count = native_word_count(ptcl, "PTCL")?;
        if segments.len() % SEGMENT_STRIDE_BYTES != 0 {
            return Err(format!(
                "segments byte length {} is not {SEGMENT_STRIDE_BYTES}-byte aligned",
                segments.len()
            ));
        }
        let tile_count = 240_usize * 135;
        if tile >= tile_count {
            return Err(format!(
                "tile {tile} exceeds native tile count {tile_count}"
            ));
        }
        let base = tile
            .checked_mul(PTCL_INITIAL_WORDS)
            .ok_or_else(|| "tile PTCL base overflowed".to_owned())?;
        let mut block_end = base
            .checked_add(PTCL_INITIAL_WORDS)
            .filter(|end| *end <= word_count)
            .ok_or_else(|| format!("tile {tile} initial PTCL allocation is truncated"))?;
        let mut command = base
            .checked_add(1)
            .filter(|start| *start < block_end)
            .ok_or_else(|| format!("tile {tile} initial PTCL allocation is truncated"))?;
        let mut seen_jumps = std::collections::BTreeSet::new();
        let mut commands = Vec::new();
        let mut captured_segments = 0_usize;
        for _ in 0..MAX_PTCL_COMMANDS {
            if command >= block_end {
                return Err(format!("PTCL command {command} crosses its allocation"));
            }
            let tag = native_u32(ptcl, command, "PTCL")?;
            if tag == 0 {
                return Ok(ParsedFineTile { commands });
            }
            if tag == 12 {
                if command.saturating_add(2) > block_end {
                    return Err(format!("PTCL jump at {command} crosses its allocation"));
                }
                let target = usize::try_from(native_u32(ptcl, command + 1, "PTCL")?)
                    .map_err(|_| format!("PTCL jump at {command} does not fit usize"))?;
                block_end = target
                    .checked_add(PTCL_CONTINUATION_WORDS)
                    .filter(|end| *end <= word_count)
                    .ok_or_else(|| {
                        format!("PTCL jump at {command} targets invalid block {target}")
                    })?;
                if !seen_jumps.insert(target) {
                    return Err(format!("PTCL jump cycle reaches {target}"));
                }
                command = target;
                continue;
            }
            let width = fine_command_width(tag)
                .ok_or_else(|| format!("unsupported fine.wgsl PTCL tag {tag} at {command}"))?;
            let end = command
                .checked_add(width)
                .ok_or_else(|| format!("PTCL command {command} width overflow"))?;
            if end > block_end {
                return Err(format!(
                    "PTCL command {command} tag {tag} crosses its allocation"
                ));
            }
            if tag == 1 {
                let size_and_rule = native_u32(ptcl, command + 1, "PTCL")?;
                let segment_start = usize::try_from(native_u32(ptcl, command + 2, "PTCL")?)
                    .map_err(|_| "fill segment start does not fit usize".to_owned())?;
                let segment_count = usize::try_from(size_and_rule >> 1)
                    .map_err(|_| "fill segment count does not fit usize".to_owned())?;
                let segment_end = segment_start
                    .checked_add(segment_count)
                    .ok_or_else(|| "fill segment range overflowed".to_owned())?;
                let segment_total = segments.len() / SEGMENT_STRIDE_BYTES;
                if segment_end > segment_total {
                    return Err(format!(
                        "fill segment range {segment_start}..{segment_end} exceeds {segment_total}"
                    ));
                }
                captured_segments = captured_segments
                    .checked_add(segment_count)
                    .filter(|total| *total <= MAX_CAPTURED_SEGMENTS)
                    .ok_or_else(|| {
                        format!("captured fill segments exceed {MAX_CAPTURED_SEGMENTS}")
                    })?;
                let mut fill_segments = Vec::with_capacity(segment_count);
                for segment_index in segment_start..segment_end {
                    let byte = segment_index
                        .checked_mul(SEGMENT_STRIDE_BYTES)
                        .expect("validated segment range");
                    fill_segments.push([
                        native_u32(segments, byte / 4, "segment")?,
                        native_u32(segments, byte / 4 + 1, "segment")?,
                        native_u32(segments, byte / 4 + 2, "segment")?,
                        native_u32(segments, byte / 4 + 3, "segment")?,
                        native_u32(segments, byte / 4 + 4, "segment")?,
                    ]);
                }
                commands.push(ParsedFineCommand::Fill {
                    size_and_rule,
                    backdrop: native_u32(ptcl, command + 3, "PTCL")?,
                    segments: fill_segments,
                });
            } else {
                commands.push(ParsedFineCommand::Other {
                    tag,
                    args: (command + 1..end)
                        .map(|word| native_u32(ptcl, word, "PTCL"))
                        .collect::<Result<Vec<_>, _>>()?,
                });
            }
            command = end;
        }
        Err(format!(
            "PTCL tile {tile} exceeded {MAX_PTCL_COMMANDS} commands without CMD_END"
        ))
    }

    fn fine_command_width(tag: u32) -> Option<usize> {
        // Exact increments in vendor/vello_shaders/shader/fine.wgsl, rather than Rust struct
        // sizes. PTCL encodes gradients, image, and indirect data as compact word records.
        match tag {
            1 => Some(4),                   // CMD_FILL
            3 | 10 => Some(1),              // CMD_SOLID, CMD_BEGIN_CLIP
            5 => Some(2),                   // CMD_COLOR
            6 | 7 | 8 | 11 | 13 => Some(3), // gradients, END_CLIP, BLUR_RECT
            9 => Some(2),                   // CMD_IMAGE
            _ => None,
        }
    }

    struct NativeOracleBridges<'a> {
        retained: &'a mut VelloDrmBridge,
        dense: &'a mut VelloDrmBridge,
    }

    struct NativeOracleFailure<'a> {
        scene: &'a vello::Scene,
        revision: u64,
        previous_revision: Option<u64>,
        damage: Option<&'a [vello::FineDamageRect]>,
        original_retained_pixels: &'a [u8],
        dense_pixels: &'a [u8],
        original_oracle_packed_scene: Vec<u8>,
        len: usize,
        pitch: usize,
    }

    struct DenseRepeatabilityHarness {
        first_context: VelloDrmBridge,
        second_context: VelloDrmBridge,
        first_baseline: Vec<u8>,
        second_baseline: Vec<u8>,
        first_pixels: Vec<u8>,
        second_pixels: Vec<u8>,
        first_packed_baseline: Vec<u8>,
        second_packed_baseline: Vec<u8>,
        pitch: usize,
    }

    impl DenseRepeatabilityHarness {
        const REPETITIONS: usize = 8;

        fn new() -> Self {
            let width = usize::try_from(WIDTH).expect("native width fits usize");
            let height = usize::try_from(HEIGHT).expect("native height fits usize");
            let pitch = width.checked_mul(4).expect("native pitch fits usize");
            let len = pitch.checked_mul(height).expect("native output fits usize");
            Self {
                first_context: VelloDrmBridge::new(WIDTH, HEIGHT).expect("first dense bridge"),
                second_context: VelloDrmBridge::new(WIDTH, HEIGHT).expect("second dense bridge"),
                first_baseline: vec![0_u8; len],
                second_baseline: vec![0_u8; len],
                first_pixels: vec![0_u8; len],
                second_pixels: vec![0_u8; len],
                first_packed_baseline: Vec::new(),
                second_packed_baseline: Vec::new(),
                pitch,
            }
        }

        fn assert_scene_exact(&mut self, scene: &vello::Scene, subject: &str) {
            for repetition in 0..Self::REPETITIONS {
                self.first_context
                    .paint_into_xrgb8888(scene, &mut self.first_pixels, self.pitch)
                    .expect("first dense repeat paint");
                let first_packed_scene = self.first_context.diagnostic_packed_scene().to_vec();
                self.second_context
                    .paint_into_xrgb8888(scene, &mut self.second_pixels, self.pitch)
                    .expect("second dense repeat paint");
                let second_packed_scene = self.second_context.diagnostic_packed_scene().to_vec();
                if repetition == 0 {
                    assert_dense_repeatability(
                        "cross_context",
                        &native_pixel_mismatch(&self.first_pixels, &self.second_pixels, WIDTH),
                        &first_packed_scene,
                        &second_packed_scene,
                        subject,
                        repetition,
                    );
                    self.first_baseline.copy_from_slice(&self.first_pixels);
                    self.second_baseline.copy_from_slice(&self.second_pixels);
                    self.first_packed_baseline = first_packed_scene;
                    self.second_packed_baseline = second_packed_scene;
                } else {
                    assert_dense_repeatability(
                        "same_context_first",
                        &native_pixel_mismatch(&self.first_pixels, &self.first_baseline, WIDTH),
                        &first_packed_scene,
                        &self.first_packed_baseline,
                        subject,
                        repetition,
                    );
                    assert_dense_repeatability(
                        "same_context_second",
                        &native_pixel_mismatch(&self.second_pixels, &self.second_baseline, WIDTH),
                        &second_packed_scene,
                        &self.second_packed_baseline,
                        subject,
                        repetition,
                    );
                    assert_dense_repeatability(
                        "cross_context",
                        &native_pixel_mismatch(&self.first_pixels, &self.second_pixels, WIDTH),
                        &first_packed_scene,
                        &second_packed_scene,
                        subject,
                        repetition,
                    );
                }
            }
            eprintln!(
                "redraw_bench dense repeatability {subject}: repetitions={} same_context=exact cross_context=exact",
                Self::REPETITIONS,
            );
        }
    }

    struct FixedQuickSettingsPoseOracle {
        shared_coarse: VelloDrmBridge,
        independent_dense: VelloDrmBridge,
        pair_full: Vec<u8>,
        pair_sparse: Vec<u8>,
        dense_pixels: Vec<u8>,
        full_pixels: u128,
        compact_damage_poses: usize,
        pitch: usize,
    }

    impl FixedQuickSettingsPoseOracle {
        fn new() -> Self {
            let width = usize::try_from(WIDTH).expect("native width fits usize");
            let height = usize::try_from(HEIGHT).expect("native height fits usize");
            let pitch = width.checked_mul(4).expect("native pitch fits usize");
            let len = pitch.checked_mul(height).expect("native output fits usize");
            Self {
                shared_coarse: VelloDrmBridge::new(WIDTH, HEIGHT).expect("shared-coarse bridge"),
                independent_dense: VelloDrmBridge::new(WIDTH, HEIGHT)
                    .expect("independent dense bridge"),
                pair_full: vec![0_u8; len],
                pair_sparse: vec![0_u8; len],
                dense_pixels: vec![0_u8; len],
                full_pixels: u128::from(WIDTH).saturating_mul(u128::from(HEIGHT)),
                compact_damage_poses: 0,
                pitch,
            }
        }

        fn assert_pose_exact(&mut self, pose: u16) {
            let candidate = fixed_quick_settings_pose_candidate(pose);
            self.record_damage_coverage(candidate.damage_regions.as_deref());
            let full_damage = [vello::FineDamageRect {
                x: 0,
                y: 0,
                width: WIDTH,
                height: HEIGHT,
            }];
            let damage = candidate.damage_regions.as_deref().unwrap_or(&full_damage);
            let last_pair_packed_scene = self.assert_shared_pair(&candidate.scene, damage, pose);
            self.assert_independent_dense(&candidate.scene, damage, pose, &last_pair_packed_scene);
        }

        fn record_damage_coverage(&mut self, damage: Option<&[vello::FineDamageRect]>) {
            let covered_pixels = damage.map_or(self.full_pixels, |regions| {
                regions.iter().fold(0_u128, |pixels, rect| {
                    pixels.saturating_add(
                        u128::from(rect.width).saturating_mul(u128::from(rect.height)),
                    )
                })
            });
            if covered_pixels < self.full_pixels {
                self.compact_damage_poses = self.compact_damage_poses.saturating_add(1);
            }
        }

        fn assert_shared_pair(
            &mut self,
            scene: &vello::Scene,
            damage: &[vello::FineDamageRect],
            pose: u16,
        ) -> Vec<u8> {
            let mut first_packed_scene = Vec::new();
            for attempt in 0..2 {
                let (stats, mismatch) = render_shared_coarse_diagnostic(
                    &mut self.shared_coarse,
                    scene,
                    damage,
                    &mut self.pair_full,
                    &mut self.pair_sparse,
                    self.pitch,
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "fixed quick-settings pose {pose} shared-coarse attempt {attempt} failed: {error}"
                    )
                });
                let packed_scene = self.shared_coarse.diagnostic_packed_scene().to_vec();
                eprintln!(
                    "redraw_bench fixed quick-settings pose={pose} attempt={attempt}: damage={damage:?} stats={stats:?}"
                );
                report_native_pixel_mismatch("sharedCoarseFull/sharedCoarseSparse", &mismatch);
                if mismatch.mismatches != 0 {
                    report_packed_scene_comparison(
                        "sharedCoarseFull/sharedCoarseSparse",
                        if attempt == 0 {
                            &packed_scene
                        } else {
                            &first_packed_scene
                        },
                        &packed_scene,
                    );
                }
                assert_fixed_quick_settings_pose_exact(
                    "shared_coarse_pair",
                    pose,
                    attempt,
                    &mismatch,
                );
                if attempt == 0 {
                    self.dense_pixels.copy_from_slice(&self.pair_full);
                    first_packed_scene = packed_scene;
                } else {
                    let repeated_full_mismatch = native_pixel_mismatch_in_damage(
                        &self.pair_full,
                        &self.dense_pixels,
                        WIDTH,
                        damage,
                    );
                    report_native_pixel_mismatch(
                        "sharedFullBetweenCoarsePasses",
                        &repeated_full_mismatch,
                    );
                    if repeated_full_mismatch.mismatches != 0 {
                        report_packed_scene_comparison(
                            "sharedFullBetweenCoarsePasses",
                            &first_packed_scene,
                            &packed_scene,
                        );
                    }
                    assert_fixed_quick_settings_pose_exact(
                        "shared_full_between_coarse_passes",
                        pose,
                        attempt,
                        &repeated_full_mismatch,
                    );
                    return packed_scene;
                }
            }
            unreachable!("the fixed shared-coarse diagnostic always runs two attempts")
        }

        fn assert_independent_dense(
            &mut self,
            scene: &vello::Scene,
            damage: &[vello::FineDamageRect],
            pose: u16,
            last_pair_packed_scene: &[u8],
        ) {
            self.independent_dense
                .paint_into_xrgb8888(scene, &mut self.dense_pixels, self.pitch)
                .expect("independent dense fixed-pose paint");
            let independent_packed_scene =
                self.independent_dense.diagnostic_packed_scene().to_vec();
            let dense_mismatch =
                native_pixel_mismatch_in_damage(&self.pair_full, &self.dense_pixels, WIDTH, damage);
            report_native_pixel_mismatch("sharedCoarseFull/independentDense", &dense_mismatch);
            if dense_mismatch.mismatches != 0 {
                report_packed_scene_comparison(
                    "sharedCoarseFull/independentDense",
                    last_pair_packed_scene,
                    &independent_packed_scene,
                );
            }
            assert_fixed_quick_settings_pose_exact(
                "shared_pair_full_vs_independent_dense",
                pose,
                0,
                &dense_mismatch,
            );
        }
    }

    fn fixed_quick_settings_pose_candidate(permille: u16) -> shell::compositor::frame::FramePaint {
        let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark())
            .expect("explicit populated shell fixture");
        let _closed_baseline = frame.paint_with_damage();
        let _settled_closed_baseline = frame.paint_with_damage();
        assert!(
            !frame.chrome().overlays.quick_settings,
            "each fixed pose starts from a painted, closed desktop"
        );
        frame.with_chrome(|chrome| chrome.overlays.quick_settings = true);
        frame
            .host_mut()
            .set_quick_settings_capture_openness(permille);
        frame.relayout();
        frame.paint_with_damage()
    }

    fn report_native_oracle_failure(
        bridges: NativeOracleBridges<'_>,
        failure: NativeOracleFailure<'_>,
        original_mismatch: &NativePixelMismatch,
        sample_index: usize,
    ) {
        eprintln!(
            "redraw_bench native 4K mismatch candidate={sample_index}: revision={} previous_revision={:?} damage={:?} sparse_stats={:?}",
            failure.revision,
            failure.previous_revision,
            failure.damage,
            bridges.retained.last_sparse_render_stats(),
        );
        report_native_pixel_mismatch("originalRetained/originalDense", original_mismatch);

        let full_damage = [vello::FineDamageRect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        }];
        let diagnostic_damage = failure.damage.unwrap_or(&full_damage);
        let mut shared_coarse_full = vec![0_u8; failure.len];
        let mut shared_coarse_sparse = vec![0_u8; failure.len];
        match render_shared_coarse_diagnostic(
            &mut *bridges.retained,
            failure.scene,
            diagnostic_damage,
            &mut shared_coarse_full,
            &mut shared_coarse_sparse,
            failure.pitch,
        ) {
            Ok((stats, mismatch)) => {
                eprintln!(
                    "redraw_bench shared-coarse diagnostic candidate={sample_index}: stats={stats:?}"
                );
                report_native_pixel_mismatch("sharedCoarseFull/sharedCoarseSparse", &mismatch);
            }
            Err(error) => eprintln!(
                "redraw_bench shared-coarse diagnostic candidate={sample_index} failed: {error}"
            ),
        }

        // These diagnostic renders happen only after the original retained versus dense result
        // has mismatched. They retain the candidate scene and original buffers unchanged while
        // distinguishing retained-state corruption from same-scene dense rendering differences.
        let mut fresh_retained_dense = vec![0_u8; failure.len];
        bridges
            .retained
            .paint_into_xrgb8888(failure.scene, &mut fresh_retained_dense, failure.pitch)
            .expect("fresh retained dense diagnostic paint");
        report_native_pixel_mismatch(
            "originalRetained/freshRetainedDense",
            &native_pixel_mismatch(
                failure.original_retained_pixels,
                &fresh_retained_dense,
                WIDTH,
            ),
        );
        let mut repeated_oracle = vec![0_u8; failure.len];
        bridges
            .dense
            .paint_into_xrgb8888(failure.scene, &mut repeated_oracle, failure.pitch)
            .expect("repeated dense oracle diagnostic paint");
        let repeated_oracle_packed_scene = bridges.dense.diagnostic_packed_scene().to_vec();
        report_packed_scene_comparison(
            "originalOracle/repeatedOracle",
            &failure.original_oracle_packed_scene,
            &repeated_oracle_packed_scene,
        );
        report_native_pixel_mismatch(
            "freshRetainedDense/originalDense",
            &native_pixel_mismatch(&fresh_retained_dense, failure.dense_pixels, WIDTH),
        );
        report_native_pixel_mismatch(
            "repeatedOracle/originalDense",
            &native_pixel_mismatch(&repeated_oracle, failure.dense_pixels, WIDTH),
        );
        report_native_pixel_mismatch(
            "freshRetainedDense/repeatedOracle",
            &native_pixel_mismatch(&fresh_retained_dense, &repeated_oracle, WIDTH),
        );
    }

    fn native_pixel_mismatch(actual: &[u8], expected: &[u8], width: u32) -> NativePixelMismatch {
        let mut mismatches = 0_usize;
        let mut max_delta = 0_u8;
        let mut first = None;
        for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
            if actual != expected {
                mismatches = mismatches.saturating_add(1);
                max_delta = max_delta.max(actual.abs_diff(expected));
                first.get_or_insert((index, actual, expected));
            }
        }
        let first = first.map(|(index, actual, expected)| {
            let pixel = index / 4;
            (
                pixel % usize::try_from(width).expect("width fits usize"),
                pixel / usize::try_from(width).expect("width fits usize"),
                index % 4,
                actual,
                expected,
            )
        });
        NativePixelMismatch {
            mismatches,
            max_delta,
            first,
        }
    }

    fn native_pixel_mismatch_in_damage(
        actual: &[u8],
        expected: &[u8],
        width: u32,
        damage: &[vello::FineDamageRect],
    ) -> NativePixelMismatch {
        let stride = usize::try_from(width)
            .expect("width fits usize")
            .checked_mul(4)
            .expect("native row stride fits usize");
        let mut mismatches = 0_usize;
        let mut max_delta = 0_u8;
        let mut first = None;
        for rect in damage {
            for y in rect.y..rect.y.saturating_add(rect.height) {
                let row = usize::try_from(y).expect("damage y fits usize");
                let start = row
                    .checked_mul(stride)
                    .and_then(|offset| {
                        offset
                            .checked_add(usize::try_from(rect.x).expect("damage x fits usize") * 4)
                    })
                    .expect("damage offset fits usize");
                let byte_count = usize::try_from(rect.width)
                    .expect("damage width fits usize")
                    .checked_mul(4)
                    .expect("damage row width fits usize");
                let end = start
                    .checked_add(byte_count)
                    .expect("damage row end fits usize");
                for (byte_offset, (&actual, &expected)) in actual[start..end]
                    .iter()
                    .zip(&expected[start..end])
                    .enumerate()
                {
                    if actual != expected {
                        mismatches = mismatches.saturating_add(1);
                        max_delta = max_delta.max(actual.abs_diff(expected));
                        first.get_or_insert((start + byte_offset, actual, expected));
                    }
                }
            }
        }
        let first = first.map(|(index, actual, expected)| {
            let pixel = index / 4;
            (
                pixel % usize::try_from(width).expect("width fits usize"),
                pixel / usize::try_from(width).expect("width fits usize"),
                index % 4,
                actual,
                expected,
            )
        });
        NativePixelMismatch {
            mismatches,
            max_delta,
            first,
        }
    }

    fn native_xrgb_byte(pixels: &[u8], width: u32, x: u32, y: u32, channel: usize) -> u8 {
        let pixel = usize::try_from(y)
            .expect("native y fits usize")
            .checked_mul(usize::try_from(width).expect("native width fits usize"))
            .and_then(|offset| offset.checked_add(usize::try_from(x).expect("native x fits usize")))
            .expect("native pixel offset fits usize");
        pixels[pixel
            .checked_mul(4)
            .and_then(|offset| offset.checked_add(channel))
            .expect("native byte offset fits usize")]
    }

    fn assert_replay_witness(label: &str, witness: &[u32], expected: &[[u32; 5]; 49]) {
        assert_eq!(witness.len(), 250, "{label} witness length");
        assert_eq!(witness[0], 1, "{label} GPU guard status: {witness:?}");
        assert_eq!(witness[2], 29, "{label} logical command");
        assert_eq!(witness[3], 98, "{label} size_and_rule");
        assert_eq!(witness[4], 0, "{label} backdrop");
        for (index, expected) in expected.iter().enumerate() {
            let offset = 5 + index * 5;
            assert_eq!(
                &witness[offset..offset + 5],
                expected,
                "{label} applied segment {index}"
            );
        }
    }

    /// Compare every byte while allowing only the one 16×16 fine tile to differ. The line is
    /// intentionally complete enough to preserve evidence if a future replay moves a pixel.
    fn assert_replay_images_exact(
        label: &str,
        left: &[u8],
        right: &[u8],
        width: u32,
        excluded_tile: Option<(u32, u32)>,
    ) {
        assert_eq!(left.len(), right.len(), "{label} output lengths");
        let mut all_differences = 0_usize;
        let mut outside_differences = 0_usize;
        let mut first = None;
        let mut target_changes = Vec::new();
        for (offset, (&lhs, &rhs)) in left.iter().zip(right).enumerate() {
            if lhs == rhs {
                continue;
            }
            all_differences = all_differences.saturating_add(1);
            let pixel = offset / 4;
            let x = u32::try_from(pixel % usize::try_from(width).expect("width fits usize"))
                .expect("x fits u32");
            let y = u32::try_from(pixel / usize::try_from(width).expect("width fits usize"))
                .expect("y fits u32");
            let in_excluded_tile = excluded_tile.is_some_and(|(tile_x, tile_y)| {
                x >= tile_x
                    && x < tile_x.saturating_add(16)
                    && y >= tile_y
                    && y < tile_y.saturating_add(16)
            });
            if !in_excluded_tile {
                outside_differences = outside_differences.saturating_add(1);
            } else {
                // A 16×16 XRGB tile has at most 1,024 byte differences, so this remains a
                // complete diagnostic rather than a sampled report.
                target_changes.push((x, y, offset % 4, lhs, rhs));
            }
            first.get_or_insert((x, y, offset % 4, lhs, rhs, in_excluded_tile));
        }
        eprintln!(
            "redraw_bench segment-order replay {label}: all_byte_differences={all_differences} outside_target_tile_differences={outside_differences} first={first:?} target_tile_changes={target_changes:?}"
        );
        assert_eq!(
            outside_differences, 0,
            "{label} changed outside target tile"
        );
        if excluded_tile.is_none() {
            assert_eq!(all_differences, 0, "{label} AA control changed pixels");
        }
    }

    fn render_shared_coarse_diagnostic(
        bridge: &mut VelloDrmBridge,
        scene: &vello::Scene,
        damage: &[vello::FineDamageRect],
        full_pixels: &mut [u8],
        sparse_pixels: &mut [u8],
        pitch: usize,
    ) -> Result<(vello::SparseRenderStats, NativePixelMismatch), BridgeError> {
        let stats = bridge
            .shared_coarse_fine_diagnostic_into_xrgb8888(
                scene,
                damage,
                full_pixels,
                sparse_pixels,
                pitch,
            )?
            .ok_or_else(|| {
                BridgeError::Render(
                    "shared-coarse Area diagnostic is unavailable for this renderer".to_owned(),
                )
            })?;
        let mismatch = native_pixel_mismatch_in_damage(full_pixels, sparse_pixels, WIDTH, damage);
        Ok((stats, mismatch))
    }

    fn report_fixed_pose_geometry_mismatch(
        label: &str,
        mismatch: &NativePixelMismatch,
        baseline: Option<&FixedPoseGeometryBaseline>,
        capture: &SharedCoarseGeometryDiagnostic,
    ) {
        report_native_pixel_mismatch(label, mismatch);
        eprintln!(
            "redraw_bench geometry {label}: stats={:?} config_len={} ptcl_len={} segments_len={}",
            capture.stats,
            capture.config.len(),
            capture.ptcl.len(),
            capture.segments.len(),
        );
        if let Some(baseline) = baseline {
            eprintln!(
                "redraw_bench geometry {label}: config_equal={} baseline_config_len={} baseline_packed_scene_len={}",
                baseline.config == capture.config,
                baseline.config.len(),
                baseline.packed_scene.len(),
            );
        }
    }

    fn fixed_pose_geometry_baseline(
        capture: &SharedCoarseGeometryDiagnostic,
        packed_scene: Vec<u8>,
    ) -> Result<FixedPoseGeometryBaseline, String> {
        if capture.config.is_empty() {
            return Err("fixed-pose geometry config is empty".to_owned());
        }
        if packed_scene.is_empty() {
            return Err("fixed-pose packed scene is empty".to_owned());
        }
        Ok(FixedPoseGeometryBaseline {
            config: capture.config.clone(),
            packed_scene,
            tiles: [
                parse_native_fine_tile(&capture.config, &capture.ptcl, &capture.segments, 1435)?,
                parse_native_fine_tile(&capture.config, &capture.ptcl, &capture.segments, 1436)?,
            ],
        })
    }

    fn report_fixed_pose_geometry_comparison(
        baseline: &FixedPoseGeometryBaseline,
        capture: &FixedPoseGeometryBaseline,
        repeat: usize,
    ) {
        eprintln!(
            "redraw_bench geometry repeat={repeat}: config_equal={} config_first_difference={:?} packed_scene_equal={} packed_scene_first_difference={:?}",
            baseline.config == capture.config,
            first_byte_difference(&baseline.config, &capture.config),
            baseline.packed_scene == capture.packed_scene,
            first_byte_difference(&baseline.packed_scene, &capture.packed_scene),
        );
        for (tile, (left, right)) in [1435_usize, 1436]
            .into_iter()
            .zip(baseline.tiles.iter().zip(&capture.tiles))
        {
            let comparison = compare_parsed_fine_tiles(left, right);
            eprintln!(
                "redraw_bench geometry tile={tile}: all_command_metadata_equal={} all_fill_multisets_equal={} any_order_changed={} baseline_commands={} capture_commands={} baseline_fills={} capture_fills={}",
                comparison.all_command_metadata_equal,
                comparison.all_fill_multisets_equal,
                comparison.any_order_changed,
                left.commands.len(),
                right.commands.len(),
                left.commands
                    .iter()
                    .filter(|command| matches!(command, ParsedFineCommand::Fill { .. }))
                    .count(),
                right
                    .commands
                    .iter()
                    .filter(|command| matches!(command, ParsedFineCommand::Fill { .. }))
                    .count(),
            );
            if !comparison.all_command_metadata_equal
                || !comparison.all_fill_multisets_equal
                || comparison.any_order_changed
            {
                report_logical_tile_difference(tile, &left, &right);
            }
        }
    }

    fn compare_parsed_fine_tiles(
        left: &ParsedFineTile,
        right: &ParsedFineTile,
    ) -> TileGeometryComparison {
        let same_count = left.commands.len() == right.commands.len();
        let mut comparison = TileGeometryComparison {
            all_command_metadata_equal: same_count,
            all_fill_multisets_equal: same_count,
            any_order_changed: false,
        };
        for (left, right) in left.commands.iter().zip(&right.commands) {
            match (left, right) {
                (
                    ParsedFineCommand::Fill {
                        size_and_rule: left_rule,
                        backdrop: left_backdrop,
                        segments: left_segments,
                    },
                    ParsedFineCommand::Fill {
                        size_and_rule: right_rule,
                        backdrop: right_backdrop,
                        segments: right_segments,
                    },
                ) => {
                    comparison.all_command_metadata_equal &=
                        left_rule == right_rule && left_backdrop == right_backdrop;
                    let mut left_sorted = left_segments.clone();
                    let mut right_sorted = right_segments.clone();
                    left_sorted.sort_unstable();
                    right_sorted.sort_unstable();
                    let multiset_equal = left_sorted == right_sorted;
                    comparison.all_fill_multisets_equal &= multiset_equal;
                    comparison.any_order_changed |=
                        multiset_equal && left_segments != right_segments;
                }
                (
                    ParsedFineCommand::Other {
                        tag: left_tag,
                        args: left_args,
                    },
                    ParsedFineCommand::Other {
                        tag: right_tag,
                        args: right_args,
                    },
                ) => {
                    comparison.all_command_metadata_equal &=
                        left_tag == right_tag && left_args == right_args;
                }
                _ => {
                    comparison.all_command_metadata_equal = false;
                    comparison.all_fill_multisets_equal = false;
                }
            }
        }
        comparison
    }

    fn first_byte_difference(left: &[u8], right: &[u8]) -> Option<usize> {
        left.iter()
            .zip(right)
            .position(|(left, right)| left != right)
            .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len())))
    }

    fn report_logical_tile_difference(tile: usize, left: &ParsedFineTile, right: &ParsedFineTile) {
        let pixel_x = (tile % 240).saturating_mul(16);
        let pixel_y = (tile / 240).saturating_mul(16);
        for (command_index, (left_command, right_command)) in
            left.commands.iter().zip(&right.commands).enumerate()
        {
            let (
                ParsedFineCommand::Fill {
                    size_and_rule: left_rule,
                    backdrop: left_backdrop,
                    segments: left_segments,
                },
                ParsedFineCommand::Fill {
                    size_and_rule: right_rule,
                    backdrop: right_backdrop,
                    segments: right_segments,
                },
            ) = (left_command, right_command)
            else {
                continue;
            };
            if left_segments == right_segments {
                continue;
            }
            let mut left_sorted = left_segments.clone();
            let mut right_sorted = right_segments.clone();
            left_sorted.sort_unstable();
            right_sorted.sort_unstable();
            eprintln!(
                "redraw_bench geometry logical difference: tile={tile} pixel=({pixel_x},{pixel_y}) command={command_index} left_rule={left_rule} right_rule={right_rule} left_backdrop={left_backdrop} right_backdrop={right_backdrop} ordered_equal=false multiset_equal={}",
                left_sorted == right_sorted,
            );
            if left_sorted == right_sorted {
                report_fill_segment_orders("left", left_segments);
                report_fill_segment_orders("right", right_segments);
            }
            return;
        }
        eprintln!(
            "redraw_bench geometry logical difference: tile={tile} pixel=({pixel_x},{pixel_y}) command structure differs left_commands={} right_commands={}",
            left.commands.len(),
            right.commands.len(),
        );
    }

    fn report_fill_segment_orders(label: &str, segments: &[[u32; 5]]) {
        if segments.len() <= 64 {
            eprintln!(
                "redraw_bench geometry ordered fill segments {label}: count={} complete={segments:?}",
                segments.len(),
            );
        } else {
            eprintln!(
                "redraw_bench geometry ordered fill segments {label}: count={} first8={:?} truncated=true",
                segments.len(),
                &segments[..8],
            );
        }
    }

    fn report_native_pixel_mismatch(label: &str, mismatch: &NativePixelMismatch) {
        eprintln!(
            "redraw_bench native 4K mismatch {label}: mismatches={} max_delta={} first={:?}",
            mismatch.mismatches, mismatch.max_delta, mismatch.first,
        );
    }

    fn assert_native_pixel_mismatch_is_zero(mismatch: &NativePixelMismatch, candidate: usize) {
        assert_eq!(
            mismatch.mismatches, 0,
            "native pixel mismatch after candidate {candidate}: mismatches={} max_delta={} first={:?}",
            mismatch.mismatches, mismatch.max_delta, mismatch.first,
        );
    }

    fn assert_dense_repeatability(
        comparison: &str,
        mismatch: &NativePixelMismatch,
        left_packed_scene: &[u8],
        right_packed_scene: &[u8],
        subject: &str,
        repetition: usize,
    ) {
        if mismatch.mismatches != 0 {
            report_packed_scene_comparison(comparison, left_packed_scene, right_packed_scene);
        }
        assert_eq!(
            mismatch.mismatches, 0,
            "dense repeatability mismatch: comparison={comparison} subject={subject} repetition={repetition} mismatches={} max_delta={} first={:?}",
            mismatch.mismatches, mismatch.max_delta, mismatch.first,
        );
    }

    fn report_packed_scene_comparison(label: &str, left: &[u8], right: &[u8]) {
        let first = left
            .iter()
            .zip(right)
            .enumerate()
            .find_map(|(offset, (&left, &right))| {
                (left != right).then_some((offset, Some(left), Some(right)))
            })
            .or_else(|| {
                (left.len() != right.len()).then(|| {
                    let offset = left.len().min(right.len());
                    (
                        offset,
                        left.get(offset).copied(),
                        right.get(offset).copied(),
                    )
                })
            });
        match first {
            Some((offset, left_byte, right_byte)) => eprintln!(
                "redraw_bench packed scene {label}: left_len={} right_len={} equal=false first_offset={offset} left={left_byte:?} right={right_byte:?}",
                left.len(),
                right.len(),
            ),
            None => eprintln!(
                "redraw_bench packed scene {label}: left_len={} right_len={} equal=true first_offset=None",
                left.len(),
                right.len(),
            ),
        }
    }

    fn assert_fixed_quick_settings_pose_exact(
        comparison: &str,
        pose: u16,
        attempt: usize,
        mismatch: &NativePixelMismatch,
    ) {
        assert_eq!(
            mismatch.mismatches, 0,
            "fixed quick-settings pose mismatch: comparison={comparison} pose={pose} attempt={attempt} mismatches={} max_delta={} first={:?}",
            mismatch.mismatches, mismatch.max_delta, mismatch.first,
        );
    }
}
