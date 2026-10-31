//! A moment as people read how long ago it was: `just now`, `2 min ago`, `3 hr ago`,
//! `Yesterday`, `Sun`, `Jun 30`, or ahead, `in 3 min`. Read in the person's language and time
//! zone; the calendar decides `Yesterday` (the previous local day), not a count of hours.
//!
//! [`RelativeTime`] shows one and says when its text next reads differently
//! ([`Widget::next_wake_deadline`](crate::widget::Widget::next_wake_deadline)): the window
//! sleeps until that instant, and nothing is formatted again before it.

use std::cell::Cell;

use chrono::{Datelike, NaiveDateTime, TimeZone as _};

use crate::component::{Component, Described, Moment};
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::widget::{AppModel, Decorator, ModelPath, Text, Widget};

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// The local wall clock at `secs` since 1970.
fn local(secs: i64) -> Option<NaiveDateTime> {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|at| at.with_timezone(&chrono::Local).naive_local())
}

/// The first local midnight after `secs`.
fn next_midnight(secs: i64) -> Option<i64> {
    let day = local(secs)?.date().succ_opt()?;
    let midnight = day.and_hms_opt(0, 0, 0)?;
    chrono::Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|at| at.timestamp())
}

/// The first local midnight of the year after the one `secs` is in.
fn next_new_year(secs: i64) -> Option<i64> {
    let year = local(secs)?.year().checked_add(1)?;
    let midnight = chrono::NaiveDate::from_ymd_opt(year, 1, 1)?.and_hms_opt(0, 0, 0)?;
    chrono::Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|at| at.timestamp())
}

/// `at` (seconds since 1970) as people read it at `now`.
#[must_use]
pub fn relative_text(at: i64, now: i64) -> String {
    let ago = now.saturating_sub(at);
    if ago < 0 {
        return crate::format::relative_ahead(ago.saturating_neg());
    }
    let (Some(then), Some(today)) = (local(at), local(now)) else {
        return crate::format::relative(ago);
    };
    if ago < HOUR || then.date() == today.date() {
        crate::format::relative(ago)
    } else {
        crate::format::Formats::current().list_date(then, today)
    }
}

/// The next instant after `now` at which [`relative_text`] of `at` may read differently: where
/// the count it shows can move, or the local day turns.
fn candidate(at: i64, now: i64) -> Option<i64> {
    let ago = now.saturating_sub(at);
    if ago < 0 {
        let ahead = ago.saturating_neg();
        let step = if ahead < HOUR {
            MINUTE
        } else if ahead < DAY {
            HOUR
        } else {
            DAY
        };
        // The next instant whose distance ahead is a whole number of steps; at the latest, `at`.
        let steps = ahead.saturating_sub(1) / step;
        return at.checked_sub(steps.checked_mul(step)?);
    }
    let (then, today) = (local(at)?, local(now)?);
    let midnight = next_midnight(now);
    if ago >= HOUR && then.date() != today.date() {
        let days = today.date().signed_duration_since(then.date()).num_days();
        // A week back and further a list shows the date, which reads differently only once a
        // new year has it carry its year.
        return if days < 7 {
            midnight
        } else if then.year() == today.year() {
            next_new_year(now)
        } else {
            None
        };
    }
    let step = if ago < HOUR { MINUTE } else { HOUR };
    let next = at.checked_add((ago / step).checked_add(1)?.checked_mul(step)?)?;
    let thresholds = [45, 90].map(|s| at.saturating_add(s));
    std::iter::once(next)
        .chain(thresholds)
        .chain(midnight)
        .filter(|t| *t > now)
        .min()
}

/// The first instant after `now` at which [`relative_text`] of `at` reads differently from how
/// it reads at `now`, or `None` when it never will.
#[must_use]
pub fn next_change(at: i64, now: i64) -> Option<i64> {
    let shown = relative_text(at, now);
    let mut t = now;
    // Each candidate is at most a step of a minute, an hour or a day on; within one count the
    // text moves within sixty steps.
    for _ in 0..128 {
        t = candidate(at, t)?;
        if relative_text(at, t) != shown {
            return Some(t);
        }
    }
    None
}

/// A moment shown as how long ago it was, or how far ahead: `2 min ago`, `Yesterday`, `in 3
/// min`, in the person's language and time zone. It changes when what it says does, and wakes
/// the window for nothing else.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "Time", role = "Label")]
pub struct TimeProps {
    /// The moment.
    #[component(required, positional)]
    pub at: Moment,
    /// What the text is, which sets its size and weight: `body` unless said.
    pub role: TextRole,
    /// It stands beside the content it is about, in secondary ink: when a change was made.
    pub secondary: bool,
}

/// [`TimeProps`]' widget: a [`Text`] that reads its moment against the wall clock.
pub struct RelativeTime {
    text: Text,
    at: i64,
    role: TextRole,
    secondary: bool,
    /// When what it says next reads differently, in seconds since 1970.
    changes_at: Cell<Option<i64>>,
}

impl RelativeTime {
    /// `at` (seconds since 1970) as it reads now.
    #[must_use]
    pub fn new(at: i64) -> Self {
        let mut time = Self {
            text: Text::new(""),
            at,
            role: TextRole::Body,
            secondary: false,
            changes_at: Cell::new(None),
        };
        time.read_clock(crate::time::unix_now_i64());
        time
    }

    /// What it says now.
    #[must_use]
    pub fn label(&self) -> String {
        self.text.resolved_label()
    }

    /// Read the moment against the wall clock `now`: what it says and when that next changes.
    /// Whether what it says changed.
    fn read_clock(&mut self, now: i64) -> bool {
        let before = self.text.resolved_label();
        let shown = relative_text(self.at, now);
        self.changes_at.set(next_change(self.at, now));
        let changed = before != shown;
        self.text.show_content(shown);
        changed
    }

    /// Read the clock again when what it says is due to change; otherwise nothing.
    fn follow_clock(&mut self) -> bool {
        let now = crate::time::unix_now_i64();
        match self.changes_at.get() {
            Some(due) if due <= now => self.read_clock(now),
            _ => false,
        }
    }
}

impl Decorator for RelativeTime {
    type Inner = Text;

    fn inner(&self) -> &Text {
        &self.text
    }

    fn inner_mut(&mut self) -> &mut Text {
        &mut self.text
    }

    fn name(&self) -> &'static str {
        "RelativeTime"
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let followed = self.follow_clock();
        let inner = Widget::update(&mut self.text, state, path);
        followed || inner
    }

    /// Its deadline is due: read the clock, and say whether what it shows changed.
    fn serve_deadline(&mut self, _now: Instant) -> bool {
        self.follow_clock()
    }

    /// When what it says next changes, on the monotonic clock the window sleeps by.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let due = self.changes_at.get()?;
        let wait_ms = due
            .saturating_mul(1000)
            .saturating_sub(crate::time::unix_now_ms_i64())
            .max(0);
        crate::time::now().checked_add(Duration::from_millis(u64::try_from(wait_ms).ok()?))
    }
}

impl Described for RelativeTime {
    type Props = TimeProps;

    fn from_props(key: &str, props: TimeProps) -> Self {
        let TimeProps {
            at,
            role,
            secondary,
        } = props;
        let mut text = Text::new("").with_role(role).with_id(key);
        if secondary {
            text = text.dimmed();
        }
        let mut time = Self {
            text,
            at: at.0,
            role,
            secondary,
            changes_at: Cell::new(None),
        };
        time.read_clock(crate::time::unix_now_i64());
        time
    }

    /// Another moment reads in place; another role or ink builds it again.
    fn set_props(&mut self, props: TimeProps) -> bool {
        if props.role != self.role || props.secondary != self.secondary {
            return false;
        }
        if props.at.0 != self.at {
            self.at = props.at.0;
            self.read_clock(crate::time::unix_now_i64());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::testing::with_wall;
    use crate::time::{SystemTime, UNIX_EPOCH};

    /// The local wall clock at `hour:minute:second` on `day` of June 2026, as seconds since 1970.
    fn local_secs(day: u32, (hour, minute, second): (u32, u32, u32)) -> i64 {
        chrono::Local
            .with_ymd_and_hms(2026, 6, day, hour, minute, second)
            .earliest()
            .expect("the time exists where the test runs")
            .timestamp()
    }

    fn wall(secs: i64) -> SystemTime {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(
                u64::try_from(secs).expect("after 1970"),
            ))
            .expect("the clock reaches it")
    }

    fn widget_at(at: i64) -> RelativeTime {
        RelativeTime::from_props(
            "t",
            TimeProps {
                at: Moment(at),
                ..TimeProps::default()
            },
        )
    }

    /// Seconds from the wall clock until the widget's deadline, as the window would sleep,
    /// rounded: the monotonic clock moves on a little between reading the deadline and now.
    fn wait(time: &RelativeTime) -> Option<i64> {
        let deadline = Widget::next_wake_deadline(time)?;
        let wait = deadline.saturating_duration_since(crate::time::now());
        i64::try_from(wait.as_millis().saturating_add(500) / 1000).ok()
    }

    #[test]
    fn minutes_ago_change_at_the_next_minute_and_not_before() {
        let at = local_secs(30, (10, 0, 0));
        let mut time = with_wall(wall(at + 150), || widget_at(at));
        assert_eq!(time.label(), "2 min ago");
        // 2 min 30 s ago: it reads `3 min ago` 30 s on, and wakes for nothing sooner.
        let waits = with_wall(wall(at + 150), || wait(&time));
        assert_eq!(waits, Some(30));
        assert_eq!(next_change(at, at + 150), Some(at + 180));
        // Before the deadline, a frame changes nothing.
        let early = with_wall(wall(at + 179), || time.follow_clock());
        assert!(!early, "nothing changes before the minute turns");
        assert_eq!(time.label(), "2 min ago");
        // At it, the text moves on.
        let due = with_wall(wall(at + 180), || time.follow_clock());
        assert!(due);
        assert_eq!(time.label(), "3 min ago");
        assert_eq!(with_wall(wall(at + 180), || wait(&time)), Some(60));
    }

    #[test]
    fn just_now_lasts_until_its_threshold() {
        let at = local_secs(30, (10, 0, 0));
        assert_eq!(relative_text(at, at + 10), "just now");
        assert_eq!(next_change(at, at + 10), Some(at + 45));
        assert_eq!(relative_text(at, at + 45), "1 min ago");
        // `1 min ago` reads the same from 45 s to two minutes.
        assert_eq!(next_change(at, at + 45), Some(at + 120));
    }

    #[test]
    fn yesterday_is_the_previous_local_day_and_turns_at_midnight() {
        // 21:00 yesterday, read at 09:00: twelve hours, but the previous calendar day.
        let at = local_secs(29, (21, 0, 0));
        let now = local_secs(30, (9, 0, 0));
        assert_eq!(relative_text(at, now), "Yesterday");
        // It stays until the next local midnight, then reads as a weekday.
        let midnight = local_secs(30, (23, 59, 59)) + 1;
        assert_eq!(next_change(at, now), Some(midnight));
        assert_ne!(relative_text(at, midnight), "Yesterday");
        // Earlier the same evening it read in hours, and turned to `Yesterday` at midnight.
        let evening = local_secs(29, (23, 30, 0));
        assert_eq!(relative_text(at, evening), "2 hr ago");
        assert_eq!(
            next_change(at, evening),
            Some(local_secs(29, (23, 59, 59)) + 1)
        );
    }

    #[test]
    fn a_moment_ahead_counts_down_and_then_is_past() {
        let at = local_secs(30, (10, 0, 0));
        assert_eq!(relative_text(at, at - 180), "in 3 min");
        assert_eq!(next_change(at, at - 180), Some(at - 120));
        assert_eq!(relative_text(at, at), "just now");
    }

    #[test]
    fn a_date_long_past_never_wakes_the_window_within_its_year() {
        let at = local_secs(1, (10, 0, 0));
        let now = local_secs(30, (10, 0, 0));
        assert_eq!(next_change(at, now), next_new_year(now));
    }
}
