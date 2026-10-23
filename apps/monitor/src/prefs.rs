//! Monitor preferences.
//!
//! The sampling cadence and kernel-thread visibility are the person's: they
//! follow them to every device as [`MonitorPreferences`], a personal
//! settings section, never the shared document. Stopping or killing a
//! process always asks for confirmation: the effect declares it. Whether this window is sampling right now
//! ([`Prefs::auto_refresh`]) belongs to the window alone.

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    /// Sampler cadence in milliseconds, one of the window bar menu's choices.
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

impl Prefs {
    /// The part of these preferences that follows the person.
    #[must_use]
    pub fn personal(&self) -> MonitorPreferences {
        MonitorPreferences {
            interval_ms: self.interval_ms,
            show_kernel_threads: self.show_kernel_threads,
        }
    }

    /// Take the person's preferences, keeping this window's sampling state.
    pub fn adopt(&mut self, personal: &MonitorPreferences) {
        self.interval_ms = personal.interval_ms;
        self.show_kernel_threads = personal.show_kernel_threads;
    }
}

/// The Monitor preferences that follow the person to every device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorPreferences {
    pub interval_ms: u64,
    pub show_kernel_threads: bool,
}

impl Default for MonitorPreferences {
    fn default() -> Self {
        Prefs::default().personal()
    }
}

impl SettingsSchema for MonitorPreferences {
    const SCOPE: Scope = Scope::Personal;
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
