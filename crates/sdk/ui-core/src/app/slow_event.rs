//! Slow-event reporting for the native runner.
//!
//! Any event or frame the main thread spends longer than the threshold on
//! (100 ms; `TILO_SLOW_EVENT_MS` overrides it) writes one line to the app's
//! stderr, which the session log keeps, naming what it was and how its time
//! split across the phases the runner marks:
//!
//! `tilo: SLOW-EVENT window_event MouseInput 412 ms: sync=3 layout=391 draw=12 a11y=0 other=6`
//!
//! A person's "every click takes seconds" then names its phase in the next
//! log instead of needing a profiler on their machine. Outside a slow event
//! the cost is two clock reads per event and one per marked phase.

use crate::time::{Duration, Instant};
use std::cell::RefCell;
use std::sync::OnceLock;

const DEFAULT_THRESHOLD_MS: u64 = 100;

fn threshold() -> Duration {
    static THRESHOLD: OnceLock<Duration> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        Duration::from_millis(
            super::native_runner::slow_event_threshold_ms().unwrap_or(DEFAULT_THRESHOLD_MS),
        )
    })
}

#[derive(Default)]
struct Phases(Vec<(&'static str, Duration)>);

thread_local! {
    /// The phases of the event being handled, when one is.
    static ACTIVE: RefCell<Option<Phases>> = const { RefCell::new(None) };
}

/// One event or frame on the main thread; reports itself on drop when slow.
pub(crate) struct Span {
    kind: &'static str,
    name: &'static str,
    start: Instant,
    /// Whether this span owns the thread's phase list (the outermost does).
    owner: bool,
}

impl Span {
    pub(crate) fn begin(kind: &'static str, name: &'static str) -> Self {
        let owner = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active.is_some() {
                false
            } else {
                *active = Some(Phases::default());
                true
            }
        });
        Self {
            kind,
            name,
            start: crate::time::now(),
            owner,
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if !self.owner {
            return;
        }
        let phases = ACTIVE.with(|active| active.borrow_mut().take());
        let total = self.start.elapsed();
        if total < threshold() {
            return;
        }
        let phases = phases.unwrap_or_default().0;
        let mut merged: Vec<(&'static str, Duration)> = Vec::new();
        for (name, took) in phases {
            match merged.iter_mut().find(|(known, _)| *known == name) {
                Some((_, sum)) => *sum += took,
                None => merged.push((name, took)),
            }
        }
        let named: Duration = merged.iter().map(|(_, took)| *took).sum();
        let mut line = String::new();
        for (name, took) in &merged {
            line.push_str(&format!(" {name}={}", took.as_millis()));
        }
        line.push_str(&format!(
            " other={}",
            total.saturating_sub(named).as_millis()
        ));
        crate::diag!(
            "tilo: SLOW-EVENT {} {} {} ms:{line}",
            self.kind,
            self.name,
            total.as_millis()
        );
    }
}

/// Time one phase of the event being handled; adds to the phase's total when
/// it is entered more than once.
pub(crate) struct Phase {
    name: &'static str,
    start: Instant,
}

pub(crate) fn phase(name: &'static str) -> Phase {
    Phase {
        name,
        start: crate::time::now(),
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        let took = self.start.elapsed();
        ACTIVE.with(|active| {
            if let Some(phases) = active.borrow_mut().as_mut() {
                phases.0.push((self.name, took));
            }
        });
    }
}

/// A short name for an event, for the report.
pub(crate) fn window_event_name(event: &winit::event::WindowEvent) -> &'static str {
    use winit::event::WindowEvent as E;
    match event {
        E::MouseInput { .. } => "MouseInput",
        E::CursorMoved { .. } => "CursorMoved",
        E::MouseWheel { .. } => "MouseWheel",
        E::KeyboardInput { .. } => "KeyboardInput",
        E::Ime(_) => "Ime",
        E::Touch(_) => "Touch",
        E::Resized(_) => "Resized",
        E::RedrawRequested => "RedrawRequested",
        E::ScaleFactorChanged { .. } => "ScaleFactorChanged",
        E::Focused(_) => "Focused",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_spans_keep_one_owner_and_phases_accumulate() {
        let outer = Span::begin("test", "outer");
        assert!(outer.owner);
        let inner = Span::begin("test", "inner");
        assert!(!inner.owner, "the outermost span owns the phases");
        drop(phase("a"));
        drop(phase("a"));
        drop(inner);
        let seen = ACTIVE.with(|active| active.borrow().as_ref().map(|p| p.0.len()));
        assert_eq!(seen, Some(2), "phases are kept until the owner ends");
        drop(outer);
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
    }

    #[test]
    fn a_phase_outside_an_event_costs_nothing_and_records_nothing() {
        drop(phase("sync"));
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
    }
}
