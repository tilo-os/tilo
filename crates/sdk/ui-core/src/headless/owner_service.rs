use super::FrameHarness;
use crate::owner_wake::{OwnerTurnBudget, OwnerTurnCutoff, OwnerTurnReport};
use crate::time::{Duration, Instant};

impl FrameHarness {
    /// Service a bounded FIFO prefix through the original owner model doors.
    /// The deadline is checked between returned attempts; an individual callback
    /// cannot be preempted. This method does not schedule a host continuation.
    pub fn service_owner_turn(&mut self, budget: OwnerTurnBudget) -> OwnerTurnReport {
        self.service_owner_turn_with_clock(budget, crate::time::now)
    }

    pub(super) fn service_owner_turn_with_clock(
        &mut self,
        budget: OwnerTurnBudget,
        mut now: impl FnMut() -> Instant,
    ) -> OwnerTurnReport {
        let mut report = OwnerTurnReport {
            attempted: 0,
            completed: 0,
            remaining: 0,
            cutoff: OwnerTurnCutoff::Idle,
            elapsed: Duration::ZERO,
            overrun: Duration::ZERO,
            result: Ok(()),
        };
        let Some(queue) = self.owner_wake_queue.clone() else {
            return report;
        };
        report.remaining = queue.pending_count();
        if report.remaining == 0 {
            return report;
        }
        if budget.max_steps == 0 {
            report.cutoff = OwnerTurnCutoff::StepLimit;
            return report;
        }
        let start = now();
        if budget.deadline.is_some_and(|deadline| start >= deadline) {
            report.cutoff = OwnerTurnCutoff::Deadline;
            report.overrun = budget.deadline.map_or(Duration::ZERO, |deadline| {
                start.saturating_duration_since(deadline)
            });
            return report;
        }
        let mut batch = queue.take_batch(budget.max_steps);
        report.cutoff = OwnerTurnCutoff::SnapshotComplete;
        while let Some(work) = batch.front() {
            if budget.deadline.is_some_and(|deadline| now() >= deadline) {
                report.cutoff = OwnerTurnCutoff::Deadline;
                break;
            }
            if report.attempted == 0 {
                // Unknown visitors may replace descendants while locating the
                // owner. Retire physical maps before any arbitrary traversal.
                self.discard_layout_tree();
                self.last_layout_viewport = None;
                self.owner_service_layout_retired = true;
            }
            report.attempted += 1;
            report.result = work.check_registration().and_then(|()| {
                crate::bound_dispatch::service_retained_owner_model(
                    self.root.as_mut(),
                    &mut self.state.doc,
                    work,
                )
            });
            if report.result.is_err() {
                report.cutoff = OwnerTurnCutoff::Blocked;
                break;
            }
            batch.complete_front();
            report.completed += 1;
        }
        // Retry custody is restored before measuring the entire turn and
        // counting remaining work. Native unwinds also run this batch's Drop.
        drop(batch);
        report.remaining = queue.pending_count();
        if report.cutoff == OwnerTurnCutoff::SnapshotComplete
            && report.attempted == budget.max_steps
            && report.remaining != 0
        {
            report.cutoff = OwnerTurnCutoff::StepLimit;
        }
        let end = now();
        report.elapsed = end.saturating_duration_since(start);
        report.overrun = budget.deadline.map_or(Duration::ZERO, |deadline| {
            end.saturating_duration_since(deadline)
        });
        report
    }
}
