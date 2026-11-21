//! The loop's turns as the watchdog counts them.

use super::LoopTurns;
use tilo_liveness::watchdog::Watchdog;

/// The loop's turns as the watchdog counts them.
pub(super) struct Turns(pub(super) Watchdog);

impl LoopTurns for Turns {
    fn parking(&self) {
        self.0.parking();
    }

    fn unparked(&self) {
        self.0.unparked();
    }
}

#[cfg(test)]
mod tests;
