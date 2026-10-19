//! Quick Settings reads device state once on open, reads back only what a
//! write touched, and shares the tray's volume.

use super::*;
use crate::shell::quick_settings::refresh::{FastReading, Kick, QsField, QsReading};
use crate::shell::status::StatusBridge;

/// Records every write, so two paths can be compared call for call.
#[derive(Clone, Default)]
pub(super) struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

impl Recorder {
    fn push(&self, call: String) {
        self.0.borrow_mut().push(call);
    }
    pub(super) fn calls(&self) -> Vec<String> {
        self.0.borrow().clone()
    }
}

impl crate::shell::quick_settings::actions::QsActions for Recorder {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        self.push(format!("brightness={pct:.1}"));
        Ok(())
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        self.push(format!("volume={pct:.1}"));
        Ok(())
    }
    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.push(format!("mute={muted}"));
        Ok(())
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.push(format!("wifi={enabled}"));
        Ok(())
    }
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        self.push(format!("bluetooth={enabled}"));
        Ok(())
    }
    fn connect_wifi(&self, ssid: &str, _: Option<&str>) -> Result<(), String> {
        self.push(format!("connect={ssid}"));
        Ok(())
    }
    fn set_power_profile(&self, profile: os_actions::PowerProfile) -> Result<(), String> {
        self.push(format!("power={}", profile.cli_arg()));
        Ok(())
    }
    fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
        self.push(format!("airplane={enabled}"));
        Ok(())
    }
    fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
        self.push(format!("vpn={name}:{enabled}"));
        Ok(())
    }
    fn connect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.push(format!("bluetooth-connect={address}"));
        Ok(())
    }
    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.push(format!("bluetooth-disconnect={address}"));
        Ok(())
    }
}

fn bridge_at(level: f32, muted: bool) -> StatusBridge {
    let bridge = StatusBridge::new();
    bridge.set_volume(os_actions::VolumeState { level, muted });
    bridge
}

fn host_with(
    actions: &Recorder,
    bridge: Option<StatusBridge>,
) -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_qs_actions(Box::new(actions.clone()));
    if let Some(bridge) = bridge {
        host = host.with_status_bridge(bridge);
    }
    (host, chrome)
}

#[test]
fn toggle_reconciles_only_its_field() {
    let actions = Recorder::default();
    let (mut host, chrome) = host_with(&actions, None);
    chrome.borrow_mut().quick_settings.bluetooth = true;

    host.reconcile_quick_settings_open_edge(true);
    assert_eq!(
        host.qs_refresh.take_kicks(),
        [Kick::Fast],
        "opening the panel reads everything once"
    );

    host.apply_quick_settings_root_action(
        RootAction::Changed(crate::shell::quick_settings::root::PendingRootActions {
            bluetooth: Some(false),
            ..Default::default()
        }),
        &chrome,
    );
    assert_eq!(actions.calls(), ["bluetooth=false"]);
    assert_eq!(
        host.qs_refresh.take_kicks(),
        [Kick::Field(QsField::Bluetooth)],
        "a Bluetooth toggle reads back Bluetooth, not every daemon on the machine"
    );

    host.apply_quick_settings_root_action(
        RootAction::Changed(crate::shell::quick_settings::root::PendingRootActions {
            power: Some(crate::shell::quick_settings::PowerProfile::Saver),
            ..Default::default()
        }),
        &chrome,
    );
    assert_eq!(host.qs_refresh.take_kicks(), [Kick::Field(QsField::Power)]);

    host.reconcile_quick_settings_open_edge(true);
    assert!(
        host.qs_refresh.take_kicks().is_empty(),
        "a panel that stays open is not opened again"
    );
}

#[test]
fn slider_and_media_key_share_commit_volume() {
    // The slider moves a muted sink from 50% to 55%...
    let slider_actions = Recorder::default();
    let slider_bridge = bridge_at(0.5, true);
    let (mut host, chrome) = host_with(&slider_actions, Some(slider_bridge.clone()));
    host.commit_quick_settings_slider(SliderCommit::Volume(55.0));
    let slider_osd = chrome.borrow().osd.expect("the slider raised a readout");
    assert_eq!(
        host.qs_refresh.take_kicks(),
        [Kick::Field(QsField::Volume)],
        "the commit reads back the volume alone"
    );

    // ...and so does a volume-up press on the same sink.
    let key_actions = Recorder::default();
    let key_bridge = bridge_at(0.5, true);
    let (transport_osd, _mailbox) = crate::session_runtime::mailbox(Arc::new(|| {}));
    let key_response = crate::shell::media_keys::apply(
        crate::shell::media_keys::MediaKey::VolumeUp,
        &crate::shell::media_keys::Context {
            actions: &key_actions,
            audio: &crate::shell::media_keys::DispatcherAudio(&key_actions),
            status: Some(&key_bridge),
            transport_osd: &transport_osd,
            airplane_mode: false,
            brightness: crate::shell::media_keys::BrightnessKeys::default(),
        },
    );

    let expected = os_actions::VolumeState {
        level: 0.55,
        muted: false,
    };
    assert_eq!(slider_bridge.snapshot().volume, Some(expected));
    assert_eq!(key_bridge.snapshot().volume, Some(expected));
    let Some(crate::shell::media_keys::Response::Osd(key_osd)) = key_response else {
        panic!("the key raised no readout: {key_response:?}");
    };
    assert_eq!(
        (key_osd.kind, key_osd.level),
        (slider_osd.kind, slider_osd.level),
        "both paths show the same readout"
    );
    assert_eq!(slider_osd.caption(), "Volume 55%");
    assert_eq!(slider_actions.calls(), ["volume=55.0", "mute=false"]);
    assert_eq!(slider_actions.calls(), key_actions.calls());
}

#[test]
fn qs_shows_the_tray_volume() {
    let actions = Recorder::default();
    let bridge = bridge_at(0.2, false);
    let (mut host, chrome) = host_with(&actions, Some(bridge.clone()));
    chrome.borrow_mut().overlays.quick_settings = true;
    let doc = automerge::AutoCommit::new();

    host.update(&doc, &automerge::ROOT);
    host.reconcile_quick_settings_open_edge(true);
    assert!(
        !host.qs_refresh.is_in_flight(),
        "the slider's value came from the tray, not a wpctl read"
    );
    assert!((chrome.borrow().quick_settings.volume - 0.2).abs() < f32::EPSILON);

    // A level that changes under the tray — another app, `wpctl` in a
    // terminal — moves the open panel's slider with it.
    bridge.set_volume(os_actions::VolumeState {
        level: 0.35,
        muted: false,
    });
    host.update(&doc, &automerge::ROOT);
    let state = chrome.borrow();
    assert!((state.quick_settings.volume - 0.35).abs() < f32::EPSILON);
    assert!(
        state
            .status
            .volume
            .is_some_and(|volume| (volume.level - 0.35).abs() < f32::EPSILON),
        "the tray shows the same level"
    );
}

#[test]
fn a_panel_volume_reading_updates_the_tray() {
    let bridge = bridge_at(0.2, false);
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_status_bridge(bridge.clone());
    let read = os_actions::VolumeState {
        level: 0.6,
        muted: true,
    };
    host.qs_refresh.kick_fast_with(move || {
        QsReading::Fast(Box::new(FastReading {
            volume_available: Some(true),
            volume: Some(read),
            ..FastReading::empty()
        }))
    });
    let deadline = tilo_test_env::hang_deadline();
    while host.qs_refresh.is_in_flight() && std::time::Instant::now() < deadline {
        host.apply_quick_settings_readings();
        std::thread::yield_now();
    }
    host.apply_quick_settings_readings();
    assert_eq!(
        bridge.snapshot().volume,
        Some(read),
        "the panel's read is the tray's read: one volume in the shell"
    );
    let state = chrome.borrow();
    assert!((state.quick_settings.volume - 0.6).abs() < f32::EPSILON);
    assert_eq!(state.status.volume, Some(read));
}
