//! What Clocks does that is not an edit of its collection: what the window
//! shows, and the per-device choices about running clocks (opening an alarm,
//! snoozing it, floating a timer or the stopwatch).
//!
//! An alarm and a timer offer theirs on their own item; the stopwatch is one
//! row, not a document item, so it declares its row type here.

use tilo_resource::{self, Effect};

/// The stopwatch as Clocks publishes it: one row of the `clocks.stopwatch`
/// type, which offers floating it above other windows.
#[derive(tilo_resource::Row)]
#[row(kind = "clocks.stopwatch", label = "Stopwatch", effects = ClocksEffect)]
pub struct StopwatchRow;

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "clocks.effects")]
pub enum ClocksEffect {
    /// Open this alarm on this device.
    #[effect(screen, item = alarms)]
    OpenAlarm {
        #[param("The alarm's id")]
        alarm: String,
    },
    /// Show a section of Clocks on this device.
    #[effect(screen)]
    Show {
        #[param("world, alarms, timers or stopwatch")]
        section: String,
    },
    /// Dismiss the due alarm or timer notice on this device.
    #[effect(screen)]
    DismissNotice,
    /// Ring this device's alarm again after a few minutes.
    #[effect(kind = "outside")]
    SnoozeAlarm {
        #[param("The alarm's id")]
        alarm: String,
        #[param("Minutes to wait before ringing again")]
        minutes: u32,
    },
    /// Float a timer above other windows.
    #[effect(screen, item = timers)]
    FloatTimer {
        #[param("The timer's id")]
        timer: String,
    },
    /// Put a floating timer back in its window.
    #[effect(screen, item = timers)]
    UnfloatTimer {
        #[param("The timer's id")]
        timer: String,
    },
    /// Float the stopwatch above other windows.
    #[effect(screen, on = StopwatchRow)]
    FloatStopwatch,
    /// Put a floating stopwatch back in its window.
    #[effect(screen, on = StopwatchRow)]
    UnfloatStopwatch,
}
