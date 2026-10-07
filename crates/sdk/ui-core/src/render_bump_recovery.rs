//! Repaint after a presented frame overflowed Vello's bump buffers.
//!
//! An overflowing render drops geometry, and Vello only learns that from a
//! counter readback that completes after the frame was presented. It grows
//! its reservation then, but the dropped pixels stay in the retained target:
//! later frames repaint only their damage, and an idle window has no later
//! frame at all. The worker therefore polls the counters without waiting for
//! the GPU — after every frame and, while a readback is outstanding, on a
//! short receive timeout — and repaints the whole surface once an overflow
//! has been collected, which is when the grown reservation takes effect.
use crate::time::Duration;
use vello::{BumpBudgetError, BumpBudgetPoll};

/// How often an otherwise idle worker looks at an outstanding readback.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(4);

/// Consecutive overflowing repaints before giving up. Vello may grow more
/// than one stage in turn (each needs its own repaint); a reservation at its
/// ceiling cannot grow and would otherwise repaint forever.
pub(crate) const MAX_REPAINTS: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BumpStep {
    /// Nothing to repair.
    Settled,
    /// A readback is still in flight; poll again after [`POLL_INTERVAL`].
    Pending,
    /// Presented pixels are missing geometry: repaint the whole surface.
    Repaint,
}

#[derive(Debug, Default)]
pub(crate) struct BumpRecovery {
    pending: bool,
    repaints: u8,
    failed: bool,
}

impl BumpRecovery {
    /// Poll `renderer`'s counters without blocking on the GPU.
    pub(crate) fn poll(
        &mut self,
        renderer: &mut vello::Renderer,
        device: &vello::wgpu::Device,
    ) -> BumpStep {
        if self.failed {
            return BumpStep::Settled;
        }
        let result = renderer.poll_bump_budget(device);
        self.observe(result)
    }

    pub(crate) fn observe(&mut self, result: Result<BumpBudgetPoll, BumpBudgetError>) -> BumpStep {
        self.pending = false;
        match result {
            Ok(BumpBudgetPoll::Pending) => {
                self.pending = true;
                BumpStep::Pending
            }
            Ok(BumpBudgetPoll::Complete { overflowed: false }) => {
                self.repaints = 0;
                BumpStep::Settled
            }
            Ok(BumpBudgetPoll::Complete { overflowed: true }) => {
                if self.repaints >= MAX_REPAINTS {
                    return BumpStep::Settled;
                }
                self.repaints = self.repaints.saturating_add(1);
                BumpStep::Repaint
            }
            Err(error) => {
                // Readback errors persist for the renderer's lifetime, so
                // polling again would only repeat this.
                self.failed = true;
                tracing::warn!(%error, "vello bump counters unavailable; overflow repaint is off");
                BumpStep::Settled
            }
        }
    }

    /// How long the worker may wait for its next frame before polling again.
    pub(crate) fn wait(&self) -> Option<Duration> {
        self.pending.then_some(POLL_INTERVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_repaints_until_it_fits_and_never_forever() {
        let mut recovery = BumpRecovery::default();
        assert_eq!(recovery.wait(), None, "nothing outstanding before a frame");
        assert_eq!(
            recovery.observe(Ok(BumpBudgetPoll::Pending)),
            BumpStep::Pending
        );
        assert_eq!(
            recovery.wait(),
            Some(POLL_INTERVAL),
            "an idle worker keeps polling an outstanding readback"
        );
        assert_eq!(
            recovery.observe(Ok(BumpBudgetPoll::Complete { overflowed: true })),
            BumpStep::Repaint
        );
        assert_eq!(recovery.wait(), None);
        // A second stage overflowing in the repaint gets its own repaint.
        assert_eq!(
            recovery.observe(Ok(BumpBudgetPoll::Complete { overflowed: true })),
            BumpStep::Repaint
        );
        assert_eq!(
            recovery.observe(Ok(BumpBudgetPoll::Complete { overflowed: false })),
            BumpStep::Settled
        );

        // A reservation that cannot grow stops repainting after the bound,
        // until a frame fits again.
        for _ in 0..MAX_REPAINTS {
            assert_eq!(
                recovery.observe(Ok(BumpBudgetPoll::Complete { overflowed: true })),
                BumpStep::Repaint
            );
        }
        assert_eq!(
            recovery.observe(Ok(BumpBudgetPoll::Complete { overflowed: true })),
            BumpStep::Settled
        );
        assert_eq!(
            recovery.observe(Ok(BumpBudgetPoll::Complete { overflowed: false })),
            BumpStep::Settled
        );
        assert_eq!(
            recovery.observe(Ok(BumpBudgetPoll::Complete { overflowed: true })),
            BumpStep::Repaint
        );

        // A failed readback stops polling instead of spinning on it.
        assert_eq!(
            recovery.observe(Err(BumpBudgetError::MapFailed)),
            BumpStep::Settled
        );
        assert_eq!(recovery.wait(), None);
    }
}
