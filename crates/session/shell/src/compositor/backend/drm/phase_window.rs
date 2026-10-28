//! Frame pacing: the phase window a presenter reports its timing through, and
//! the flush that emits it.

#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

/// How often the presenter reports its frame timing.
#[cfg(test)]
const PHASE_WINDOW_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(super) struct PhaseWindow {
    pub(super) started: Instant,
    pub(super) frames: u64,
    pub(super) direct_frames: u64,
    pub(super) direct_dropped: u64,
    pub(super) flip_completions: u64,
    pub(super) submit_us: u128,
    pub(super) gpu_readback_map_us: u128,
    pub(super) settle_us: u128,
    pub(super) cpu_swizzle_us: u128,
    pub(super) backdrop_us: u128,
    /// Render-thread time from a direct frame's start to its completed GPU
    /// work, summed and at most, over the window's direct frames.
    pub(super) direct_us: u128,
    pub(super) max_direct_us: u128,
    /// Direct frames the renderer repaired sparsely rather than in full.
    pub(super) direct_sparse_frames: u64,
    /// Frames an output showed by flipping a fullscreen client's own buffer,
    /// rendering nothing.
    pub(super) client_scanout_frames: u64,
    pub(super) readback_bytes: u64,
    pub(super) output_bytes: u64,
    /// Passes the readback presenter made over an output's pixels on the CPU (rotate and
    /// swizzle or PQ-encode, ICC copy, qcms), summed over the window's frames and outputs.
    /// Zero while every output renders direct.
    pub(super) cpu_pixel_passes: u64,
    /// Wall time around the complete renderer bridge call. This remains
    /// available when a failed/timed-out paint cannot publish its detailed
    /// Vello phase record.
    pub(super) bridge_us: u128,
    /// Wall time after the bridge returns until the scanout submission path
    /// completes (dumb-buffer cursor work, dirtyfb/page-flip included).
    pub(super) scanout_us: u128,
    /// Legacy readback attempts which returned without detailed bridge phase
    /// timing. They are still frames attempted by the physical presenter and
    /// must not disappear from a performance incident report.
    pub(super) unprofiled_frames: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::backend::drm::DirectFrame;
    use crate::shell::render::paint;

    fn timing(value: u64) -> paint::BridgePhaseTiming {
        paint::BridgePhaseTiming {
            submit_us: u128::from(value),
            completed_readback_map_us: u128::from(value.saturating_mul(2)),
            settle_us: u128::from(value.saturating_mul(3)),
            cpu_copy_swizzle_us: u128::from(value.saturating_mul(4)),
            readback_bytes: value.saturating_mul(5),
            cpu_output_bytes: value.saturating_mul(6),
            backdrop_us: u128::from(value.saturating_mul(7)),
        }
    }

    #[test]
    fn phase_window_accumulates_and_records_every_frame_outcome() {
        {
            let mut window = PhaseWindow::default();
            window.record(timing(2));
            window.record(timing(3));

            assert_eq!(window.frames, 2);
            assert_eq!(window.direct_frames, 0);
            assert_eq!(window.direct_dropped, 0);
            assert_eq!(window.flip_completions, 0);
            assert_eq!(window.submit_us, 5);
            assert_eq!(window.gpu_readback_map_us, 10);
            assert_eq!(window.settle_us, 15);
            assert_eq!(window.cpu_swizzle_us, 20);
            assert_eq!(window.backdrop_us, 35);
            assert_eq!(window.readback_bytes, 25);
            assert_eq!(window.output_bytes, 30);
            assert_eq!(window.bridge_us, 0);
            assert_eq!(window.scanout_us, 0);
            assert_eq!(window.unprofiled_frames, 0);
        }
        {
            let mut window = PhaseWindow::default();
            window.record_legacy_attempt(None, 41, 7);
            window.record_legacy_attempt(Some(timing(2)), 23, 5);

            assert_eq!(window.frames, 2);
            assert_eq!(window.unprofiled_frames, 1);
            assert_eq!(window.bridge_us, 64);
            assert_eq!(window.scanout_us, 12);
            assert_eq!(window.submit_us, 2);
            assert_eq!(window.gpu_readback_map_us, 4);
            let summary = window.summary_fields(window.started + Duration::from_micros(99));
            assert_eq!(summary.frames, 2);
            assert_eq!(summary.unprofiled_frames, 1);
            assert_eq!(summary.bridge_us, 64);
            assert_eq!(summary.scanout_us, 12);
            assert_eq!(summary.window_us, 99);
        }
        {
            let mut window = PhaseWindow::default();
            window.accumulate(paint::BridgePhaseTiming {
                submit_us: 2,
                completed_readback_map_us: 4,
                settle_us: 6,
                backdrop_us: 14,
                ..Default::default()
            });
            window.record_direct_frame(7_000, false);
            window.record_direct_frame(3_000, true);
            window.record_direct_drop();
            window.record_flip_completions(2);

            assert_eq!(window.frames, 2);
            assert_eq!(window.direct_frames, 2);
            assert_eq!(window.submit_us, 2);
            assert_eq!(window.gpu_readback_map_us, 4);
            assert_eq!(window.settle_us, 6);
            assert_eq!(window.backdrop_us, 14);
            assert_eq!(window.direct_us, 10_000);
            assert_eq!(window.max_direct_us, 7_000);
            assert_eq!(window.direct_sparse_frames, 1);
            assert_eq!(window.direct_dropped, 1);
            assert_eq!(window.flip_completions, 2);
            assert_eq!(window.readback_bytes, 0);
            assert_eq!(window.output_bytes, 0);
        }
        {
            let mut window = PhaseWindow::default();
            let mut frame = DirectFrame::default();
            frame.add(4_000, true);
            frame.add(5_000, false);
            frame.record(&mut window);
            let mut sparse = DirectFrame::default();
            sparse.add(1_000, true);
            sparse.add(2_000, true);
            sparse.record(&mut window);
            DirectFrame::default().record(&mut window);

            assert_eq!(window.frames, 2, "each frame counts once, not per output");
            assert_eq!(window.direct_frames, 2);
            assert_eq!(window.direct_us, 12_000);
            assert_eq!(window.max_direct_us, 9_000);
            assert_eq!(
                window.direct_sparse_frames, 1,
                "a frame is sparse only when every output repaired damage only"
            );
        }
    }

    #[test]
    fn phase_window_flush_timing_and_reset() {
        {
            let mut window = PhaseWindow::default();
            let started = Instant::now();
            window.started = started;

            let just_before = (started + PHASE_WINDOW_INTERVAL)
                .checked_sub(Duration::from_nanos(1))
                .unwrap();
            assert!(!window.due(just_before));
            assert!(window.due(started + PHASE_WINDOW_INTERVAL));
        }
        {
            let mut window = PhaseWindow::default();
            let started = Instant::now();
            window.started = started;

            assert!(!window.should_emit(started + Duration::from_millis(1), true));
            window.record_legacy_attempt(None, 41, 7);
            assert!(window.should_emit(started + Duration::from_millis(1), true));
            assert!(!window.should_emit(started + Duration::from_millis(1), false));
            assert!(window.should_emit(started + PHASE_WINDOW_INTERVAL, false));
        }
        {
            let mut window = PhaseWindow::default();
            let now = Instant::now();
            window.started = now;
            window.record_direct_drop();
            window.record_flip_completions(1);

            assert!(window.should_emit(now, true));
        }
        {
            let mut window = PhaseWindow::default();
            window.record(timing(7));
            let now = Instant::now();
            window.reset(now);

            assert_eq!(window.started, now);
            assert_eq!(window.frames, 0);
            assert_eq!(window.direct_frames, 0);
            assert_eq!(window.direct_dropped, 0);
            assert_eq!(window.flip_completions, 0);
            assert_eq!(window.submit_us, 0);
            assert_eq!(window.gpu_readback_map_us, 0);
            assert_eq!(window.settle_us, 0);
            assert_eq!(window.cpu_swizzle_us, 0);
            assert_eq!(window.readback_bytes, 0);
            assert_eq!(window.output_bytes, 0);
            assert_eq!(window.bridge_us, 0);
            assert_eq!(window.scanout_us, 0);
            assert_eq!(window.unprofiled_frames, 0);
        }
    }
}
