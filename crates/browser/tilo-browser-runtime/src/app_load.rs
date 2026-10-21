//! Fetches of applications' code that the bundle defers, each bounded in
//! time: a window waiting for its application says that the code could not
//! be loaded once the bound passes, instead of saying "Opening…" forever.

use std::collections::HashMap;

/// How long a window waits for its application's code. A slow connection
/// fetches a few megabytes well within it; a fetch still pending after it is
/// stalled, and opening the window again starts a fresh one.
pub(crate) const LOAD_BOUND_MS: u32 = 45_000;

/// The fetches in flight, each with the attempt that started it.
#[derive(Debug, Default)]
pub(crate) struct Loads {
    in_flight: HashMap<&'static str, u64>,
    attempts: u64,
}

impl Loads {
    /// Start fetching `app`'s code: the attempt, or `None` while a fetch of
    /// it is already in flight (its windows wait for that one).
    pub(crate) fn start(&mut self, app: &'static str) -> Option<u64> {
        if self.in_flight.contains_key(app) {
            return None;
        }
        self.attempts = self.attempts.wrapping_add(1);
        self.in_flight.insert(app, self.attempts);
        Some(self.attempts)
    }

    /// The fetch `attempt` of `app` ended, or its bound passed: `true` for
    /// whichever comes first, which decides what the waiting windows show.
    /// The other one finds the attempt settled and changes nothing.
    pub(crate) fn settle(&mut self, app: &str, attempt: u64) -> bool {
        if self.in_flight.get(app) == Some(&attempt) {
            self.in_flight.remove(app);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stalled_fetch_settles_at_its_bound_and_a_reopen_fetches_again() {
        let mut loads = Loads::default();
        let first = loads.start("so.tilo.themes").unwrap();
        assert_eq!(
            loads.start("so.tilo.themes"),
            None,
            "a second window waits for the fetch in flight"
        );
        assert!(
            loads.settle("so.tilo.themes", first),
            "the bound passing first says the code could not be loaded"
        );
        let second = loads.start("so.tilo.themes").unwrap();
        assert_ne!(first, second, "opening again starts a fresh fetch");
        assert!(
            !loads.settle("so.tilo.themes", first),
            "the stalled fetch ending late decides nothing for the new one"
        );
        assert!(loads.settle("so.tilo.themes", second));
    }

    #[test]
    fn a_fetch_that_ends_in_time_is_not_reported_by_its_bound() {
        let mut loads = Loads::default();
        let attempt = loads.start("so.tilo.notes").unwrap();
        assert!(loads.settle("so.tilo.notes", attempt), "the fetch ended");
        assert!(
            !loads.settle("so.tilo.notes", attempt),
            "its bound passing later changes nothing"
        );
    }
}
