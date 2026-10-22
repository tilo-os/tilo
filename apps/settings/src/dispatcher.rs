//! Shared dispatcher plumbing for the Settings panes.
//!
//! Every pane that drives a real hardware action (display, audio,
//! network, bluetooth) holds an `Arc<dyn os_actions::Dispatcher + Send +
//! Sync>` so tests can substitute a `testing::RecordingDispatcher`.
//! [`dispatcher_or_real`] is the small ergonomic helper the pane
//! constructors call: pass `Some(arc)` from a test to wire the recorder,
//! pass `None` from production to get `os_actions::RealDispatcher`.

use std::sync::Arc;

use os_actions::{Dispatcher, RealDispatcher};

/// Default `Dispatcher` factory used by the live binary: returns the
/// supplied `Arc` if one was passed (i.e. a test wired a recorder),
/// otherwise produces a fresh `RealDispatcher`.
pub fn dispatcher_or_real(
    explicit: Option<Arc<dyn Dispatcher + Send + Sync>>,
) -> Arc<dyn Dispatcher + Send + Sync> {
    explicit.unwrap_or_else(|| Arc::new(RealDispatcher) as Arc<dyn Dispatcher + Send + Sync>)
}

#[cfg(test)]
pub mod testing {

    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use os_actions::{Dispatcher, PowerProfile};
    use ui::lock::MutexRecover as _;

    /// Captures every dispatch call into a `Vec<String>` so tests can
    /// assert on what landed without spawning processes. The dispatcher
    /// must be `Send + Sync` because the panes hold them inside `Arc`s
    /// — a `Mutex` is the simplest way to get interior mutability with
    /// those bounds.
    #[derive(Default)]
    pub struct RecordingDispatcher {
        calls: Mutex<Vec<String>>,
        fail_power_profile: AtomicBool,
    }

    impl RecordingDispatcher {
        /// Snapshot the recorded calls. Returns a fresh `Vec` so the
        /// caller can iterate / index without holding a borrow. A poisoned
        /// lock (another test thread panicked while holding it) still
        /// yields whatever calls were recorded, rather than panicking here
        /// too and hiding the original failure.
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock_recover().clone()
        }

        fn push(&self, s: String) {
            self.calls.lock().expect("recorder lock poisoned").push(s);
        }
    }

    impl Dispatcher for RecordingDispatcher {
        fn set_brightness(&self, pct: f32) -> Result<(), String> {
            self.push(format!("set_brightness({pct:.2})"));
            Ok(())
        }
        fn set_display_brightness(&self, connector: &str, pct: f32) -> Result<(), String> {
            self.push(format!("set_display_brightness({connector}, {pct:.2})"));
            Ok(())
        }
        fn set_volume(&self, pct: f32) -> Result<(), String> {
            self.push(format!("set_volume({pct:.2})"));
            Ok(())
        }
        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.push(format!("set_wifi({enabled})"));
            Ok(())
        }
        fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
            self.push(format!("set_bluetooth({enabled})"));
            Ok(())
        }
        fn set_wired(&self, enabled: bool) -> Result<(), String> {
            self.push(format!("set_wired({enabled})"));
            Ok(())
        }
        fn connect_wifi(&self, ssid: &str, _password: Option<&str>) -> Result<(), String> {
            self.push(format!("connect_wifi({ssid})"));
            Ok(())
        }
        fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
            self.push(format!("set_power_profile({})", profile.cli_arg()));
            if self.fail_power_profile.load(Ordering::Relaxed) {
                Err("simulated power-profile failure".into())
            } else {
                Ok(())
            }
        }
        fn toggle_mute(&self) -> Result<(), String> {
            self.push("toggle_mute()".to_string());
            Ok(())
        }
        fn set_mute(&self, muted: bool) -> Result<(), String> {
            self.push(format!("set_mute({muted})"));
            Ok(())
        }
    }
}
