//! qOS **power management** — suspend-on-idle, lid/power-key handling,
//! suspend inhibitors, and battery thresholds, built on `systemd-logind` and
//! `UPower` over D-Bus.
//!
//! # Shape
//!
//! The crate is split into a **pure policy core** and a **real D-Bus glue**,
//! so every decision is unit-testable without a bus and the actuator stays
//! thin and honest:
//!
//! * [`policy`] — the [`PowerPolicy`] and the lid / power-key decisions.
//! * [`battery`] — the [`BatteryMonitor`] threshold state machine
//!   (low-warning + critical auto-suspend, with hysteresis).
//! * [`inhibit`] — the [`InhibitRegistry`], the ref-counted "don't suspend"
//!   API the shell and apps hold while media plays / a task runs.
//! * [`config`] — reading the policy from the shared `settings.json` the
//!   Settings pane writes.
//! * [`logind`] / [`upower`] — the real `zbus` proxies (suspend, inhibit
//!   locks, battery + lid reads, sleep signals).
//! * [`sysfs`] — a synchronous, bus-free battery read for callers (the
//!   compositor's status indicator) that need a number on a plain
//!   thread rather than a subscription on a runtime.
//! * [`bridge`] — the `Send + Sync` mailbox between the async service and the
//!   synchronous compositor loop.
//! * [`service`] — the [`PowerService`] async orchestrator the compositor
//!   spawns.
//!
//! # Wiring
//!
//! The compositor's idle state machine (`shell::compositor::idle`) grows a
//! *suspend* threshold and emits a suspend transition, which the compositor
//! loop turns into a [`bridge::PowerCommand::Suspend`] — gated by
//! [`InhibitRegistry::is_inhibited`]. The service issues the real
//! `logind.Suspend`, mirrors the registry into a logind `idle:sleep` lock,
//! watches the battery, and reacts to the lid.

pub mod battery;
pub mod bridge;
pub mod config;
pub mod history;
pub mod inhibit;
pub mod logind;
pub mod policy;
pub mod profiles;
pub mod service;
pub mod sysfs;
pub mod upower;

pub use battery::{BatteryEvent, BatteryMonitor, BatterySnapshot, BatteryState};
pub use bridge::{PowerBridge, PowerCommand, PowerNotification, PowerSignal};
pub use config::{default_policy, load_policy, policy_from_settings_json};
pub use history::{
    BatteryHistory, BatteryHistoryPoint, PowerSourceKind, load_battery_history, load_ups_history,
    record_power_history,
};
pub use inhibit::{InhibitRegistry, InhibitToken};
pub use logind::{LogindInhibitor, LogindManager};
pub use policy::{PowerAction, PowerPolicy};
pub use profiles::PowerProfilesMonitor;
pub use service::PowerService;
pub use sysfs::{battery_snapshot as sysfs_battery_snapshot, on_battery as sysfs_on_battery};
pub use upower::UPowerMonitor;
