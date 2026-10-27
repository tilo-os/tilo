//! The Wi-Fi tile against a fake `NetworkManager`: every press lands in the
//! state the radio is really in, read back after the change, and a radio
//! that cannot turn on says why.

use super::*;
use crate::shell::quick_settings::radio::WifiProblem;
use parking_lot::Mutex;

/// `NetworkManager` and the kernel's kill switch, as far as Wi-Fi on/off
/// goes: its flag, a soft block (which turning the flag off sets, and
/// turning it on lifts) and a hard block nothing lifts.
#[derive(Clone)]
struct FakeNm(Arc<Mutex<FakeRadio>>);

struct FakeRadio {
    flag: bool,
    soft: bool,
    hard: bool,
}

impl FakeNm {
    fn on() -> Self {
        Self(Arc::new(Mutex::new(FakeRadio {
            flag: true,
            soft: false,
            hard: false,
        })))
    }

    fn radios(&self) -> os_actions::RadioState {
        let radio = self.0.lock();
        os_actions::RadioState {
            wifi_available: true,
            wifi_enabled: radio.flag && !radio.soft && !radio.hard,
            wifi_hardware_blocked: radio.hard,
            ..os_actions::RadioState::default()
        }
    }
}

impl os_actions::Dispatcher for FakeNm {
    fn prepare_radio_change(
        &self,
        change: os_actions::RadioChange,
    ) -> Option<os_actions::RadioWork> {
        let nm = self.clone();
        Some(Box::new(move || nm.apply_radio_change(change)))
    }
    fn apply_radio_change(
        &self,
        change: os_actions::RadioChange,
    ) -> Result<Option<os_actions::RadioState>, String> {
        if let os_actions::RadioChange::Wifi(on) = change {
            let mut radio = self.0.lock();
            radio.flag = on;
            radio.soft = !on;
        }
        Ok(Some(self.radios()))
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.apply_radio_change(os_actions::RadioChange::Wifi(enabled))
            .map(|_| ())
    }
    fn set_brightness(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_volume(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_bluetooth(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn connect_wifi(&self, _: &str, _: Option<&str>) -> Result<(), String> {
        Ok(())
    }
    fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
        Ok(())
    }
}

struct Panel {
    host: ShellOverlayHost,
    chrome: Chrome,
    wake: std::sync::mpsc::Receiver<()>,
}

impl Panel {
    fn new(nm: &FakeNm) -> Self {
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::bootstrap_demo(),
        );
        {
            let mut state = chrome.borrow_mut();
            state.quick_settings.radios = nm.radios();
            state.quick_settings.wifi = nm.radios().wifi_enabled;
        }
        let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
            .with_chrome(chrome.clone())
            .with_qs_actions(Box::new(nm.clone()));
        let (wake_tx, wake) = std::sync::mpsc::channel();
        host.set_quick_settings_wake(Arc::new(move || {
            let _ = wake_tx.send(());
        }));
        Self { host, chrome, wake }
    }

    /// Press the Wi-Fi tile's body (not its chevron), as a pointer would.
    fn press_wifi(&mut self) {
        let panel = Rect::new(100.0, 48.0, 500.0, 900.0);
        let action = {
            let mut state = self.chrome.borrow_mut();
            let qs = &mut state.quick_settings;
            let _ = self
                .host
                .qs_root
                .handle_click(Point::new(-10.0, -10.0), panel, qs);
            let tile = self
                .host
                .qs_root
                .semantic_rects(qs)
                .into_iter()
                .find(|(name, _)| name == "quick_settings_network")
                .expect("the Wi-Fi tile")
                .1;
            self.host
                .qs_root
                .handle_click(Point::new(tile.x0 + 8.0, tile.center().y), panel, qs)
        };
        self.host
            .apply_quick_settings_root_action(action, &self.chrome.clone());
    }

    /// Let the radio worker finish and land its result.
    fn settle(&mut self) {
        tilo_test_env::recv("the radio change to complete", &self.wake);
        let doc = automerge::AutoCommit::new();
        self.host.update(&doc, &automerge::ROOT);
    }
}

#[test]
fn the_wifi_tile_turns_wifi_off_and_on_again_and_shows_what_the_radio_did() {
    let nm = FakeNm::on();
    let mut panel = Panel::new(&nm);
    for on in [false, true, false, true] {
        panel.press_wifi();
        {
            let state = panel.chrome.borrow();
            assert!(state.quick_settings.radio_change_pending, "turning {on}");
            assert_eq!(
                state.quick_settings.radio_change,
                Some(os_actions::RadioChange::Wifi(on)),
                "the tile says which way it is turning"
            );
            assert_eq!(
                state.quick_settings.wifi, !on,
                "not before the radio answers"
            );
        }
        panel.settle();
        let state = panel.chrome.borrow();
        assert!(!state.quick_settings.radio_change_pending);
        assert_eq!(state.quick_settings.wifi, on, "after pressing for {on}");
        assert_eq!(state.quick_settings.radios.wifi_enabled, on);
        assert_eq!(nm.radios().wifi_enabled, on, "and the radio is");
        assert_eq!(state.quick_settings.wifi_problem, None);
        assert!(state.notifications().is_empty());
    }
}

#[test]
fn wifi_held_off_by_the_hardware_switch_stays_off_and_says_why() {
    let nm = FakeNm::on();
    let mut panel = Panel::new(&nm);
    panel.press_wifi();
    panel.settle();
    assert!(!panel.chrome.borrow().quick_settings.wifi);
    nm.0.lock().hard = true;

    panel.press_wifi();
    panel.settle();
    {
        let state = panel.chrome.borrow();
        assert!(
            !state.quick_settings.wifi,
            "the radio's state, not the request"
        );
        assert!(state.quick_settings.radios.wifi_hardware_blocked);
        assert_eq!(
            state.quick_settings.wifi_problem,
            Some(WifiProblem::HardwareSwitch)
        );
        assert_eq!(state.notifications().len(), 1);
    }

    // The switch is flipped back: the next press turns it on and the
    // reason goes.
    nm.0.lock().hard = false;
    panel.press_wifi();
    panel.settle();
    let state = panel.chrome.borrow();
    assert!(state.quick_settings.wifi);
    assert_eq!(state.quick_settings.wifi_problem, None);
}
