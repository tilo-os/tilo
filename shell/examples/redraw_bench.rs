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

use std::time::Instant;

use automerge::{AutoCommit, ROOT};
use shell::compositor::backend::drm::paint::{BridgePhaseTiming, VelloDrmBridge};
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

fn diagnostic_mean(total: u128, count: u128) -> Result<u128, String> {
    total
        .checked_div(count)
        .ok_or_else(|| "diagnostic sample count was zero".to_owned())
}

fn run() -> Result<(), String> {
    let diagnostic = profiling_requested();
    let mut frame = FrameBuilder::new((WIDTH, HEIGHT));
    let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).map_err(|error| error.to_string())?;
    bridge.set_phase_timing_enabled(diagnostic);
    let adapter = bridge.adapter_info().map_err(|error| error.to_string())?;
    println!(
        "DRM bridge adapter: name={:?} backend={} device_type={} driver={:?}",
        adapter.name, adapter.backend, adapter.device_type, adapter.driver
    );
    if diagnostic {
        println!(
            "diagnostic: {PROFILE_ENV}=1; phase timing is enabled and acceptance gates are disabled"
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
    let mut total_render_attempts = 0_u64;
    let mut recovery_attempts = 0_u64;
    let mut phase_totals = BenchmarkPhaseTotals::default();
    paint_initial_desktop(&mut frame, &mut bridge, &mut destination, destination_pitch)?;
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
        let scene = frame.paint();
        let scene_us = scene_started.map_or(0, |started| started.elapsed().as_micros());
        bridge
            .paint_into_xrgb8888(&scene, &mut destination, destination_pitch)
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
        total_render_attempts = total_render_attempts.saturating_add(attempts);
        recovery_attempts = recovery_attempts.saturating_add(attempts.saturating_sub(1));
        samples.push(Some(start.elapsed().as_micros()));
    }
    finish_run(
        &samples,
        diagnostic,
        total_render_attempts,
        recovery_attempts,
        phase_totals,
    )
}

/// Establish the displayed desktop and its live input-target registry before timed redraws.
fn paint_initial_desktop(
    frame: &mut FrameBuilder,
    bridge: &mut VelloDrmBridge,
    destination: &mut [u8],
    destination_pitch: usize,
) -> Result<(), String> {
    let initial_scene = frame.paint();
    bridge
        .paint_into_xrgb8888(&initial_scene, destination, destination_pitch)
        .map_err(|error| format!("initial desktop paint: {error}"))
}

fn finish_run(
    samples: &[Option<u128>],
    diagnostic: bool,
    total_render_attempts: u64,
    recovery_attempts: u64,
    phase_totals: BenchmarkPhaseTotals,
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
        "Vello full-render attempts: total={total_render_attempts} recovery={recovery_attempts}"
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
        BenchmarkPhaseTiming, BenchmarkPhaseTotals, BridgePhaseTiming, GateOutcome, MEAN_LIMIT_US,
        P99_LIMIT_US, SampleError, gate_outcome, meets_gates, summarize_samples,
    };

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
}
