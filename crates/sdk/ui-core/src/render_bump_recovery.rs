//! Repaint after a presented frame overflowed Vello's bump buffers.
//!
//! An overflowing render drops geometry, and Vello only learns that from a
//! counter readback that completes after the frame was presented. It grows
//! its reservation then, but the dropped pixels stay in the retained target:
//! later frames repaint only their damage, and an idle window has no later
//! frame at all. A host that keeps its target (the native render worker, the
//! browser) therefore polls the counters without waiting for the GPU — after
//! every frame and, while a readback is outstanding, again shortly after —
//! and repaints the whole surface once an overflow has been collected, which
//! is when the grown reservation takes effect.
use crate::time::Duration;
use vello::{BumpBudgetError, BumpBudgetPoll};

/// How often an otherwise idle host looks at an outstanding readback.
pub const POLL_INTERVAL: Duration = Duration::from_millis(4);

/// Consecutive overflowing repaints before giving up. Vello may grow more
/// than one stage in turn (each needs its own repaint); a reservation at its
/// ceiling cannot grow and would otherwise repaint forever.
pub const MAX_REPAINTS: u8 = 4;

/// What a poll of the counters asks of the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BumpStep {
    /// Nothing to repair.
    Settled,
    /// A readback is still in flight; poll again after [`POLL_INTERVAL`].
    Pending,
    /// Presented pixels are missing geometry: repaint the whole surface.
    Repaint,
}

/// One renderer's overflow recovery: whether a readback is out, and how
/// many repaints in a row an overflow has asked for.
#[derive(Debug, Default)]
pub struct BumpRecovery {
    pending: bool,
    repaints: u8,
    failed: bool,
}

impl BumpRecovery {
    /// Poll `renderer`'s counters without blocking on the GPU.
    pub fn poll(
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

    /// Step on the outcome of one poll ([`Self::poll`] does both).
    pub fn observe(&mut self, result: Result<BumpBudgetPoll, BumpBudgetError>) -> BumpStep {
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

    /// How long the host may wait for its next frame before polling again.
    #[must_use]
    pub fn wait(&self) -> Option<Duration> {
        self.pending.then_some(POLL_INTERVAL)
    }
}

/// The overflows collected from one renderer, for a host whose retained
/// targets it draws ([`crate::retained_raster::RetainedFrames::note_overflows`]):
/// each target repaints whole once after each.
#[derive(Debug, Default)]
pub struct Overflows {
    recovery: BumpRecovery,
    collected: u64,
}

impl Overflows {
    /// Poll `renderer`'s counters without blocking on the GPU; while a
    /// readback is still out, how soon to poll again.
    pub fn poll(
        &mut self,
        renderer: &mut vello::Renderer,
        device: &vello::wgpu::Device,
    ) -> Option<Duration> {
        let step = self.recovery.poll(renderer, device);
        self.step(step)
    }

    /// [`Self::poll`] on the outcome of one poll.
    pub fn observe(&mut self, result: Result<BumpBudgetPoll, BumpBudgetError>) -> Option<Duration> {
        let step = self.recovery.observe(result);
        self.step(step)
    }

    fn step(&mut self, step: BumpStep) -> Option<Duration> {
        match step {
            BumpStep::Repaint => {
                self.collected = self.collected.wrapping_add(1);
                None
            }
            BumpStep::Pending => self.recovery.wait(),
            BumpStep::Settled => None,
        }
    }

    /// How many overflows have been collected.
    #[must_use]
    pub const fn collected(&self) -> u64 {
        self.collected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame drawn while the renderer's bump buffers overflowed shows
    /// with geometry missing; once the overflow is collected, the next
    /// frame repaints the whole target, even one that changed nowhere, and
    /// the frame after that repaints only what changed again.
    #[test]
    fn a_collected_overflow_repaints_the_whole_target_once() {
        use crate::retained_raster::{Repaint, RetainedFrames, TargetKey, Unknown};
        let scene = vello::Scene::new();
        let parts = crate::scene_parts::SectionParts::default();
        let key = TargetKey {
            width: 800,
            height: 600,
            to_device: vello::kurbo::Affine::IDENTITY,
            base_color: vello::peniko::Color::BLACK,
            generation: 0,
        };
        let mut overflows = Overflows::default();
        let mut frames = RetainedFrames::default();
        frames.presented(&scene, Some(&parts), key);
        let unchanged = Repaint::Regions(Vec::new());

        assert_eq!(
            overflows.observe(Ok(BumpBudgetPoll::Pending)),
            Some(POLL_INTERVAL),
            "a readback still out is polled again soon"
        );
        frames.note_overflows(overflows.collected());
        assert!(!frames.shows_lost_geometry(overflows.collected()));
        assert_eq!(frames.repaint(&scene, Some(&parts), key), unchanged);

        assert_eq!(
            overflows.observe(Ok(BumpBudgetPoll::Complete { overflowed: true })),
            None
        );
        assert!(
            frames.shows_lost_geometry(overflows.collected()),
            "the target asks for a frame"
        );
        frames.note_overflows(overflows.collected());
        assert_eq!(
            frames.repaint(&scene, Some(&parts), key),
            Repaint::Full(Unknown::LostGeometry),
            "the frame after the overflow repaints whole"
        );
        frames.presented(&scene, Some(&parts), key);
        frames.note_overflows(overflows.collected());
        assert!(!frames.shows_lost_geometry(overflows.collected()));
        assert_eq!(
            frames.repaint(&scene, Some(&parts), key),
            unchanged,
            "once repainted, only changes repaint"
        );

        assert_eq!(
            overflows.observe(Ok(BumpBudgetPoll::Complete { overflowed: false })),
            None
        );
        frames.note_overflows(overflows.collected());
        assert_eq!(frames.repaint(&scene, Some(&parts), key), unchanged);
    }

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
