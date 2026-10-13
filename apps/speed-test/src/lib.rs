//! A small, responsive internet speed test.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic over small bounded quantities"
)]

pub mod app;
pub mod locales;
mod measure;
mod model;
pub mod resource;
mod view;

pub use resource::{Measurement, SpeedEffect, SpeedTest, SpeedTestShell};

ui::dev_reload_entry!(SpeedTestShell);
