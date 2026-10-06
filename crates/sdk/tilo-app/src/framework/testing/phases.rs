//! Bounded observations around the real framework functions on this UI thread.
//! Scopes may nest; their durations are not additive. No recorder runs by default.

use std::cell::RefCell;
use std::time::Instant;

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
