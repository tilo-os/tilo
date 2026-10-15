//! A leading-edge throttle: the first change goes out at once, later changes
//! inside the window wait for its end. Ported from PR 541's publication
//! schedule, without its peer and live-collaboration parts.
use ui::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PublicationDecision {
    Now,
    Deferred(Instant),
}

pub(crate) struct PublicationSchedule {
    window: Duration,
    last: Option<Instant>,
    due: Option<Instant>,
}

impl PublicationSchedule {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window,
            last: None,
            due: None,
        }
    }

    /// Called when there is something to deliver.
    pub(crate) fn decide(&mut self, now: Instant) -> PublicationDecision {
        if let Some(last) = self.last
            && let Some(due) = last.checked_add(self.window)
            && now < due
        {
            self.due = Some(due);
            return PublicationDecision::Deferred(due);
        }
        self.mark_published(now);
        PublicationDecision::Now
    }

    /// Hold a change that was not even looked at: inside the window it
    /// waits for the window's end, as [`Self::decide`] would defer it, and
    /// the instant is returned; outside it `None`, and the caller must look.
    pub(crate) fn defer(&mut self, now: Instant) -> Option<Instant> {
        let due = self.last?.checked_add(self.window)?;
        if now >= due {
            return None;
        }
        self.due = Some(due);
        Some(due)
    }

    /// A delivery ran now: scheduled, at its deadline, or forced.
    pub(crate) fn mark_published(&mut self, now: Instant) {
        self.last = Some(now);
        self.due = None;
    }

    /// Nothing is left to deliver.
    pub(crate) fn clear_due(&mut self) {
        self.due = None;
    }

    pub(crate) fn due(&self) -> Option<Instant> {
        self.due
    }

    /// Hold a change until `at`, as if [`Self::defer`] had.
    pub(crate) fn hold(&mut self, at: Instant) {
        self.due = Some(at);
    }

    /// Whether [`Self::defer`] would hold a change at `now`, without
    /// scheduling anything.
    pub(crate) fn would_defer(&self, now: Instant) -> bool {
        self.last
            .and_then(|last| last.checked_add(self.window))
            .is_some_and(|due| now < due)
    }
}
