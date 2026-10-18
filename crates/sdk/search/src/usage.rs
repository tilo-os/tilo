//! Frequency and recency of what a person chose, for breaking ties between
//! equally good matches: the app opened daily outranks the one opened once
//! last spring, while a better textual match is never overtaken.

use std::collections::HashMap;

/// How many days halve the weight of a past choice.
const HALF_LIFE_DAYS: f64 = 14.0;
/// Choices, all of them recent, that earn the full boost.
const FULL_BOOST_USES: f64 = 20.0;
/// Most entries kept; the least used are dropped first.
const MAX_ENTRIES: usize = 512;
const SECONDS_PER_DAY: f64 = 86_400.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    count: u32,
    last: u64,
}

/// What was chosen how often and when, by an id the caller picks (a package
/// id, an action id). Time is passed in as seconds since the epoch, so the
/// ranking is testable without a clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frecency {
    entries: HashMap<String, Entry>,
}

impl Frecency {
    /// Note that `id` was chosen at `now` (seconds since the epoch).
    pub fn record(&mut self, id: &str, now: u64) {
        let entry = self.entries.entry(id.to_owned()).or_insert(Entry {
            count: 0,
            last: now,
        });
        entry.count = entry.count.saturating_add(1);
        entry.last = entry.last.max(now);
        if self.entries.len() > MAX_ENTRIES {
            self.prune(now);
        }
    }

    fn prune(&mut self, now: u64) {
        let mut weights: Vec<(String, f64)> = self
            .entries
            .iter()
            .map(|(id, entry)| (id.clone(), weight(*entry, now)))
            .collect();
        weights.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (id, _) in weights.into_iter().skip(MAX_ENTRIES) {
            self.entries.remove(&id);
        }
    }

    /// `0..=max`: how strongly `id` is preferred among equal matches.
    #[must_use]
    pub fn boost(&self, id: &str, now: u64, max: i32) -> i32 {
        let Some(entry) = self.entries.get(id) else {
            return 0;
        };
        let full = (1.0 + FULL_BOOST_USES).ln();
        let share = (weight(*entry, now) / full).clamp(0.0, 1.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a share in 0..=1 of a small integer"
        )]
        let boost = (share * f64::from(max)).ceil() as i32;
        boost.clamp(0, max)
    }

    /// `0.0..=1.0`: how strongly `id` is preferred, at full resolution: the
    /// share of [`Self::boost`]'s scale before it is rounded to steps.
    #[must_use]
    pub fn share(&self, id: &str, now: u64) -> f32 {
        let Some(entry) = self.entries.get(id) else {
            return 0.0;
        };
        let full = (1.0 + FULL_BOOST_USES).ln();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a share in 0..=1 needs no more than f32"
        )]
        let share = (weight(*entry, now) / full).clamp(0.0, 1.0) as f32;
        share
    }

    /// The ids chosen most recently, newest first, at most `limit` of them.
    #[must_use]
    pub fn most_recent(&self, limit: usize) -> Vec<&str> {
        let mut entries: Vec<(&str, u64)> = self
            .entries
            .iter()
            .map(|(id, entry)| (id.as_str(), entry.last))
            .collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        entries.into_iter().take(limit).map(|(id, _)| id).collect()
    }

    /// One `id`, count and last-used line per entry.
    #[must_use]
    pub fn serialize(&self) -> String {
        let mut lines: Vec<String> = self
            .entries
            .iter()
            .map(|(id, entry)| format!("{id}\t{}\t{}", entry.count, entry.last))
            .collect();
        lines.sort();
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }

    /// Read [`Self::serialize`]'s text, skipping anything malformed.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let entries = text
            .lines()
            .filter_map(|line| {
                let mut parts = line.split('\t');
                let id = parts.next().filter(|id| !id.is_empty())?;
                let count = parts.next()?.parse().ok()?;
                let last = parts.next()?.parse().ok()?;
                Some((id.to_owned(), Entry { count, last }))
            })
            .collect();
        Self { entries }
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "counts and ages are far below 2^52"
)]
fn weight(entry: Entry, now: u64) -> f64 {
    let age_days = now.saturating_sub(entry.last) as f64 / SECONDS_PER_DAY;
    (1.0 + f64::from(entry.count)).ln() * 0.5_f64.powf(age_days / HALF_LIFE_DAYS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn frequent_and_recent_choices_earn_a_bigger_boost() {
        let mut usage = Frecency::default();
        let now = 100 * DAY;
        for _ in 0..12 {
            usage.record("daily", now - DAY);
        }
        usage.record("once-recently", now - DAY);
        usage.record("once-long-ago", now - 90 * DAY);
        let daily = usage.boost("daily", now, 8);
        let recent = usage.boost("once-recently", now, 8);
        let old = usage.boost("once-long-ago", now, 8);
        assert!(daily > recent && recent > old, "{daily} {recent} {old}");
        assert_eq!(usage.boost("never", now, 8), 0);
        assert!(daily <= 8);
    }

    #[test]
    fn a_boost_fades_as_a_choice_ages() {
        let mut usage = Frecency::default();
        for _ in 0..30 {
            usage.record("app", 0);
        }
        assert_eq!(usage.boost("app", 0, 8), 8);
        assert!(usage.boost("app", 60 * DAY, 8) < 8);
    }

    #[test]
    fn usage_round_trips_through_text_and_ignores_garbage() {
        let mut usage = Frecency::default();
        usage.record("so.tilo.tube", 5);
        usage.record("so.tilo.tube", 9);
        let mut text = usage.serialize();
        text.push_str("broken line\n\tx\ty\n");
        assert_eq!(Frecency::parse(&text), usage);
    }

    #[test]
    fn shares_grow_with_use_and_recents_come_newest_first() {
        let mut usage = Frecency::default();
        usage.record("a", 10);
        usage.record("b", 30);
        for _ in 0..6 {
            usage.record("c", 20);
        }
        assert!(usage.share("c", 40) > usage.share("a", 40));
        assert!(usage.share("a", 40) > 0.0 && usage.share("c", 40) <= 1.0);
        assert!(usage.share("never", 40).abs() < f32::EPSILON);
        assert_eq!(usage.most_recent(2), ["b", "c"]);
        assert_eq!(usage.most_recent(9), ["b", "c", "a"]);
    }

    #[test]
    fn only_the_most_used_entries_are_kept() {
        let mut usage = Frecency::default();
        for _ in 0..5 {
            usage.record("favourite", 10);
        }
        for at in 0..=MAX_ENTRIES {
            usage.record(&format!("one-off-{at}"), 10);
        }
        assert!(usage.entries.len() <= MAX_ENTRIES);
        assert!(usage.boost("favourite", 10, 8) > 0);
    }
}
