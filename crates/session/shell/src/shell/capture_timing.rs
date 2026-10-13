//! Bounded, opt-in timing of the existing foreign-frame capture path.
//! No pixels, documents or client credentials are recorded. Timestamps are
//! `CLOCK_MONOTONIC` nanoseconds, and each explicit enable window has a fixed quota.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const CAPTURE_LIMIT: usize = 2048;
struct WindowBudget {
    captures: AtomicUsize,
    pipes: AtomicUsize,
    exhausted: AtomicBool,
}

impl WindowBudget {
    const fn new() -> Self {
        Self {
            captures: AtomicUsize::new(0),
            pipes: AtomicUsize::new(0),
            exhausted: AtomicBool::new(false),
        }
    }

    fn transition(&self, previous: bool, next: bool) {
        if !previous && next {
            self.captures.store(0, Ordering::Relaxed);
            self.pipes.store(0, Ordering::Relaxed);
            self.exhausted.store(false, Ordering::Relaxed);
        }
    }
}

static WINDOW: WindowBudget = WindowBudget::new();

/// Called only by the existing session diagnostic configuration setter, whose
/// public action admission remains unchanged. Reading flags never renews quotas.
pub(crate) fn observation_transition(previous: bool, next: bool) {
    WINDOW.transition(previous, next);
}

#[derive(Debug, Default)]
pub(crate) struct CaptureTiming {
    pub prepare: Option<u64>,
    pub enqueue: Option<u64>,
    pub worker_start: Option<u64>,
    pub fence_start: Option<u64>,
    pub fence_end: Option<u64>,
    pub fence_waited: bool,
    pub native_import_end: Option<u64>,
    pub copy_submit: Option<u64>,
    pub copy_submit_return: Option<u64>,
    pub copy_end: Option<u64>,
}

fn admit(enabled: bool, budget: &AtomicUsize) -> bool {
    enabled
        && budget
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                (used < CAPTURE_LIMIT).then_some(used.saturating_add(1))
            })
            .is_ok()
}

pub(crate) fn now() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        crate::compositor::metrics::monotonic_time_ns().and_then(|value| u64::try_from(value).ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

impl CaptureTiming {
    pub(crate) fn start() -> Option<Self> {
        let enabled = super::launcher::LaunchSession::current().presentation_observer_enabled();
        if !enabled {
            return None;
        }
        if !admit(true, &WINDOW.captures) {
            if !WINDOW.exhausted.swap(true, Ordering::Relaxed) {
                let record = serde_json::json!({"event": "budget-exhausted", "capture_limit": CAPTURE_LIMIT});
                eprintln!("tilo-foreign-capture {record}");
            }
            return None;
        }
        Some(Self {
            prepare: now(),
            ..Self::default()
        })
    }

    pub(crate) fn imported(
        &self,
        surface: u64,
        token: u64,
        staged: Option<u64>,
        barrier: (Option<u64>, Option<u64>),
        outcome: (bool, bool),
    ) {
        let (accepted, shared_gpu) = outcome;
        let record = serde_json::json!({
            "event": "imported", "clock_id": 1, "surface_id": surface, "capture_token": token,
            "prepare_ns": self.prepare, "enqueue_ns": self.enqueue,
            "worker_start_ns": self.worker_start, "fence_start_ns": self.fence_start,
            "fence_end_ns": self.fence_end, "native_import_end_ns": self.native_import_end,
            "copy_submit_ns": self.copy_submit, "copy_submit_return_ns": self.copy_submit_return,
            "copy_end_ns": self.copy_end, "staged_ns": staged,
            "barrier_ns": barrier.0, "barrier_return_ns": barrier.1,
            "accepted": accepted, "shared_gpu": shared_gpu,
            "borrowed_gpu": shared_gpu, "fence_waited": self.fence_waited,
        });
        eprintln!("tilo-foreign-capture {record}");
    }
}

pub(crate) fn activated(surface: u64, token: u64, seq: u64, accepted: bool) {
    let record = serde_json::json!({
        "event": "activated", "clock_id": 1, "surface_id": surface,
        "capture_token": token, "capture_seq": seq, "activated_ns": now(), "accepted": accepted,
    });
    eprintln!("tilo-foreign-capture {record}");
}

#[derive(Debug)]
pub(crate) struct PipeTiming {
    enqueue: Option<u64>,
    render_start: Option<u64>,
}

impl PipeTiming {
    pub(crate) fn start() -> Option<Self> {
        let enabled = super::launcher::LaunchSession::current().presentation_observer_enabled();
        admit(enabled, &WINDOW.pipes).then(|| Self {
            enqueue: now(),
            render_start: None,
        })
    }

    pub(crate) fn render_started(observed: &mut Option<Self>) {
        if let Some(timing) = observed {
            timing.render_start = now();
        }
    }

    pub(crate) fn retire(observed: Option<Self>, serial: u64) {
        if let Some(timing) = observed {
            timing.retired(serial);
        }
    }

    fn retired(self, serial: u64) {
        let record = serde_json::json!({
            "event": "retired", "clock_id": 1, "pipe_serial": serial,
            "enqueue_ns": self.enqueue, "render_start_ns": self.render_start, "render_end_ns": now(),
        });
        eprintln!("tilo-present-pipe {record}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_observation_window_renews_only_after_an_explicit_disabled_to_enabled_transition() {
        let budget = WindowBudget::new();
        budget.captures.store(CAPTURE_LIMIT, Ordering::Relaxed);
        budget.pipes.store(CAPTURE_LIMIT, Ordering::Relaxed);
        budget.exhausted.store(true, Ordering::Relaxed);
        for (previous, next) in [(true, true), (false, false), (true, false)] {
            budget.transition(previous, next);
            assert!(!admit(true, &budget.captures));
            assert!(!admit(true, &budget.pipes));
            assert!(budget.exhausted.load(Ordering::Relaxed));
        }
        budget.transition(false, true);
        assert_eq!(budget.captures.load(Ordering::Relaxed), 0);
        assert_eq!(budget.pipes.load(Ordering::Relaxed), 0);
        assert!(!budget.exhausted.load(Ordering::Relaxed));
        assert!(!admit(false, &budget.captures));
        assert_eq!(budget.captures.load(Ordering::Relaxed), 0);
        assert!(admit(true, &budget.captures));
        assert!(admit(true, &budget.pipes));
        budget.transition(true, true);
        assert_eq!(budget.captures.load(Ordering::Relaxed), 1);
        assert_eq!(budget.pipes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn capture_observation_is_disabled_without_spending_budget_and_bounded_when_enabled() {
        let budget = AtomicUsize::new(0);
        assert!(!admit(false, &budget));
        assert_eq!(budget.load(Ordering::Relaxed), 0);
        for _ in 0..CAPTURE_LIMIT {
            assert!(admit(true, &budget));
        }
        assert!(!admit(true, &budget));
        assert_eq!(budget.load(Ordering::Relaxed), CAPTURE_LIMIT);
    }
}
