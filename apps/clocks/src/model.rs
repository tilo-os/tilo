//! What a person keeps in Clocks: the cities they compare, their alarms,
//! their timers and the stopwatch. It is the app's resource, so it follows
//! them to every device and every edit is an action: the palette, an agent
//! or a script sets an alarm exactly as the buttons do. Names, alarm times
//! and repeat days are field edits at their paths; everything that runs
//! against the clock (turning an alarm on, starting and pausing timers and
//! the stopwatch) is a verb, which reads the action's own time, and the
//! instants those verbs keep are read-only to field edits.
//!
//! Running clocks are stored as absolute instants (when a timer ends, when
//! the stopwatch started), taken from the action's own time, so a timer
//! started on one device counts down to the same moment on every other.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{DateTime, NaiveTime, Utc, Weekday};
use serde::{Deserialize, Serialize};
use ui::prelude::*;

/// A city whose time is shown beside the local one.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = WorldClock::valid)]
pub struct WorldClock {
    /// City
    #[model(label)]
    pub city: String,
    /// The IANA time zone, such as `Asia/Tokyo`.
    #[model(readonly)]
    pub timezone: Atom,
}

/// An alarm at a time of day, once or on chosen weekdays.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Alarm::valid)]
pub struct Alarm {
    /// Label
    #[model(label)]
    pub label: String,
    /// Hour
    pub hour: u8,
    /// Minute
    pub minute: u8,
    /// The weekdays it repeats on, Monday as bit 0; none rings once.
    pub repeat: u8,
    /// On: turning it on is a verb, which notes when.
    #[model(readonly)]
    pub enabled: bool,
    /// When it was last turned on, so a deadline from before that (a device
    /// that was closed while it was turned off and on again) never rings.
    #[model(readonly)]
    pub enabled_at_ms: i64,
}

/// A countdown. Running while `ends_at_ms` lies ahead; finished once it has
/// passed; paused while `paused_remaining_ms` holds what is left.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Timer::valid)]
pub struct Timer {
    /// Label
    #[model(label)]
    pub label: String,
    #[model(readonly)]
    pub duration_ms: u64,
    #[model(readonly)]
    pub ends_at_ms: Option<i64>,
    #[model(readonly)]
    pub paused_remaining_ms: Option<u64>,
}

/// The stopwatch: time accumulated before the current run, when the current
/// run started, and the laps recorded (each the total elapsed time).
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Stopwatch {
    pub accumulated_ms: u64,
    pub started_at_ms: Option<i64>,
    pub laps_ms: Vec<u64>,
}

/// Everything Clocks keeps for a person.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Clocks {
    pub world_clocks: Collection<WorldClock>,
    pub alarms: Collection<Alarm>,
    pub timers: Collection<Timer>,
    /// Only its verbs run the stopwatch.
    #[model(readonly)]
    pub stopwatch: Stopwatch,
}

/// A label shows on one line: it is not blank and at most 80 characters.
fn valid_label(label: &str) -> Result<(), Refusal> {
    if label.trim().is_empty() || label.chars().count() > 80 {
        return Err(Refusal::from("a label has 1 to 80 characters"));
    }
    Ok(())
}

impl WorldClock {
    fn valid(&self) -> Result<(), Refusal> {
        valid_label(&self.city)?;
        if self.timezone.0.parse::<chrono_tz::Tz>().is_err() {
            return Err(Refusal::new(format!(
                "`{}` is not a time zone",
                self.timezone.0
            )));
        }
        Ok(())
    }
}

impl Timer {
    fn valid(&self) -> Result<(), Refusal> {
        valid_label(&self.label)
    }
}

impl Alarm {
    /// A time of day from 00:00 to 23:59, repeating on weekdays only.
    fn valid(&self) -> Result<(), Refusal> {
        valid_label(&self.label)?;
        if self.hour > 23 || self.minute > 59 {
            return Err(Refusal::from("an alarm needs a time from 00:00 to 23:59"));
        }
        if self.repeat > 0x7f {
            return Err(Refusal::from("an alarm repeats on weekdays only"));
        }
        Ok(())
    }
}

/// A weekday an alarm repeats on.
#[derive(
    Clone,
    Copy,
    Debug,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    ui::tilo_resource::JsonSchema,
)]
pub enum WeekdayKey {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl WeekdayKey {
    /// Every weekday, Monday first.
    pub const ALL: [Self; 7] = [
        Self::Mon,
        Self::Tue,
        Self::Wed,
        Self::Thu,
        Self::Fri,
        Self::Sat,
        Self::Sun,
    ];

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

impl From<Weekday> for WeekdayKey {
    fn from(day: Weekday) -> Self {
        match day {
            Weekday::Mon => Self::Mon,
            Weekday::Tue => Self::Tue,
            Weekday::Wed => Self::Wed,
            Weekday::Thu => Self::Thu,
            Weekday::Fri => Self::Fri,
            Weekday::Sat => Self::Sat,
            Weekday::Sun => Self::Sun,
        }
    }
}

/// The repeat mask of `days`.
#[must_use]
pub fn repeat_mask(days: &BTreeSet<WeekdayKey>) -> u8 {
    days.iter().fold(0, |mask, day| mask | day.bit())
}

/// The instant `ms` milliseconds after the Unix epoch.
#[must_use]
pub fn instant(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn after(now_ms: i64, ms: u64) -> i64 {
    now_ms.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX))
}

impl Alarm {
    /// The time of day it rings.
    #[must_use]
    pub fn local_time(&self) -> NaiveTime {
        NaiveTime::from_hms_opt(self.hour.into(), self.minute.into(), 0).unwrap_or_default()
    }

    /// The weekdays it repeats on; empty when it rings once.
    #[must_use]
    pub fn weekdays(&self) -> BTreeSet<WeekdayKey> {
        WeekdayKey::ALL
            .into_iter()
            .filter(|day| self.repeat & day.bit() != 0)
            .collect()
    }

    /// Whether it rings on `day`.
    #[must_use]
    pub fn repeats_on(&self, day: WeekdayKey) -> bool {
        self.repeat & day.bit() != 0
    }
}

impl Timer {
    /// Whether it is counting down at `now`.
    #[must_use]
    pub fn is_running(&self, now: DateTime<Utc>) -> bool {
        self.ends_at_ms
            .is_some_and(|end| end > now.timestamp_millis())
    }

    /// Whether its deadline has passed.
    #[must_use]
    pub fn is_finished(&self, now: DateTime<Utc>) -> bool {
        self.ends_at_ms
            .is_some_and(|end| end <= now.timestamp_millis())
    }

    /// What is left at `now`.
    #[must_use]
    pub fn remaining(&self, now: DateTime<Utc>) -> Duration {
        if let Some(ms) = self.paused_remaining_ms {
            return Duration::from_millis(ms);
        }
        self.ends_at_ms
            .and_then(|end| instant(end).signed_duration_since(now).to_std().ok())
            .unwrap_or_default()
    }

    /// When it ends, while it has a deadline.
    #[must_use]
    pub fn ends_at(&self) -> Option<DateTime<Utc>> {
        self.ends_at_ms.map(instant)
    }

    fn start(&mut self, now_ms: i64, left_ms: u64) {
        self.paused_remaining_ms = None;
        self.ends_at_ms = Some(after(now_ms, left_ms));
    }
}

impl Stopwatch {
    /// The time elapsed at `now`.
    #[must_use]
    pub fn elapsed(&self, now: DateTime<Utc>) -> Duration {
        let running = self
            .started_at_ms
            .and_then(|start| now.signed_duration_since(instant(start)).to_std().ok())
            .unwrap_or_default();
        Duration::from_millis(self.accumulated_ms).saturating_add(running)
    }

    /// Whether it is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.started_at_ms.is_some()
    }
}

fn timer_mut<'a>(clocks: &'a mut Clocks, timer: &Id) -> Result<&'a mut Timer, Refusal> {
    clocks
        .timers
        .get_mut(timer)
        .ok_or_else(|| Refusal::from("there is no such timer"))
}

fn alarm_mut<'a>(clocks: &'a mut Clocks, alarm: &Id) -> Result<&'a mut Alarm, Refusal> {
    clocks
        .alarms
        .get_mut(alarm)
        .ok_or_else(|| Refusal::from("there is no such alarm"))
}

/// A label of at most 80 characters, or `fallback` when it is blank.
#[must_use]
pub fn bounded_label(input: &str, fallback: &str) -> String {
    let label = input.trim();
    if label.is_empty() {
        fallback.into()
    } else {
        label.chars().take(80).collect()
    }
}

#[resource(kind = "clocks.collection", label = "Clocks")]
impl Clocks {
    /// Add a city.
    #[action]
    pub fn add_world_clock(
        &mut self,
        cx: &mut ActionCx,
        #[param("City name")] city: String,
        #[param("IANA time zone, such as Asia/Tokyo")] timezone: String,
    ) -> Result<(), Refusal> {
        // A city already shown stays where it is.
        if self
            .world_clocks
            .values()
            .any(|clock| clock.timezone.0 == timezone)
        {
            return Ok(());
        }
        let city = bounded_label(&city, timezone.rsplit('/').next().unwrap_or(&timezone));
        self.world_clocks.push(
            cx.id(),
            WorldClock {
                city,
                timezone: Atom(timezone),
            },
        );
        Ok(())
    }

    /// Remove city.
    #[action(item = world_clocks)]
    pub fn remove_world_clock(&mut self, world_clock: Id) -> Result<(), Refusal> {
        self.world_clocks
            .remove(&world_clock)
            .map(drop)
            .ok_or_else(|| Refusal::from("there is no such city"))
    }

    /// Add an alarm.
    #[action]
    pub fn add_alarm(
        &mut self,
        cx: &mut ActionCx,
        #[param("What the alarm is for")] label: String,
        #[param("Hour, 0 to 23")] hour: u8,
        #[param("Minute, 0 to 59")] minute: u8,
        #[param("Weekdays it repeats on (Mon … Sun); none rings once")] weekdays: Vec<WeekdayKey>,
    ) -> Result<(), Refusal> {
        let now = cx.now_ms();
        self.alarms.push(
            cx.id(),
            Alarm {
                label: bounded_label(&label, "Alarm"),
                hour,
                minute,
                repeat: repeat_mask(&weekdays.into_iter().collect()),
                enabled: true,
                enabled_at_ms: now,
            },
        );
        Ok(())
    }

    /// Turn an alarm on or off.
    #[action(item = alarms)]
    pub fn set_alarm_enabled(
        &mut self,
        cx: &mut ActionCx,
        alarm: Id,
        #[param("On")] enabled: bool,
    ) -> Result<(), Refusal> {
        let now = cx.now_ms();
        let alarm = alarm_mut(self, &alarm)?;
        if enabled && !alarm.enabled {
            alarm.enabled_at_ms = now;
        }
        alarm.enabled = enabled;
        Ok(())
    }

    /// Remove alarm.
    #[action(item = alarms)]
    pub fn remove_alarm(&mut self, alarm: Id) -> Result<(), Refusal> {
        self.alarms
            .remove(&alarm)
            .map(drop)
            .ok_or_else(|| Refusal::from("there is no such alarm"))
    }

    /// Start a timer.
    #[action]
    pub fn add_timer(
        &mut self,
        cx: &mut ActionCx,
        #[param("What the timer is for")] label: String,
        #[param("Duration in seconds")] seconds: u64,
    ) -> Result<(), Refusal> {
        if seconds == 0 {
            return Err(Refusal::from("a timer needs a duration greater than zero"));
        }
        let duration_ms = millis(Duration::from_secs(seconds));
        let now = cx.now_ms();
        let mut timer = Timer {
            label: bounded_label(&label, "Timer"),
            duration_ms,
            ends_at_ms: None,
            paused_remaining_ms: None,
        };
        timer.start(now, duration_ms);
        self.timers.push(cx.id(), timer);
        Ok(())
    }

    /// Pause timer.
    #[action(item = timers)]
    pub fn pause_timer(&mut self, cx: &mut ActionCx, timer: Id) -> Result<(), Refusal> {
        let now = instant(cx.now_ms());
        let timer = timer_mut(self, &timer)?;
        if timer.is_running(now) {
            timer.paused_remaining_ms = Some(millis(timer.remaining(now)));
            timer.ends_at_ms = None;
        }
        Ok(())
    }

    /// Resume timer. A finished timer starts over.
    #[action(item = timers)]
    pub fn resume_timer(&mut self, cx: &mut ActionCx, timer: Id) -> Result<(), Refusal> {
        let now_ms = cx.now_ms();
        let timer = timer_mut(self, &timer)?;
        if timer.is_running(instant(now_ms)) {
            return Ok(());
        }
        let left = timer
            .paused_remaining_ms
            .filter(|left| *left > 0)
            .unwrap_or(timer.duration_ms);
        timer.start(now_ms, left);
        Ok(())
    }

    /// Add one minute.
    #[action(item = timers)]
    pub fn add_minute(&mut self, cx: &mut ActionCx, timer: Id) -> Result<(), Refusal> {
        let now_ms = cx.now_ms();
        let timer = timer_mut(self, &timer)?;
        let left = timer
            .remaining(instant(now_ms))
            .saturating_add(Duration::from_secs(60));
        timer.start(now_ms, millis(left));
        Ok(())
    }

    /// Remove timer.
    #[action(item = timers)]
    pub fn remove_timer(&mut self, timer: Id) -> Result<(), Refusal> {
        self.timers
            .remove(&timer)
            .map(drop)
            .ok_or_else(|| Refusal::from("there is no such timer"))
    }

    /// Start the stopwatch.
    #[action]
    pub fn start_stopwatch(&mut self, cx: &mut ActionCx) {
        if self.stopwatch.started_at_ms.is_none() {
            self.stopwatch.started_at_ms = Some(cx.now_ms());
        }
    }

    /// Pause the stopwatch.
    #[action]
    pub fn pause_stopwatch(&mut self, cx: &mut ActionCx) {
        if self.stopwatch.is_running() {
            self.stopwatch.accumulated_ms = millis(self.stopwatch.elapsed(instant(cx.now_ms())));
            self.stopwatch.started_at_ms = None;
        }
    }

    /// Record lap.
    #[action]
    pub fn lap(&mut self, cx: &mut ActionCx) {
        if self.stopwatch.is_running() {
            let elapsed = millis(self.stopwatch.elapsed(instant(cx.now_ms())));
            self.stopwatch.laps_ms.push(elapsed);
        }
    }

    /// Reset the stopwatch.
    #[action]
    pub fn reset_stopwatch(&mut self) {
        self.stopwatch = Stopwatch::default();
    }
}
