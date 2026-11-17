//! When a trigger on the clock fires, worked out the same everywhere: a
//! person's device and a workspace's host read the same fire times from the
//! same spec, so a schedule moved between them keeps its times, and a moment
//! is answered once by whichever keeps it.

use chrono::{DateTime, Datelike as _, FixedOffset, NaiveDate, NaiveTime, TimeZone as _, Utc};

use crate::shortcut::{CatchUp, Schedule, TriggerSpec, Weekday};

/// A fire time this far in the past counts as missed rather than on time.
pub const MISSED_AFTER_MS: i64 = 90_000;

/// [`MISSED_AFTER_MS`] as a duration.
pub const MISSED_AFTER: chrono::Duration = chrono::Duration::milliseconds(MISSED_AFTER_MS);

/// The shortest interval a schedule repeats in, however short the spec says:
/// a shortcut firing more often than this is a loop, not a schedule.
pub const MIN_INTERVAL_SECONDS: u64 = 60;

/// The wall-clock offset the local time of a calendar schedule is read in, at
/// a given instant.
pub type Zone<'a> = dyn Fn(DateTime<Utc>) -> FixedOffset + 'a;

/// The first fire time of `schedule` strictly after `now`. An interval counts
/// from the Unix epoch, so every device computes the same fire times whenever
/// it armed the schedule; a calendar time is a wall-clock time in `zone`.
#[must_use]
pub fn next_fire_time(
    schedule: &Schedule,
    now: DateTime<Utc>,
    zone: &Zone<'_>,
) -> Option<DateTime<Utc>> {
    match schedule {
        Schedule::Interval { seconds } => {
            let period = i64::try_from((*seconds).max(MIN_INTERVAL_SECONDS)).ok()?;
            let periods = now.timestamp().div_euclid(period).checked_add(1)?;
            DateTime::from_timestamp(period.checked_mul(periods)?, 0)
        }
        Schedule::Calendar { at, weekdays } => {
            let time = NaiveTime::from_hms_opt(u32::from(at.hour), u32::from(at.minute), 0)?;
            let today = now.with_timezone(&zone(now)).date_naive();
            (0..=8_i64)
                .filter_map(|day| today.checked_add_signed(chrono::Duration::days(day)))
                .filter(|date| weekdays.is_empty() || weekdays.contains(&weekday_of(*date)))
                .filter_map(|date| local_instant(date, time, zone))
                .find(|candidate| *candidate > now)
        }
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
    pub fn first_ms(&self, now: DateTime<Utc>, zone: &Zone<'_>) -> Option<i64> {
        match self {
            Self::Repeating { schedule, .. } => {
                next_fire_time(schedule, now, zone).map(|at| at.timestamp_millis())
            }
            Self::Once { at_ms } => Some(*at_ms),
        }
    }

    /// The fire time to wait for once the one before it was answered at
    /// `now`: a schedule's next after `now`, so the times missed in between
    /// are answered once between them; none for a moment, which is spent.
    #[must_use]
    pub fn following_ms(&self, now: DateTime<Utc>, zone: &Zone<'_>) -> Option<i64> {
        match self {
            Self::Repeating { schedule, .. } => {
                next_fire_time(schedule, now, zone).map(|at| at.timestamp_millis())
            }
            Self::Once { .. } => None,
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

fn local_instant(date: NaiveDate, time: NaiveTime, zone: &Zone<'_>) -> Option<DateTime<Utc>> {
    let naive = date.and_time(time);
    // Read the offset at a first guess of the instant, so a change of offset between now and then
    // (daylight saving) is taken into account.
    let first_offset = zone(naive.and_utc()).local_minus_utc();
    let guess = naive
        .and_utc()
        .checked_sub_signed(chrono::Duration::seconds(i64::from(first_offset)))?;
    let offset = zone(guess);
    offset
        .from_local_datetime(&naive)
        .single()
        .map(|local| local.with_timezone(&Utc))
}

fn weekday_of(date: NaiveDate) -> Weekday {
    match date.weekday() {
        chrono::Weekday::Mon => Weekday::Monday,
        chrono::Weekday::Tue => Weekday::Tuesday,
        chrono::Weekday::Wed => Weekday::Wednesday,
        chrono::Weekday::Thu => Weekday::Thursday,
        chrono::Weekday::Fri => Weekday::Friday,
        chrono::Weekday::Sat => Weekday::Saturday,
        chrono::Weekday::Sun => Weekday::Sunday,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Offset as _;

    use super::*;

    tilo_test_env::scenario! {
        fn a_trigger_on_the_clock_reads_the_same_for_whoever_keeps_its_time;

        phase a_schedule_starts_after_it_is_armed_and_a_moment_at_itself_even_when_it_passed;
        {
            let zone = |_: DateTime<Utc>| Utc.fix();
            let now = DateTime::from_timestamp_millis(3_600_001).unwrap_or_default();
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
