//! Imports over `std.Repeat`, `std.Reminder` and the person's clock: the next occurrence of a
//! repeating item, the moment to remind of it, the words a row says it in, and the wall-clock
//! time a time of day names for the person.
//!
//! The values are their RFC 5545 text (see `tilolang::repeat`), expanded by the shared
//! `tilo-recurrence` crate, so a rule Calendar draws and one a task repeats by come from the same
//! reading. The zone is the person's: an occurrence keeps its wall time of day across a change of
//! clocks, and a day is theirs before it is UTC's (`crate::time` reads days and moments by it).
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone};

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::time::day_text;
use crate::value::Value;

impl Core {
    pub(crate) fn recurrence_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::RepeatNext => {
                let held = self.text(a.h(0)?)?;
                let repeat = Self::repeat(&held)?;
                let from = self.day(a.h(1)?)?;
                let done = self.day(a.h(2)?)?;
                let rule = Self::rule(&repeat, &held)?;
                // Counting from the due day, the next occurrence steps from the day it was due
                // (so two people completing the same item on that day agree on where it goes);
                // counting from completion, it steps from the day it was done.
                let (seed, after) = if repeat.from_done() {
                    (done, done)
                } else {
                    (from, from)
                };
                let Some(next) = rule.next_after(seed, after) else {
                    return trap(format!(
                        "`{held}` has no occurrence after {}: the rule has ended (its `UNTIL` or `COUNT`)",
                        day_text(after)
                    ));
                };
                self.ret_h(Value::Text(day_text(next)))
            }
            Imp::RepeatNextAt => {
                self.clock()?;
                let at = self.text(a.h(1)?)?;
                let day = self.day(a.h(2)?)?;
                let Some(instant) = tilolang::calendar::instant(&at) else {
                    return trap(format!("next_at: `{at}` is not a time"));
                };
                // The same wall time of day, on the day it is next due, read in the person's
                // zone: a 09:30 standup stays at 09:30 after the clocks change.
                let local = self.local_wall(instant);
                let time_of_day = local.time();
                let wall = day.and_time(time_of_day);
                self.ret_h(Value::Text(tilolang::calendar::time_text(
                    self.instant_of(wall),
                )))
            }
            Imp::WallClock => {
                self.clock()?;
                let text = self.text(a.h(0)?)?;
                self.ret_h(Value::Text(self.wall_clock(&text)?))
            }
            Imp::RepeatLabel => {
                let held = self.text(a.h(0)?)?;
                let repeat = Self::repeat(&held)?;
                // A schedule no phrase names reads as its rule text, which says as much.
                match repeat.label() {
                    Some(label) => self.ret_h(Value::Text(self.word_of(&label))),
                    None => self.ret_h(Value::Text(held)),
                }
            }
            Imp::ReminderAt => {
                self.clock()?;
                let held = self.text(a.h(0)?)?;
                let reminder = Self::reminder(&held)?;
                let due = self.day(a.h(1)?)?;
                let instant = match self.val(a.h(2)?)? {
                    Value::Opt(Some(at)) => {
                        let Value::Text(at) = *at else {
                            return trap("reminder_at: the time is not a time");
                        };
                        let Some(instant) = tilolang::calendar::instant(&at) else {
                            return trap(format!("reminder_at: `{at}` is not a time"));
                        };
                        instant.saturating_add(reminder.seconds())
                    }
                    // An item with no time of day is due at the start of its day, so `PT9H`
                    // comes at 09:00 and `-PT15M` a quarter of an hour before midnight.
                    _ => self
                        .instant_of(due.and_time(chrono::NaiveTime::MIN))
                        .saturating_add(reminder.seconds()),
                };
                self.ret_h(Value::Text(tilolang::calendar::time_text(instant)))
            }
            Imp::ReminderLabel => {
                let held = self.text(a.h(0)?)?;
                let reminder = Self::reminder(&held)?;
                let text = self.word_of(&reminder.label());
                self.ret_h(Value::Text(text))
            }
            _ => trap("not a repeat, reminder or clock import"),
        }
    }

    /// `std.Repeat` text, as held.
    fn repeat(held: &str) -> HResult<tilolang::repeat::Repeat> {
        tilolang::repeat::Repeat::parse(held).map_err(crate::engine::HostFail::Trap)
    }

    /// `std.Reminder` text, as held.
    fn reminder(held: &str) -> HResult<tilolang::repeat::Reminder> {
        tilolang::repeat::Reminder::parse(held).map_err(crate::engine::HostFail::Trap)
    }

    /// The rule a `std.Repeat` expands, shared with every other iCalendar reader.
    fn rule(repeat: &tilolang::repeat::Repeat, held: &str) -> HResult<tilo_recurrence::Rule> {
        tilo_recurrence::Rule::parse(repeat.rule())
            .ok_or_else(|| crate::engine::HostFail::Trap(format!("`{held}` is not a rule")))
    }

    /// The words a label is said in: the reader's catalogue's, or its source words.
    fn word_of(&self, label: &tilolang::repeat::Label) -> String {
        self.words
            .borrow()
            .word(label.id, &label.args, &label.source)
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
    fn zone(&self) -> Option<chrono_tz::Tz> {
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

    /// The moment `wall` names for the person: read in their zone, so an occurrence keeps its
    /// wall time of day; by their clock's offset where the host named no zone.
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
