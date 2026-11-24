//! The session's system resources, as an app and the session share them.
//!
//! The resources' ids, their actions ([`effects`]) and the types their
//! readings serialise ([`model`], [`update`], and [`printers`], which keeps
//! its own small vocabulary beside them). What reads or changes the machine
//! is the session's system service; this crate holds only the contract, so
//! an app that shows or drives these resources depends on it and not on the
//! session.

pub mod codec_pack;
pub mod containers;
pub mod disk_unlock;
pub mod disks;
pub mod drives;
pub mod effects;
pub mod firewall;
pub mod firmware;
pub mod host_info;
pub mod journal;
pub mod model;
pub mod printers;
pub mod processes;
pub mod scanners;
pub mod storage;
pub mod update;
pub mod users;
pub mod windows_apps;
pub mod windows_compat;

pub use codec_pack::CODEC_PACK;
pub use containers::CONTAINERS;
pub use disk_unlock::DISK_UNLOCK;
pub use disks::DISKS;
pub use drives::DRIVES;
pub use firewall::FIREWALL;
pub use firmware::FIRMWARE;
pub use host_info::HOST_INFO;
pub use journal::JOURNAL;
pub use printers::PRINTERS;
pub use processes::PROCESSES;
pub use scanners::SCANNERS;
pub use storage::STORAGE;
pub use users::USERS;
pub use windows_apps::{WINDOWS_APPS, WINDOWS_PROGRAM};
pub use windows_compat::WINDOWS_COMPAT;

/// The resources' ids.
pub const POWER: &str = "power";
pub const SYSTEM_UPDATE: &str = "system-update";
pub const TIME_ZONE: &str = "time-zone";
pub const KEYBOARD_BACKLIGHT: &str = "device:keyboard-backlight";
