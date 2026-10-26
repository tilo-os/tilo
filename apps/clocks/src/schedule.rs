//! When alarms and timers ring on this device.
//!
//! What rings is the person's [`Clocks`], the same on every
//! device. Whether this device has already rung it is not: each device keeps
//! its own delivery ledger ([`OnThisDevice`]), persisted across restarts and
//! never synced, so every device rings a deadline exactly once however often
//! the app is reopened, and no two devices race to write the same marker.
//! Wall time is passed in explicitly, which keeps suspend, clock
//! corrections, time zone changes and tests deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use ui::prelude::*;

use crate::model::{Alarm, Clocks, instant};

/// What Clocks keeps on this device only: which clocks float above other
/// windows here, and the delivery ledger of the alarms and timers this
/// device has rung or handed to the platform's scheduler.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OnThisDevice {
    /// Timers floating above other windows on this screen.
    #[serde(default)]
    pub floating_timers: BTreeSet<String>,
    #[serde(default)]
    pub floating_stopwatch: bool,
    #[serde(default)]
    pub delivery: Delivery,
}

impl SettingsSchema for OnThisDevice {
    const SCOPE: Scope = Scope::DeviceLocal;
}

/// The delivery ledger of one device.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    /// The next occurrence armed for each alarm, by alarm id.
    #[serde(default)]
    pub alarms: BTreeMap<String, ArmedAlarm>,
    /// The deadline this device last rang, by timer id.
    #[serde(default)]
    pub timers_rung: BTreeMap<String, i64>,
    /// The deadline the platform's scheduler accepted, by timer id: it shows
    /// that notification itself, so the app must not show it again.
    #[serde(default)]
    pub native_scheduled: BTreeMap<String, i64>,
    /// Timers whose platform notification this device may still have to
    /// cancel, kept until cancellation or delivery is confirmed even when
    /// the timer was deleted.
    #[serde(default)]
    pub native_owners: BTreeSet<String>,
    /// Alarm occurrences handed to the host, retained across app restarts.
    #[serde(default)]
    pub native_alarms_scheduled: BTreeMap<String, i64>,
    /// Alarm tags this device owns until cancellation is confirmed.
    #[serde(default)]
    pub native_alarm_owners: BTreeSet<String>,
    /// Completed host one-shot generations: automatic switch-off must not
    /// cancel a snooze retained by the host. Edits and deletion still cancel.
    #[serde(default)]
    pub native_completed_alarms: BTreeMap<String, (u8, u8, u8, i64)>,
    /// Alarm occurrences this device snoozed, by alarm id. Kept beside the
    /// alarm's ordinary armed occurrence so a one-shot alarm that already
    /// rang (and was turned off) can still ring once more.
    #[serde(default)]
    pub snoozes: BTreeMap<String, Snooze>,
}

/// One snoozed alarm occurrence on this device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snooze {
    /// When it rings again.
    pub due_ms: i64,
    /// The alarm settings it was snoozed under; an edit arms it afresh and
    /// cancels the snooze.
    pub schedule: (u8, u8, u8, i64),
}

/// One alarm's armed occurrence on this device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmedAlarm {
    /// The alarm settings the occurrence was computed for; a change on any
    /// device arms it afresh.
    pub schedule: (u8, u8, u8, i64),
    /// The occurrence, kept rather than only its time of day: reopening
    /// after midnight must still ring the alarm that was armed yesterday.
    pub next_due_ms: Option<i64>,
    /// The local date it last rang, so a clock moved backwards does not
    /// ring it twice.
    pub last_rung: Option<NaiveDate>,
}

pub(crate) fn schedule_of(alarm: &Alarm) -> (u8, u8, u8, i64) {
    (alarm.hour, alarm.minute, alarm.repeat, alarm.enabled_at_ms)
}

/// Snooze `alarm` on this device for `minutes`, keeping the settings it was
/// snoozed under so an edit cancels the snooze. A snooze is an occurrence of
/// its own: it rings once at `now_ms + minutes` even when the alarm has
/// already turned itself off.
///
/// # Errors
/// A snooze of no minutes.
pub fn snooze(
    delivery: &mut Delivery,
    id: &Id,
    alarm: &Alarm,
    now_ms: i64,
    minutes: u32,
) -> Result<(), String> {
    if minutes == 0 {
        return Err("a snooze needs at least one minute".into());
    }
    let millis = i64::from(minutes).saturating_mul(60_000);
    delivery.snoozes.insert(
        id.as_str().to_owned(),
        Snooze {
            due_ms: now_ms.saturating_add(millis),
            schedule: schedule_of(alarm),
        },
    );
    Ok(())
}

/// An alarm or timer that is due now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DueEvent {
    Alarm {
        id: Id,
        label: String,
        scheduled_for: DateTime<Utc>,
        native_scheduled: bool,
    },
    Timer {
        id: Id,
        label: String,
        /// The platform's scheduler shows this one itself.
        native_scheduled: bool,
    },
}

/// What one tick found: what rings, and the one-shot alarms that rang and
/// turn off (an edit of the person's clocks, made by the device that rang).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tick {
    pub due: Vec<DueEvent>,
    pub finished_alarms: Vec<Id>,
}

/// Ring what is due at `now_utc` (`now_local` on this device's clock), each
/// deadline once, and update `delivery` to match.
pub fn tick(
    clocks: &Clocks,
    delivery: &mut Delivery,
    now_utc: DateTime<Utc>,
    now_local: DateTime<Local>,
) -> Tick {
    let mut found = Tick::default();
    let now_ms = now_utc.timestamp_millis();
    let mut snoozed_due = Vec::new();
    delivery.snoozes.retain(|id, snooze| {
        let Some(alarm) = clocks.alarms.get(&Id::from_raw(id.as_str())) else {
            return false;
        };
        if schedule_of(alarm) != snooze.schedule {
            return false;
        }
        if snooze.due_ms > now_ms {
            return true;
        }
        snoozed_due.push((id.clone(), snooze.due_ms));
        false
    });
    for (id, due_ms) in snoozed_due {
        let id = Id::from_raw(id);
        let Some(alarm) = clocks.alarms.get(&id) else {
            continue;
        };
        found.due.push(DueEvent::Alarm {
            id,
            label: alarm.label.clone(),
            scheduled_for: instant(due_ms),
            native_scheduled: false,
        });
    }
    delivery
        .alarms
        .retain(|id, _| clocks.alarms.contains(&Id::from_raw(id.as_str())));
    for (id, alarm) in clocks.alarms.iter() {
        if !alarm.enabled {
            delivery.alarms.remove(id.as_str());
            continue;
        }
        let armed = delivery.alarms.entry(id.as_str().to_owned()).or_default();
        let rearmed = armed.schedule != schedule_of(alarm);
        if rearmed {
            *armed = ArmedAlarm {
                schedule: schedule_of(alarm),
                next_due_ms: None,
                last_rung: armed.last_rung,
            };
        }
        if !rearmed && alarm.repeat == 0 && armed.next_due_ms.is_none() && armed.last_rung.is_some()
        {
            continue;
        }
        if armed.next_due_ms.is_none_or(|deadline| deadline > now_ms) {
            // Resolve in local time, so future deadlines follow time zone
            // changes. A persisted deadline that has passed stays due, even
            // when the app was closed for several days.
            armed.next_due_ms = next_occurrence(alarm, armed.last_rung, now_local)
                .map(|deadline| deadline.timestamp_millis());
        }
        let Some(deadline) = armed
            .next_due_ms
            .filter(|deadline| *deadline <= now_ms && *deadline > alarm.enabled_at_ms)
        else {
            continue;
        };
        let scheduled_for = instant(deadline);
        let native_scheduled =
            delivery.native_alarms_scheduled.remove(id.as_str()) == Some(deadline);
        if alarm.repeat == 0 && native_scheduled {
            delivery
                .native_completed_alarms
                .insert(id.as_str().to_owned(), schedule_of(alarm));
        }
        found.due.push(DueEvent::Alarm {
            id: id.clone(),
            label: alarm.label.clone(),
            scheduled_for,
            native_scheduled,
        });
        armed.last_rung = Some(scheduled_for.with_timezone(&Local).date_naive());
        if alarm.repeat == 0 {
            armed.next_due_ms = None;
            found.finished_alarms.push(id.clone());
        } else {
            // Catch up once after suspend or exit, then arm the next future
            // occurrence instead of replaying the missed days.
            armed.next_due_ms = next_occurrence(alarm, armed.last_rung, now_local)
                .map(|deadline| deadline.timestamp_millis());
        }
    }
    delivery
        .timers_rung
        .retain(|id, _| clocks.timers.contains(&Id::from_raw(id.as_str())));
    for (id, timer) in clocks.timers.iter() {
        let Some(end) = timer.ends_at_ms.filter(|end| *end <= now_ms) else {
            continue;
        };
        if delivery.timers_rung.get(id.as_str()) == Some(&end) {
            continue;
        }
        delivery.timers_rung.insert(id.as_str().to_owned(), end);
        let native_scheduled = delivery.native_scheduled.remove(id.as_str()) == Some(end);
        found.due.push(DueEvent::Timer {
            id: id.clone(),
            label: timer.label.clone(),
            native_scheduled,
        });
    }
    found
}

/// The next occurrence of `alarm` after `after`, on this device's clock.
/// Missing spring-forward times advance minute by minute; an ambiguous
/// autumn time takes the first.
#[must_use]
pub fn next_occurrence(
    alarm: &Alarm,
    last_rung: Option<NaiveDate>,
    after: DateTime<Local>,
) -> Option<DateTime<Local>> {
    next_occurrence_in(alarm, last_rung, &after)
}

pub(crate) fn next_occurrence_in<Tz: TimeZone>(
    alarm: &Alarm,
    last_rung: Option<NaiveDate>,
    after: &DateTime<Tz>,
) -> Option<DateTime<Tz>> {
    if !alarm.enabled {
        return None;
    }
    ui::notify::CalendarAlarm {
        hour: alarm.hour,
        minute: alarm.minute,
        weekdays: alarm.repeat,
    }
    .next_in(last_rung, after)
}

/// The clock the shell shows above other windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellActivity {
    /// The timer's id; `None` is the stopwatch.
    pub activity_id: Option<String>,
    pub label: String,
    pub remaining: Duration,
    pub overlay: bool,
    pub stopwatch: bool,
    pub paused: bool,
}

/// What the shell shows at `now`: the stopwatch while it floats, else the
/// floating timer, else the running timer that ends first.
#[must_use]
pub fn shell_activity(
    clocks: &Clocks,
    device: &OnThisDevice,
    now: DateTime<Utc>,
) -> Option<ShellActivity> {
    let stopwatch = &clocks.stopwatch;
    if device.floating_stopwatch && (stopwatch.is_running() || stopwatch.accumulated_ms > 0) {
        return Some(ShellActivity {
            activity_id: None,
            label: "Stopwatch".into(),
            remaining: stopwatch.elapsed(now),
            overlay: true,
            stopwatch: true,
            paused: !stopwatch.is_running(),
        });
    }
    clocks
        .timers
        .iter()
        .map(|(id, timer)| (id, timer, device.floating_timers.contains(id.as_str())))
        .filter(|(_, timer, floating)| timer.is_running(now) || *floating)
        // A timer the person floated owns the controller, even if some
        // unrelated timer ends sooner. Within each class the most urgent
        // timer wins.
        .min_by_key(|(_, timer, floating)| (!floating, timer.remaining(now)))
        .map(|(id, timer, floating)| ShellActivity {
            activity_id: Some(id.as_str().to_owned()),
            label: if timer.label.is_empty() {
                "Timer".into()
            } else {
                timer.label.clone()
            },
            remaining: timer.remaining(now),
            overlay: floating,
            stopwatch: false,
            paused: !timer.is_running(now),
        })
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
