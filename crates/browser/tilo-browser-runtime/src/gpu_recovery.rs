//! A lost GPU is made again, and the shell drawn on it; a GPU that will not
//! draw is drawn around, and asked again later. Neither ends the page.
//!
//! A device can be lost for reasons the page never caused (a driver reset,
//! memory pressure, a browser that took the device for another tab), and a
//! device that errors cannot be trusted to draw again either. Neither is a
//! reason for the desktop to stop: the mount makes a new device, surface and
//! renderer and redraws, keeping every document it holds.
//!
//! A GPU that keeps failing is not chased for ever: past a few remakes in a
//! short while ([`REMAKES`] within [`WITHIN`]) the mount draws its frames on
//! the CPU and puts them on the page itself
//! (`crate::browser::software::Software`), and tries the GPU again when
//! [`SOFTWARE_WAIT`] is over. A try that fails again waits twice as long,
//! up to [`SOFTWARE_WAIT_MAX`]: a device that cannot hold the page is not
//! worth remaking every half minute, while a driver that recovers (a
//! machine woken from sleep) is drawn on the GPU again within minutes.
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

/// How long a mount draws on the CPU before it tries the GPU again.
pub const SOFTWARE_WAIT: Duration = Duration::from_secs(30);

/// The longest a mount waits between tries at the GPU.
pub const SOFTWARE_WAIT_MAX: Duration = Duration::from_secs(300);

/// What a mount does when its GPU cannot draw a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Make the device, surface and renderer again, and redraw on them.
    Remake,
    /// Draw the frames on the CPU, and try the GPU again later
    /// ([`GpuRecovery::retry`]).
    Software,
}

/// A mount's losses: when it made its GPU again lately, and the wait before
/// the next try while it draws on the CPU.
#[derive(Default)]
pub struct GpuRecovery {
    made: VecDeque<Instant>,
    /// When the GPU may be tried again while the frames draw on the CPU, and
    /// the wait it had before it: a try that fails again waits longer.
    waiting: Option<(Instant, Duration)>,
    /// A try at the GPU is out and has not answered yet.
    trying: bool,
}

impl GpuRecovery {
    /// The GPU drew no frame `lived` after it was made, at `now`.
    pub fn failed(&mut self, now: Instant, lived: Duration) -> Next {
        self.trying = false;
        if lived >= SHORT_LIVED {
            // A device that drew for a while was reset: its GPU works, so the
            // remakes start over and no wait carries into the next loss.
            self.made.clear();
            self.waiting = None;
            return Next::Remake;
        }
        if self.waiting.is_none() && self.admit(now) {
            return Next::Remake;
        }
        self.waits(now)
    }

    /// When a mount drawing on the CPU may try the GPU again; `None` while
    /// the GPU draws or a try is out.
    pub fn retry(&self) -> Option<Instant> {
        if self.trying {
            return None;
        }
        self.waiting.map(|(at, _)| at)
    }

    /// Whether the GPU may be tried again at `now`, taking that try if so.
    pub fn try_due(&mut self, now: Instant) -> bool {
        if self.trying {
            return false;
        }
        if self.retry().is_none_or(|at| at > now) {
            return false;
        }
        self.trying = true;
        true
    }

    /// Draw on the CPU from `now` until a try at the GPU is due.
    fn waits(&mut self, now: Instant) -> Next {
        let wait = self
            .waiting
            .map_or(SOFTWARE_WAIT, |(_, wait)| wait.saturating_mul(2))
            .min(SOFTWARE_WAIT_MAX);
        self.waiting = now.checked_add(wait).map(|at| (at, wait));
        Next::Software
    }

    /// Whether the GPU may be made again at `now`, counting it if so.
    fn admit(&mut self, now: Instant) -> bool {
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

    tilo_test_env::scenario! {
        fn a_lost_gpu_is_recovered_or_drawn_around;

        phase a_device_that_never_lasted_gives_way_to_the_other_gpu;
        // Catches a hybrid laptop whose browser hands the page the NVIDIA GPU:
        // every device it made there was lost on its first frames, and the
        // desktop stopped after three remakes on the same GPU.
        {
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

        phase a_gpu_that_keeps_failing_is_drawn_around_and_asked_again;
        // Catches the desktop stopping on "My workspace didn't start" when the
        // GPU kept losing every device it was given: there is no answer that
        // stops it, only remakes and the CPU.
        {
            let start = ui::time::now();
            let at = |secs: u64| {
                start
                    .checked_add(Duration::from_secs(secs))
                    .unwrap_or(start)
            };
            let soon = Duration::from_millis(400);
            let mut recovery = GpuRecovery::default();
            assert_eq!(recovery.failed(at(0), soon), Next::Remake);
            assert_eq!(recovery.failed(at(10), soon), Next::Remake);
            assert_eq!(recovery.failed(at(20), soon), Next::Remake);
            assert_eq!(
                recovery.failed(at(30), soon),
                Next::Software,
                "a fourth loss within a minute draws on the CPU"
            );
            assert_eq!(recovery.retry(), Some(at(60)), "and the GPU is asked again 30 s later");
            assert!(!recovery.try_due(at(59)));
            assert!(recovery.try_due(at(60)), "the try is taken when it is due");
            assert!(!recovery.try_due(at(61)), "and not taken twice");
            assert_eq!(recovery.retry(), None, "nothing wakes the page for a try in hand");
            assert_eq!(
                recovery.failed(at(70), soon),
                Next::Software,
                "a try that fails again goes on drawing on the CPU"
            );
            assert_eq!(recovery.retry(), Some(at(130)), "waiting twice as long");
            let mut waits = Vec::new();
            let mut now = at(130);
            for _ in 0..20 {
                assert_eq!(recovery.failed(now, soon), Next::Software);
                let next = recovery.retry().expect("every failure asks the GPU again");
                waits.push(next.saturating_duration_since(now));
                now = next;
            }
            assert_eq!(
                waits.last().copied(),
                Some(SOFTWARE_WAIT_MAX),
                "the wait grows to a ceiling and no further"
            );
            assert!(waits.iter().all(|wait| *wait >= SOFTWARE_WAIT));
        }

        phase a_gpu_that_drew_for_a_while_is_made_again_at_once;
        // A driver reset after minutes of drawing: the GPU is made again in the
        // same second, and no earlier loss's wait keeps it on the CPU.
        {
            let start = ui::time::now();
            let at = |secs: u64| start.checked_add(Duration::from_secs(secs)).unwrap_or(start);
            let soon = Duration::from_millis(400);
            let mut recovery = GpuRecovery::default();
            for second in [0, 1, 2, 3] {
                recovery.failed(at(second), soon);
            }
            assert_eq!(recovery.failed(at(4), soon), Next::Software);
            assert_eq!(
                recovery.failed(at(400), Duration::from_secs(300)),
                Next::Remake,
                "a device that drew for five minutes was reset, not refused"
            );
            assert_eq!(recovery.retry(), None);
        }
    }
}
