//! Bounded observations around the real framework functions on this UI thread.
//! Scopes may nest; their durations are not additive. No recorder runs by default.

use std::cell::RefCell;
use ui_core::time::Instant;

const MAX_SAMPLES: usize = 1024;

/// One actual function scope, without resource identities or document values.
#[derive(Debug, serde::Serialize)]
pub struct Sample {
    pub phase: &'static str,
    pub elapsed_ns: u128,
}

#[derive(Default)]
struct Capture {
    samples: Vec<Sample>,
    overflow: bool,
}

thread_local! {
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

struct Recording;

impl Drop for Recording {
    fn drop(&mut self) {
        CAPTURE.with(|capture| *capture.borrow_mut() = None);
    }
}

/// Observe only `operation` on the calling UI thread. Nested observations or
/// more than 1024 scopes fail rather than silently return incomplete timings.
/// Captured functions execute normally; this neither pumps nor forces a frame.
///
/// # Panics
/// Panics for a nested observation, a lost recorder, or more than 1024 scopes.
pub fn observe<T>(operation: impl FnOnce() -> T) -> (T, Vec<Sample>) {
    CAPTURE.with(|capture| {
        let mut capture = capture.borrow_mut();
        assert!(
            capture.is_none(),
            "a framework phase observation is already active"
        );
        *capture = Some(Capture::default());
    });
    let recording = Recording;
    let result = operation();
    let captured = CAPTURE.with(RefCell::take);
    assert!(
        captured.is_some(),
        "framework observation lost its recorder"
    );
    let capture = captured.unwrap_or_default();
    drop(recording);
    assert!(
        !capture.overflow,
        "framework phase observation exceeded its bound"
    );
    (result, capture.samples)
}

/// Record the real operation within an active observation on this thread.
/// Without an observation this executes the operation without reading time.
pub fn measure<T>(phase: &'static str, operation: impl FnOnce() -> T) -> T {
    let _scope = Scope::start(phase);
    operation()
}

pub(crate) struct Scope(Option<(&'static str, Instant)>);

impl Scope {
    pub(crate) fn start(phase: &'static str) -> Self {
        Self(CAPTURE.with(|capture| {
            capture
                .borrow()
                .as_ref()
                .map(|_| (phase, ui_core::time::now()))
        }))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let Some((phase, start)) = self.0 else {
            return;
        };
        CAPTURE.with(|capture| {
            let mut capture = capture.borrow_mut();
            if let Some(capture) = capture.as_mut() {
                if capture.samples.len() == MAX_SAMPLES {
                    capture.overflow = true;
                } else {
                    capture.samples.push(Sample {
                        phase,
                        elapsed_ns: start.elapsed().as_nanos(),
                    });
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_SAMPLES, Scope, observe};

    #[test]
    fn phase_recording_is_opt_in_and_preserves_nested_scopes_and_return_value() {
        drop(Scope::start("unobserved"));
        let (value, samples) = observe(|| {
            let _outer = Scope::start("outer");
            drop(Scope::start("inner"));
            42
        });
        assert_eq!(value, 42);
        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.phase)
                .collect::<Vec<_>>(),
            ["inner", "outer"]
        );
        assert!(observe(|| ()).1.is_empty());
    }

    #[test]
    fn phase_recording_recovers_after_a_nested_observation_panics() {
        let result = std::panic::catch_unwind(|| observe(|| observe(|| ())));
        assert!(result.is_err());
        assert!(observe(|| ()).1.is_empty());
    }

    #[test]
    fn phase_recording_refuses_incomplete_coverage_and_retires_its_recorder() {
        let result = std::panic::catch_unwind(|| {
            observe(|| {
                for _ in 0..=MAX_SAMPLES {
                    drop(Scope::start("bounded"));
                }
            })
        });
        assert!(result.is_err());
        assert!(observe(|| ()).1.is_empty());
    }
}
