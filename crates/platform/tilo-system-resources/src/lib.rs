//! The session's system resources, as an app and the session share them.
//!
//! The resources' ids, their actions ([`effects`]) and the types their
//! readings serialise ([`model`], [`update`], and [`printers`], which keeps
//! its own small vocabulary beside them). What reads or changes the machine
//! is the session's system service; this crate holds only the contract, so
//! an app that shows or drives these resources depends on it and not on the
//! session.

pub mod effects;
pub mod model;
pub mod codec_pack;
pub mod disk_unlock;
pub mod firmware;
pub mod drives;
pub mod host_info;
pub mod printers;
pub mod scanners;
pub mod storage;
pub mod update;
pub mod users;
pub mod windows_compat;

pub use codec_pack::CODEC_PACK;
pub use disk_unlock::DISK_UNLOCK;
pub use firmware::FIRMWARE;
pub use drives::DRIVES;
pub use host_info::HOST_INFO;
pub use printers::PRINTERS;
pub use scanners::SCANNERS;
pub use storage::STORAGE;
pub use users::USERS;
pub use windows_compat::WINDOWS_COMPAT;

/// The resources' ids.
pub const POWER: &str = "power";
pub const SYSTEM_UPDATE: &str = "system-update";
pub const TIME_ZONE: &str = "time-zone";
pub const KEYBOARD_BACKLIGHT: &str = "device:keyboard-backlight";
