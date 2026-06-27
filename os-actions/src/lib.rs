//! OS-level action wrappers (brightness, audio, Wi-Fi, Bluetooth,
//! power-profile) shared between the qOS compositor (`shell`) and the
//! standalone Settings app (`apps/settings`).
//!
//! Originally this code lived inside `shell::shell::qs_actions`. Both the
//! compositor's quick-settings popover and the Settings app needed the
//! same brightnessctl/wpctl/nmcli/bluetoothctl/powerprofilesctl bindings,
//! and the Settings app must not depend on the compositor — they are
//! peers, not parent/child. Lifting the bindings into this crate gives
//! both call sites a single source of truth.
//!
//! Design notes carried over from the shell-internal version:
//!
//! - Every helper checks the binary's presence first via [`which`] and
//!   returns a clear `"<binary> not installed"` error otherwise. The QS
//!   panel and the Settings app must stay functional on machines where
//!   one of the daemons is missing (e.g. a desktop without
//!   `power-profiles-daemon`).
//! - We use [`std::process::Command`] (sync) deliberately. Both call
//!   sites are user-driven (slider drag, toggle click) so getter calls
//!   happen only on rising-edge open. No tokio runtime is needed.
//! - Parsers are extracted into private free functions so they can be
//!   unit-tested against sample stdout pasted into the test module
//!   without spawning processes.
//!
//! ## Dispatcher trait
//!
//! [`Dispatcher`] is the side-effect interface used by the click
//! handlers. The default production impl ([`RealDispatcher`]) calls the
//! free functions in the per-domain modules; tests substitute a
//! recording impl so the click handler can be asserted on without
//! shelling out. The shell crate re-exports the trait as `QsActions`
//! and the impl as `RealQsActions` for source-compatibility with the
//! previous in-shell version.

pub mod audio;
pub mod bluetooth;
pub mod display;
pub mod network;
pub mod power;
mod process;

pub use audio::{get_volume, set_mute, set_volume, toggle_mute};
pub use bluetooth::{get_bluetooth, list_bt_devices, set_bluetooth, BtDeviceInfo};
pub use display::{get_brightness, set_brightness};
pub use network::{connect_wifi, get_wifi, list_wifi_networks, set_wifi, WifiNetworkInfo};
pub use power::{get_power_profile, set_power_profile, PowerProfile};

use std::time::Duration;

use tracing::warn;

/// Side-effect interface for the OS actions. The default production impl
/// ([`RealDispatcher`]) calls into the free functions in the per-domain
/// modules; tests substitute a recording impl so the click handler can
/// be asserted on without shelling out.
///
/// Shell call sites historically named this trait `QsActions`; a
/// type-alias in `shell::qs_actions` preserves the old name.
pub trait Dispatcher {
    fn set_brightness(&self, pct: f32) -> Result<(), String>;
    fn set_volume(&self, pct: f32) -> Result<(), String>;
    fn set_wifi(&self, enabled: bool) -> Result<(), String>;
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String>;
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String>;
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String>;
    /// Toggle the default audio sink mute state. Default impl delegates
    /// to [`audio::toggle_mute`]. Has a default body so existing impls
    /// in the shell crate continue to compile.
    fn toggle_mute(&self) -> Result<(), String> {
        audio::toggle_mute()
    }
}

/// Production impl — every method forwards to the free shell-out helper
/// of the same name in the per-domain modules. Stateless; cheap to
/// construct (zero-sized struct).
#[derive(Default, Clone, Copy, Debug)]
pub struct RealDispatcher;

impl Dispatcher for RealDispatcher {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        display::set_brightness(pct)
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        audio::set_volume(pct)
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        network::set_wifi(enabled)
    }
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        bluetooth::set_bluetooth(enabled)
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        network::connect_wifi(ssid, password)
    }
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
        power::set_power_profile(profile)
    }
    fn toggle_mute(&self) -> Result<(), String> {
        audio::toggle_mute()
    }
}

/// Wrapper around a `Result<(), String>` that funnels the error into
/// `tracing::warn!` with a stable prefix. Call-sites use this so a
/// click-handler stays a one-liner:
///
/// ```ignore
/// log_result("set_brightness", os_actions::set_brightness(value));
/// ```
pub fn log_result(op: &str, result: Result<(), String>) {
    if let Err(e) = result {
        warn!(target: "qos::os_actions", op = op, error = %e, "os action failed");
    }
}

/// Same as [`log_result`] but for getter-shaped results that return a
/// value alongside the error.
pub fn log_get<T>(op: &str, result: Result<T, String>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(target: "qos::os_actions", op = op, error = %e, "os get failed");
            None
        }
    }
}

/// Default short timeout for any future async/blocking guard — kept as a
/// constant so call-sites that want to enforce a deadline have a single
/// value to import. Not used by the sync shell-outs because
/// `std::process::Command::output` already inherits the OS-level SIGTERM
/// behaviour of the parent shell, and the controls are user-driven so a
/// hung command surfaces as visible latency.
#[allow(dead_code)]
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2_500);

#[cfg(test)]
mod dispatcher_tests {
    use super::*;
    use std::cell::RefCell;

    /// Recording dispatcher used by the dispatcher-level tests. Captures
    /// every call into a `Vec<String>` so the test can assert the click
    /// translated into the right shell-out.
    #[derive(Default)]
    struct RecordingDispatcher {
        calls: RefCell<Vec<String>>,
    }

    impl RecordingDispatcher {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Dispatcher for RecordingDispatcher {
        fn set_brightness(&self, pct: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_brightness({pct:.2})"));
            Ok(())
        }
        fn set_volume(&self, pct: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_volume({pct:.2})"));
            Ok(())
        }
        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_wifi({enabled})"));
            Ok(())
        }
        fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_bluetooth({enabled})"));
            Ok(())
        }
        fn connect_wifi(&self, ssid: &str, _password: Option<&str>) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("connect_wifi({ssid})"));
            Ok(())
        }
        fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_power_profile({})", profile.cli_arg()));
            Ok(())
        }
        fn toggle_mute(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("toggle_mute()".to_string());
            Ok(())
        }
    }

    #[test]
    fn dispatcher_records_calls() {
        let d = RecordingDispatcher::default();
        d.set_brightness(60.0).unwrap();
        d.set_volume(75.0).unwrap();
        d.set_wifi(false).unwrap();
        d.set_bluetooth(true).unwrap();
        d.connect_wifi("MyNet", Some("hunter2")).unwrap();
        d.set_power_profile(PowerProfile::Performance).unwrap();
        d.toggle_mute().unwrap();

        let calls = d.calls();
        assert_eq!(calls.len(), 7);
        assert_eq!(calls[0], "set_brightness(60.00)");
        assert_eq!(calls[1], "set_volume(75.00)");
        assert_eq!(calls[2], "set_wifi(false)");
        assert_eq!(calls[3], "set_bluetooth(true)");
        assert_eq!(calls[4], "connect_wifi(MyNet)");
        assert_eq!(calls[5], "set_power_profile(performance)");
        assert_eq!(calls[6], "toggle_mute()");
    }
}
