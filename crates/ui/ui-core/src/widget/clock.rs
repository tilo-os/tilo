//! A wall clock read live: the time of day where a zone says it is, with the day there and how
//! far its clock stands from the person's.
//!
//! A view cannot read the clock itself — what it shows is drawn again from what it was given — so
//! a clock a person watches (`9:41` in Tokyo, moving on) is the toolkit's: the widget reads the
//! device's clock, redraws itself when its minute turns, and reads every zone the way the person's
//! own is read, daylight saving and half-hour offsets included.

use std::cell::Cell;

use jiff::civil::Date;

use crate::component::{Component, Described};
use crate::i18n::{ArgValue, widget_format};
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::widget::{AppModel, Column, Decorator, ModelPath, Text, Widget};

/// Seconds in a minute, as a reading's minute is counted.
const MINUTE: i64 = 60;

/// The zone `name` names, or the person's own when it names none or one this device does not
/// know.
fn zone(name: Option<&str>) -> Option<crate::clock::Zone> {
    match name.filter(|name| !name.is_empty()) {
        Some(name) => crate::clock::Zone::named(name),
        None => Some(crate::clock::Zone::here()),
    }
}

/// The wall clock `at` names in `zone`.
fn local_in(zone: &crate::clock::Zone, at: i64) -> Option<jiff::Zoned> {
    jiff::Timestamp::from_second(at).ok().map(|at| zone.at(at))
}

/// The instant the minute turns after `now`: when a clock reading `9:41` becomes `9:42`.
fn next_minute(now: i64) -> i64 {
    (now.div_euclid(MINUTE) + 1) * MINUTE
}

/// What a clock reads: the time of day, the day there and how far its clock stands from here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockReading {
    /// The time of day, as the person's clock style writes it (`9:41`, `9:41 AM`).
    pub time: String,
    /// The day there, as a person reads a day near today (`Today`, `Tomorrow`, `Tue, 30 Jun`).
    pub day: String,
    /// How far its clock is from here (`2 hours ahead`, `Same time as here`).
    pub offset: String,
}

/// What the clock in `zone_name` reads at `now`: `None` for a zone this device does not know.
#[must_use]
pub fn reading(zone_name: Option<&str>, now: i64) -> Option<ClockReading> {
    let here = crate::clock::Zone::here();
    let there = zone(zone_name)?;
    let today = local_in(&here, now)?;
    let moment = local_in(&there, now)?;
    Some(ClockReading {
        time: crate::clock::time_of_day(moment.time()),
        day: day_text(moment.date(), today.date()),
        offset: offset_text(&moment, &today),
    })
}

/// The day `there` names, read against the person's `today`.
fn day_text(there: Date, today: Date) -> String {
    if there == today {
        return widget_format("clock-today", &[]);
    }
    if today.tomorrow().ok() == Some(there) {
        return widget_format("clock-tomorrow", &[]);
    }
    crate::format::Formats::current().day_heading_short(there)
}

/// How far the clock of `there` stands from `here`'s, in whole hours and minutes.
fn offset_text(there: &jiff::Zoned, here: &jiff::Zoned) -> String {
    let difference = there.offset().seconds() - here.offset().seconds();
    if difference == 0 {
        return widget_format("clock-same-time", &[]);
    }
    let ahead = difference > 0;
    let secs = i64::from(difference.abs());
    let (hours, minutes) = (secs / 3600, (secs % 3600) / 60);
    let side = if ahead { "ahead" } else { "behind" };
    if hours > 0 && minutes > 0 {
        return widget_format(
            &format!("clock-hours-minutes-{side}"),
            &[
                ("hours", ArgValue::Text(hours.to_string())),
                ("minutes", ArgValue::Text(minutes.to_string())),
            ],
        );
    }
    let (unit, count) = if hours > 0 {
        ("hours", hours)
    } else {
        ("minutes", minutes)
    };
    widget_format(
        &format!("clock-{unit}-{side}"),
        &[("count", ArgValue::Text(count.to_string()))],
    )
}

/// A wall clock, live: the time of day in `zone` (the person's when it names none), the day
/// there, and how far its clock stands from here.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "Clock", role = "Label", label = "zone")]
pub struct ClockProps {
    /// The IANA zone (`Asia/Tokyo`) to read the clock in; the person's own when it names none.
    pub zone: Option<String>,
    /// Show the day there and how far its clock stands from here, under the time.
    pub facts: bool,
    /// What the time is, which sets its size and weight: `body` unless said.
    pub role: TextRole,
    /// In secondary ink, beside the content it is about.
    pub secondary: bool,
}

/// [`ClockProps`]' widget: the texts a clock reads, each redrawn as its minute turns.
pub struct WallClock {
    zone: Option<String>,
    column: Column,
    role: TextRole,
    secondary: bool,
    facts: bool,
    /// When what it says changes next, in seconds since 1970.
    changes_at: Cell<Option<i64>>,
}

impl WallClock {
    fn new(zone: Option<String>, role: TextRole, facts: bool, secondary: bool, key: &str) -> Self {
        let mut time = Text::new("").with_role(role).with_id(key);
        if secondary {
            time = time.dimmed();
        }
        let mut children: Vec<Box<dyn Widget>> = vec![Box::new(time)];
        if facts {
            children.push(Box::new(Text::new("").dimmed()));
        }
        let mut column = Column::new().with_no_padding().with_intrinsic_height();
        column.set_children(children);
        let mut clock = Self {
            zone,
            column,
            role,
            secondary,
            facts,
            changes_at: Cell::new(None),
        };
        clock.read(crate::time::unix_now_i64());
        clock
    }

    /// Read the clock at `now`: what the texts say, and when they next change.
    fn read(&mut self, now: i64) -> bool {
        let reading = reading(self.zone.as_deref(), now);
        self.changes_at
            .set(reading.as_ref().map(|_| next_minute(now)));
        let (time, facts) = match &reading {
            Some(reading) => (
                reading.time.clone(),
                format!("{} · {}", reading.day, reading.offset),
            ),
            None => (String::new(), String::new()),
        };
        let mut said = [time, facts].into_iter();
        let mut changed = false;
        let mut children = self.column.take_children();
        for child in &mut children {
            let Some(text) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Text>())
            else {
                continue;
            };
            let want = said.next().unwrap_or_default();
            let before = text.resolved_label();
            text.show_content(want);
            changed |= text.resolved_label() != before;
        }
        self.column.set_children(children);
        changed
    }

    fn follow_clock(&mut self) -> bool {
        let now = crate::time::unix_now_i64();
        match self.changes_at.get() {
            Some(due) if due > now => false,
            _ => self.read(now),
        }
    }
}

impl Decorator for WallClock {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.column
    }

    fn name(&self) -> &'static str {
        "WallClock"
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let followed = self.follow_clock();
        let inner = Widget::update(&mut self.column, state, path);
        followed || inner
    }

    fn serve_deadline(&mut self, _now: Instant) -> bool {
        self.follow_clock()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let due = self.changes_at.get()?;
        let wait_ms = due
            .saturating_mul(1000)
            .saturating_sub(crate::time::unix_now_ms_i64())
            .max(0);
        crate::time::now().checked_add(Duration::from_millis(u64::try_from(wait_ms).ok()?))
    }
}

impl Described for WallClock {
    type Props = ClockProps;

    fn from_props(key: &str, props: ClockProps) -> Self {
        Self::new(props.zone, props.role, props.facts, props.secondary, key)
    }

    /// Another zone's clock is another clock, never this one read differently: a change of the
    /// zone, the facts or the ink builds it again.
    fn set_props(&mut self, props: ClockProps) -> bool {
        props.zone == self.zone
            && props.facts == self.facts
            && props.role == self.role
            && props.secondary == self.secondary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-06-30T09:00:30Z.
    const NOW: i64 = 1_782_810_030;

    #[test]
    fn a_clock_reads_its_minute_and_wakes_when_it_turns() {
        let reading = reading(Some("Asia/Tokyo"), NOW).expect("Tokyo is a zone");
        assert_eq!(next_minute(NOW), 1_782_810_060);
        assert!(!reading.time.is_empty());
        assert!(!reading.offset.is_empty());
    }

    #[test]
    fn the_persons_own_zone_is_the_same_time_as_here() {
        let reading = reading(None, NOW).expect("here is a zone");
        assert_eq!(reading.offset, widget_format("clock-same-time", &[]));
    }

    #[test]
    fn a_zone_this_device_does_not_know_reads_no_clock() {
        assert!(reading(Some("Nowhere/Middle"), NOW).is_none());
    }

    #[test]
    fn the_day_there_is_said_against_today() {
        let today = jiff::civil::date(2026, 6, 30);
        assert_eq!(day_text(today, today), widget_format("clock-today", &[]));
        assert_eq!(
            day_text(jiff::civil::date(2026, 7, 1), today),
            widget_format("clock-tomorrow", &[])
        );
    }
}
