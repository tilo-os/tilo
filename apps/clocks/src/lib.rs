//! Clocks: world time, alarms, timers and a stopwatch.
//!
//! What a person keeps (cities, alarms, timers, the stopwatch) is the app's
//! resource, [`Clocks`], and follows them to every device. What one device
//! decides for itself (which clocks float on its screen, which deadlines it
//! has already rung) stays in that device's settings ([`OnThisDevice`]).

pub mod app;
pub mod locales;
pub mod model;
mod native_timers;
pub mod resource;
pub mod schedule;
pub mod transport;

pub use app::ClocksApp;
pub use model::{Alarm, Clocks, ClocksAction, Stopwatch, Timer, WeekdayKey, WorldClock};
pub use resource::{ClocksEffect, ClocksShell};
pub use schedule::{DueEvent, OnThisDevice};
pub use transport::ShellSnapshot;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(ClocksShell)
}
