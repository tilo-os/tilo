//! Imports over days, moments and lengths of time: the day it is today, the day a moment falls
//! on and the time of day it is then, a day moved by days and how far two days or two moments are
//! apart, the parts of a day and the week it stands in, and the day or moment a text names.
//!
//! A day is the person's before it is UTC's: what a moment is read as (`t.day()`,
//! `t.time_of_day()`) and `today` read the zone the host names, the one their region starts its
//! week in, and the one a written wall-clock time is read in. A length of time is its number of
//! seconds, so a day moving by one checks that it is whole days first: a written length that is
//! no whole day the checker refuses, one code holds is refused here.
//!
//! The zone is an IANA name where the host names one, else the clock's offset: a wall time a
//! person writes (`2026-06-30T09:00`) is that minute where they are, and a day is theirs before
//! it is UTC's.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use chrono_tz::Tz;

use tilolang::calendar::{DAY, civil_from_days, time_text};

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i64, trap};
use crate::engine::Wasm;
use crate::value::Value;

impl Core {
    pub(crate) fn time_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::Today => {
                let now = self.now_text()?;
                let Some(at) = tilolang::calendar::instant(&now) else {
                    return trap(format!("today: `{now}` is not a time"));
                };
                self.ret_h(Value::Text(day_text(self.local_day(at))))
            }
            Imp::TimeDay => {
                let at = self.instant_arg(a.h(0)?)?;
                self.ret_h(Value::Text(day_text(self.local_day(at))))
            }
            Imp::TimeOfDay => {
                let at = self.instant_arg(a.h(0)?)?;
                let local = self.local_wall(at);
                ret_i64(i64::from(local.time().num_seconds_from_midnight()))
            }
            Imp::DayShift => {
                let day = self.day(a.h(0)?)?;
                let secs = a.long(1)?;
                if secs % DAY != 0 {
                    return trap(format!(
                        "a day moves by whole days, and {} is no whole day",
                        tilolang::duration::text(secs)
                    ));
                }
                let days = secs.checked_div(DAY);
                let days = if a.h(2)? != 0 {
                    days.and_then(i64::checked_neg)
                } else {
                    days
                };
                let Some(days) = days else {
                    return trap("the day is out of range");
                };
                let Some(moved) = day.checked_add_signed(Duration::days(days)) else {
                    return trap("the day is out of range");
                };
                self.ret_h(Value::Text(day_text(moved)))
            }
            Imp::TimeSpan => {
                let from = self.text(a.h(0)?)?;
                let to = self.text(a.h(1)?)?;
                // Two days are days apart, two moments seconds, and either reads as the same
                // number of seconds.
                let secs = match (day_of(&from), day_of(&to)) {
                    (Some(from), Some(to)) => from.signed_duration_since(to).num_seconds(),
                    _ => {
                        Self::instant_arg_text(&from)?.saturating_sub(Self::instant_arg_text(&to)?)
                    }
                };
                ret_i64(secs)
            }
            Imp::DayPart => {
                let day = self.day(a.h(0)?)?;
                let part = match a.h(1)? {
                    0 => i64::from(day.day()),
                    1 => i64::from(day.month()),
                    _ => i64::from(day.year()),
                };
                ret_i64(part)
            }
            Imp::DayWeekday => {
                let day = self.day(a.h(0)?)?;
                ret_i64(i64::from(day.weekday().num_days_from_monday()))
            }
            Imp::DayStartOfWeek => {
                let day = self.day(a.h(0)?)?;
                // The week runs from the day the person's region starts it on: back that many
                // days from the day, a week round.
                let back = i64::from(day.weekday().num_days_from_monday())
                    .wrapping_add(7)
                    .wrapping_sub(self.week_start())
                    .rem_euclid(7);
                let Some(start) = day.checked_sub_signed(Duration::days(back)) else {
                    return trap("the day is out of range");
                };
                self.ret_h(Value::Text(day_text(start)))
            }
            Imp::DayParse => self.day_parse(a),
            Imp::WallClock => {
                self.clock()?;
                let text = self.text(a.h(0)?)?;
                self.ret_h(Value::Text(self.wall_clock(&text)?))
            }
            Imp::TimeParse => self.time_parse(a),
            _ => trap("not a day or moment import"),
        }
    }

    /// The day a text names, near the day a person says it on.
    fn day_parse(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let (text, near) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
        let read = self.when_of(&text, &near)?;
        let day = match read.day {
            Some(days) => {
                let day = Self::day_of_days(days)?;
                Some(Box::new(Value::Text(day_text(day))))
            }
            None => None,
        };
        self.ret_h(Value::Opt(day))
    }

    /// The moment a text names, near the day a person says it on.
    fn time_parse(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let (text, near) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
        let read = self.when_of(&text, &near)?;
        // A text that names a day and no time is the moment that day starts.
        let at = match (read.day, read.minutes) {
            (Some(days), minutes) => Some(self.moment_of(days, minutes.unwrap_or(0))?),
            _ => None,
        };
        let at = at.map(|at| Box::new(Value::Text(time_text(at))));
        self.ret_h(Value::Opt(at))
    }

    /// A `Date` value as a day.
    pub(crate) fn day(&self, h: i32) -> HResult<NaiveDate> {
        let text = self.text(h)?;
        day_of(&text).ok_or_else(|| crate::engine::HostFail::Trap(format!("`{text}` is not a day")))
    }

    /// A `Time` value as the instant it is.
    pub(crate) fn instant_arg(&self, h: i32) -> HResult<i64> {
        let text = self.text(h)?;
        Self::instant_arg_text(&text)
    }

    /// A time's text as the instant it is.
    fn instant_arg_text(text: &str) -> HResult<i64> {
        tilolang::calendar::instant(text)
            .ok_or_else(|| crate::engine::HostFail::Trap(format!("`{text}` is not a time")))
    }

    /// The day `days` after 1970-01-01, as the calendar holds a day number.
    fn day_of_days(days: i64) -> HResult<NaiveDate> {
        let (y, m, d) = civil_from_days(days);
        match (i32::try_from(y), u32::try_from(m), u32::try_from(d)) {
            (Ok(y), Ok(m), Ok(d)) => NaiveDate::from_ymd_opt(y, m, d)
                .ok_or_else(|| crate::engine::HostFail::Trap("the day is out of range".into())),
            _ => trap("the day is out of range"),
        }
    }

    /// The moment a day and a time of day name for the person.
    fn moment_of(&self, days: i64, minutes: i64) -> HResult<i64> {
        let day = Self::day_of_days(days)?;
        let secs = u32::try_from(minutes.rem_euclid(1440).saturating_mul(60)).unwrap_or(0);
        let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(secs, 0)
            .unwrap_or(chrono::NaiveTime::MIN);
        Ok(self.instant_of(day.and_time(time)))
    }

    /// What `text` says of a day and a time of day, read against the person's day and wall
    /// clock at `near`, in the language they read in.
    fn when_of(&self, text: &str, near: &str) -> HResult<tilolang::natural::When> {
        let at = Self::instant_arg_text(near)?;
        let local = self.local_wall(at);
        let epoch = Self::day_of_days(0)?;
        let days = local.date().signed_duration_since(epoch).num_days();
        let second_of_day = i64::from(local.time().num_seconds_from_midnight());
        let lang = tilolang::natural::Language::of(&self.words.borrow().language());
        Ok(tilolang::natural::when(text, days, second_of_day, lang))
    }

    /// The day the person's week starts on, as days from Monday (Monday where the host names
    /// none), which their region knows and their zone does not.
    fn week_start(&self) -> i64 {
        self.week_start.borrow().as_ref().map_or(0, |start| start())
    }

    /// The instant a wall-clock time names for the person: `2026-06-30T09:00`, read in their
    /// zone, as the instant it is (`2026-06-30T16:00:00Z` where their clock is six hours behind
    /// UTC). A time that is no time of day traps: the checker admits only well-formed ones.
    fn wall_clock(&self, text: &str) -> HResult<String> {
        let wall = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"]
            .into_iter()
            .find_map(|shape| NaiveDateTime::parse_from_str(text, shape).ok());
        let Some(wall) = wall else {
            return trap(format!("`{text}` is not a wall-clock time"));
        };
        Ok(tilolang::calendar::time_text(self.instant_of(wall)))
    }

    /// The person's zone as its IANA name: the one the invocation pinned when it read the
    /// clock, else the host's, when it named one this build knows.
    fn zone_name(&self) -> Option<String> {
        if let Some(clock) = self.pinned_clock() {
            return clock
                .zone
                .and_then(|zone| tilo_recurrence::canonical_zone(&zone));
        }
        let name = self.zone_name.borrow().as_ref().and_then(|name| name())?;
        tilo_recurrence::canonical_zone(&name)
    }

    /// The zone with that name, or `None` for a host that named none or one this platform does
    /// not know.
    fn zone(&self) -> Option<Tz> {
        self.zone_name()?.parse().ok()
    }

    /// The person's offset from UTC now, in seconds: what the invocation pinned, else what a
    /// host that names no zone gives.
    fn offset(&self) -> i64 {
        if let Some(clock) = self.pinned_clock() {
            return clock.offset;
        }
        self.zone.borrow().as_ref().map_or(0, |zone| zone())
    }

    /// The moment `wall` names for the person: read in their zone, so a time of day keeps its
    /// minutes across a change of clocks; by their clock's offset where the host named no zone.
    pub(crate) fn instant_of(&self, wall: NaiveDateTime) -> i64 {
        let resolved = self
            .zone_name()
            .and_then(|zone| tilo_recurrence::wall_to_utc(&zone, wall))
            .map(|(instant, _)| instant.timestamp());
        resolved.unwrap_or_else(|| wall.and_utc().timestamp().saturating_sub(self.offset()))
    }

    /// The wall time the moment `instant` reads as for the person.
    pub(crate) fn local_wall(&self, instant: i64) -> NaiveDateTime {
        let Some(utc) = DateTime::from_timestamp(instant, 0) else {
            return NaiveDateTime::default();
        };
        match self.zone() {
            Some(zone) => zone.from_utc_datetime(&utc.naive_utc()).naive_local(),
            None => utc
                .checked_add_signed(Duration::seconds(self.offset()))
                .unwrap_or(utc)
                .naive_utc(),
        }
    }

    /// The day `instant` falls on for the person.
    pub(crate) fn local_day(&self, instant: i64) -> NaiveDate {
        self.local_wall(instant).date()
    }
}

/// The day `text` holds, `YYYY-MM-DD`.
pub(crate) fn day_of(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// A day as the language holds one, `YYYY-MM-DD`.
pub(crate) fn day_text(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}
