//! Slow-event reporting for the native runner.
//!
//! Any event or frame the main thread spends longer than the threshold on
//! (100 ms; `TILO_SLOW_EVENT_MS` overrides it) writes one line to the app's
//! stderr, which the session log keeps, naming what it was and how its time
//! split across the phases the runner marks:
//!
//! `tilo: SLOW-EVENT window_event MouseInput 412 ms: sync=3 layout=391 draw=12 a11y=0 other=6`
//!
//! A frame names what came since the one before it, the events that gave it
//! its work (`after: Resized×3 MouseInput`): a slow frame after a resize
//! reads apart from one after a click that switched a page. A slow event that
//! laid out says what its solves did (`solved: 2 solves, 3 passes, 1019
//! measures`), and one whose text first read a font file names the file
//! (`fonts read: NotoColorEmoji.ttf`): a solve slow for its count of measures
//! reads apart from one slow for what a measure loaded.
//!
//! A person's "every click takes seconds" then names its phase in the next
//! log instead of needing a profiler on their machine. The line ends with the
//! named scopes (`tilo_actions::profiling::Phase`) the event ran, the costliest
//! first, as `scopes: ui.publish_action_surface=812/3` (milliseconds, then how
//! many times): what inside a phase took the time. Outside a slow event the
//! cost is two clock reads per event and per marked phase or scope.

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
    /// The events since the last frame, each with how often it came.
    static SINCE_FRAME: RefCell<Vec<(&'static str, u32)>> = const { RefCell::new(Vec::new()) };
}

/// The name a frame's span carries: the frame takes the events noted since the last one.
const FRAME: &str = "RedrawRequested";

/// The most distinct events a frame names.
const MOST_CAUSES: usize = 8;

/// Note `name` as an event the next frame follows.
pub(crate) fn note_cause(name: &'static str) {
    SINCE_FRAME.with(|since| {
        let mut since = since.borrow_mut();
        if let Some((_, count)) = since.iter_mut().find(|(known, _)| *known == name) {
            *count = count.saturating_add(1);
        } else if since.len() < MOST_CAUSES {
            since.push((name, 1));
        }
    });
}

/// The events since the last frame, as the report names them: ` after: Resized×3 MouseInput`.
fn causes_line(since: &[(&'static str, u32)]) -> String {
    if since.is_empty() {
        return String::new();
    }
    let mut line = " after:".to_owned();
    for (name, count) in since {
        line.push(' ');
        line.push_str(name);
        if *count > 1 {
            line.push_str(&format!("×{count}"));
        }
    }
    line
}

/// One event or frame on the main thread; reports itself on drop when slow.
pub(crate) struct Span {
    kind: &'static str,
    name: &'static str,
    start: Instant,
    /// Whether this span owns the thread's phase list (the outermost does).
    owner: bool,
    /// Whether this span started the thread's observation of named scopes,
    /// so it takes it when it ends.
    scopes: bool,
    /// What the thread's layout solver had done as the span began.
    solved: crate::profiling::layout_counts::LayoutCounts,
    /// How many font files text had read as the span began.
    fonts_read: usize,
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
        if owner && name != FRAME && name != "other" && !name.is_empty() {
            note_cause(name);
        }
        let scopes = owner && tilo_actions::profiling::begin_live();
        Self {
            kind,
            name,
            start: crate::time::now(),
            owner,
            scopes,
            solved: crate::profiling::layout_counts::read(),
            fonts_read: crate::font_catalog::font_files_read_count(),
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if !self.owner {
            return;
        }
        let phases = ACTIVE.with(|active| active.borrow_mut().take());
        let scopes = if self.scopes {
            tilo_actions::profiling::take_live()
        } else {
            Vec::new()
        };
        let causes = if self.name == FRAME {
            SINCE_FRAME.with(|since| std::mem::take(&mut *since.borrow_mut()))
        } else {
            Vec::new()
        };
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
        line.push_str(&causes_line(&causes));
        line.push_str(&solved_line(
            self.solved,
            crate::profiling::layout_counts::read(),
        ));
        let fonts = crate::font_catalog::font_files_read_after(self.fonts_read);
        if !fonts.is_empty() {
            line.push_str(&format!(" fonts read: {}", fonts.join(" ")));
        }
        line.push_str(&scopes_line(scopes));
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

/// What the layout solver did between `before` and `after`, as the report
/// says it: nothing when it solved nothing.
fn solved_line(
    before: crate::profiling::layout_counts::LayoutCounts,
    after: crate::profiling::layout_counts::LayoutCounts,
) -> String {
    let solves = after.solves.saturating_sub(before.solves);
    if solves == 0 {
        return String::new();
    }
    format!(
        " solved: {solves} solves, {} passes, {} probes, {} measures",
        after.passes.saturating_sub(before.passes),
        after.probes.saturating_sub(before.probes),
        after.measures.saturating_sub(before.measures),
    )
}

/// The most named scopes a report lists.
const MOST_SCOPES: usize = 10;

/// The costliest named scopes an event ran, as the report's tail: each
/// `name=ms/calls`, the costliest first. Scopes nest, so they do not add up.
fn scopes_line(mut scopes: Vec<tilo_actions::profiling::Sample>) -> String {
    scopes.retain(|scope| scope.elapsed_ns >= 1_000_000);
    if scopes.is_empty() {
        return String::new();
    }
    scopes.sort_by_key(|scope| std::cmp::Reverse(scope.elapsed_ns));
    let mut line = " scopes:".to_owned();
    for scope in scopes.iter().take(MOST_SCOPES) {
        line.push_str(&format!(
            " {}={}/{}",
            scope.phase,
            scope.elapsed_ns / 1_000_000,
            scope.calls
        ));
    }
    line
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

    tilo_test_env::scenario! {
        fn phases_are_kept_only_while_a_span_owns_them;

        phase a_phase_outside_an_event_costs_nothing_and_records_nothing;
        {
            drop(phase("sync"));
            assert!(ACTIVE.with(|active| active.borrow().is_none()));
        }

        phase nested_spans_keep_one_owner_and_phases_accumulate;
        {
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

        phase a_span_observes_the_named_scopes_it_runs_and_ends_the_observation;
        {
            let span = Span::begin("test", "scoped");
            assert!(span.scopes, "the owner observes the scopes inside it");
            drop(tilo_actions::profiling::Phase::start("ui.draw"));
            drop(span);
            assert!(
                tilo_actions::profiling::begin_live(),
                "the span ended its observation"
            );
            drop(tilo_actions::profiling::take_live());
        }
    }

    #[test]
    fn a_frame_names_the_events_since_the_last_one() {
        drop(Span::begin("window_event", "Resized"));
        drop(Span::begin("window_event", "Resized"));
        drop(Span::begin("window_event", "MouseInput"));
        let since = SINCE_FRAME.with(|since| since.borrow().clone());
        assert_eq!(causes_line(&since), " after: Resized×2 MouseInput");
        drop(Span::begin("window_event", FRAME));
        assert!(
            SINCE_FRAME.with(|since| since.borrow().is_empty()),
            "a frame takes the events it follows"
        );
    }

    #[test]
    fn the_report_says_what_the_solves_did() {
        use crate::profiling::layout_counts::LayoutCounts;
        let before = LayoutCounts {
            solves: 4,
            measures: 100,
            ..LayoutCounts::default()
        };
        assert_eq!(solved_line(before, before), "", "no solve, nothing said");
        let after = LayoutCounts {
            solves: 5,
            passes: 2,
            measures: 1119,
            ..before
        };
        assert_eq!(
            solved_line(before, after),
            " solved: 1 solves, 2 passes, 0 probes, 1019 measures"
        );
    }

    #[test]
    fn the_report_names_the_costliest_scopes_first() {
        let scope = |phase, ms: u128, calls| tilo_actions::profiling::Sample {
            phase,
            elapsed_ns: ms * 1_000_000,
            calls,
        };
        assert_eq!(
            scopes_line(vec![
                scope("ui.draw", 40, 1),
                scope("cheap", 0, 9),
                scope("ui.publish_action_surface", 812, 3),
            ]),
            " scopes: ui.publish_action_surface=812/3 ui.draw=40/1"
        );
        assert_eq!(scopes_line(vec![scope("cheap", 0, 1)]), "");
    }
}
