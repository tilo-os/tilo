//! The device's time as it publishes it.

use serde::{Deserialize, Serialize};
use tilo_resource::NoEffect;

/// The time now, as the bar shows it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Time {
    /// The minute, in the person's style (`4:28 PM`, `16:28`).
    pub clock: String,
    /// The timer or stopwatch Clocks floats, by its name: its label, or `Timer` or `Stopwatch`
    /// when the app named none; empty when none floats.
    pub timer: String,
    /// What the timer has left, or what the stopwatch has run.
    pub timer_left: String,
    pub timer_paused: bool,
}

/// The time as a view of another language reads it (`device.time`).
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Time, NoEffect>("time", "Time")
}
