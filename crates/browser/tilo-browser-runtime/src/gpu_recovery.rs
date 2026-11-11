//! A lost GPU is made again, and the desk drawn on it.
//!
//! A device can be lost for reasons the page never caused (a driver reset,
//! memory pressure, a browser that took the device for another tab), and a
//! device that errors cannot be trusted to draw again either. Neither is a
//! reason for the desktop to stop: the mount makes a new device, surface and
//! renderer and redraws, keeping every document it holds. A GPU that keeps
//! failing is not chased for ever: past a few remakes in a short while the
//! desktop stops and says why, as it did before.
use std::collections::VecDeque;

use ui::time::{Duration, Instant};

/// Remakes allowed within [`WITHIN`].
pub const REMAKES: usize = 3;

/// The while [`REMAKES`] count over.
pub const WITHIN: Duration = Duration::from_secs(60);

/// When a mount made its GPU again lately.
#[derive(Default)]
pub struct GpuRemakes {
    made: VecDeque<Instant>,
}

impl GpuRemakes {
    /// Whether the GPU may be made again at `now`, counting it if so.
    pub fn admit(&mut self, now: Instant) -> bool {
        while self
            .made
            .front()
            .is_some_and(|&made| now.saturating_duration_since(made) >= WITHIN)
        {
            self.made.pop_front();
        }
        if self.made.len() >= REMAKES {
            return false;
        }
        self.made.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lost_gpu_is_made_again_but_one_that_keeps_failing_is_let_go() {
        let start = ui::time::now();
        let at = |secs: u64| {
            start
                .checked_add(Duration::from_secs(secs))
                .unwrap_or(start)
        };
        let mut remakes = GpuRemakes::default();
        assert!(remakes.admit(at(0)), "a device lost once is made again");
        assert!(remakes.admit(at(10)));
        assert!(remakes.admit(at(20)));
        assert!(
            !remakes.admit(at(30)),
            "a fourth loss within a minute stops the desktop"
        );
        assert!(
            remakes.admit(at(70)),
            "a minute after the first, a loss is made good again"
        );
    }
}
