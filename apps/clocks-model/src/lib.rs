//! The `clocks.collection` and `clocks.on-devices` resources: what a person keeps in Clocks and
//! what each device has rung of it.

pub mod effects;
pub mod model;
pub mod schedule;

pub use effects::{ClocksEffect, StopwatchRow};
pub use model::*;
pub use schedule::{ArmedAlarm, ClocksOnDevices, Delivery, OnThisDevice, Snooze};
