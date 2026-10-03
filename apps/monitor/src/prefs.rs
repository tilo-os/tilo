//! Per-window Monitor preferences.
//!
//! These values describe how **this device** observes a live system:
//! sampling cadence, whether it is currently sampling, and kernel-thread
//! visibility. They must not enter Automerge, history, or collaboration:
//! another window's paused sampler is never authority over this machine.
//! `MonitorApp` owns one [`Prefs`] value per window; a host may later bind it
//! to a device-preference service without changing that boundary.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    /// Sampler cadence in milliseconds, one of the header menu's choices.
    pub interval_ms: u64,
    /// Whether periodic sampling is running. Manual Refresh remains available
    /// while paused, making pause a low-power inspection mode rather than a
    /// dead-end state.
    pub auto_refresh: bool,
    /// Whether the process table lists Linux kernel worker threads.
    pub show_kernel_threads: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            interval_ms: 1500,
            auto_refresh: true,
            show_kernel_threads: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_favor_periodic_sampling_without_kernel_threads() {
        let prefs = Prefs::default();
        assert_eq!(prefs.interval_ms, 1500);
        assert!(prefs.auto_refresh);
        assert!(!prefs.show_kernel_threads);
    }
}
