//! What the harness's frames cost and what they draw: the per-pass timing a scenario reads off, the
//! drawing each frame composed, and the counters a test holds a frame to (see [`crate::profiling`]).

use super::FrameHarness;

impl FrameHarness {
    /// Install a capture profiler so [`Self::relayout`] / [`Self::send`] time each pass and commit a
    /// per-frame sample.
    pub fn begin_profiling(&mut self) {
        self.invalidate_local();
        self.pump_component_wakes();
        self.profiler = Some(crate::profiling::FrameProfiler::for_capture());
    }

    /// The draw commands the frames drawn since construction encoded: a frame that draws only what
    /// changed adds few, one that draws the whole tree adds all of it, every frame.
    #[must_use]
    pub const fn drawn_commands(&self) -> u64 {
        self.drawn_commands
    }

    /// Stop per-pass timing; subsequent frames pay nothing again.
    pub fn end_profiling(&mut self) {
        self.invalidate_local();
        self.profiler = None;
    }

    /// Render one frame with no input and record it.
    pub fn profile_frame(&mut self) {
        self.invalidate_local();
        self.pump_component_wakes();
        self.relayout();
    }

    /// Discard captured frames (e.g. warmup) without stopping capture.
    /// Quiet: drains the profiler's samples; nothing a frame reads.
    pub fn clear_profile_samples(&mut self) {
        self.pump_component_wakes();
        if let Some(p) = self.profiler.as_mut() {
            p.clear_capture();
        }
    }

    /// Take every captured frame so far, draining the buffer.
    /// Quiet: drains the profiler's samples; nothing a frame reads.
    pub fn take_profile_samples(&mut self) -> Vec<crate::profiling::FrameTiming> {
        self.pump_component_wakes();
        self.profiler
            .as_mut()
            .map(crate::profiling::FrameProfiler::take_capture)
            .unwrap_or_default()
    }
}
