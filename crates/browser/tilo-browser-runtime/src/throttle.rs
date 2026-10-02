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
    /// Configured window; `window` is shorter while [`Self::set_live_window`]
    /// applies (live collaborators receive each publication at once).
    configured: Duration,
    last: Option<Instant>,
    due: Option<Instant>,
}

impl PublicationSchedule {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window,
            configured: window,
            last: None,
            due: None,
        }
    }

    /// LIVE_V1: while other members are present, shorten the publication
    /// window (never lengthen it); `None` restores the configured window.
    /// A pending deadline moves with the window.
    pub(crate) fn set_live_window(&mut self, window: Option<Duration>) {
        self.window = window.map_or(self.configured, |window| window.min(self.configured));
        if self.due.is_some()
            && let Some(last) = self.last
        {
            self.due = last.checked_add(self.window);
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
}
