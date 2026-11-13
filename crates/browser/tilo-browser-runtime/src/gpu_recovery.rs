//! A lost GPU is made again, and the desk drawn on it.
//!
//! A device can be lost for reasons the page never caused (a driver reset,
//! memory pressure, a browser that took the device for another tab), and a
//! device that errors cannot be trusted to draw again either. Neither is a
//! reason for the desktop to stop: the mount makes a new device, surface and
//! renderer and redraws, keeping every document it holds. A GPU that keeps
//! failing is not chased for ever: past a few remakes in a short while the
//! desktop stops and says why, as it did before.
//!
//! Which GPU it is made on matters on a laptop with two. The browser draws
//! the page on the GPU that drives the display; a canvas another GPU draws
//! must be shared over to it, and Chromium on Linux could not share an
//! NVIDIA GPU's canvas with the AMD GPU driving the panel: each device was
//! lost (`VK_ERROR_OUT_OF_DEVICE_MEMORY`) on its first frames, however
//! little it held. So the page asks for the GPU that drives the display
//! first ([`FIRST_POWER`]), and a device lost before it lived
//! [`SHORT_LIVED`] gives way to the machine's other GPU ([`power_after_loss`]).
use std::cell::Cell;
use std::collections::VecDeque;

use ui::time::{Duration, Instant};
use ui::vello::wgpu::PowerPreference;

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

/// The GPU a page asks for first: the one that drives the display, which
/// a laptop's integrated GPU does, and the only one a desktop has.
pub const FIRST_POWER: PowerPreference = PowerPreference::LowPower;

/// A device lost sooner than this after it was made could not draw the
/// page on its GPU at all; one that lasted longer was reset, and its GPU
/// draws again.
pub const SHORT_LIVED: Duration = Duration::from_secs(20);

/// The GPU to make a device on after one on `current` was lost `lived`
/// after it was made: the other GPU when it never lasted, else the same.
#[must_use]
pub fn power_after_loss(current: PowerPreference, lived: Duration) -> PowerPreference {
    if lived >= SHORT_LIVED {
        return current;
    }
    match current {
        PowerPreference::LowPower => PowerPreference::HighPerformance,
        PowerPreference::HighPerformance | PowerPreference::None => PowerPreference::LowPower,
    }
}

thread_local! {
    static POWER: Cell<PowerPreference> = const { Cell::new(FIRST_POWER) };
}

/// The GPU the page's devices are asked for now.
pub fn power() -> PowerPreference {
    POWER.get()
}

/// Ask for the page's next devices on the GPU [`power`] names: called before
/// every device the page opens.
pub fn ask_for_power() {
    ui::gpu::prefer_power(power());
}

/// A device was lost `lived` after it was made: the next is asked of the GPU
/// [`power_after_loss`] picks. Says whether that is another GPU.
pub fn device_lost(lived: Duration) -> bool {
    let next = power_after_loss(power(), lived);
    let other = next != power();
    POWER.set(next);
    other
}

#[cfg(test)]
mod tests {
    use super::*;

    // Catches a hybrid laptop whose browser hands the page the NVIDIA GPU:
    // every device it made there was lost on its first frames, and the
    // desktop stopped after three remakes on the same GPU.
    #[test]
    fn a_device_that_never_lasted_gives_way_to_the_other_gpu() {
        let soon = Duration::from_millis(400);
        assert_eq!(
            power_after_loss(PowerPreference::LowPower, soon),
            PowerPreference::HighPerformance
        );
        assert_eq!(
            power_after_loss(PowerPreference::HighPerformance, soon),
            PowerPreference::LowPower,
            "and back, when the other cannot either"
        );
        assert_eq!(
            power_after_loss(PowerPreference::HighPerformance, Duration::from_secs(600)),
            PowerPreference::HighPerformance,
            "a device that drew for minutes was reset: its GPU works"
        );
        assert_eq!(FIRST_POWER, PowerPreference::LowPower);
    }

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
