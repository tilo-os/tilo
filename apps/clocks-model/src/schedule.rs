//! What Clocks keeps for one device alone: the clocks floating there and the log of what it
//! has rung or handed to the platform. When they ring is computed by the app, which passes
//! the wall time in.

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use tilo_resource::{self, Model, Refusal, resource};
/// What Clocks keeps for one device alone: the clocks floating there, and the
/// log of what it has rung or handed to the platform.
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
    pub last_rung: Option<Date>,
}
