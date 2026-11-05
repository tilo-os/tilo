//! Imports over days, moments and lengths of time: the day it is today, the day a moment falls
//! on and the time of day it is then, a day moved by days and how far two days or two moments are
//! apart, the parts of a day and the week it stands in, and the day or moment a text names.
//!
//! A day is the person's before it is UTC's: what a moment is read as (`t.day()`,
//! `t.time_of_day()`) and `today` read the zone `crate::recurrence` reads, and the week
//! `d.start_of_week()` starts is the one their region starts, which the host names. A length of
//! time is its number of seconds, so a day moving by one checks that it is whole days first: a
//! written length that is no whole day the checker refuses, one code holds is refused here.

use chrono::{Datelike, Duration, NaiveDate, Timelike};

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
            Imp::DayParse => {
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
            Imp::TimeParse => {
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
            _ => trap("not a day or moment import"),
        }
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
}

/// The day `text` holds, `YYYY-MM-DD`.
pub(crate) fn day_of(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// A day as the language holds one, `YYYY-MM-DD`.
pub(crate) fn day_text(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}
