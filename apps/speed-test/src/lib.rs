//! A small, responsive internet speed test.

// Apps run isolated, so app-local arithmetic checks are relaxed; a fault in a shared crate reaches the session, so its lints stay.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "app-local arithmetic over small bounded quantities"
)]

pub mod locales;
mod measure;
mod model;
pub mod resource;
mod run;
mod view;

pub use resource::{Measurement, SpeedEffect, SpeedTest, SpeedTestShell};

ui::dev_reload_entry!(SpeedTestShell);
