//! The OS side of Quick Settings: the dispatcher the panel's effects reach
//! the device through, what this device's backends can do, the radio change
//! under way, and the checks every value passes before it reaches the OS.
//!
//! Search, the launcher and agents reach Quick Settings through the panel's
//! own effects, published in this device's catalogue of private windows
//! ([`crate::shell::device_local_actions`]); the overlay host carries each
//! one out through this dispatcher.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::shell::chrome_state::Chrome;
use tilo_quick_settings::actions::{PowerProfile, QsActionAvailability, QsActions};

/// The dispatcher the overlay host carries the panel's effects out with,
/// swapped whole when the session's backend changes.
pub(crate) struct SharedQsDispatcher {
    actions: RefCell<Rc<dyn QsActions>>,
    availability: Cell<QsActionAvailability>,
    radio_job: RefCell<Option<tilo_quick_settings::radio::RadioTicket>>,
    radio_wake: RefCell<Option<crate::session_runtime::Wake>>,
}

impl SharedQsDispatcher {
    pub(crate) fn new(actions: Rc<dyn QsActions>, availability: QsActionAvailability) -> Self {
        Self {
            actions: RefCell::new(actions),
            availability: Cell::new(availability),
            radio_job: RefCell::new(None),
            radio_wake: RefCell::new(None),
        }
    }

    pub(crate) fn replace(&self, actions: Rc<dyn QsActions>, availability: QsActionAvailability) {
        if let Some(job) = self.radio_job.borrow().as_ref() {
            job.retire();
        }
        *self.actions.borrow_mut() = actions;
        self.availability.set(availability);
    }

    /// What this device's backends can do, as last probed or read.
    pub(crate) fn availability(&self) -> QsActionAvailability {
        self.availability.get()
    }

    pub(crate) fn set_radio_wake(&self, wake: crate::session_runtime::Wake) {
        *self.radio_wake.borrow_mut() = Some(wake);
    }

    pub(crate) fn observe_radio_availability(&self, radios: &os_actions::RadioState) {
        let mut availability = self.availability.get();
        availability.wifi = radios.wifi_available;
        availability.airplane_mode = radios.wifi_available || radios.wwan_available;
        self.availability.set(availability);
    }

    pub(crate) fn start_radio(
        &self,
        change: os_actions::RadioChange,
        chrome: &Chrome,
    ) -> Result<Option<tilo_quick_settings::radio::RadioTicket>, String> {
        let availability = self.availability.get();
        let available = match change {
            os_actions::RadioChange::Wifi(_) => availability.wifi,
            os_actions::RadioChange::Airplane(_) => availability.airplane_mode,
        };
        if !available {
            return Err("radio action is unavailable".into());
        }
        if self.radio_job.borrow().is_some() {
            return Err("a radio change is already pending".into());
        }
        let actions = self.actions.borrow().clone();
        if let Some(work) = actions.prepare_radio_change(change) {
            let job = tilo_quick_settings::radio::RadioTicket::start(
                change,
                work,
                self.radio_wake.borrow().clone(),
            )?;
            *self.radio_job.borrow_mut() = Some(job.clone());
            let mut state = chrome.borrow_mut();
            state.quick_settings.radio_change_pending = true;
            state.quick_settings.radio_change = Some(change);
            state.bump();
            Ok(Some(job))
        } else {
            let radios = change.run(actions.as_ref())?;
            let mut state = chrome.borrow_mut();
            let landed =
                tilo_quick_settings::radio::apply(change, Ok(radios), &mut state.quick_settings);
            state.land_radio_change(landed);
            Ok(None)
        }
    }

    pub(crate) fn take_radio_completion(&self) -> Option<tilo_quick_settings::radio::RadioTicket> {
        let mut pending = self.radio_job.borrow_mut();
        if !pending.as_ref()?.finished() {
            return None;
        }
        let _ = pending.as_ref()?.result()?;
        pending.take()
    }
}

pub(crate) type SharedQsActions = Rc<SharedQsDispatcher>;

/// Exact percentages the OS takes: 0 through 100.
fn percent(value: f32) -> Result<f32, String> {
    if value.is_finite() && (0.0..=100.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{value} is not a percentage from 0 through 100"))
    }
}

/// A name the OS takes: not empty and at most `max` characters.
fn name(value: &str, what: &str, max: usize) -> Result<(), String> {
    let length = value.chars().count();
    if length == 0 || length > max {
        Err(format!("a {what} is 1 to {max} characters"))
    } else {
        Ok(())
    }
}

/// The dispatcher as the panel's effects reach it: every level, connector
/// and profile name is checked before it reaches the OS, and a refusal is
/// the effect's answer. Operations that take nothing to check pass through.
pub(crate) struct ValidatedQsActions<'a> {
    inner: &'a dyn QsActions,
}

impl<'a> ValidatedQsActions<'a> {
    pub(crate) fn new(inner: &'a dyn QsActions) -> Self {
        Self { inner }
    }
}

impl QsActions for ValidatedQsActions<'_> {
    fn prepare_radio_change(
        &self,
        change: os_actions::RadioChange,
    ) -> Option<os_actions::RadioWork> {
        self.inner.prepare_radio_change(change)
    }

    fn apply_radio_change(
        &self,
        change: os_actions::RadioChange,
    ) -> Result<Option<os_actions::RadioState>, String> {
        self.inner.apply_radio_change(change)
    }

    fn set_brightness(&self, percent_value: f32) -> Result<(), String> {
        self.inner.set_brightness(percent(percent_value)?)
    }

    fn set_display_brightness(&self, connector: &str, percent_value: f32) -> Result<(), String> {
        name(connector, "display connector", 64)?;
        self.inner
            .set_display_brightness(connector, percent(percent_value)?)
    }

    fn set_volume(&self, percent_value: f32) -> Result<(), String> {
        self.inner.set_volume(percent(percent_value)?)
    }

    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_wifi(enabled)
    }

    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
        self.inner.set_power_profile(profile)
    }

    fn set_vpn_connection(&self, connection: &str, enabled: bool) -> Result<(), String> {
        name(connection, "VPN connection name", 255)?;
        self.inner.set_vpn_connection(connection, enabled)
    }

    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        self.inner.connect_wifi(ssid, password)
    }

    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_bluetooth(enabled)
    }

    fn connect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.inner.connect_bluetooth(address)
    }

    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.inner.disconnect_bluetooth(address)
    }

    fn set_wired(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_wired(enabled)
    }

    fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_airplane_mode(enabled)
    }

    fn set_hotspot(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_hotspot(enabled)
    }

    fn set_mobile_broadband(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_mobile_broadband(enabled)
    }

    fn toggle_mute(&self) -> Result<(), String> {
        self.inner.toggle_mute()
    }

    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.inner.set_mute(muted)
    }

    fn brightness(&self) -> Result<f32, String> {
        self.inner.brightness()
    }

    fn keyboard_backlight(&self) -> Result<os_actions::KeyboardBacklight, String> {
        self.inner.keyboard_backlight()
    }

    fn set_keyboard_backlight(&self, level: u32) -> Result<(), String> {
        self.inner.set_keyboard_backlight(level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
        fail: Cell<bool>,
    }

    impl Recorder {
        fn result(&self) -> Result<(), String> {
            if self.fail.get() {
                Err("controlled provider failure".into())
            } else {
                Ok(())
            }
        }
    }

    impl QsActions for Recorder {
        fn set_brightness(&self, percent: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("brightness:{percent}"));
            self.result()
        }

        fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("brightness:{connector}:{percent}"));
            self.result()
        }

        fn set_volume(&self, percent: f32) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("volume:{percent}"));
            self.result()
        }

        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("wifi:{enabled}"));
            self.result()
        }

        fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("bluetooth:{enabled}"));
            self.result()
        }

        fn connect_wifi(&self, ssid: &str, _password: Option<&str>) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("connect-wifi:{ssid}"));
            self.result()
        }

        fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("vpn:{name}:{enabled}"));
            self.result()
        }

        fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("power:{profile:?}"));
            self.result()
        }
    }

    /// A value the OS would refuse never reaches it, and the OS's own
    /// refusal is the caller's answer.
    #[test]
    fn values_are_checked_before_they_reach_the_os() {
        let recorder = Recorder::default();
        let validated = ValidatedQsActions::new(&recorder);
        validated.set_brightness(37.5).expect("a percentage passes");
        validated.set_volume(64.0).expect("a percentage passes");
        validated
            .set_display_brightness("DP-2", 40.0)
            .expect("a connector and a percentage pass");
        validated.set_wifi(true).expect("a switch passes");
        validated
            .set_power_profile(PowerProfile::Saver)
            .expect("a profile passes");
        validated
            .set_vpn_connection("Work", false)
            .expect("a profile name passes");
        assert_eq!(
            recorder.calls.borrow().as_slice(),
            [
                "brightness:37.5",
                "volume:64",
                "brightness:DP-2:40",
                "wifi:true",
                "power:Saver",
                "vpn:Work:false",
            ]
        );
        assert!(validated.set_brightness(101.0).is_err());
        assert!(validated.set_volume(-1.0).is_err());
        assert!(validated.set_volume(f32::NAN).is_err());
        assert!(validated.set_display_brightness("", 40.0).is_err());
        assert!(validated.set_vpn_connection("", true).is_err());
        assert_eq!(
            recorder.calls.borrow().len(),
            6,
            "nothing invalid dispatched"
        );
        recorder.fail.set(true);
        let error = validated
            .set_brightness(50.0)
            .expect_err("the OS's refusal reaches the caller");
        assert!(error.contains("controlled"));
    }

    /// A radio change starts only where the device has the radio and only
    /// one at a time; its work runs off the loop and lands once.
    #[test]
    fn a_radio_change_waits_for_its_work_and_lands_once() {
        use tilo_quick_settings::radio::fixture::Fixture;
        let fixture = Fixture::new();
        let slot = SharedQsDispatcher::new(
            Rc::new(fixture.dispatcher.clone()),
            QsActionAvailability::default(),
        );
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::bootstrap_default(),
        );
        assert!(
            slot.start_radio(os_actions::RadioChange::Wifi(true), &chrome)
                .is_err(),
            "no radio, no change"
        );
        assert!(fixture.entered.try_recv().is_err());
        slot.replace(
            Rc::new(fixture.dispatcher.clone()),
            QsActionAvailability::all(),
        );
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        slot.set_radio_wake(std::sync::Arc::new(move || {
            let _ = wake_tx.send(());
        }));
        let job = slot
            .start_radio(os_actions::RadioChange::Wifi(true), &chrome)
            .expect("the radio is there")
            .expect("the change runs off the loop");
        assert_eq!(
            tilo_test_env::recv("the radio work to start", &fixture.entered),
            os_actions::RadioChange::Wifi(true)
        );
        assert!(chrome.borrow().quick_settings.radio_change_pending);
        assert!(
            slot.start_radio(os_actions::RadioChange::Airplane(true), &chrome)
                .is_err(),
            "one radio change at a time"
        );
        assert!(slot.take_radio_completion().is_none(), "still running");
        fixture.release.send(Ok(())).unwrap();
        tilo_test_env::recv("the radio work's wake", &wake_rx);
        assert!(job.finished());
        let completion = slot
            .take_radio_completion()
            .expect("the finished change is taken");
        crate::shell::chrome_state::ChromeState::apply_radio_ticket(&chrome, &completion);
        assert!(chrome.borrow().quick_settings.wifi);
        assert!(slot.take_radio_completion().is_none(), "taken once");
    }
}
