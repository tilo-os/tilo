//! RFC 5545 recurrence rules, shared by every iCalendar consumer.
//!
//! A calendar event, a repeating task and a snooze control all ask the same
//! questions of a stored rule: which dates does it produce, which is the next
//! one, and what instant does an authored wall time mean in the series' own
//! zone. This crate answers them from the text RFC 5545 already stores, so
//! Calendar, the language runtime and any later app share one expansion
//! instead of each growing its own.
//!
//! Two types share the work:
//!
//! * [`Rule`] is one parsed rule — `FREQ`, `INTERVAL`, `BYDAY`,
//!   `BYMONTHDAY`, `UNTIL`, `COUNT` and `EXDATE` — and expands it from a seed
//!   date (RFC 5545's `DTSTART`).
//! * [`Repeat`] is the small preset set a picker cycles through ("Every
//!   week", …) and the canonical rule text for each.
//!
//! Expansion follows RFC 5545: each period of the rule's frequency keeps the
//! days its `BYDAY` and `BYMONTHDAY` allow, and `COUNT` counts them from
//! `DTSTART`. [`Rule`] moves a rule without day filters forward by whole
//! periods and starts there with what is left of its `COUNT`: a monthly view
//! of a series seeded in 1996 costs its window, not the thirty years before
//! it.
//!
//! Occurrences keep their authored wall time: [`zone`] resolves a wall time to
//! an instant with one cross-device policy — a fold takes the earlier instant,
//! a gap advances to the first valid minute — so a 09:00 series stays at
//! 09:00 across a daylight-saving change and still lands on the right minute
//! on every device.
//!
//! Stored spellings are format: [`Rule::to_rrule`] and [`Repeat::to_rrule`]
//! write the same text the documents already hold, so adopting this crate
//! never rewrites a saved rule.

pub mod zone;

use std::fmt::Write as _;
use std::ops::ControlFlow;

use jiff::Timestamp;
use jiff::civil::{Date, DateTime, Weekday};
use jiff::tz::TimeZone;

pub use zone::{Resolution, canonical_zone, normalize_wall, wall_instant, wall_to_utc};

/// A rule this crate accepts produces a date within any 400-year span (the
/// Gregorian leap cycle), so a search that walks further has run past the end
/// of the series. It only bounds a search for the next date; window expansion
/// is bounded by the window the caller asked for.
const SEARCH_DAYS: i64 = 146_100;

/// How many counted periods a skip-ahead may pass over looking for a month or
/// a year that holds the seed's day: 29 February skips three years, the 31st
/// at most one month.
const SKIPPED_PERIODS: usize = 32;

#[cfg(test)]
thread_local! {
    /// The dates [`Rule`] has expanded on this thread, so window
    /// expansion's cost can be pinned to the window it was asked for.
    static DATES_TAKEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One `FREQ` value this crate expands. RFC 5545's sub-daily frequencies are
/// refused rather than approximated: a calendar occurrence is a date.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    /// The RFC 5545 spelling, without the `FREQ=` name.
    #[must_use]
    pub fn as_rrule(self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
            Self::Yearly => "YEARLY",
        }
    }

    /// Parse one `FREQ` value, case-insensitively. `None` for a frequency this
    /// crate cannot expand.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "DAILY" => Some(Self::Daily),
            "WEEKLY" => Some(Self::Weekly),
            "MONTHLY" => Some(Self::Monthly),
            "YEARLY" => Some(Self::Yearly),
            _ => None,
        }
    }
}

/// One parsed recurrence rule.
///
/// The seed date is not part of the rule text — RFC 5545 keeps it in
/// `DTSTART` — so every expansion takes it as an argument. That is also what
/// lets one parsed rule serve a series whose seed later moves.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rule {
    freq: Freq,
    interval: u32,
    by_day: Vec<Weekday>,
    by_month_day: Vec<i8>,
    until: Option<Date>,
    count: Option<u32>,
    exdates: Vec<Date>,
}

impl Rule {
    /// A rule that fires every `freq` from the seed, with no limits.
    #[must_use]
    pub fn new(freq: Freq) -> Self {
        Self {
            freq,
            interval: 1,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            until: None,
            count: None,
            exdates: Vec::new(),
        }
    }

    /// Parse the text [`Rule::to_text`] writes: the `RRULE` body, optionally
    /// with the `RRULE:` prefix, plus an `EXDATE=` list when the set excludes
    /// dates.
    ///
    /// Returns `None` when a part is present that this crate cannot expand
    /// exactly (`SECONDLY`, `BYSETPOS`, an ordinal `BYDAY` such as `-1FR`, …)
    /// or that RFC 5545 does not allow (`BYMONTHDAY` in a weekly rule).
    /// Refusing is deliberate: silently dropping a limit the author wrote
    /// would show occurrences the rule does not have.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let body = strip_rrule_prefix(text.trim());
        if body.is_empty() {
            return None;
        }
        let mut freq = None;
        let mut interval = 1;
        let mut by_day = Vec::new();
        let mut by_month_day = Vec::new();
        let mut until = None;
        let mut count = None;
        let mut exdates = Vec::new();
        for part in body.split(';').filter(|part| !part.trim().is_empty()) {
            let (name, value) = part.split_once('=')?;
            match name.trim().to_ascii_uppercase().as_str() {
                "FREQ" => freq = Some(Freq::parse(value)?),
                "INTERVAL" => interval = parse_positive(value)?,
                "BYDAY" => {
                    by_day = value
                        .split(',')
                        .map(|code| parse_weekday(code.trim()))
                        .collect::<Option<Vec<_>>>()?;
                }
                "BYMONTHDAY" => {
                    by_month_day = value
                        .split(',')
                        .map(|day| parse_month_day(day.trim()))
                        .collect::<Option<Vec<_>>>()?;
                }
                "UNTIL" => until = Some(parse_cal_date(value)?),
                "COUNT" => count = Some(parse_positive(value)?),
                "EXDATE" => {
                    exdates = value
                        .split(',')
                        .map(|date| parse_cal_date(date.trim()))
                        .collect::<Option<Vec<_>>>()?;
                }
                _ => return None,
            }
        }
        let freq = freq?;
        // RFC 5545 gives `BYMONTHDAY` no meaning in a weekly rule.
        if freq == Freq::Weekly && !by_month_day.is_empty() {
            return None;
        }
        Some(
            Self::new(freq)
                .with_interval(interval)
                .with_by_day(by_day)
                .with_month_days(by_month_day)
                .with_until(until)
                .with_count(count)
                .with_exdates(exdates),
        )
    }

    /// The `RRULE` body (no `RRULE:` prefix): `FREQ`, `INTERVAL`, `BYDAY`,
    /// `BYMONTHDAY`, `UNTIL`, `COUNT`, in that order.
    ///
    /// `EXDATE` is a content line of its own in RFC 5545, so this writes the
    /// rule alone; [`Rule::to_text`] writes the excluded dates with it.
    #[must_use]
    pub fn to_rrule(&self) -> String {
        let mut out = format!("FREQ={}", self.freq.as_rrule());
        if self.interval > 1 {
            let _ = write!(out, ";INTERVAL={}", self.interval);
        }
        if !self.by_day.is_empty() {
            let codes: Vec<&str> = self.by_day.iter().map(|day| weekday_code(*day)).collect();
            let _ = write!(out, ";BYDAY={}", codes.join(","));
        }
        if !self.by_month_day.is_empty() {
            let days: Vec<String> = self.by_month_day.iter().map(i8::to_string).collect();
            let _ = write!(out, ";BYMONTHDAY={}", days.join(","));
        }
        if let Some(until) = self.until {
            let _ = write!(out, ";UNTIL={}", tilo_formats::compact_date_text(until));
        }
        if let Some(count) = self.count {
            let _ = write!(out, ";COUNT={count}");
        }
        out
    }

    /// One string carrying the whole rule set, including exclusions: the
    /// [`Rule::to_rrule`] body followed by `;EXDATE=YYYYMMDD,…`.
    ///
    /// RFC 5545 stores excluded dates in their own `EXDATE` content line.
    /// Consumers that keep a rule as a single value (a language-level repeat
    /// type, a settings field) would otherwise have two strings to keep in
    /// step, so this compact form round-trips through [`Rule::parse`].
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = self.to_rrule();
        if !self.exdates.is_empty() {
            let dates: Vec<String> = self
                .exdates
                .iter()
                .map(|date| tilo_formats::compact_date_text(*date))
                .collect();
            let _ = write!(out, ";EXDATE={}", dates.join(","));
        }
        out
    }

    #[must_use]
    pub fn freq(&self) -> Freq {
        self.freq
    }

    /// Positive step between firings; values below 1 are treated as 1.
    #[must_use]
    pub fn interval(&self) -> u32 {
        self.interval.max(1)
    }

    /// Weekdays a `WEEKLY` rule fires on, in week order. Empty means the
    /// seed's own weekday.
    #[must_use]
    pub fn by_day(&self) -> &[Weekday] {
        &self.by_day
    }

    /// Days of the month a `MONTHLY` or `YEARLY` rule fires on. Empty means
    /// the seed's own day; negative values count back from the month's end.
    #[must_use]
    pub fn by_month_day(&self) -> &[i8] {
        &self.by_month_day
    }

    /// Inclusive last date of the series.
    #[must_use]
    pub fn until(&self) -> Option<Date> {
        self.until
    }

    /// How many firings the series has, counting the seed.
    #[must_use]
    pub fn count(&self) -> Option<u32> {
        self.count
    }

    /// Dates dropped from the series. They still count towards
    /// [`Rule::count`], which is how RFC 5545 ranges a series before
    /// exclusions are applied.
    #[must_use]
    pub fn exdates(&self) -> &[Date] {
        &self.exdates
    }

    #[must_use]
    pub fn with_interval(mut self, interval: u32) -> Self {
        self.interval = interval.max(1);
        self
    }

    #[must_use]
    pub fn with_by_day(mut self, days: impl IntoIterator<Item = Weekday>) -> Self {
        self.by_day = canonical_days(days);
        self
    }

    #[must_use]
    pub fn with_month_days(mut self, days: impl IntoIterator<Item = i8>) -> Self {
        let mut days: Vec<i8> = days
            .into_iter()
            .filter(|day| (1..=31).contains(day) || (-31..=-1).contains(day))
            .collect();
        days.sort_unstable();
        days.dedup();
        self.by_month_day = days;
        self
    }

    #[must_use]
    pub fn with_until(mut self, until: Option<Date>) -> Self {
        self.until = until;
        self
    }

    #[must_use]
    pub fn with_count(mut self, count: Option<u32>) -> Self {
        self.count = count.filter(|count| *count > 0);
        self
    }

    #[must_use]
    pub fn with_exdates(mut self, exdates: impl IntoIterator<Item = Date>) -> Self {
        let mut exdates: Vec<Date> = exdates.into_iter().collect();
        exdates.sort_unstable();
        exdates.dedup();
        self.exdates = exdates;
        self
    }

    /// The rule's dates within `[from, to]`, ascending, without duplicates.
    ///
    /// `UNTIL` and `COUNT` bound the series, exclusions remove dates from it.
    /// A rule that runs out before `from` yields nothing.
    #[must_use]
    pub fn occurrences(&self, seed: Date, from: Date, to: Date) -> Vec<Date> {
        let last = self.until.map_or(to, |until| until.min(to));
        let mut out = Vec::new();
        if last < from {
            return out;
        }
        let Some((start, count)) = self.skip_to(seed, from) else {
            return out;
        };
        self.walk(start, count, true, from, last, |date| {
            out.push(date);
            ControlFlow::Continue(())
        });
        out
    }

    /// The first date of the series on or after `date`, or `None` once the
    /// series has ended. Excluded dates are stepped over.
    #[must_use]
    pub fn next_on_or_after(&self, seed: Date, date: Date) -> Option<Date> {
        let horizon = date
            .checked_add(jiff::Span::new().try_days(SEARCH_DAYS).ok()?)
            .ok()?;
        let last = self.until.map_or(horizon, |until| until.min(horizon));
        if last < date {
            return None;
        }
        let (start, count) = self.skip_to(seed, date)?;
        let mut found = None;
        self.walk(start, count, true, date, last, |day| {
            found = Some(day);
            ControlFlow::Break(())
        });
        found
    }

    /// The first date of the series after `after`.
    #[must_use]
    pub fn next_after(&self, seed: Date, after: Date) -> Option<Date> {
        self.next_on_or_after(seed, after.tomorrow().ok()?)
    }

    /// The rule's occurrences for a timed series that starts at `seed`: each
    /// occurrence's authored wall time plus, for a named zone, the instant it
    /// denotes.
    ///
    /// The wall time is the same clock time on every date, which is what keeps
    /// a 09:00 series at 09:00 after a daylight-saving change. `floating` and
    /// unknown zones have no instant.
    #[must_use]
    pub fn timed_occurrences(
        &self,
        seed: DateTime,
        zone: &str,
        from: Date,
        to: Date,
    ) -> Vec<TimedOccurrence> {
        let time = seed.time();
        self.occurrences(seed.date(), from, to)
            .into_iter()
            .map(|date| {
                let wall = date.to_datetime(time);
                TimedOccurrence {
                    wall,
                    instant: zone::wall_to_utc(zone, wall).map(|(instant, _)| instant),
                }
            })
            .collect()
    }

    /// How many original dates the rule produces before `date`, counting the
    /// seed: exclusions and edits aside, as `COUNT` counts them.
    ///
    /// Arithmetic for the presets a picker writes, so a counted series stays
    /// as cheap to show as an unbounded one however old it is; a rule with
    /// `BYDAY` or `BYMONTHDAY` filters of its own is counted by walking it,
    /// as its filters do not divide into the frequency's steps.
    #[must_use]
    pub fn count_before(&self, seed: Date, date: Date) -> u64 {
        if date <= seed {
            return 0;
        }
        if !self.by_day.is_empty() || !self.by_month_day.is_empty() {
            let mut count = 0_u64;
            self.walk(seed, None, false, seed, date, |day| {
                if day >= date {
                    return ControlFlow::Break(());
                }
                count = count.saturating_add(1);
                ControlFlow::Continue(())
            });
            return count;
        }
        let interval = i64::from(self.interval());
        // The dates before `date` are the seed plus every `interval`th step
        // below it, so their count is the distance rounded up.
        let steps_before = |steps: i64| u64::try_from(ceil_div(steps, interval)).unwrap_or(0);
        match self.freq {
            Freq::Daily => steps_before(days_between(seed, date)),
            Freq::Weekly => u64::try_from(ceil_div(
                days_between(seed, date),
                interval.saturating_mul(7),
            ))
            .unwrap_or(0),
            Freq::Monthly => months_before(seed, date, interval),
            Freq::Yearly => years_before(seed, date, interval),
        }
    }

    /// Where, and with how much `COUNT` left, this rule starts when only the
    /// dates on or after `from` are asked for; `None` once the series has run
    /// out before `from`.
    ///
    /// A rule without day filters steps forward by whole periods, so a counted
    /// series costs its window rather than the years since its seed. A
    /// filtered rule ("every weekday") keeps its seed, whose dates have to be
    /// walked to apply the filter.
    fn skip_to(&self, seed: Date, from: Date) -> Option<(Date, Option<u32>)> {
        if from <= seed || !self.by_day.is_empty() || !self.by_month_day.is_empty() {
            return Some((seed, self.count));
        }
        let Some(start) = self.period_at_or_after(seed, from) else {
            return Some((seed, self.count));
        };
        let Some(count) = self.count else {
            return Some((start, None));
        };
        // Every date of the series before the window has spent its place in
        // the count, exclusions aside, which is exactly how `COUNT` counts.
        let spent = u32::try_from(self.count_before(seed, start)).unwrap_or(u32::MAX);
        Some((
            start,
            Some(count.checked_sub(spent).filter(|left| *left > 0)?),
        ))
    }

    /// The first date of this rule on or after `from`, reached by whole
    /// `INTERVAL` steps from `seed`. Only a rule without day filters is moved
    /// this way: its dates are exactly the period steps.
    fn period_at_or_after(&self, seed: Date, from: Date) -> Option<Date> {
        let interval = i64::from(self.interval());
        let days = days_between(seed, from);
        match self.freq {
            Freq::Daily => {
                let steps = ceil_div(days, interval).checked_mul(interval)?;
                seed.checked_add(jiff::Span::new().try_days(steps).ok()?)
                    .ok()
            }
            Freq::Weekly => {
                let weeks = ceil_div(days, interval.checked_mul(7)?).checked_mul(interval)?;
                seed.checked_add(jiff::Span::new().try_days(weeks.checked_mul(7)?).ok()?)
                    .ok()
            }
            Freq::Monthly => month_at_or_after(seed, from, interval),
            Freq::Yearly => year_at_or_after(seed, from, interval),
        }
    }

    /// Applies `visit` to the rule's dates from `seed` that fall on or after
    /// `from`, in order, until it asks to stop or the series passes `end`.
    ///
    /// `count` stands in for [`Rule::count`], so a skip-ahead hands over what
    /// is left of it. Without `limits` it ignores `UNTIL` and the exclusions,
    /// which is how counting the authored dates sees them.
    fn walk(
        &self,
        seed: Date,
        count: Option<u32>,
        limits: bool,
        from: Date,
        end: Date,
        mut visit: impl FnMut(Date) -> ControlFlow<()>,
    ) {
        let end = match self.until {
            Some(until) if limits => until.min(end),
            _ => end,
        };
        let mut left = count;
        for period in 0_i64.. {
            let Some((first, last)) = self.period(seed, period) else {
                return;
            };
            if first > end {
                return;
            }
            let mut day = first;
            loop {
                if day >= seed && self.fires_on(seed, day) {
                    #[cfg(test)]
                    DATES_TAKEN.with(|taken| taken.set(taken.get().saturating_add(1)));
                    if day > end || left == Some(0) {
                        return;
                    }
                    left = left.map(|left| left.saturating_sub(1));
                    let excluded = limits && self.exdates.binary_search(&day).is_ok();
                    if day >= from && !excluded && visit(day).is_break() {
                        return;
                    }
                }
                if day >= last {
                    break;
                }
                let Ok(next) = day.tomorrow() else {
                    return;
                };
                day = next;
            }
        }
    }

    /// The first and last day of the rule's `index`th period from `seed`: a
    /// day, a week from Monday (RFC 5545's default `WKST`), a month or a year,
    /// `INTERVAL` of them apart.
    fn period(&self, seed: Date, index: i64) -> Option<(Date, Date)> {
        let step = index.checked_mul(i64::from(self.interval()))?;
        let span = jiff::Span::new();
        match self.freq {
            Freq::Daily => {
                let day = seed.checked_add(span.try_days(step).ok()?).ok()?;
                Some((day, day))
            }
            Freq::Weekly => {
                let monday = seed
                    .checked_sub(span.days(seed.weekday().to_monday_zero_offset()))
                    .ok()?;
                let first = monday
                    .checked_add(span.try_days(step.checked_mul(7)?).ok()?)
                    .ok()?;
                Some((first, first.checked_add(span.days(6)).ok()?))
            }
            Freq::Monthly => {
                let first = seed
                    .first_of_month()
                    .checked_add(span.try_months(step).ok()?)
                    .ok()?;
                Some((first, first.last_of_month()))
            }
            Freq::Yearly => {
                let first = seed
                    .first_of_year()
                    .checked_add(span.try_years(step).ok()?)
                    .ok()?;
                Some((first, first.last_of_year()))
            }
        }
    }

    /// Whether a day of a period is one of the rule's dates. Without day
    /// filters a rule keeps the seed's weekday, day of the month, or month and
    /// day, as RFC 5545 takes them from `DTSTART`.
    fn fires_on(&self, seed: Date, day: Date) -> bool {
        if self.by_day.is_empty() && self.by_month_day.is_empty() {
            return match self.freq {
                Freq::Daily => true,
                Freq::Weekly => day.weekday() == seed.weekday(),
                Freq::Monthly => day.day() == seed.day(),
                Freq::Yearly => (day.month(), day.day()) == (seed.month(), seed.day()),
            };
        }
        let from_end = day
            .day()
            .saturating_sub(day.days_in_month())
            .saturating_sub(1);
        (self.by_day.is_empty() || self.by_day.contains(&day.weekday()))
            && (self.by_month_day.is_empty()
                || self
                    .by_month_day
                    .iter()
                    .any(|month_day| *month_day == day.day() || *month_day == from_end))
    }
}

/// The days the series of `rule` falls on from the day `from` to the day `to` inclusive, the
/// days in `exdates` left out: the one day `seed` for an entry with no rule at all, and none
/// for a rule this crate cannot read exactly. A view, an entry's reminder and a trigger at an
/// entry's time all read the repeat through here.
#[must_use]
pub fn days(rule: &str, seed: Date, exdates: &[Date], from: Date, to: Date) -> Option<Vec<Date>> {
    if to < from {
        return Some(Vec::new());
    }
    match Rule::parse(rule) {
        Some(rule) => Some(
            rule.with_exdates(exdates.iter().copied())
                .occurrences(seed, from, to),
        ),
        // No rule and no words: the one occurrence, where it falls in the window.
        None if rule.trim().is_empty() => Some(
            (from <= seed && seed <= to && !exdates.contains(&seed))
                .then_some(seed)
                .into_iter()
                .collect(),
        ),
        None => None,
    }
}

/// The instants the series of `rule` starts at, from the day `from` to the day `to` inclusive,
/// in seconds since the epoch: the entry starts at the wall time `start` (`09:00`), read in
/// `timezone` (an IANA name, `UTC`, or `floating`/empty for a time written nowhere in
/// particular, read as UTC), and the days in `exdates` are left out. It keeps the authored
/// wall time across a daylight-saving change, and a rule it cannot read yields nothing rather
/// than wrong days.
#[must_use]
pub fn occurrences(
    rule: &str,
    start: DateTime,
    timezone: &str,
    exdates: &[Date],
    from: Date,
    to: Date,
) -> Vec<i64> {
    days(rule, start.date(), exdates, from, to)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|date| instant(timezone, date.to_datetime(start.time())))
        .collect()
}

/// The instant the wall time `at` is, in `timezone`, in seconds since the epoch.
fn instant(timezone: &str, at: DateTime) -> Option<i64> {
    match timezone.trim() {
        "" | "floating" => Some(at.to_zoned(TimeZone::UTC).ok()?.timestamp().as_second()),
        name => zone::wall_to_utc(name, at).map(|(instant, _)| instant.as_second()),
    }
}

/// How many days `to` is after `from`.
fn days_between(from: Date, to: Date) -> i64 {
    i64::from(to.since(from).map_or(0, |span| span.get_days()))
}

/// [`Rule::count_before`] for yearly rules, which skip the years that have no
/// 29 February.
fn years_before(seed: Date, date: Date, interval: i64) -> u64 {
    let years = i64::from(date.year()).saturating_sub(i64::from(seed.year()));
    if (seed.month(), seed.day()) != (2, 29) {
        let reached = years.saturating_add(i64::from(
            (date.month(), date.day()) > (seed.month(), seed.day()),
        ));
        return u64::try_from(ceil_div(reached, interval)).unwrap_or(0);
    }
    let mut count = 0_u64;
    let mut offset = 0_i64;
    while offset <= years {
        let year = seed.year().checked_add(i16::try_from(offset).unwrap_or(0));
        if let Some(candidate) = year.and_then(|year| Date::new(year, 2, 29).ok()) {
            if candidate >= date {
                break;
            }
            count = count.saturating_add(1);
        }
        offset = offset.saturating_add(interval);
    }
    count
}

/// The first counted year at or after `from` that holds the seed's month and
/// day, which skips the years that have no 29 February.
fn year_at_or_after(seed: Date, from: Date, interval: i64) -> Option<Date> {
    let mut step = ceil_div(
        i64::from(from.year()).saturating_sub(i64::from(seed.year())),
        interval,
    );
    for _ in 0..SKIPPED_PERIODS {
        let year = seed
            .year()
            .checked_add(i16::try_from(step.checked_mul(interval)?).ok()?)?;
        if let Some(date) = Date::new(year, seed.month(), seed.day())
            .ok()
            .filter(|date| *date >= from)
        {
            return Some(date);
        }
        step = step.checked_add(1)?;
    }
    None
}

/// The first counted month at or after `from` that holds the seed's day: a
/// month without it (the 31st, 29 February) carries no occurrence, so the step
/// moves on to the next counted month.
fn month_at_or_after(seed: Date, from: Date, interval: i64) -> Option<Date> {
    let mut step = ceil_div(months_between(seed, from), interval);
    for _ in 0..SKIPPED_PERIODS {
        let total = i64::from(seed.month())
            .saturating_sub(1)
            .checked_add(step.checked_mul(interval)?)?;
        let year = seed
            .year()
            .checked_add(i16::try_from(total.div_euclid(12)).ok()?)?;
        let month = i8::try_from(total.rem_euclid(12)).ok()?.checked_add(1)?;
        if let Some(date) = Date::new(year, month, seed.day())
            .ok()
            .filter(|date| *date >= from)
        {
            return Some(date);
        }
        step = step.checked_add(1)?;
    }
    None
}

/// [`Rule::count_before`] for monthly rules, which skip the months that have
/// no such day.
fn months_before(seed: Date, date: Date, interval: i64) -> u64 {
    let months = months_between(seed, date);
    if seed.day() <= 28 {
        let reached = months.saturating_add(i64::from(date.day() > seed.day()));
        return u64::try_from(ceil_div(reached, interval)).unwrap_or(0);
    }
    let mut count = 0_u64;
    let mut offset = 0_i64;
    while offset <= months {
        let total = i64::from(seed.month())
            .saturating_sub(1)
            .saturating_add(offset);
        let year = seed
            .year()
            .checked_add(i16::try_from(total.div_euclid(12)).unwrap_or(0));
        let month = i8::try_from(total.rem_euclid(12))
            .unwrap_or(0)
            .saturating_add(1);
        if let Some(candidate) = year.and_then(|year| Date::new(year, month, seed.day()).ok()) {
            if candidate >= date {
                break;
            }
            count = count.saturating_add(1);
        }
        offset = offset.saturating_add(interval);
    }
    count
}

/// One occurrence of a timed series: the authored wall time and, in a named
/// zone, the absolute instant it denotes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimedOccurrence {
    pub wall: DateTime,
    pub instant: Option<Timestamp>,
}

/// The preset set a repeat picker cycles through, and the canonical rule text
/// for each. Reading it back from arbitrary rule text is lossy on purpose:
/// [`Repeat::from_rrule`] names the preset that best describes a stored rule,
/// while [`Rule::parse`] keeps every part of it.
///
/// A repeat is a resource value, so its variants' stored names are format:
/// adding one is a document change, not a rename.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, tilo_resource::Model)]
pub enum Repeat {
    /// A single occurrence.
    #[default]
    None,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
}

impl Repeat {
    /// RFC 5545 rule text (no `RRULE:` prefix), or empty for a single
    /// occurrence.
    #[must_use]
    pub fn to_rrule(self) -> String {
        match self {
            Self::None => String::new(),
            Self::Daily => "FREQ=DAILY".into(),
            Self::Weekdays => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into(),
            Self::Weekly => "FREQ=WEEKLY".into(),
            Self::Monthly => "FREQ=MONTHLY".into(),
            Self::Yearly => "FREQ=YEARLY".into(),
        }
    }

    /// The preset that best describes stored rule text, or [`Repeat::None`]
    /// when it repeats on no schedule a preset names.
    #[must_use]
    pub fn from_rrule(text: &str) -> Self {
        let up = text.to_uppercase();
        if up.contains("FREQ=DAILY") {
            Self::Daily
        } else if up.contains("BYDAY=MO,TU,WE,TH,FR") {
            Self::Weekdays
        } else if up.contains("FREQ=WEEKLY") {
            Self::Weekly
        } else if up.contains("FREQ=MONTHLY") {
            Self::Monthly
        } else if up.contains("FREQ=YEARLY") {
            Self::Yearly
        } else {
            Self::None
        }
    }

    /// The next preset in the picker's cycle.
    #[must_use]
    pub fn cycle(self) -> Self {
        match self {
            Self::None => Self::Daily,
            Self::Daily => Self::Weekdays,
            Self::Weekdays => Self::Weekly,
            Self::Weekly => Self::Monthly,
            Self::Monthly => Self::Yearly,
            Self::Yearly => Self::None,
        }
    }

    /// The expansion rule for this preset, with the series' limits. `None`
    /// for a single occurrence.
    #[must_use]
    pub fn rule(
        self,
        interval: u32,
        until: Option<Date>,
        count: Option<u32>,
        exdates: &[Date],
    ) -> Option<Rule> {
        let freq = match self {
            Self::None => return None,
            Self::Daily => Freq::Daily,
            Self::Weekdays | Self::Weekly => Freq::Weekly,
            Self::Monthly => Freq::Monthly,
            Self::Yearly => Freq::Yearly,
        };
        let rule = Rule::new(freq)
            .with_interval(interval)
            .with_until(until)
            .with_count(count)
            .with_exdates(exdates.to_vec());
        Some(if self == Self::Weekdays {
            rule.with_by_day([
                Weekday::Monday,
                Weekday::Tuesday,
                Weekday::Wednesday,
                Weekday::Thursday,
                Weekday::Friday,
            ])
        } else {
            rule
        })
    }
}

fn strip_rrule_prefix(text: &str) -> &str {
    text.split_once(':')
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("RRULE"))
        .map_or(text, |(_, body)| body.trim())
}

fn parse_positive(value: &str) -> Option<u32> {
    let parsed: u32 = value.trim().parse().ok()?;
    (parsed > 0).then_some(parsed)
}

fn parse_cal_date(value: &str) -> Option<Date> {
    let value = value.trim();
    let compact = value
        .get(..8)
        .filter(|head| head.bytes().all(|byte| byte.is_ascii_digit()));
    match compact {
        Some(head) => tilo_formats::compact_date_from_text(head),
        None => tilo_formats::date_from_text(value),
    }
}

fn parse_month_day(value: &str) -> Option<i8> {
    let day: i8 = value.parse().ok()?;
    ((1..=31).contains(&day) || (-31..=-1).contains(&day)).then_some(day)
}

fn parse_weekday(code: &str) -> Option<Weekday> {
    match code.to_ascii_uppercase().as_str() {
        "MO" => Some(Weekday::Monday),
        "TU" => Some(Weekday::Tuesday),
        "WE" => Some(Weekday::Wednesday),
        "TH" => Some(Weekday::Thursday),
        "FR" => Some(Weekday::Friday),
        "SA" => Some(Weekday::Saturday),
        "SU" => Some(Weekday::Sunday),
        _ => None,
    }
}

fn weekday_code(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "MO",
        Weekday::Tuesday => "TU",
        Weekday::Wednesday => "WE",
        Weekday::Thursday => "TH",
        Weekday::Friday => "FR",
        Weekday::Saturday => "SA",
        Weekday::Sunday => "SU",
    }
}

fn canonical_days(days: impl IntoIterator<Item = Weekday>) -> Vec<Weekday> {
    let mut days: Vec<Weekday> = days.into_iter().collect();
    days.sort_by_key(|day| day.to_monday_zero_offset());
    days.dedup();
    days
}

/// `steps / step` rounded up, for a positive `step`.
fn ceil_div(steps: i64, step: i64) -> i64 {
    steps
        .saturating_add(step.saturating_sub(1))
        .div_euclid(step)
}

fn months_between(seed: Date, date: Date) -> i64 {
    i64::from(date.year())
        .saturating_sub(i64::from(seed.year()))
        .saturating_mul(12)
        .saturating_add(i64::from(date.month()).saturating_sub(i64::from(seed.month())))
}
