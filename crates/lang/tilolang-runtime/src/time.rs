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

use jiff::civil::{Date, DateTime};

use tilolang::calendar::{DAY, civil_from_days, time_text};

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i64, trap};
use crate::engine::Wasm;
use crate::value::Value;

impl Core {
    /// A day (`a.h(1)` 0) or a moment as a sentence names it, in the person's formats (`Imp::DateShow`).
    fn date_show(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let text = self.text(a.h(0)?)?;
        let kind = if a.h(1)? == 0 { "date" } else { "time" };
        self.read_person();
        let shown = match self.sentence_by.borrow().as_ref() {
            Some(read) => read(kind, &text),
            None => text,
        };
        self.ret_h(Value::Text(shown))
    }

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
            // The day a moment falls on in a named zone: the same moment, read where people there
            // are, from the zone's own rules rather than the person's clock.
            Imp::TimeDayIn => self.time_day_in_import(a),
            // The moment a wall-clock time names in a named zone: the zone's own rules decide it,
            // the person's clock none of it, so a view draws it and an import needs no clock.
            Imp::TimeFromWall => self.time_from_wall_import(a),
            Imp::TimeWall => self.time_wall_import(a),
            Imp::TimeOfDay => {
                let at = self.instant_arg(a.h(0)?)?;
                let local = self.local_wall(at);
                ret_i64(
                    local
                        .time()
                        .duration_since(jiff::civil::Time::MIN)
                        .as_secs(),
                )
            }
            // The time of day a moment falls at, as the person reads a clock: `10:41` or
            // `10:41 AM`, in the style they chose in Settings.
            Imp::TimeClock => self.time_clock_import(a),
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
                let Some(moved) = day.checked_add(jiff::Span::new().days(days)).ok() else {
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
                    (Some(from), Some(to)) => from.duration_since(to).as_secs(),
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
                ret_i64(i64::from(day.weekday().to_monday_zero_offset()))
            }
            Imp::DayStartOfWeek => {
                let day = self.day(a.h(0)?)?;
                // The week runs from the day the person's region starts it on: back that many
                // days from the day, a week round.
                let back = i64::from(day.weekday().to_monday_zero_offset())
                    .wrapping_add(7)
                    .wrapping_sub(self.week_start())
                    .rem_euclid(7);
                let Some(start) = day.checked_sub(jiff::Span::new().days(back)).ok() else {
                    return trap("the day is out of range");
                };
                self.ret_h(Value::Text(day_text(start)))
            }
            // A day or a moment as a sentence names it in the person's formats: `Jun 29`, or
            // `Jun 29, 2025` another year, `Jun 29, 6:00 PM` a moment. The person's words, and
            // its stored text where none is set.
            Imp::DateShow => self.date_show(a),
            Imp::DayParse => self.day_parse(a),
            Imp::WallClock => {
                self.clock()?;
                let text = self.text(a.h(0)?)?;
                self.ret_h(Value::Text(self.wall_clock(&text)?))
            }
            Imp::TimeParse => self.time_parse(a),
            Imp::Occurrences => self.occurrences_import(a),
            _ => trap("not a day or moment import"),
        }
    }

    /// The time of day a moment falls at, as the person reads a clock (`Imp::TimeClock`).
    fn time_clock_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let at = self.instant_arg(a.h(0)?)?;
        let local = self.local_wall(at);
        self.ret_h(Value::Text(self.clock_text(
            u32::from(local.hour().unsigned_abs()),
            u32::from(local.minute().unsigned_abs()),
        )))
    }

    /// The moment a wall-clock time names in a named zone (`Imp::TimeFromWall`).
    fn time_from_wall_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let zone = self.text(a.h(0)?)?;
        let text = self.text(a.h(1)?)?;
        let at = self.moment_of_wall(&zone, &text);
        self.ret_h(Value::Opt(at.map(|at| {
            Box::new(Value::Text(tilolang::calendar::time_text(at)))
        })))
    }

    /// The wall time a moment is in a named zone (`Imp::TimeWall`): what a calendar stores.
    fn time_wall_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let at = self.instant_arg(a.h(0)?)?;
        let name = self.text(a.h(1)?)?;
        let wall = self.wall_in(at, &name);
        self.ret_h(Value::Opt(wall.map(|wall| Box::new(Value::Text(wall)))))
    }

    /// The day a moment falls on in a named zone (`Imp::TimeDayIn`).
    fn time_day_in_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let at = self.instant_arg(a.h(0)?)?;
        let name = self.text(a.h(1)?)?;
        self.ret_h(Value::Text(day_text(self.day_in(at, &name)?)))
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
    pub(crate) fn day(&self, h: i32) -> HResult<Date> {
        let text = self.text(h)?;
        day_of(&text).ok_or_else(|| crate::engine::HostFail::Trap(format!("`{text}` is not a day")))
    }

    /// A list of `Date` texts as the days since the epoch it holds.
    fn day_texts(&self, h: i32) -> HResult<Vec<i64>> {
        let Value::List(items) = self.val(h)? else {
            return trap("expected a list of days");
        };
        items
            .into_iter()
            .map(|item| match item {
                Value::Text(text) => day_of(&text)
                    .map(days_of)
                    .ok_or_else(|| crate::engine::HostFail::Trap(format!("`{text}` is not a day"))),
                other => trap(format!("expected a day, got {}", other.kind_name())),
            })
            .collect()
    }

    /// A `Time` value as the instant it is.
    pub(crate) fn instant_arg(&self, h: i32) -> HResult<i64> {
        let text = self.text(h)?;
        Self::instant_arg_text(&text)
    }

    /// The moments a repeat's rule falls on between two days, in the entry's zone: what a view
    /// shows of it in a window, read from the rule alone.
    fn occurrences_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let rule = self.text(a.h(0)?)?;
        let start = self.text(a.h(1)?)?;
        let zone = self.text(a.h(2)?)?;
        let exdates = self.day_texts(a.h(3)?)?;
        let from = self.day(a.h(4)?)?;
        let to = self.day(a.h(5)?)?;
        let times = crate::recurrence::occurrences(
            &rule,
            &start,
            &zone,
            &exdates,
            days_of(from),
            days_of(to),
        );
        self.ret_h(Value::List(
            times
                .into_iter()
                .map(|secs| Value::Text(tilolang::calendar::time_text(secs)))
                .collect(),
        ))
    }

    /// A time's text as the instant it is.
    fn instant_arg_text(text: &str) -> HResult<i64> {
        tilolang::calendar::instant(text)
            .ok_or_else(|| crate::engine::HostFail::Trap(format!("`{text}` is not a time")))
    }

    /// The day `days` after 1970-01-01, as the calendar holds a day number.
    fn day_of_days(days: i64) -> HResult<Date> {
        let (y, m, d) = civil_from_days(days);
        match (i16::try_from(y), i8::try_from(m), i8::try_from(d)) {
            (Ok(y), Ok(m), Ok(d)) => Date::new(y, m, d)
                .ok()
                .ok_or_else(|| crate::engine::HostFail::Trap("the day is out of range".into())),
            _ => trap("the day is out of range"),
        }
    }

    /// The moment a day and a time of day name for the person.
    fn moment_of(&self, days: i64, minutes: i64) -> HResult<i64> {
        let day = Self::day_of_days(days)?;
        let time = jiff::civil::Time::MIN
            .wrapping_add(jiff::SignedDuration::from_mins(minutes.rem_euclid(1440)));
        Ok(self.instant_of(day.to_datetime(time)))
    }

    /// What `text` says of a day and a time of day, read against the person's day and wall
    /// clock at `near`, in the language they read in, falling back to English when that reading
    /// names neither a day nor a time (so English phrases written by apps or people still read,
    /// while the person's language wins whenever both read the text).
    fn when_of(&self, text: &str, near: &str) -> HResult<tilolang::natural::When> {
        let at = Self::instant_arg_text(near)?;
        let local = self.local_wall(at);
        let epoch = Self::day_of_days(0)?;
        let days = i64::from(local.date().since(epoch).map_or(0, |span| span.get_days()));
        let second_of_day = local
            .time()
            .duration_since(jiff::civil::Time::MIN)
            .as_secs();
        let lang = tilolang::natural::Language::of(&self.words.borrow().language());
        let read = tilolang::natural::when(text, days, second_of_day, lang);
        if read.day.is_none()
            && read.minutes.is_none()
            && lang != tilolang::natural::Language::English
        {
            return Ok(tilolang::natural::when(
                text,
                days,
                second_of_day,
                tilolang::natural::Language::English,
            ));
        }
        Ok(read)
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
            .find_map(|shape| DateTime::strptime(shape, text).ok());
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
    fn zone(&self) -> Option<jiff::tz::TimeZone> {
        tilo_recurrence::zone::named(&self.zone_name()?)
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
    pub(crate) fn instant_of(&self, wall: DateTime) -> i64 {
        let resolved = self
            .zone_name()
            .and_then(|zone| tilo_recurrence::wall_to_utc(&zone, wall))
            .map(|(instant, _)| instant.as_second());
        resolved.unwrap_or_else(|| {
            wall.to_zoned(jiff::tz::TimeZone::UTC)
                .map_or(0, |at| at.timestamp().as_second())
                .saturating_sub(self.offset())
        })
    }

    /// The wall time the moment `instant` reads as for the person.
    pub(crate) fn local_wall(&self, instant: i64) -> DateTime {
        let Ok(utc) = jiff::Timestamp::from_second(instant) else {
            return DateTime::default();
        };
        let zone = self.zone().unwrap_or_else(|| {
            let offset = i32::try_from(self.offset()).unwrap_or(0);
            jiff::tz::TimeZone::fixed(
                jiff::tz::Offset::from_seconds(offset).unwrap_or(jiff::tz::Offset::UTC),
            )
        });
        utc.to_zoned(zone).datetime()
    }

    /// The day `instant` falls on for the person.
    pub(crate) fn local_day(&self, instant: i64) -> Date {
        self.local_wall(instant).date()
    }

    /// The wall time the moment `instant` is in the zone `name`, as a calendar writes one
    /// (`2026-11-24T09:00:00`); `floating`'s is where the reader is. None for a zone this build
    /// does not know.
    fn wall_in(&self, instant: i64, name: &str) -> Option<String> {
        let name = tilo_recurrence::canonical_zone(name)?;
        let Ok(utc) = jiff::Timestamp::from_second(instant) else {
            return None;
        };
        let wall = if name == "floating" {
            self.local_wall(instant)
        } else {
            let zone = if name == "UTC" {
                jiff::tz::TimeZone::UTC
            } else {
                tilo_recurrence::zone::named(&name)?
            };
            utc.to_zoned(zone).datetime()
        };
        Some(wall.strftime("%Y-%m-%dT%H:%M:%S").to_string())
    }

    /// The moment a wall-clock time names in the zone `name`, as the zone's own rules resolve it:
    /// the earlier instant of a fold and the first real minute out of a gap, so every device reads
    /// the same text the same way. `floating` is a wall time with no zone of its own — nine is nine
    /// wherever the reader is — so it is resolved in theirs; a zone this build does not know, and a
    /// text that is no wall time, answer none.
    fn moment_of_wall(&self, name: &str, text: &str) -> Option<i64> {
        let wall = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"]
            .into_iter()
            .find_map(|shape| DateTime::strptime(shape, text).ok())?;
        let zone = tilo_recurrence::canonical_zone(name)?;
        // A floating time is a wall time with no zone of its own: nine is nine wherever the
        // reader is, so it is resolved in their zone, as the Rust Calendar read it.
        if zone == "floating" {
            return Some(self.instant_of(wall));
        }
        let (instant, _) = tilo_recurrence::wall_to_utc(&zone, wall)?;
        Some(instant.as_second())
    }

    /// The day `instant` falls on in the zone `name`: a moment's date where people there read it,
    /// resolved from the zone's own rules, so two people in different zones read it alike and no
    /// clock is asked. `floating` is a wall time with no zone of its own, so its day is the
    /// reader's, as `day` reads it.
    fn day_in(&self, instant: i64, name: &str) -> HResult<Date> {
        let Some(name) = tilo_recurrence::canonical_zone(name) else {
            return trap(format!("`{name}` is not a time zone"));
        };
        if name == "floating" {
            return Ok(self.local_day(instant));
        }
        let Ok(utc) = jiff::Timestamp::from_second(instant) else {
            return trap("the moment is out of range");
        };
        let zone = if name == "UTC" {
            jiff::tz::TimeZone::UTC
        } else {
            let Some(zone) = tilo_recurrence::zone::named(&name) else {
                return trap(format!("`{name}` is not a time zone this build knows"));
            };
            zone
        };
        Ok(utc.to_zoned(zone).date())
    }
}

/// The day `text` holds, `YYYY-MM-DD`.
pub(crate) fn day_of(text: &str) -> Option<Date> {
    Date::strptime("%Y-%m-%d", text).ok()
}

/// A day as the language holds one, `YYYY-MM-DD`.
pub(crate) fn day_text(day: Date) -> String {
    day.strftime("%Y-%m-%d").to_string()
}

/// The days `day` is since the epoch.
fn days_of(day: Date) -> i64 {
    tilolang::calendar::days_from_civil(
        i64::from(day.year()),
        i64::from(day.month()),
        i64::from(day.day()),
    )
}
