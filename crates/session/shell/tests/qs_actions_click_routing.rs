//! Click-routing wire-up for the quick-settings panel — not a repeat of
//! the parser-level unit tests in `os-actions`, which
//! pin the brightnessctl/wpctl/nmcli/bluetoothctl stdout shapes. Builds a
//! real [`ShellOverlayHost`], swaps in a [`RecordingDispatcher`], forces
//! the popover open, drives `handle_event` with a synthetic click at the
//! slider's bar rect, and reads the dispatcher's call log.

#![allow(
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::FreshId as _;
use std::cell::RefCell;
use std::collections::HashMap;

use settings_store::hardware::QuickSettingsConfig;
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use taffy::{NodeId, TaffyTree};
use tilo_quick_settings::WifiNetwork;
use tilo_quick_settings::actions::{PowerProfile, QsActions};
use vello::kurbo::Point;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::support::host::{click, layout_host, paint, press_key};

#[derive(Default)]
struct RecordingDispatcher {
    calls: RefCell<Vec<String>>,
}

impl RecordingDispatcher {
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl QsActions for RecordingDispatcher {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("set_brightness({pct:.0})"));
        Ok(())
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("set_volume({pct:.0})"));
        Ok(())
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("set_wifi({enabled})"));
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
}

/// Wraps the recorder in an `Rc` so the test can read calls back after
/// the dispatcher moves into the host's `Box<dyn QsActions>`.
#[derive(Default, Clone)]
struct SharedRecorder(std::rc::Rc<RecordingDispatcher>);

impl QsActions for SharedRecorder {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        self.0.set_brightness(pct)
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        self.0.set_volume(pct)
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.0.set_wifi(enabled)
    }
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        self.0.set_bluetooth(enabled)
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        self.0.connect_wifi(ssid, password)
    }
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
        self.0.set_power_profile(profile)
    }
}

/// The smallest topology the chrome can render — the QS popover sits
/// over workspace content the test never touches.
fn minimal_tree() -> WorkspaceTree {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 1;
    let tab_id: u64 = 0xc0;
    let group = WindowGroup {
        id: group_id,
        tabs: vec![Tab {
            id: tab_id,
            title: "T".into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: "stub".into(),
            },
            app_title: None,
        }],
        active_tab: tab_id,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let strip = Strip {
        id: strip_id,
        name: "S".into(),
        window_groups: vec![group],
    };
    let ws = {
        let mut workspace = Workspace::new(ws_id, "W");
        workspace.strips = vec![strip];
        workspace
    };
    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, group_id);
    WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    }
}

fn open_quick_settings(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    chrome: &shell::shell::chrome_state::Chrome,
) -> (TaffyTree<()>, NodeId) {
    {
        let mut state = chrome.borrow_mut();
        state.overlays.quick_settings = true;
        state.quick_settings.brightness_available = true;
        state.quick_settings.radios.wifi_available = true;
        state.quick_settings.bluetooth_available = true;
        state.quick_settings.status.volume = Some(SOUND_OUTPUT_READING);
        state.bump();
    }
    // The panel opens on a laid-out desktop, under its tray.
    let _ = layout_host(host);
    host.force_quick_settings_settled();
    layout_host(host)
}

/// A volume slider shows only once the sound output has been read, as a
/// real session reads it on open.
const SOUND_OUTPUT_READING: media::audio::default_node::VolumeState =
    media::audio::default_node::VolumeState {
        level: 0.4,
        muted: false,
    };

#[test]
fn workspace_settings_invite_button_opens_the_signed_composer() {
    #[cfg(feature = "test-support")]
    {
        use shell::shell::chrome_state::WsTab;

        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let hosted = shell::shell::test_support::host_active_room(&chrome.borrow().tree_handle());
        chrome.borrow_mut().act_as(hosted.person);
        host.set_peer_sync_enabled(true);
        {
            let mut st = chrome.borrow_mut();
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::General;
            st.bump();
        }
        let (taffy, host_node) = layout_host(&mut host);
        host.force_ws_settings_settled();
        // Paint once so the modal's geometry is populated the way it is in a
        // real frame before any click arrives.
        paint(&mut host);

        let btn = host.ws_settings_invite_rect_for_tests();
        assert!(
            btn.area() > 0.0,
            "the Create button has no geometry to click"
        );
        assert!(!host.peer_sync_card_visible());

        let c = btn.center();
        click(&mut host, &taffy, host_node, c);

        assert!(
            host.peer_sync_card_visible(),
            "clicking Create did not open the signed invite composer"
        );
        assert!(!chrome.borrow().overlays.ws_settings);
    }
}

// Guards against a hit rect widened until it swallows the rest of the tab.
#[test]
fn a_click_beside_the_invite_button_does_not_open_the_composer() {
    use shell::shell::chrome_state::WsTab;

    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = WsTab::General;
        st.bump();
    }
    let (taffy, host_node) = layout_host(&mut host);
    host.force_ws_settings_settled();
    paint(&mut host);

    let btn = host.ws_settings_invite_rect_for_tests();
    assert!(btn.area() > 0.0, "the Create button is laid out");
    let p = Point::new(btn.x1 + 160.0, btn.center().y);
    click(&mut host, &taffy, host_node, p);

    assert!(
        !host.peer_sync_card_visible(),
        "a click beside the Create button opened the composer anyway"
    );
}

/// The ribbon's own geometry is unit-tested in `ui`, but geometry that
/// nothing routes is exactly the defect the ws-settings Copy button was
/// — this drives real clicks and asserts `active_strip` moved.
#[test]
fn clicking_a_strip_ribbon_segment_moves_focus() {
    let mut tree = minimal_tree();
    let ws_id = tree.desktop.workspaces[0].id;
    for n in 0..2u64 {
        tree.desktop.workspaces[0].strips.push(Strip {
            id: StripId(700 + n),
            name: format!("S{n}"),
            window_groups: vec![],
        });
    }
    let first = tree.desktop.workspaces[0].strips[0].id;
    tree.desktop.active_strip.insert(ws_id, first);

    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = layout_host(&mut host);

    let ribbon = host.strip_ribbon_for_tests();
    assert_eq!(ribbon.count, 3, "fixture should present three strips");
    assert_eq!(ribbon.focused, 0, "focus starts on the first strip");

    let target = ribbon.hit_rect(2).expect("third segment has a target");
    let c = target.center();
    // A tap on the ribbon opens the strip switcher's labels, pinned (a
    // finger cannot rest there to open them); the third strip's label row
    // reaches under its mark, so a second tap there takes it.
    click(&mut host, &taffy, host_node, c);
    assert!(
        chrome.borrow().strip_switcher_open(),
        "a tap on the ribbon opens the strip switcher"
    );
    assert_eq!(chrome.borrow().strip_focus().1, 0, "nothing moved yet");
    click(&mut host, &taffy, host_node, c);

    let (_, focused) = chrome.borrow().strip_focus();
    assert_eq!(
        focused, 2,
        "the third strip's label, over its ribbon segment, focuses the third strip"
    );
}

/// Where the panel's control `name` is on screen (`qs-tile-wifi`, …).
fn target(host: &shell::shell::overlay_host::ShellOverlayHost, name: &str) -> vello::kurbo::Rect {
    host.quick_settings_target(name)
        .unwrap_or_else(|| panic!("{name} is not on the panel"))
}

/// A host with Quick Settings open over a laptop's readings, its OS writes
/// recorded.
fn open_panel(
    recorder: &SharedRecorder,
) -> (
    shell::shell::overlay_host::ShellOverlayHost,
    shell::shell::chrome_state::Chrome,
    TaffyTree<()>,
    NodeId,
) {
    let tree = minimal_tree();
    let (host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let mut host = host
        .with_qs_actions(Box::new(recorder.clone()))
        .with_quick_settings_config(QuickSettingsConfig::default());
    {
        let mut state = chrome.borrow_mut();
        state.quick_settings.wifi = true;
        state.quick_settings.bluetooth = true;
        state.quick_settings.wifi_networks = vec![WifiNetwork {
            ssid: "studio-5g".into(),
            signal: 4,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected: false,
            known: true,
        }];
        state.quick_settings.power_profiles = vec![
            tilo_quick_settings::PowerProfile::Saver,
            tilo_quick_settings::PowerProfile::Balanced,
        ];
    }
    let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
    (host, chrome, taffy, host_node)
}

/// Press `point` and let the panel answer.
fn press(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    taffy: &TaffyTree<()>,
    node: NodeId,
    point: Point,
) {
    click(host, taffy, node, point);
    host.force_quick_settings_settled();
}

#[test]
fn quick_settings_controls_reach_the_device_by_pointer_and_keyboard() {
    let recorder = SharedRecorder::default();
    let (mut host, chrome, taffy, node) = open_panel(&recorder);

    // The Wi-Fi chevron opens its networks; a known one joins at a press.
    // (Its tile, pressed below, turns Wi-Fi off and the list with it.)
    let wifi = target(&host, "qs-tile-wifi");
    press(
        &mut host,
        &taffy,
        node,
        Point::new(wifi.x1 - 8.0, wifi.center().y),
    );
    assert_eq!(
        chrome.borrow().quick_settings.section,
        Some(tilo_quick_settings::QsSection::Wifi)
    );
    let network = target(&host, "qs-network-studio-5g");
    press(&mut host, &taffy, node, network.center());
    assert_eq!(
        chrome.borrow().quick_settings.wifi_joining.as_deref(),
        Some("studio-5g"),
        "a known network joins without asking"
    );

    let brightness = target(&host, "qs-brightness");
    press(
        &mut host,
        &taffy,
        node,
        Point::new(
            brightness.x0 + brightness.width() / 3.0,
            brightness.center().y,
        ),
    );
    let volume = target(&host, "qs-volume");
    press(&mut host, &taffy, node, volume.center());
    for tile in ["qs-tile-wifi", "qs-tile-bluetooth", "qs-tile-power_mode"] {
        let rect = target(&host, tile);
        press(
            &mut host,
            &taffy,
            node,
            Point::new(rect.x0 + 12.0, rect.center().y),
        );
    }
    let calls = recorder.0.calls();
    for expected in [
        "set_brightness(",
        "set_volume(",
        "set_wifi(false)",
        "set_bluetooth(false)",
        "set_power_profile(",
    ] {
        assert!(
            calls.iter().any(|call| call.starts_with(expected)),
            "{expected} did not reach the device: {calls:?}"
        );
    }

    // The keyboard: the first Tab reaches brightness, an arrow sets it.
    let before = recorder.0.calls().len();
    assert!(press_key(
        &mut host,
        &taffy,
        node,
        Key::Named(NamedKey::Tab),
        ModifiersState::empty()
    ));
    assert!(press_key(
        &mut host,
        &taffy,
        node,
        Key::Named(NamedKey::ArrowRight),
        ModifiersState::empty()
    ));
    host.force_quick_settings_settled();
    assert!(
        recorder.0.calls()[before..]
            .iter()
            .any(|call| call.starts_with("set_brightness(")),
        "an arrow on the focused slider sets it: {:?}",
        recorder.0.calls()
    );
}

#[test]
fn every_quick_settings_destination_click_lands_on_its_surface() {
    for (name, expect) in [
        ("qs-row-notifications", "notifications"),
        ("qs-row-devices", "devices"),
        ("qs-row-clipboard", "clipboard"),
        ("qs-power", "power"),
    ] {
        let recorder = SharedRecorder::default();
        let (mut host, chrome, taffy, node) = open_panel(&recorder);
        let rect = target(&host, name);
        press(&mut host, &taffy, node, rect.center());
        let state = chrome.borrow();
        match expect {
            "notifications" => assert_eq!(
                state.quick_settings.current_level(),
                &tilo_quick_settings::QsLevel::Notifications
            ),
            "devices" => assert_eq!(
                state.quick_settings.section,
                Some(tilo_quick_settings::QsSection::Devices)
            ),
            "clipboard" => assert_eq!(
                state.quick_settings.section,
                Some(tilo_quick_settings::QsSection::Clipboard)
            ),
            _ => {
                assert!(
                    !state.overlays.quick_settings,
                    "the power menu replaces the panel"
                );
                drop(state);
                assert!(host.power_menu_open());
            }
        }
    }
}

#[test]
fn do_not_disturb_tile_click_holds_notifications() {
    let recorder = SharedRecorder::default();
    let (mut host, chrome, taffy, node) = open_panel(&recorder);
    assert!(!chrome.borrow().dnd());
    let tile = target(&host, "qs-tile-do_not_disturb");
    let at = Point::new(tile.x0 + 12.0, tile.center().y);
    press(&mut host, &taffy, node, at);
    {
        let st = chrome.borrow();
        assert!(st.dnd(), "the tile turns Do Not Disturb on");
        assert!(st.quick_settings.do_not_disturb);
    }
    chrome.borrow_mut().push_notification("Held", "while quiet");
    {
        let st = chrome.borrow();
        assert!(st.notifications().is_empty());
        assert_eq!(st.notification_history().len(), 1);
    }
    press(&mut host, &taffy, node, at);
    assert!(!chrome.borrow().dnd(), "a second press turns it back off");
}
