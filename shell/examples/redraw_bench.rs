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

use std::array;
use std::time::Instant;

use automerge::{AutoCommit, ROOT};
use shell::compositor::backend::drm::paint::{BridgePhaseTiming, SceneDamage, VelloDrmBridge};
use shell::compositor::frame::FrameBuilder;
use ui::taffy::NodeId;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

const WIDTH: u32 = 3_840;
const HEIGHT: u32 = 2_160;
const SAMPLE_COUNT: usize = 1_000;
const MEAN_LIMIT_US: u128 = 8_000;
const P99_LIMIT_US: u128 = 30_000;
const ACTION_TARGETS: [&str; 2] = ["topbar_tray_pill", "topbar_overview_button"];
const PROFILE_ENV: &str = "QOS_REDRAW_BENCH_PROFILE";
const VELLO_GPU_TIMESTAMPS_ENV: &str = "VELLO_GPU_TIMESTAMPS";
const VELLO_GPU_DISPATCH_TIMESTAMPS_ENV: &str = "VELLO_GPU_DISPATCH_TIMESTAMPS";

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RenderAttemptTotals {
    total: u64,
    sparse: u64,
    full: u64,
    recovery: u64,
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

/// Read the diagnostic slot only after the bridge has synchronously completed the action's
/// texture readback. The expected submission id is captured before consuming the sample so a
/// delayed sample is recorded as stale/missing, never assigned to the wrong action.
fn record_gpu_dispatch_timestamps(
    bridge: &mut VelloDrmBridge,
    totals: &mut GpuDispatchTimingTotals,
    recovery_submissions: u64,
) {
    let expected_submission_id = bridge.gpu_dispatch_timestamp_latest_submission_id();
    let sample = bridge.take_gpu_dispatch_timestamp_sample();
    let status = dispatch_timestamp_status_after_take(sample.as_ref(), || {
        bridge.gpu_dispatch_timestamp_status()
    });
    totals.record_completed_paint(
        expected_submission_id,
        status,
        sample.as_ref(),
        recovery_submissions,
    );
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
    reason = "the benchmark keeps setup, all required samples, and final gate reporting in one auditable linear execution path"
)]
fn run() -> Result<(), String> {
    let diagnostic = profiling_requested();
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
    let mut frame = FrameBuilder::new((WIDTH, HEIGHT));
    let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).map_err(|error| error.to_string())?;
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
        bridge
            .paint_damage_into_xrgb8888(
                &frame_paint.scene,
                SceneDamage {
                    revision: frame_paint.revision,
                    previous_revision: frame_paint.previous_revision,
                    regions: frame_paint.damage_regions.as_deref(),
                },
                &mut destination,
                destination_pitch,
            )
            .map_err(|error| format!("sample {sample_index}: {error}"))?;
        if diagnostic {
            let bridge_timing = bridge.last_phase_timing().ok_or_else(|| {
                format!("sample {sample_index}: diagnostic bridge timing was not recorded")
            })?;
            phase_totals.add_sample(BenchmarkPhaseTiming {
                input_us,
                registry_us,
                layout_us,
                scene_us,
                bridge: bridge_timing,
            });
        }
        let attempts = u64::try_from(bridge.last_render_attempts())
            .map_err(|_| "render attempt count does not fit u64".to_owned())?;
        let sparse_attempts = u64::try_from(bridge.last_sparse_render_attempts())
            .map_err(|_| "sparse render attempt count does not fit u64".to_owned())?;
        let full_attempts = u64::try_from(bridge.last_full_render_attempts())
            .map_err(|_| "full render attempt count does not fit u64".to_owned())?;
        render_attempts.total = render_attempts.total.saturating_add(attempts);
        render_attempts.sparse = render_attempts.sparse.saturating_add(sparse_attempts);
        render_attempts.full = render_attempts.full.saturating_add(full_attempts);
        let sample_recovery_attempts = attempts.saturating_sub(1);
        render_attempts.recovery = render_attempts
            .recovery
            .saturating_add(sample_recovery_attempts);
        if let Some(totals) = gpu_dispatch_timestamps.as_mut() {
            record_gpu_dispatch_timestamps(&mut bridge, totals, sample_recovery_attempts);
        }
        samples.push(Some(start.elapsed().as_micros()));
    }
    let gpu_dispatch_timestamp_profile =
        gpu_dispatch_timestamps.map(|totals| GpuDispatchTimestampProfile {
            totals,
            buffer_bytes: bridge.gpu_dispatch_timestamp_buffer_bytes(),
        });
    finish_run(
        &samples,
        diagnostic,
        render_attempts,
        phase_totals,
        gpu_dispatch_timestamp_profile.as_ref(),
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

fn finish_run(
    samples: &[Option<u128>],
    diagnostic: bool,
    render_attempts: RenderAttemptTotals,
    phase_totals: BenchmarkPhaseTotals,
    gpu_dispatch_timestamp_profile: Option<&GpuDispatchTimestampProfile>,
) -> Result<(), String> {
    let summary =
        summarize_samples(samples).map_err(|error| format!("invalid sample set: {error:?}"))?;
    println!(
        "DRM render/readback at {WIDTH}x{HEIGHT}: samples={} mean={}us p99={}us min={}us max={}us",
        summary.count, summary.mean_us, summary.p99_us, summary.min_us, summary.max_us
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
        "presentation: unavailable in this harness (no DRM/KMS device, dumb-buffer page flip, or display scanout timed)"
    );

    if diagnostic {
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
    }
    match gate {
        GateOutcome::Pass => Ok(()),
        GateOutcome::Fail => Err(format!(
            "redraw gate failed: total={}us / {} samples, p99={}us",
            summary.total_us, summary.count, summary.p99_us
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
    let _ = frame.dispatch_event(&cursor, &modifiers, document, &ROOT, focus);
    let pressed = frame.dispatch_event(&press, &modifiers, document, &ROOT, focus);
    let released = frame.dispatch_event(&release, &modifiers, document, &ROOT, focus);
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
        ACTION_TARGETS, BenchmarkPhaseTiming, BenchmarkPhaseTotals, BridgePhaseTiming, GateOutcome,
        GpuDispatchTimingTotals, HEIGHT, MEAN_LIMIT_US, P99_LIMIT_US, SampleError, WIDTH,
        direct_path_workgroup_configuration_is_valid, dispatch_click,
        dispatch_timestamp_status_after_take, gate_outcome, meets_gates,
        summarize_diagnostic_values, summarize_samples, timestamp_profile_configuration_is_valid,
    };
    use automerge::AutoCommit;
    use shell::compositor::backend::drm::paint::{SceneDamage, VelloDrmBridge};
    use shell::compositor::frame::FrameBuilder;
    use ui::taffy::NodeId;
    use winit::event::Modifiers;

    #[test]
    fn aggregation_uses_all_samples_and_nearest_rank_p99() {
        let mut samples = vec![Some(1_000); 989];
        samples.push(Some(30_000));
        samples.extend(std::iter::repeat_n(Some(50_000), 10));

        let summary = summarize_samples(&samples).expect("valid samples");
        assert_eq!(summary.count, 1_000);
        assert_eq!(summary.total_us, 1_519_000);
        assert_eq!(summary.mean_us, 1_519);
        assert_eq!(summary.p99_us, 30_000);
        assert_eq!(summary.min_us, 1_000);
        assert_eq!(summary.max_us, 50_000);
    }

    #[test]
    fn empty_zero_and_missing_samples_are_rejected() {
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

    #[test]
    fn gates_check_exact_mean_and_p99_boundaries() {
        let mut passing = vec![Some(7_755); 989];
        passing.extend(std::iter::repeat_n(Some(P99_LIMIT_US), 11));
        let passing = summarize_samples(&passing).expect("valid samples");
        let count = u128::try_from(passing.count).expect("usize must fit u128");
        assert!(passing.total_us <= MEAN_LIMIT_US * count);
        assert_eq!(passing.p99_us, P99_LIMIT_US);
        assert!(meets_gates(passing));

        let mut mean_fail = vec![Some(7_756); 989];
        mean_fail.extend(std::iter::repeat_n(Some(P99_LIMIT_US), 11));
        assert!(!meets_gates(
            summarize_samples(&mean_fail).expect("valid samples")
        ));

        let mut p99_fail = vec![Some(7_755); 989];
        p99_fail.extend(std::iter::repeat_n(Some(P99_LIMIT_US + 1), 11));
        assert!(!meets_gates(
            summarize_samples(&p99_fail).expect("valid samples")
        ));
    }

    #[test]
    fn diagnostic_runs_cannot_pass_acceptance() {
        let summary = summarize_samples(&vec![Some(1_000); 1_000]).expect("valid samples");
        assert!(meets_gates(summary));
        assert_eq!(gate_outcome(summary, false), GateOutcome::Pass);
        assert_eq!(
            gate_outcome(summary, true),
            GateOutcome::DisabledDiagnostic,
            "diagnostic timestamps perturb timing and must never qualify a release gate"
        );
    }

    #[test]
    fn timestamp_flags_require_explicit_diagnostic_mode() {
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

    #[test]
    fn direct_path_workgroup_override_requires_explicit_diagnostic_mode() {
        assert!(direct_path_workgroup_configuration_is_valid(false, 128));
        assert!(!direct_path_workgroup_configuration_is_valid(false, 1));
        assert!(!direct_path_workgroup_configuration_is_valid(false, 8));
        assert!(direct_path_workgroup_configuration_is_valid(true, 1));
    }

    #[test]
    fn diagnostic_phase_aggregation_keeps_every_phase_and_byte() {
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

    #[test]
    fn dispatch_timestamp_aggregation_requires_exact_submission_match() {
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

    #[test]
    fn dispatch_timestamp_mismatch_and_unavailable_states_are_never_zero_time() {
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

    #[test]
    fn dispatch_timestamp_missing_sample_reads_post_take_invalid_status() {
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

    #[test]
    fn benchmark_actions_alternate_quick_settings_open_and_outside_dismissal() {
        let mut frame = FrameBuilder::new((WIDTH, HEIGHT));
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

    #[test]
    fn benchmark_actions_produce_bounded_damage() {
        let mut frame = FrameBuilder::new((WIDTH, HEIGHT));
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

    #[test]
    fn native_4k_retained_pixels_match_dense_oracle_for_benchmark_actions() {
        let mut frame = FrameBuilder::new((WIDTH, HEIGHT));
        let width = usize::try_from(WIDTH).expect("native width fits usize");
        let height = usize::try_from(HEIGHT).expect("native height fits usize");
        let pitch = width.checked_mul(4).expect("native pitch fits usize");
        let len = pitch.checked_mul(height).expect("native output fits usize");
        let mut retained = VelloDrmBridge::new(WIDTH, HEIGHT).expect("retained bridge");
        let mut dense = VelloDrmBridge::new(WIDTH, HEIGHT).expect("dense oracle bridge");
        retained.set_phase_timing_enabled(true);
        let mut retained_pixels = vec![0_u8; len];
        let mut dense_pixels = vec![0_u8; len];
        let mut document = AutoCommit::new();
        let modifiers = Modifiers::default();
        let mut focus: Option<NodeId> = None;
        let mut sparse_attempts = 0_u64;
        let mut full_attempts = 0_u64;
        let mut readback_bytes = 0_u128;

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
            dense
                .paint_into_xrgb8888(&candidate.scene, &mut dense_pixels, pitch)
                .expect("dense native oracle paint");
            assert_native_pixels_equal(&retained_pixels, &dense_pixels, WIDTH, sample_index);
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
            "redraw_bench native 4K oracle: sparse_attempts={sparse_attempts} full_attempts={full_attempts} readback_payload_bytes={readback_bytes}"
        );
        assert!(
            sparse_attempts > 0,
            "benchmark candidates must exercise sparse paint"
        );
    }

    fn assert_native_pixels_equal(actual: &[u8], expected: &[u8], width: u32, candidate: usize) {
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
        assert_eq!(
            mismatches, 0,
            "native pixel mismatch after candidate {candidate}: mismatches={mismatches} max_delta={max_delta} first={first:?}"
        );
    }
}
