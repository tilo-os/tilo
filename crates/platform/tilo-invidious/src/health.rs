//! Which public instances have been answering lately.
//!
//! Public instances come and go: at any time most of the list is down, slow,
//! or answering an anti-bot page. Asking them in a fixed order makes every
//! request wait out the same dead ones first. Instead each answer is
//! recorded, and [`ranked`] puts instances that answered first, fastest
//! first, then the untried, then those that failed, the most recently failed
//! last. A failed instance rests for a while (longer each time it fails in a
//! row) and is then tried again ahead of the ones still resting, so one that
//! comes back is found again. The record lives as long as the process.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use tilo_clock::{Duration, Instant};

/// How long an instance rests after its first failure in a row; each further
/// failure doubles it, up to [`LONGEST_REST`].
const FIRST_REST: Duration = Duration::from_secs(30);
const LONGEST_REST: Duration = Duration::from_secs(15 * 60);

static RECORD: Mutex<Option<Record>> = Mutex::new(None);

/// [`crate::instances`], best first by how each has answered in this
/// process.
pub fn ranked() -> Vec<String> {
    let instances = crate::instances();
    with_record(|record| record.rank(instances, tilo_clock::now()))
}

/// `instance` answered, taking `took`.
pub fn answered(instance: &str, took: Duration) {
    with_record(|record| record.answered(instance, took));
}

/// `instance` failed: unreachable, too slow, or not answering what was asked.
pub fn failed(instance: &str) {
    with_record(|record| record.failed(instance, tilo_clock::now()));
}

fn with_record<T>(use_record: impl FnOnce(&mut Record) -> T) -> T {
    let mut record = RECORD.lock().unwrap_or_else(PoisonError::into_inner);
    use_record(record.get_or_insert_with(Record::default))
}

/// How one instance has been answering.
#[derive(Clone, Copy, Debug)]
struct Standing {
    /// Failures since its last answer.
    failures: u32,
    failed_at: Option<Instant>,
    /// How long its last answer took.
    took: Option<Duration>,
}

/// Every instance's standing.
#[derive(Debug, Default)]
pub(crate) struct Record {
    standings: HashMap<String, Standing>,
}

impl Record {
    pub(crate) fn answered(&mut self, instance: &str, took: Duration) {
        self.standings.insert(
            instance.to_string(),
            Standing {
                failures: 0,
                failed_at: None,
                took: Some(took),
            },
        );
    }

    pub(crate) fn failed(&mut self, instance: &str, now: Instant) {
        let standing = self
            .standings
            .entry(instance.to_string())
            .or_insert(Standing {
                failures: 0,
                failed_at: None,
                took: None,
            });
        standing.failures = standing.failures.saturating_add(1);
        standing.failed_at = Some(now);
    }

    /// `instances`, best first; the list's own order breaks ties.
    pub(crate) fn rank(&self, instances: Vec<String>, now: Instant) -> Vec<String> {
        let mut keyed: Vec<((u8, u128), usize, String)> = instances
            .into_iter()
            .enumerate()
            .map(|(order, instance)| (self.key(&instance, now), order, instance))
            .collect();
        keyed.sort_by_key(|(key, order, _)| (*key, *order));
        keyed.into_iter().map(|(.., instance)| instance).collect()
    }

    /// Sort key: answering (by speed), untried, rested failures (by how
    /// long ago), resting failures (by how long ago).
    fn key(&self, instance: &str, now: Instant) -> (u8, u128) {
        let Some(standing) = self.standings.get(instance) else {
            return (1, 0);
        };
        match (standing.failures, standing.failed_at) {
            (0, _) | (_, None) => (0, standing.took.map_or(0, |took| took.as_millis())),
            (failures, Some(at)) => {
                let since = now.saturating_duration_since(at);
                let rest = FIRST_REST
                    .saturating_mul(1 << failures.saturating_sub(1).min(16))
                    .min(LONGEST_REST);
                let resting = u8::from(since < rest);
                // Longer ago sorts first.
                (
                    resting.saturating_add(2),
                    u128::MAX.saturating_sub(since.as_millis()),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<String> {
        ["https://a", "https://b", "https://c", "https://d"]
            .map(str::to_string)
            .to_vec()
    }

    #[test]
    fn instances_that_answered_come_first_fastest_first_and_failures_last() {
        let start = tilo_clock::now();
        let mut record = Record::default();
        record.failed("https://a", start);
        record.answered("https://c", Duration::from_millis(900));
        record.answered("https://d", Duration::from_millis(200));
        assert_eq!(
            record.rank(list(), start),
            ["https://d", "https://c", "https://b", "https://a"]
        );
    }

    #[test]
    fn a_failed_instance_rests_longer_each_time_it_fails_in_a_row() {
        let start = tilo_clock::now();
        let mut record = Record::default();
        record.failed("https://b", start);
        record.failed("https://a", start + Duration::from_secs(1));
        // Both resting: the one that failed longer ago goes first.
        let soon = start + Duration::from_secs(5);
        assert_eq!(record.rank(list(), soon)[2..], ["https://b", "https://a"]);
        // b fails again at the same moment a fails once more.
        let again = start + Duration::from_secs(20);
        record.failed("https://b", again);
        record.failed("https://c", again);
        // 50 s later c (one failure: 30 s rest) is tried again, while b (two
        // in a row: a minute) still rests behind a, which failed earlier.
        let later = again + Duration::from_secs(50);
        assert_eq!(
            record.rank(list(), later),
            ["https://d", "https://a", "https://c", "https://b"]
        );
        record.answered("https://b", Duration::from_millis(300));
        assert_eq!(record.rank(list(), later)[0], "https://b");
    }
}
