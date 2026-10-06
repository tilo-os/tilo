//! A small, responsive internet speed test.

pub mod app;
pub mod resource;

pub use resource::{Measurement, SpeedEffect, SpeedTest, SpeedTestShell};

ui::dev_reload_entry!(SpeedTestShell);
