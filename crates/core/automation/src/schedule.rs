//! When a trigger on the clock fires, worked out the same everywhere: a
//! person's device and a workspace's host read the same fire times from the
//! same spec, so a schedule moved between them keeps its times, and a moment
//! is answered once by whichever keeps it.

use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Span, Timestamp};

use crate::shortcut::{CatchUp, Schedule, TriggerSpec, Weekday};

/// A fire time this far in the past counts as missed rather than on time.
pub const MISSED_AFTER_MS: i64 = 90_000;

/// [`MISSED_AFTER_MS`] as a duration.
pub const MISSED_AFTER: SignedDuration = SignedDuration::from_millis(MISSED_AFTER_MS);

/// The shortest interval a schedule repeats in, however short the spec says:
/// a shortcut firing more often than this is a loop, not a schedule.
pub const MIN_INTERVAL_SECONDS: u64 = 60;

/// The first fire time of `schedule` strictly after `now`. An interval counts
/// from the Unix epoch, so every device computes the same fire times whenever
/// it armed the schedule; a calendar time is a wall-clock time in its own
/// zone, else in `zone`.
#[must_use]
pub fn next_fire_time(schedule: &Schedule, now: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
    match schedule {
        Schedule::Interval { seconds } => {
            let period = i64::try_from((*seconds).max(MIN_INTERVAL_SECONDS)).ok()?;
            let periods = now.as_second().div_euclid(period).checked_add(1)?;
            Timestamp::from_second(period.checked_mul(periods)?).ok()
        }
        Schedule::Calendar {
            at,
            weekdays,
            zone: named,
        } => {
            let named = named.as_deref().and_then(zone_named);
            let zone = named.as_ref().unwrap_or(zone);
            let time = Time::new(
                i8::try_from(at.hour).ok()?,
                i8::try_from(at.minute).ok()?,
                0,
                0,
            )
            .ok()?;
            let today = now.to_zoned(zone.clone()).date();
            (0..=8_i32)
                .filter_map(|day| today.checked_add(Span::new().days(day)).ok())
                .filter(|date| weekdays.is_empty() || weekdays.contains(&weekday_of(*date)))
                .filter_map(|date| local_instant(date, time, zone))
                .find(|candidate| *candidate > now)
        }
    }
}

/// The zone the device's time-zone database names `name`: the runtime every app maps carries
/// no copy of its own, so the zone is read from the one the system ships.
#[must_use]
pub fn zone_named(name: &str) -> Option<TimeZone> {
    jiff::tz::db().get(name).ok()
}

/// Where `trigger` keeps the zone it is still to be given: a time of day on
/// a schedule that names no zone. A schedule saved for a person fires on their
/// clock, so whoever saves it fills this with theirs.
pub fn unzoned(trigger: &mut TriggerSpec) -> Option<&mut Option<String>> {
    match trigger {
        TriggerSpec::Schedule {
            schedule: Schedule::Calendar { zone, .. },
            ..
        } if zone.is_none() => Some(zone),
        _ => None,
    }
}

/// A trigger that fires by the clock, read the same by whoever keeps its
/// time: a schedule's fire times one after another, or one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timed<'a> {
    /// A schedule, and what it does about a fire time it missed.
    Repeating {
        schedule: &'a Schedule,
        catch_up: CatchUp,
    },
    /// One moment, ms since the epoch.
    Once { at_ms: i64 },
}

impl<'a> Timed<'a> {
    /// How `trigger` fires by the clock; `None` for a trigger that waits for
    /// something else.
    #[must_use]
    pub const fn of(trigger: &'a TriggerSpec) -> Option<Self> {
        match trigger {
            TriggerSpec::Schedule { schedule, catch_up } => Some(Self::Repeating {
                schedule,
                catch_up: *catch_up,
            }),
            TriggerSpec::Once { at_ms } => Some(Self::Once { at_ms: *at_ms }),
            _ => None,
        }
    }

    /// The fire time to wait for when the trigger is first seen at `now`, ms
    /// since the epoch. A schedule answers what comes after it was armed: its
    /// first time after `now`. A moment is itself, also when it has passed
    /// already, so one set while its keeper was down still fires.
    #[must_use]
    pub fn first_ms(&self, now: Timestamp, zone: &TimeZone) -> Option<i64> {
        match self {
            Self::Repeating { schedule, .. } => {
                next_fire_time(schedule, now, zone).map(Timestamp::as_millisecond)
            }
            Self::Once { at_ms } => Some(*at_ms),
        }
    }

    /// The fire time to wait for once the one before it was answered at
    /// `now`: a schedule's next after `now`, so the times missed in between
    /// are answered once between them; none for a moment, which is spent.
    #[must_use]
    pub fn following_ms(&self, now: Timestamp, zone: &TimeZone) -> Option<i64> {
        match self {
            Self::Repeating { schedule, .. } => {
                next_fire_time(schedule, now, zone).map(Timestamp::as_millisecond)
            }
            Self::Once { .. } => None,
        }
    }

    /// The fire time to wait for while `answered_ms` is the last fire time
    /// any keeper answered, ms since the epoch: a schedule's first time after
    /// it, and a moment's own time while it has not been answered yet. `None`
    /// when there is nothing more to wait for.
    #[must_use]
    pub fn waiting_ms(&self, answered_ms: i64, zone: &TimeZone) -> Option<i64> {
        match self {
            Self::Repeating { schedule, .. } => {
                let from = Timestamp::from_millisecond(answered_ms).ok()?;
                next_fire_time(schedule, from, zone).map(Timestamp::as_millisecond)
            }
            Self::Once { at_ms } => (answered_ms < *at_ms).then_some(*at_ms),
        }
    }

    /// Whether a fire time answered `late_ms` after it came still runs. A
    /// schedule's does when it is on time or the trigger catches up; a
    /// moment's always does, since it comes once.
    #[must_use]
    pub const fn runs(&self, late_ms: i64) -> bool {
        match self {
            Self::Repeating { catch_up, .. } => {
                late_ms <= MISSED_AFTER_MS || matches!(catch_up, CatchUp::RunOnce)
            }
            Self::Once { .. } => true,
        }
    }
}

/// What one key holder should do about a trigger on the clock now, given
/// the last fire time any key holder answered: the progress kept in the
/// workspace, not in one host's file, so whichever key holder takes the
/// trigger up next carries on where the last one stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulePlan {
    /// No fire time has come: keep `answered_ms` as the progress. A trigger
    /// seen for the first time stores the moment it was seen.
    Waiting { answered_ms: i64 },
    /// `due_ms` has come: fire it once — on time, once for however many were
    /// missed by a trigger that catches up, or a moment however late — then
    /// keep `answered_ms` as the progress.
    Fire {
        due_ms: i64,
        answered_ms: i64,
        missed: bool,
    },
}

/// What `timed` asks of a key holder at `now_ms`, `answered_due_ms` being the
/// last fire time any key holder answered, or `None` if none has.
///
/// A fire time missed while every key holder was down runs once when the
/// trigger catches up and not at all when it skips, never once per period: the
/// progress moves to `now_ms`, so the next fire time is the first after it
/// however many periods passed. A moment that passed while every key holder
/// was down fires once, however late, and never again.
#[must_use]
pub fn plan(
    timed: Timed<'_>,
    answered_due_ms: Option<i64>,
    now_ms: i64,
    zone: &TimeZone,
) -> SchedulePlan {
    match timed {
        Timed::Once { at_ms } => {
            // An answer at or past the moment is the moment spent. Seen
            // before it comes, none fires; seen for the first time after it
            // passed, it fires in this same pass.
            let spent = answered_due_ms.is_some_and(|answered| answered >= at_ms);
            if spent {
                return SchedulePlan::Waiting { answered_ms: at_ms };
            }
            if at_ms > now_ms {
                return SchedulePlan::Waiting {
                    answered_ms: answered_due_ms.unwrap_or(now_ms),
                };
            }
            SchedulePlan::Fire {
                due_ms: at_ms,
                answered_ms: at_ms,
                missed: now_ms.saturating_sub(at_ms) > MISSED_AFTER_MS,
            }
        }
        Timed::Repeating { schedule, catch_up } => {
            let Some(answered) = answered_due_ms else {
                return SchedulePlan::Waiting {
                    answered_ms: now_ms,
                };
            };
            let from = Timestamp::from_millisecond(answered).unwrap_or(Timestamp::UNIX_EPOCH);
            let Some(due) = next_fire_time(schedule, from, zone) else {
                return SchedulePlan::Waiting {
                    answered_ms: answered,
                };
            };
            let due_ms = due.as_millisecond();
            if due_ms > now_ms {
                return SchedulePlan::Waiting {
                    answered_ms: answered,
                };
            }
            let missed = now_ms.saturating_sub(due_ms) > MISSED_AFTER_MS;
            if missed && catch_up != CatchUp::RunOnce {
                return SchedulePlan::Waiting {
                    answered_ms: now_ms,
                };
            }
            SchedulePlan::Fire {
                due_ms,
                answered_ms: now_ms.max(due_ms),
                missed,
            }
        }
    }
}

/// The instant `time` on `date` is in `zone`; none when the zone skips or repeats it.
fn local_instant(date: Date, time: Time, zone: &TimeZone) -> Option<Timestamp> {
    zone.to_ambiguous_timestamp(date.to_datetime(time))
        .unambiguous()
        .ok()
}

fn weekday_of(date: Date) -> Weekday {
    match date.weekday() {
        jiff::civil::Weekday::Monday => Weekday::Monday,
        jiff::civil::Weekday::Tuesday => Weekday::Tuesday,
        jiff::civil::Weekday::Wednesday => Weekday::Wednesday,
        jiff::civil::Weekday::Thursday => Weekday::Thursday,
        jiff::civil::Weekday::Friday => Weekday::Friday,
        jiff::civil::Weekday::Saturday => Weekday::Saturday,
        jiff::civil::Weekday::Sunday => Weekday::Sunday,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_trigger_on_the_clock_reads_the_same_for_whoever_keeps_its_time;

        phase a_schedule_starts_after_it_is_armed_and_a_moment_at_itself_even_when_it_passed;
        {
            let zone = TimeZone::UTC;
            let now = Timestamp::from_millisecond(3_600_001).unwrap();
            let every_minute = TriggerSpec::Schedule {
                schedule: Schedule::Interval { seconds: 60 },
                catch_up: CatchUp::Skip,
            };
            let schedule = Timed::of(&every_minute);
            assert_eq!(
                schedule.and_then(|timed| timed.first_ms(now, &zone)),
                Some(3_660_000)
            );
            assert_eq!(
                schedule.and_then(|timed| timed.following_ms(now, &zone)),
                Some(3_660_000)
            );
            let passed = TriggerSpec::Once { at_ms: 1_000 };
            let moment = Timed::of(&passed);
            assert_eq!(moment, Some(Timed::Once { at_ms: 1_000 }));
            assert_eq!(
                moment.and_then(|timed| timed.first_ms(now, &zone)),
                Some(1_000),
                "a moment that passed is still waited for, so it fires"
            );
            assert_eq!(
                moment.and_then(|timed| timed.following_ms(now, &zone)),
                None,
                "an answered moment is spent"
            );
            let opened = TriggerSpec::AppOpened {
                app_id: "so.tilo.notes".into(),
            };
            assert_eq!(Timed::of(&opened), None, "an app opening is no time");
        }

        phase a_late_schedule_runs_only_when_it_catches_up_and_a_late_moment_always;
        {
            let schedule = Schedule::Interval { seconds: 60 };
            let skips = Timed::Repeating {
                schedule: &schedule,
                catch_up: CatchUp::Skip,
            };
            let catches_up = Timed::Repeating {
                schedule: &schedule,
                catch_up: CatchUp::RunOnce,
            };
            let late = MISSED_AFTER_MS.saturating_add(1);
            assert!(skips.runs(MISSED_AFTER_MS), "on time");
            assert!(!skips.runs(late), "missed, and let go");
            assert!(catches_up.runs(late), "missed, and run once");
            assert!(Timed::Once { at_ms: 1_000 }.runs(i64::MAX), "a moment comes once");
        }
    }
}

#[cfg(test)]
mod plan_tests;
