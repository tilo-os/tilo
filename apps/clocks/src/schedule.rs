//! When alarms and timers ring on this device. The workspace keeps the clocks.
//! The device records which alerts it delivered and which controls it floated.
//! Wall time is passed in so suspend, clock corrections and tests are deterministic.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use settings_model::{Scope, SettingsSchema};
use ui::framework::Setting;
use ui::prelude::*;

use crate::model::{Alarm, Clocks, instant};
pub use clocks_model::schedule::{ArmedAlarm, ClocksOnDevices, Delivery, OnThisDevice, Snooze};

/// The schedule settings an alarm rings under: its time, repeat days and when it was armed.
pub(crate) fn schedule_of(alarm: &Alarm) -> (u8, u8, u8, i64) {
    (alarm.hour, alarm.minute, alarm.repeat, alarm.enabled_at_ms)
}

/// Device settings keyed by workspace so different workspaces keep their
/// own delivery receipts and floating controls.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClockDeviceSettings {
    #[serde(default)]
    pub(crate) clocks_on_this_device: BTreeMap<String, OnThisDevice>,
}

impl SettingsSchema for ClockDeviceSettings {
    const SCOPE: Scope = Scope::DeviceLocal;
}

#[derive(Clone)]
pub(crate) struct DeviceChoices {
    settings: Option<(Setting<ClockDeviceSettings>, String)>,
    held: Rc<RefCell<OnThisDevice>>,
}

impl DeviceChoices {
    pub(crate) fn new(cx: &ViewCx<Clocks>) -> Self {
        let settings = cx
            .mode()
            .is_live()
            .then(|| {
                cx.workspace()
                    .map(|workspace| (cx.settings::<ClockDeviceSettings>(), workspace))
            })
            .flatten();
        Self {
            settings,
            held: Rc::default(),
        }
    }

    pub(crate) fn get(&self) -> OnThisDevice {
        self.settings
            .as_ref()
            .and_then(|(settings, workspace)| {
                settings
                    .state()
                    .with(|all| all.clocks_on_this_device.get(workspace).cloned())
            })
            .unwrap_or_else(|| self.held.borrow().clone())
    }

    pub(crate) fn update(&self, change: impl FnOnce(&mut OnThisDevice)) -> Result<(), String> {
        let before = self.get();
        let mut kept = before.clone();
        change(&mut kept);
        if kept == before {
            return Ok(());
        }
        let Some((settings, workspace)) = &self.settings else {
            *self.held.borrow_mut() = kept;
            return Ok(());
        };
        settings
            .update(|all| {
                all.clocks_on_this_device.insert(workspace.clone(), kept);
            })
            .map_err(|error| error.to_string())
    }
}
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
        scheduled_for: jiff::Timestamp,
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
    now_utc: jiff::Timestamp,
    now_local: &jiff::Zoned,
) -> Tick {
    let mut found = Tick::default();
    let now_ms = now_utc.as_millisecond();
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
                .map(|deadline| deadline.timestamp().as_millisecond());
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
        armed.last_rung = Some(ui::clock::at(scheduled_for).date());
        if alarm.repeat == 0 {
            armed.next_due_ms = None;
            found.finished_alarms.push(id.clone());
        } else {
            // Catch up once, then arm the next occurrence instead of replaying
            // the missed days.
            armed.next_due_ms = next_occurrence(alarm, armed.last_rung, now_local)
                .map(|deadline| deadline.timestamp().as_millisecond());
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
    last_rung: Option<Date>,
    after: &jiff::Zoned,
) -> Option<jiff::Zoned> {
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
    now: jiff::Timestamp,
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
