//! Power management — suspend on idle, lid and power-key handling, suspend
//! inhibitors and battery thresholds — built on `systemd-logind` and `UPower`
//! over D-Bus. The decisions are pure policy ([`policy`], [`battery`]) so they
//! are testable without a bus; the D-Bus I/O sits behind them.

pub mod battery;
#[cfg(feature = "service")]
pub mod bridge;
#[cfg(feature = "service")]
pub mod config;
#[cfg(feature = "service")]
pub mod history;
pub mod inhibit;
#[cfg(feature = "service")]
pub mod logind;
pub mod policy;
#[cfg(feature = "service")]
pub mod profiles;
#[cfg(feature = "service")]
pub mod service;
pub mod sysfs;
#[cfg(feature = "service")]
pub mod upower;

pub use battery::{BatteryEvent, BatteryMonitor, BatterySnapshot, BatteryState};
#[cfg(feature = "service")]
pub use bridge::{PowerBridge, PowerCommand, PowerNotification, PowerSignal};
#[cfg(feature = "service")]
pub use config::{default_policy, load_policy_for_profile, policy_from_settings_json};
#[cfg(feature = "service")]
pub use history::{
    BatteryHistory, BatteryHistoryPoint, PowerHistoryWriter, PowerSourceKind, load_battery_history,
    load_ups_history, record_power_history,
};
pub use inhibit::{InhibitRegistry, InhibitToken};
#[cfg(feature = "service")]
pub use logind::{LogindInhibitor, LogindManager};
pub use policy::{MachineDuty, PowerAction, PowerPolicy, this_machine_duty};
#[cfg(feature = "service")]
pub use profiles::PowerProfilesMonitor;
#[cfg(feature = "service")]
pub use service::PowerService;
pub use sysfs::{
    BatteryTime, battery_snapshot as sysfs_battery_snapshot, battery_time as sysfs_battery_time,
    on_battery as sysfs_on_battery,
};
#[cfg(feature = "service")]
pub use upower::UPowerMonitor;
