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
use shell::shell::quick_settings::WifiNetwork;
use shell::shell::quick_settings::actions::{PowerProfile, QsActions};
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use taffy::{NodeId, TaffyTree};
use vello::kurbo::Point;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::widget::Widget as _;

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
    host.force_quick_settings_settled();
    layout_host(host)
}

/// A volume slider shows only once the sound output has been read, as a
/// real session reads it on open.
const SOUND_OUTPUT_READING: os_actions::VolumeState = os_actions::VolumeState {
    level: 0.4,
    muted: false,
};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn quick_settings_sliders_route_clicks_and_arrows_through_the_host() {
    // Phase `brightness_slider_click_routes_to_set_brightness`.
    {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let recorder = SharedRecorder::default();
        host = host.with_qs_actions(Box::new(recorder.clone()));

        {
            let mut st = chrome.borrow_mut();
            st.overlays.quick_settings = true;
            st.quick_settings.brightness_available = true;
            st.bump();
        }
        host.force_quick_settings_settled();
        let (taffy, host_node) = layout_host(&mut host);
        let bar = host.quick_settings_slider_geometry(0).0;
        let click_x = bar.x0 + bar.width() / 3.0;
        let click_y = f64::midpoint(bar.y0, bar.y1);
        click(&mut host, &taffy, host_node, Point::new(click_x, click_y));

        let calls = recorder.0.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("set_brightness(")),
            "expected a set_brightness call; got {calls:?}"
        );
    }

    // Phase `volume_slider_click_routes_to_set_volume`.
    {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let recorder = SharedRecorder::default();
        host = host.with_qs_actions(Box::new(recorder.clone()));

        {
            let mut st = chrome.borrow_mut();
            st.overlays.quick_settings = true;
            st.quick_settings.brightness_available = true;
            st.quick_settings.status.volume = Some(SOUND_OUTPUT_READING);
            st.bump();
        }
        host.force_quick_settings_settled();
        let (taffy, host_node) = layout_host(&mut host);
        let bar = host.quick_settings_slider_geometry(1).0;
        let click_x = bar.x0 + bar.width() / 2.0;
        let click_y = f64::midpoint(bar.y0, bar.y1);
        click(&mut host, &taffy, host_node, Point::new(click_x, click_y));

        let calls = recorder.0.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("set_volume(")),
            "expected a set_volume call; got {calls:?}"
        );
    }

    // Phase `keyboard_slider_arrows_route_through_the_host`.
    // The root owns keyboard input before ordinary desktop shortcuts. Exercise
    // that complete host path rather than calling `QuickSettingsRoot` directly:
    // Tab establishes paint-order focus, arrows commit the matching live slider,
    // and no app/input child is allowed to consume the key first.
    {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let recorder = SharedRecorder::default();
        host = host
            .with_qs_actions(Box::new(recorder.clone()))
            .with_quick_settings_config(QuickSettingsConfig::default());
        let (taffy, host_node) = open_quick_settings(&mut host, &chrome);

        // Controls come first: the first Tab reaches brightness immediately.
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::Tab),
            ModifiersState::empty(),
        ));
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::ArrowRight),
            ModifiersState::empty(),
        ));
        // Two more Tabs move brightness → the volume's mute switch → volume.
        for _ in 0..2 {
            assert!(press_key(
                &mut host,
                &taffy,
                host_node,
                Key::Named(NamedKey::Tab),
                ModifiersState::empty(),
            ));
        }
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::ArrowLeft),
            ModifiersState::empty(),
        ));

        let calls = recorder.0.calls();
        assert!(
            calls.iter().any(|call| call.starts_with("set_brightness(")),
            "Right Arrow on the focused brightness slider did not reach QsActions: {calls:?}"
        );
        assert!(
            calls.iter().any(|call| call.starts_with("set_volume(")),
            "Left Arrow on the focused volume slider did not reach QsActions: {calls:?}"
        );
        let osd = chrome
            .borrow()
            .osd
            .expect("a successful slider interaction raises the shared HUD");
        assert!(matches!(
            osd.kind,
            shell::shell::osd::OsdKind::Volume { muted: false }
        ));
        assert_eq!(osd.caption(), "Volume 35%");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn quick_settings_keyboard_actions_and_edit_reorder_route_through_the_host() {
    {
        // Sliders, live controls and both radio detail targets, Devices,
        // Clipboard, power profile and Edit precede Settings.
        // Enter on Settings dismisses the popover. The launcher itself is covered by the
        // serial `quick_settings_settings_action_launches_personal_settings_app`
        // test; this route test also guards against reopening the unrelated
        // workspace-sharing settings surface.
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
        // The footer ends the page: Edit, Settings, Lock, Power. Shift+Tab
        // from no focus wraps to Power; two more reach Settings.
        for _ in 0..3 {
            assert!(press_key(
                &mut host,
                &taffy,
                host_node,
                Key::Named(NamedKey::Tab),
                ModifiersState::SHIFT,
            ));
        }
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::Enter),
            ModifiersState::empty(),
        ));
        assert!(!chrome.borrow().overlays.quick_settings);
        assert!(
            !chrome.borrow().overlays.ws_settings,
            "Personal Settings must not be conflated with workspace sharing settings"
        );

        // A fresh host: Space on Edit enters edit mode without dismissing.
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
        // Edit leads the footer, before Settings, Lock and Power.
        for _ in 0..4 {
            assert!(press_key(
                &mut host,
                &taffy,
                host_node,
                Key::Named(NamedKey::Tab),
                ModifiersState::SHIFT,
            ));
        }
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::Space),
            ModifiersState::empty(),
        ));
        assert!(chrome.borrow().overlays.quick_settings);

        // Shift+Tab from no focus wraps to the final visible utility: Power.
        // Activating it dismisses Quick Settings and must not accidentally cycle
        // the power-profile row immediately above the utility rail.
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let recorder = SharedRecorder::default();
        host = host.with_qs_actions(Box::new(recorder.clone()));
        let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::Tab),
            ModifiersState::SHIFT,
        ));
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::Space),
            ModifiersState::empty(),
        ));
        assert!(
            !chrome.borrow().overlays.quick_settings,
            "Shift+Tab did not wrap to the final Power utility"
        );
        assert!(
            !recorder
                .0
                .calls()
                .iter()
                .any(|call| call.starts_with("set_power_profile(")),
            "the Power utility must not be conflated with Power profile"
        );
    }

    // Phase `keyboard_edit_reorder_routes_through_the_host_and_updates_semantics`.
    // Ctrl+Arrow reorders a focused edit row through the production keyboard
    // dispatcher. The injected config is deliberately non-persisting, proving
    // tests/embedders cannot alter a user's real Quick Settings file.
    {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        host = host.with_quick_settings_config(QuickSettingsConfig {
            controls: vec![
                settings_store::hardware::QuickSettingsControl::Wifi,
                settings_store::hardware::QuickSettingsControl::Bluetooth,
                settings_store::hardware::QuickSettingsControl::NightLight,
            ],
        });
        let (taffy, host_node) = open_quick_settings(&mut host, &chrome);

        // Edit leads the footer: four Shift+Tabs from no focus reach it.
        // Space enters edit mode with no hidden selection. Two Tabs then
        // select Bluetooth. Move it below Night light with Ctrl+Down.
        for _ in 0..4 {
            assert!(press_key(
                &mut host,
                &taffy,
                host_node,
                Key::Named(NamedKey::Tab),
                ModifiersState::SHIFT,
            ));
        }
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::Space),
            ModifiersState::empty(),
        ));
        for _ in 0..2 {
            assert!(press_key(
                &mut host,
                &taffy,
                host_node,
                Key::Named(NamedKey::Tab),
                ModifiersState::empty(),
            ));
        }
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::ArrowDown),
            ModifiersState::CONTROL,
        ));

        let semantic_root = host.accessibility().expect("shell semantics");
        let quick_settings = semantic_root
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Quick Settings"))
            .expect("Quick Settings must remain published while editing");
        let names = quick_settings
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(
            names[..3],
            ["Wi-Fi", "Night light", "Bluetooth"],
            "Ctrl+Down must reorder the semantic and visual row order"
        );

        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::ArrowUp),
            ModifiersState::CONTROL,
        ));
        let semantic_root = host.accessibility().expect("shell semantics");
        let quick_settings = semantic_root
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Quick Settings"))
            .expect("Quick Settings must remain published while editing");
        let names = quick_settings
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(
            names[..3],
            ["Wi-Fi", "Bluetooth", "Night light"],
            "Ctrl+Up must reverse a keyboard reorder"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn quick_settings_toggles_chips_and_wifi_rows_route_to_their_actions() {
    // Phase `wifi_toggle_click_routes_to_set_wifi`.
    {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let recorder = SharedRecorder::default();
        host = host
            .with_qs_actions(Box::new(recorder.clone()))
            .with_quick_settings_config(QuickSettingsConfig::default());

        {
            let mut st = chrome.borrow_mut();
            st.overlays.quick_settings = true;
            st.quick_settings.brightness_available = true;
            st.quick_settings.radios.wifi_available = true;
            st.bump();
        }
        host.force_quick_settings_settled();
        let (taffy, host_node) = layout_host(&mut host);
        paint(&mut host);
        // The tile's body, not its chevron (which opens its networks).
        let tile = quick_settings_target(&host, "quick_settings_network");
        click(
            &mut host,
            &taffy,
            host_node,
            Point::new(tile.x0 + 12.0, tile.center().y),
        );

        let calls = recorder.0.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("set_wifi(")),
            "expected a set_wifi call; got {calls:?}"
        );
    }

    // Phase `bluetooth_toggle_click_routes_to_set_bluetooth`.
    {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let recorder = SharedRecorder::default();
        host = host
            .with_qs_actions(Box::new(recorder.clone()))
            .with_quick_settings_config(QuickSettingsConfig::default());

        {
            let mut st = chrome.borrow_mut();
            st.overlays.quick_settings = true;
            st.quick_settings.brightness_available = true;
            st.quick_settings.radios.wifi_available = true;
            st.quick_settings.bluetooth_available = true;
            st.bump();
        }
        host.force_quick_settings_settled();
        let (taffy, host_node) = layout_host(&mut host);
        paint(&mut host);
        let tile = quick_settings_target(&host, "quick_settings_bluetooth");
        click(
            &mut host,
            &taffy,
            host_node,
            Point::new(tile.x0 + 12.0, tile.center().y),
        );

        let calls = recorder.0.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("set_bluetooth(")),
            "expected a set_bluetooth call; got {calls:?}"
        );
    }

    // Phase `power_profile_chip_click_routes_to_set_power_profile`.
    {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let recorder = SharedRecorder::default();
        host = host.with_qs_actions(Box::new(recorder.clone()));

        {
            let mut st = chrome.borrow_mut();
            st.overlays.quick_settings = true;
            st.quick_settings.brightness_available = true;
            st.quick_settings.power_profiles = vec![
                shell::shell::quick_settings::PowerProfile::Saver,
                shell::shell::quick_settings::PowerProfile::Balanced,
            ];
            st.bump();
        }
        host.force_quick_settings_settled();
        let (taffy, host_node) = layout_host(&mut host);
        paint(&mut host);
        let tile = quick_settings_target(&host, "quick_settings_power_mode");
        click(&mut host, &taffy, host_node, tile.center());

        let calls = recorder.0.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("set_power_profile(")),
            "expected a set_power_profile call; got {calls:?}"
        );
    }

    // Phase `wifi_section_row_click_prompts_for_a_password_in_place`.
    {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let recorder = SharedRecorder::default();
        host = host.with_qs_actions(Box::new(recorder.clone()));

        // A secured network not yet joined and one already connected take
        // different branches.
        let scanned = vec![
            WifiNetwork {
                ssid: "hallway-2g".into(),
                signal: 3,
                secure: true,
                security: os_actions::WifiSecurity::Wpa2,
                connected: false,
                known: false,
            },
            WifiNetwork {
                ssid: "studio-5g".into(),
                signal: 4,
                secure: true,
                security: os_actions::WifiSecurity::Wpa2,
                connected: true,
                known: true,
            },
        ];
        {
            let mut st = chrome.borrow_mut();
            st.overlays.quick_settings = true;
            st.quick_settings.wifi = true;
            st.quick_settings.radios.wifi_available = true;
            st.quick_settings.section = Some(shell::shell::quick_settings::QsSection::Wifi);
            st.quick_settings.wifi_networks = scanned.clone();
            st.bump();
        }
        host.force_quick_settings_settled();
        let (taffy, host_node) = layout_host(&mut host);
        paint(&mut host);
        assert!(
            host.quick_settings_target("quick_settings_wifi_network_studio-5g")
                .is_none(),
            "the network in use is status, not a second join"
        );
        let row = quick_settings_target(&host, "quick_settings_wifi_network_hallway-2g");
        // A secured network this machine does not know asks for its
        // passphrase rather than firing a connect that would fail silently.
        click(&mut host, &taffy, host_node, row.center());
        {
            let st = chrome.borrow();
            let prompt = st
                .quick_settings
                .wifi_password
                .as_ref()
                .expect("clicking a secured network must ask for its passphrase");
            assert_eq!(
                prompt.ssid, scanned[0].ssid,
                "the prompt must name the network that was clicked"
            );
            assert_eq!(
                st.quick_settings.section,
                Some(shell::shell::quick_settings::QsSection::Wifi),
                "in place, in the Wi-Fi section"
            );
        }
        assert!(
            recorder.0.calls().is_empty(),
            "no connect may be attempted before the passphrase is entered; got {:?}",
            recorder.0.calls()
        );
        assert_eq!(chrome.borrow().quick_settings.wifi_joining.as_deref(), None);
    }
}

#[test]
fn only_the_invite_button_opens_the_signed_composer() {
    // Phase `workspace_settings_invite_button_opens_the_signed_composer`.
    {
        use shell::shell::chrome_state::WsTab;

        let mut tree = minimal_tree();
        tree.adopt_ownerless_workspaces_with(shell::shell::state::local_user_id_for_room);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
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

    // Phase `a_click_beside_the_invite_button_does_not_open_the_composer`.
    // Guards against a hit rect widened until it swallows the rest of the tab.
    {
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
}

/// The ribbon's own geometry is unit-tested in `ui`, but geometry that
/// nothing routes is exactly the defect the ws-settings Copy button was
/// — this drives a real click and asserts `active_strip` moved.
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
    click(&mut host, &taffy, host_node, c);

    let (_, focused) = chrome.borrow().strip_focus();
    assert_eq!(
        focused, 2,
        "clicking the third ribbon segment must focus the third strip"
    );
}

fn quick_settings_target(
    host: &shell::shell::overlay_host::ShellOverlayHost,
    name: &str,
) -> vello::kurbo::Rect {
    host.quick_settings_semantic_rects()
        .into_iter()
        .find(|(target, _)| target == name)
        .map_or_else(
            || panic!("Quick Settings publishes no {name} target"),
            |(_, rect)| rect,
        )
}

/// Every labelled root destination is live: a click on each lands on the
/// surface its label names instead of doing nothing.
#[test]
fn every_quick_settings_destination_click_lands_on_its_surface() {
    use shell::shell::quick_settings::{QsLevel, QsSection};

    // Notifications is a page; Devices and the Clipboard open in place.
    for (target, level, section) in [
        ("quick_settings_notifications", QsLevel::Notifications, None),
        (
            "quick_settings_devices",
            QsLevel::Root,
            Some(QsSection::Devices),
        ),
        (
            "quick_settings_clipboard",
            QsLevel::Root,
            Some(QsSection::Clipboard),
        ),
    ] {
        let tree = minimal_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
        paint(&mut host);
        let centre = quick_settings_target(&host, target).center();
        click(&mut host, &taffy, host_node, centre);
        let st = chrome.borrow();
        assert!(
            st.overlays.quick_settings,
            "{target} keeps the popover open"
        );
        assert_eq!(
            st.quick_settings.current_level(),
            &level,
            "{target} must open its pane"
        );
        assert_eq!(
            st.quick_settings.section, section,
            "{target} must open its section"
        );
    }

    // Lock locks the screen and closes the popover.
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
    paint(&mut host);
    let centre = quick_settings_target(&host, "quick_settings_lock").center();
    click(&mut host, &taffy, host_node, centre);
    assert!(!chrome.borrow().overlays.quick_settings);
    assert!(chrome.borrow_mut().take_session_lock_request());

    // Power opens the session menu and closes the popover.
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
    paint(&mut host);
    let centre = quick_settings_target(&host, "quick_settings_power").center();
    click(&mut host, &taffy, host_node, centre);
    assert!(!chrome.borrow().overlays.quick_settings);
    assert!(!host.power_menu_semantic_rects().is_empty());
}

/// Tab visits every root stop in turn, and at each one exactly one node of
/// the published Quick Settings tree is focused — the node Enter would
/// activate — until Tab wraps back to the first.
#[test]
fn tab_through_every_quick_settings_stop_focuses_exactly_one_node() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
    paint(&mut host);
    let quick_settings = |host: &shell::shell::overlay_host::ShellOverlayHost| {
        host.accessibility()
            .expect("shell semantics")
            .children
            .into_iter()
            .find(|node| node.name.as_deref() == Some("Quick Settings"))
            .expect("Quick Settings is published")
    };
    let stops = quick_settings(&host).children.len();
    assert!(
        quick_settings(&host)
            .children
            .iter()
            .all(|node| !node.state.focused),
        "opening moves no focus"
    );
    let mut visited = Vec::new();
    for _ in 0..stops {
        assert!(press_key(
            &mut host,
            &taffy,
            host_node,
            Key::Named(NamedKey::Tab),
            ModifiersState::empty(),
        ));
        let dialog = quick_settings(&host);
        let focused: Vec<_> = dialog
            .children
            .iter()
            .filter(|node| node.state.focused)
            .filter_map(|node| node.name.clone())
            .collect();
        assert_eq!(focused.len(), 1, "exactly one focused node: {focused:?}");
        visited.push(focused[0].clone());
    }
    for name in [
        "Brightness",
        "Volume",
        "Wi-Fi",
        "Bluetooth",
        "Wi-Fi networks",
        "Night light",
        "Devices",
        "Clipboard",
        "Notifications",
        "Edit controls",
        "Settings",
        "Lock",
        "Power",
    ] {
        assert!(
            visited.iter().any(|v| v == name),
            "Tab never reached {name}: {visited:?}"
        );
    }
    assert_eq!(visited.first().map(String::as_str), Some("Brightness"));
    assert!(press_key(
        &mut host,
        &taffy,
        host_node,
        Key::Named(NamedKey::Tab),
        ModifiersState::empty(),
    ));
    let dialog = quick_settings(&host);
    assert_eq!(
        dialog
            .children
            .iter()
            .find(|node| node.state.focused)
            .and_then(|node| node.name.as_deref()),
        Some("Brightness"),
        "Tab wraps to the first stop"
    );
}

/// The Do Not Disturb tile edits the one persisted switch the palette action
/// and Personal Settings share, and the notification stack honours it.
#[test]
fn do_not_disturb_tile_click_holds_notifications() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    host = host.with_quick_settings_config(QuickSettingsConfig {
        controls: vec![
            settings_store::hardware::QuickSettingsControl::Wifi,
            settings_store::hardware::QuickSettingsControl::DoNotDisturb,
        ],
    });
    let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
    paint(&mut host);
    let centre = quick_settings_target(&host, "quick_settings_do_not_disturb").center();
    click(&mut host, &taffy, host_node, centre);
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
    paint(&mut host);
    click(&mut host, &taffy, host_node, centre);
    assert!(!chrome.borrow().dnd(), "a second click turns it back off");
}
