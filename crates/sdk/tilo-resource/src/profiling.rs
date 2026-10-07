//! Explicit test-support observations of actual resource publication.
//! Inactive scopes do not read the clock; nested times are not additive.

use std::cell::RefCell;
use std::time::Instant;

thread_local! {
    static RECORDS: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

const MAX_PHASES: usize = 32;

#[derive(Default)]
struct Capture {
    rows: Vec<Sample>,
    overflow: bool,
}

/// Repeated scopes aggregate by name, with exact counts and bounded memory.
/// These nested durations must not be added to their enclosing scope.
#[derive(Debug, serde::Serialize)]
pub struct Sample {
    /// Static operation name; no resource identities or document values.
    pub phase: &'static str,
    /// Aggregate duration of this actual scope, nested and nonadditive.
    pub elapsed_ns: u128,
    /// Exact number of completed scopes on the observed thread.
    pub calls: usize,
}

pub(crate) struct Phase(Option<(&'static str, Instant)>);

impl Phase {
    pub(crate) fn start(name: &'static str) -> Self {
        Self(RECORDS.with(|records| records.borrow().as_ref().map(|_| (name, tilo_clock::now()))))
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if let Some((name, start)) = self.0 {
            RECORDS.with(|records| {
                if let Some(capture) = records.borrow_mut().as_mut() {
                    let elapsed_ns = start.elapsed().as_nanos();
                    if let Some(row) = capture.rows.iter_mut().find(|row| row.phase == name) {
                        match (
                            row.elapsed_ns.checked_add(elapsed_ns),
                            row.calls.checked_add(1),
                        ) {
                            (Some(elapsed), Some(calls)) => {
                                row.elapsed_ns = elapsed;
                                row.calls = calls;
                            }
                            _ => capture.overflow = true,
                        }
                    } else if capture.rows.len() < MAX_PHASES {
                        capture.rows.push(Sample {
                            phase: name,
                            elapsed_ns,
                            calls: 1,
                        });
                    } else {
                        capture.overflow = true;
                    }
                }
            });
        }
    }
}

struct Recording;

impl Drop for Recording {
    fn drop(&mut self) {
        RECORDS.with(|records| *records.borrow_mut() = None);
    }
}

/// Run the real operation with bounded aggregate phase observations.
///
/// # Panics
/// Panics for nested observations, missing capture or aggregate overflow.
pub fn observe<T>(operation: impl FnOnce() -> T) -> (T, Vec<Sample>) {
    RECORDS.with(|records| {
        let mut records = records.borrow_mut();
        assert!(records.is_none(), "resource observation already active");
        *records = Some(Capture::default());
    });
    let recording = Recording;
    let result = operation();
    let captured = RECORDS.with(RefCell::take);
    assert!(captured.is_some(), "resource recorder missing");
    let capture = captured.unwrap_or_default();
    drop(recording);
    assert!(
        !capture.overflow,
        "resource phase observation exceeded its bound"
    );
    (result, capture.rows)
}
