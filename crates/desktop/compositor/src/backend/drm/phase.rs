//! What the DRM presenter's frames cost over a window, written to the session's flight
//! record every couple of seconds as one `DRM render phase summary` line.
//!
//! `tilo-session-report --render-check` reads those lines and holds a login's presentation
//! to the release's budgets: a frame the compositor did not have to draw is a direct frame
//! and must reach the display within the render budget, and a frame it drew (a composited
//! one) must cost no more than that budget itself.
//!
//! A frame is direct when every element the display showed was scanned out zero-copy: no
//! plane was composited. Otherwise the compositor drew the output, and the output's pixels
//! are the bytes counted as read back through the compositor, as the flight report's
//! `readback_bytes` names them.

use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;

/// How long a window collects before its summary is written.
pub(crate) const EMIT_EVERY: Duration = Duration::from_secs(2);

/// One output frame as the presenter saw it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameCost {
    /// Nothing in the frame was composited: every element was scanned out directly.
    pub(crate) direct: bool,
    /// The compositor's own work for the frame: building and queueing it.
    pub(crate) submit_us: u64,
    /// The output's pixels, in the swapchain's format.
    pub(crate) output_bytes: u64,
}

/// The window's frames, summed for one summary line.
#[derive(Debug, Default)]
pub(crate) struct PhaseWindow {
    started: Option<Instant>,
    frames: u64,
    direct_frames: u64,
    flip_completions: u64,
    direct_us_total: u64,
    direct_us_max: u64,
    bridge_us_total: u64,
    readback_bytes: u64,
    output_bytes: u64,
}

impl PhaseWindow {
    /// Add a frame the presenter queued.
    pub(crate) fn record(&mut self, now: Instant, cost: FrameCost) {
        self.started.get_or_insert(now);
        self.frames = self.frames.saturating_add(1);
        self.output_bytes = self.output_bytes.saturating_add(cost.output_bytes);
        if cost.direct {
            self.direct_frames = self.direct_frames.saturating_add(1);
            self.direct_us_total = self.direct_us_total.saturating_add(cost.submit_us);
            self.direct_us_max = self.direct_us_max.max(cost.submit_us);
        } else {
            self.bridge_us_total = self.bridge_us_total.saturating_add(cost.submit_us);
            self.readback_bytes = self.readback_bytes.saturating_add(cost.output_bytes);
        }
    }

    /// A frame the presenter queued reached its display.
    pub(crate) fn flip(&mut self) {
        self.flip_completions = self.flip_completions.saturating_add(1);
    }

    /// The summary line, once a window has run long enough and showed something; the
    /// counters then start over at `now`. A window that showed nothing keeps collecting.
    pub(crate) fn take(&mut self, now: Instant) -> Option<String> {
        let started = self.started?;
        if self.frames == 0 || now.saturating_duration_since(started) < EMIT_EVERY {
            return None;
        }
        let window_us =
            u64::try_from(now.saturating_duration_since(started).as_micros()).unwrap_or(u64::MAX);
        let average_direct_us = self
            .direct_us_total
            .checked_div(self.direct_frames)
            .unwrap_or(0);
        let (status, reason) = if self.direct_frames == self.frames {
            ("enabled", None)
        } else if self.direct_frames == 0 {
            ("unavailable", Some("the compositor drew every frame"))
        } else {
            ("mixed", Some("the compositor drew some frames"))
        };
        // A window with nothing to explain says no reason: the report reads the field only
        // when it is there.
        let reason =
            reason.map_or_else(String::new, |why| format!(" direct_scanout_reason={why:?}"));
        let line = format!(
            "frames={} direct_frames={} direct_dropped=0 flip_completions={} unprofiled_frames=0 \
             window_us={window_us} bridge_us={} readback_bytes={} output_bytes={} \
             avg_direct_us={average_direct_us} max_direct_us={} direct_failures=0 \
             direct_scanout_status={status}{reason} DRM render phase summary",
            self.frames,
            self.direct_frames,
            self.flip_completions,
            self.bridge_us_total,
            self.readback_bytes,
            self.output_bytes,
            self.direct_us_max,
        );
        self.started = Some(now);
        self.frames = 0;
        self.direct_frames = 0;
        self.flip_completions = 0;
        self.direct_us_total = 0;
        self.direct_us_max = 0;
        self.bridge_us_total = 0;
        self.readback_bytes = 0;
        self.output_bytes = 0;
        Some(line)
    }
}

/// The bytes an output frame of `size` at `format` holds.
pub(crate) fn output_bytes(size: (u32, u32), format: Fourcc) -> u64 {
    // The swapchain formats the DRM backend asks for are 4 bytes a pixel; an HDR output
    // runs at half float, and an 8-bit sink may take RGB565.
    let per_pixel = match format {
        Fourcc::Abgr16161616f | Fourcc::Xrgb16161616f => 8,
        Fourcc::Rgb565 => 2,
        _ => 4,
    };
    u64::from(size.0)
        .saturating_mul(u64::from(size.1))
        .saturating_mul(per_pixel)
}

/// Whether the elements `states` reports were all scanned out directly: at least one was
/// shown, and none of the shown ones was composited.
pub(crate) fn all_scanned_out(
    states: impl Iterator<Item = smithay::backend::renderer::element::RenderElementPresentationState>,
) -> bool {
    use smithay::backend::renderer::element::RenderElementPresentationState;
    let mut shown = 0_u64;
    for state in states {
        match state {
            RenderElementPresentationState::Skipped => {}
            RenderElementPresentationState::ZeroCopy => shown = shown.saturating_add(1),
            RenderElementPresentationState::Rendering { .. } => return false,
        }
    }
    shown > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(direct: bool) -> FrameCost {
        FrameCost {
            direct,
            submit_us: if direct { 6_000 } else { 9_000 },
            output_bytes: 1920 * 1080 * 4,
        }
    }

    tilo_test_env::scenario! {
        /// A window's counters read back as the summary the release's render check parses.
        fn a_phase_summary_carries_what_the_release_report_reads;

        phase a_window_reports_its_direct_and_composited_frames;
        {
            let mut window = PhaseWindow::default();
            let start = Instant::now();
            assert_eq!(window.take(start), None, "a window with no frames emits nothing");
            window.record(start, frame(true));
            window.record(start, frame(false));
            window.record(start, frame(false));
            window.flip();
            window.flip();
            window.flip();
            assert_eq!(window.take(start + EMIT_EVERY / 2), None, "not yet due");
            let line = window.take(start + EMIT_EVERY).expect("due after the interval");
            assert!(line.ends_with(" DRM render phase summary"), "{line}");
            for field in [
                "frames=3",
                "direct_frames=1",
                "flip_completions=3",
                "bridge_us=18000",
                "readback_bytes=16588800",
                "output_bytes=24883200",
                "avg_direct_us=6000",
                "max_direct_us=6000",
                "direct_scanout_status=mixed",
            ] {
                assert!(line.contains(field), "{field} in {line}");
            }
            assert_eq!(window.take(start + EMIT_EVERY * 2), None, "the counters started over");
        }

        phase all_direct_and_all_composited_windows_say_which;
        {
            let mut direct = PhaseWindow::default();
            let start = Instant::now();
            direct.record(start, frame(true));
            let line = direct.take(start + EMIT_EVERY).expect("due");
            assert!(line.contains("direct_scanout_status=enabled"), "{line}");
            assert!(line.contains("readback_bytes=0"), "{line}");

            let mut drawn = PhaseWindow::default();
            drawn.record(start, frame(false));
            let line = drawn.take(start + EMIT_EVERY).expect("due");
            assert!(line.contains("direct_scanout_status=unavailable"), "{line}");
            assert!(line.contains("readback_bytes=8294400"), "{line}");
        }

        phase a_format_has_the_bytes_its_pixels_take;
        {
            assert_eq!(output_bytes((1920, 1080), Fourcc::Xrgb8888), 1920 * 1080 * 4);
            assert_eq!(output_bytes((1920, 1080), Fourcc::Abgr16161616f), 1920 * 1080 * 8);
            assert_eq!(output_bytes((1920, 1080), Fourcc::Rgb565), 1920 * 1080 * 2);
        }
    }
}
