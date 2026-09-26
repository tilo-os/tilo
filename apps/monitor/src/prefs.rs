//! Per-window Monitor preferences.
//!
//! These values describe how **this device** observes a live system:
//! sampling cadence, whether it is currently sampling, kernel-thread
//! visibility, and the local confirmation affordance. They must not enter
//! Automerge, history, or collaboration: another window's paused sampler or
//! relaxed confirmation preference is never authority over this machine.
//! `MonitorApp` owns one [`Prefs`] value per window; a host may later bind it
//! to a device-preference service without changing that boundary.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prefs {
    /// Sampler cadence in milliseconds. Cycled through a small set in the
    /// header control.
    pub interval_ms: u64,
    /// Whether periodic sampling is running. Manual Refresh remains available
    /// while paused, making pause a low-power inspection mode rather than a
    /// dead-end state.
    pub auto_refresh: bool,
    /// Whether the process table lists Linux kernel worker threads.
    pub show_kernel_threads: bool,
    /// Whether the local Kill button pops a confirmation dialog first. The
    /// resource action remains contract-confirmed independently.
    pub confirm_kill: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            interval_ms: 1500,
            auto_refresh: true,
            show_kernel_threads: false,
            confirm_kill: true,
        }
    }
}

impl Prefs {
    /// Advance to the next refresh cadence (1s → 1.5s → 2s → 3s → 5s → 1s).
    /// Any off-grid value (e.g. loaded from an older document) snaps back
    /// to the start of the cycle.
    pub fn cycle_interval(&mut self) {
        self.interval_ms = match self.interval_ms {
            1000 => 1500,
            1500 => 2000,
            2000 => 3000,
            3000 => 5000,
            // 5000 falls back here too, alongside any off-grid value.
            _ => 1000,
        };
    }

    pub fn interval_label(&self) -> String {
        if self.interval_ms.is_multiple_of(1000) {
            format!("{}s", self.interval_ms / 1000)
        } else {
            format!("{:.1}s", self.interval_ms as f32 / 1000.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_favor_safe_periodic_sampling_and_interval_cycles() {
        let prefs = Prefs::default();

        assert_eq!(prefs.interval_ms, 1500);
        assert!(prefs.auto_refresh);
        assert!(!prefs.show_kernel_threads);
        assert!(prefs.confirm_kill);

        // The interval cycles 1000 → 1500 → 2000 → 3000 → 5000 → 1000.
        let mut p = Prefs {
            interval_ms: 1000,
            ..Prefs::default()
        };
        for want in [1500, 2000, 3000, 5000, 1000] {
            p.cycle_interval();
            assert_eq!(p.interval_ms, want, "case cycle to {want}");
        }
    }
}
