//! When alarms and timers ring on this device. What rings is the person's
//! [`Clocks`], the same on every device; whether this device has rung it is not,
//! so each keeps its own delivery ledger in the [`ClocksOnDevices`] record beside
//! the clocks and writes only its own entry. Wall time is passed in, so suspend,
//! clock corrections and tests are deterministic.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use ui::prelude::*;

use crate::model::{Alarm, Clocks, instant};

/// What Clocks keeps for one device alone: the clocks floating there, and the
/// ledger of what it has rung or handed to the platform.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OnThisDevice {
    #[serde(default)]
    pub floating_timers: BTreeSet<String>,
    #[serde(default)]
    pub floating_stopwatch: bool,
    #[serde(default)]
    pub delivery: Delivery,
}

tilo_resource::json_model!(OnThisDevice);

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct ClocksOnDevices {
    pub devices: BTreeMap<String, OnThisDevice>,
}

impl ClocksOnDevices {
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "clocks.on-devices",
    read_content = ClocksOnDevices::read_document
)]
impl ClocksOnDevices {
    /// What `device` keeps for itself is now `kept`.
    #[action(housekeeping)]
    pub fn keep(
        &mut self,
        device: String,
        #[param("What the device keeps, as it stores it")] kept: serde_json::Value,
    ) -> Result<(), Refusal> {
        let kept = serde_json::from_value(kept)
            .map_err(|error| Refusal::new(format!("not what a device keeps: {error}")))?;
        self.devices.insert(device, kept);
        Ok(())
    }
}

impl ClocksOnDevices {
    /// What `device` keeps for itself.
    #[must_use]
    pub fn on(&self, device: &str) -> OnThisDevice {
        self.devices.get(device).cloned().unwrap_or_default()
    }
}

/// This device's entry in the [`ClocksOnDevices`] record, or memory where there
/// is no record (a capture, a past version).
#[derive(Clone)]
pub(crate) struct DeviceChoices {
    record: Option<(PersonRecord<ClocksOnDevices>, String)>,
    held: Rc<RefCell<OnThisDevice>>,
}

impl DeviceChoices {
    pub(crate) fn new(record: Option<(PersonRecord<ClocksOnDevices>, String)>) -> Self {
        Self {
            record,
            held: Rc::default(),
        }
    }

    pub(crate) fn get(&self) -> OnThisDevice {
        match &self.record {
            Some((record, device)) => record.with(|kept| kept.on(device)),
            None => self.held.borrow().clone(),
        }
    }

    pub(crate) fn update(&self, change: impl FnOnce(&mut OnThisDevice)) -> Result<(), String> {
        let mut kept = self.get();
        change(&mut kept);
        let Some((record, device)) = &self.record else {
            *self.held.borrow_mut() = kept;
            return Ok(());
        };
        if record.with(|all| all.devices.get(device) == Some(&kept)) {
            return Ok(());
        }
        let kept = serde_json::to_value(&kept).map_err(|error| error.to_string())?;
        record
            .invoke(&ClocksOnDevices::KEEP.with((device.clone(), kept)))
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    #[serde(default)]
    pub alarms: BTreeMap<String, ArmedAlarm>,
    #[serde(default)]
    pub timers_rung: BTreeMap<String, i64>,
    /// The deadline the host accepted, by timer id: it shows that notification
    /// itself, so the app must not show it again.
    #[serde(default)]
    pub native_scheduled: BTreeMap<String, i64>,
    /// Timers whose notification this device may still have to cancel, kept until
    /// cancellation or delivery is confirmed, even for a deleted timer.
    #[serde(default)]
    pub native_owners: BTreeSet<String>,
    #[serde(default)]
    pub native_alarms_scheduled: BTreeMap<String, i64>,
    #[serde(default)]
    pub native_alarm_owners: BTreeSet<String>,
    /// Completed host one-shot generations: automatic switch-off must not cancel a
    /// snooze the host retains. Edits and deletion still cancel.
    #[serde(default)]
    pub native_completed_alarms: BTreeMap<String, (u8, u8, u8, i64)>,
    /// Snoozed occurrences, by alarm id: beside the armed one, so an alarm that
    /// already rang and was turned off can ring once more.
    #[serde(default)]
    pub snoozes: BTreeMap<String, Snooze>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snooze {
    pub due_ms: i64,
    pub schedule: (u8, u8, u8, i64),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmedAlarm {
    pub schedule: (u8, u8, u8, i64),
    /// The occurrence, not only its time of day: reopening after midnight still
    /// rings the alarm armed yesterday.
    pub next_due_ms: Option<i64>,
    pub last_rung: Option<NaiveDate>,
}

pub(crate) fn schedule_of(alarm: &Alarm) -> (u8, u8, u8, i64) {
    (alarm.hour, alarm.minute, alarm.repeat, alarm.enabled_at_ms)
}

/// Snooze `alarm` on this device for `minutes`, keeping the settings it was
/// snoozed under so an edit cancels it. It rings once at `now_ms + minutes`, even
/// when the alarm has turned itself off.
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
        native_scheduled: bool,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tick {
    pub due: Vec<DueEvent>,
    pub finished_alarms: Vec<Id>,
}

fn ring_snoozes(clocks: &Clocks, delivery: &mut Delivery, now_ms: i64, found: &mut Tick) {
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
}

/// Ring what is due at `now_utc` (`now_local` on this device's clock), each
/// deadline once.
pub fn tick(
    clocks: &Clocks,
    delivery: &mut Delivery,
    now_utc: DateTime<Utc>,
    now_local: DateTime<Local>,
) -> Tick {
    let mut found = Tick::default();
    let now_ms = now_utc.timestamp_millis();
    ring_snoozes(clocks, delivery, now_ms, &mut found);
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
            // Local time, so future deadlines follow time zone changes; a passed
            // one stays due, even after days closed.
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
            // Catch up once, then arm the next occurrence instead of replaying
            // the missed days.
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

/// The next occurrence after `after`: a missing spring-forward time advances
/// minute by minute, an ambiguous autumn time takes the first.
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
        // A floated timer wins even against a sooner one; otherwise the most
        // urgent wins.
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
