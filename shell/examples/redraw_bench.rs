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
use shell::compositor::backend::drm::paint::VelloDrmBridge;
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

fn run() -> Result<(), String> {
    let mut frame = FrameBuilder::new((WIDTH, HEIGHT));
    let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).map_err(|error| error.to_string())?;
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

    // A real compositor has already displayed an initial desktop before it receives the actions
    // below. Paint it through the same bridge so target rectangles are the ones the renderer
    // produced; do not treat this startup frame as one of the action-triggered redraw samples.
    let initial_scene = frame.paint();
    bridge
        .paint_into_xrgb8888(&initial_scene, &mut destination, destination_pitch)
        .map_err(|error| format!("initial desktop paint: {error}"))?;

    for sample_index in 0..SAMPLE_COUNT {
        let start = Instant::now();
        let target_index = sample_index
            .checked_rem(ACTION_TARGETS.len())
            .ok_or_else(|| "benchmark action target list is empty".to_owned())?;
        let target = *ACTION_TARGETS
            .get(target_index)
            .ok_or_else(|| "benchmark action target list is empty".to_owned())?;
        click_named_target(&mut frame, target, modifiers, &mut document, &mut focus)?;

        // This is the native compositor's per-redraw sequence: input mutates state, then layout
        // and paint produce the scene. `paint_settled` is deliberately not used: it can perform a
        // capture-only second layout/draw that production redraws do not unconditionally take.
        frame.relayout();
        let scene = frame.paint();
        bridge
            .paint_into_xrgb8888(&scene, &mut destination, destination_pitch)
            .map_err(|error| format!("sample {sample_index}: {error}"))?;
        let attempts = u64::try_from(bridge.last_render_attempts())
            .map_err(|_| "render attempt count does not fit u64".to_owned())?;
        total_render_attempts = total_render_attempts.saturating_add(attempts);
        recovery_attempts = recovery_attempts.saturating_add(attempts.saturating_sub(1));
        samples.push(Some(start.elapsed().as_micros()));
    }

    let summary =
        summarize_samples(&samples).map_err(|error| format!("invalid sample set: {error:?}"))?;
    println!(
        "DRM render/readback at {WIDTH}x{HEIGHT}: samples={} mean={}us p99={}us min={}us max={}us",
        summary.count, summary.mean_us, summary.p99_us, summary.min_us, summary.max_us
    );
    println!(
        "gates: mean<={MEAN_LIMIT_US}us p99<={P99_LIMIT_US}us ({})",
        if meets_gates(summary) { "PASS" } else { "FAIL" }
    );
    println!(
        "Vello full-render attempts: total={total_render_attempts} recovery={recovery_attempts}"
    );
    println!(
        "presentation: unavailable in this harness (no DRM/KMS device, dumb-buffer page flip, or display scanout timed)"
    );

    if meets_gates(summary) {
        Ok(())
    } else {
        Err(format!(
            "redraw gate failed: total={}us / {} samples, p99={}us",
            summary.total_us, summary.count, summary.p99_us
        ))
    }
}

fn click_named_target(
    frame: &mut FrameBuilder,
    target: &str,
    modifiers: Modifiers,
    document: &mut AutoCommit,
    focus: &mut Option<NodeId>,
) -> Result<(), String> {
    let rect = frame
        .widget_registry()
        .get(target)
        .copied()
        .filter(|rect| rect.area() > 0.0)
        .ok_or_else(|| format!("required input target {target:?} is absent or has zero area"))?;
    let center = rect.center();
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
    use super::{MEAN_LIMIT_US, P99_LIMIT_US, SampleError, meets_gates, summarize_samples};

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
}
