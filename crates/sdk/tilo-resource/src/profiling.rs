//! Bounded observations of named scopes in the real code on this thread.
//!
//! Resource publication, the app framework and apps mark their scopes with
//! [`Phase::start`] or [`measure`]; a test wraps an interaction in
//! [`observe`] and reads every scope that ran, aggregated by name. Scopes may
//! nest, so their durations are not additive. Without an active observation a
//! scope reads no clock, and builds without `test-support` record nothing.

#[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
pub use recorder::{Sample, observe};

#[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
pub use recorder::Phase;

/// A named scope, recorded when it drops inside an active observation.
#[cfg(not(all(feature = "test-support", not(target_arch = "wasm32"))))]
pub struct Phase;

#[cfg(not(all(feature = "test-support", not(target_arch = "wasm32"))))]
impl Phase {
    /// Open the scope `_name`; it closes when the value drops.
    #[inline]
    #[must_use]
    pub const fn start(_name: &'static str) -> Self {
        Self
    }
}

/// Run `operation` as the scope `phase`.
#[inline]
pub fn measure<T>(phase: &'static str, operation: impl FnOnce() -> T) -> T {
    let _scope = Phase::start(phase);
    operation()
}

#[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
mod recorder {
    use std::cell::RefCell;
    use tilo_clock::Instant;

    thread_local! {
        static RECORDS: RefCell<Option<Capture>> = const { RefCell::new(None) };
    }

    /// Distinct scope names one observation keeps.
    pub(super) const MAX_PHASES: usize = 64;

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

    /// A named scope, recorded when it drops inside an active observation.
    #[must_use]
    pub struct Phase(Option<(&'static str, Instant)>);

    impl Phase {
        /// Open the scope `name`; it closes when the value drops.
        pub fn start(name: &'static str) -> Self {
            Self(
                RECORDS
                    .with(|records| records.borrow().as_ref().map(|_| (name, tilo_clock::now()))),
            )
        }
    }

    impl Drop for Phase {
        fn drop(&mut self) {
            let Some((name, start)) = self.0 else {
                return;
            };
            RECORDS.with(|records| {
                let mut records = records.borrow_mut();
                let Some(capture) = records.as_mut() else {
                    return;
                };
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
            });
        }
    }

    struct Recording;

    impl Drop for Recording {
        fn drop(&mut self) {
            RECORDS.with(|records| *records.borrow_mut() = None);
        }
    }

    /// Run the real operation and return every scope it ran, by name.
    ///
    /// # Panics
    /// Panics for a nested observation, a lost recorder, or more distinct
    /// scope names than one observation keeps.
    pub fn observe<T>(operation: impl FnOnce() -> T) -> (T, Vec<Sample>) {
        RECORDS.with(|records| {
            let mut records = records.borrow_mut();
            assert!(records.is_none(), "a phase observation is already active");
            *records = Some(Capture::default());
        });
        let recording = Recording;
        let result = operation();
        let captured = RECORDS.with(RefCell::take);
        assert!(captured.is_some(), "phase observation lost its recorder");
        let capture = captured.unwrap_or_default();
        drop(recording);
        assert!(!capture.overflow, "phase observation exceeded its bound");
        (result, capture.rows)
    }
}

#[cfg(all(test, feature = "test-support", not(target_arch = "wasm32")))]
mod tests {
    use super::recorder::MAX_PHASES;
    use super::{Phase, measure, observe};

    #[test]
    fn recording_is_opt_in_and_aggregates_nested_scopes_by_name() {
        drop(Phase::start("unobserved"));
        let (value, samples) = observe(|| {
            let _outer = Phase::start("outer");
            drop(Phase::start("inner"));
            measure("inner", || 42)
        });
        assert_eq!(value, 42);
        let calls: Vec<_> = samples
            .iter()
            .map(|sample| (sample.phase, sample.calls))
            .collect();
        assert_eq!(calls, [("inner", 2), ("outer", 1)]);
        assert!(observe(|| ()).1.is_empty());
    }

    #[test]
    fn recording_recovers_after_a_nested_observation_panics() {
        let result = std::panic::catch_unwind(|| observe(|| observe(|| ())));
        assert!(result.is_err());
        assert!(observe(|| ()).1.is_empty());
    }

    #[test]
    fn recording_refuses_incomplete_coverage_and_retires_its_recorder() {
        let result = std::panic::catch_unwind(|| {
            observe(|| {
                for index in 0..=MAX_PHASES {
                    let name: &'static str = Box::leak(format!("phase-{index}").into_boxed_str());
                    drop(Phase::start(name));
                }
            })
        });
        assert!(result.is_err());
        assert!(observe(|| ()).1.is_empty());
    }
}
