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
pub mod printers;
pub mod update;
pub mod users;

pub use printers::PRINTERS;
pub use users::USERS;

/// The resources' ids.
pub const POWER: &str = "power";
pub const SYSTEM_UPDATE: &str = "system-update";
pub const TIME_ZONE: &str = "time-zone";
pub const KEYBOARD_BACKLIGHT: &str = "device:keyboard-backlight";
