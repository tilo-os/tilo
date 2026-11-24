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
    /// The year today falls in.
    pub year: i64,
    /// The month today falls in, 1 to 12.
    pub month: i64,
    /// The day of the month today is.
    pub day: i64,
    /// Today's weekday, 1 (Monday) to 7 (Sunday).
    pub weekday: i64,
    /// The first day of the person's week, 1 (Monday) to 7 (Sunday).
    pub week_start: i64,
    /// The month's name, in the person's language (`November`).
    pub month_name: String,
    /// How many days this month has.
    pub days_in_month: i64,
    /// The weekday the month's first day falls on, 1 to 7.
    pub first_weekday: i64,
    /// The person's week, short names, seven from the day their week starts on (`Mon`).
    pub weekdays: Vec<String>,
    /// The person's week, full names, seven from the day their week starts on (`Monday`).
    pub weekday_names: Vec<String>,
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
