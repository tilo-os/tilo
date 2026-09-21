//! Click-routing wire-up for the quick-settings panel — not a repeat of
//! the parser-level unit tests in `os-actions`, which
//! pin the brightnessctl/wpctl/nmcli/bluetoothctl stdout shapes. Builds a
//! real [`ShellOverlayHost`], swaps in a [`RecordingDispatcher`], forces
//! the popover open, drives `handle_event` with a synthetic click at the
//! slider's bar rect, and reads the dispatcher's call log.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

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
use taffy::prelude::TaffyMaxContent;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

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
        }],
        active_tab: tab_id,
        width: WindowWidth::Normal,
        maximized: false,
    };
    let strip = Strip {
        id: strip_id,
        name: "S".into(),
        window_groups: vec![group],
    };
    let ws = Workspace {
        strips: vec![strip],
        // Everything else, including the workspace's durable collaboration room,
        // comes from the builder, so a field added to `Workspace` does not break
        // this fixture.
        ..Workspace::test_default(ws_id, "W")
    };
    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, group_id);
    WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    }
}

/// `apply_layout` sets `self.rect`, which geometry helpers like
/// `quick_settings_slider_geometry` need.
fn layout_host(host: &mut shell::shell::overlay_host::ShellOverlayHost) -> (TaffyTree<()>, NodeId) {
    use taffy::{Dimension, Style};
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let host_node = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[host_node],
        )
        .unwrap();
    let map_for_measure = node_map.clone();
    let mut tr = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1600.0),
                height: AvailableSpace::Definite(900.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .or_else(|_| taffy.compute_layout(root, taffy::Size::MAX_CONTENT))
        .expect("layout must compute");
    host.apply_layout(&taffy, host_node, Point::ZERO);
    (taffy, host_node)
}

/// Drive a whole click — `CursorMoved`, `Pressed`, `Released` — after
/// [`layout_host`]. The release matters: sliders hold their value until
/// button-up (so a drag doesn't spawn one process call per frame), while
/// toggles and the power chip dispatch on press.
fn click_at(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    taffy: &TaffyTree<()>,
    host_node: NodeId,
    x: f64,
    y: f64,
) {
    let mut state = automerge::AutoCommit::new();
    let root_id = automerge::ROOT;
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    let moved = WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: PhysicalPosition::new(x, y),
    };
    host.handle_event(
        &moved,
        1.0,
        &winit::event::Modifiers::default(),
        &mut state,
        &root_id,
        &mut handled,
        &mut focus_target,
        host_node,
        taffy,
    );

    for button_state in [ElementState::Pressed, ElementState::Released] {
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        let ev = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: button_state,
            button: MouseButton::Left,
        };
        host.handle_event(
            &ev,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            host_node,
            taffy,
        );
    }
}

/// Route a pressed logical key through the host dispatcher. `KeyEvent`
/// deliberately has no public constructor because a small platform payload is
/// opaque, so initialise that payload to zero and set every public field just
/// as the compositor's own test harness does.
fn press_key(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    taffy: &TaffyTree<()>,
    host_node: NodeId,
    key: Key,
    modifiers: ModifiersState,
) -> bool {
    let mut key_event: KeyEvent = unsafe { std::mem::zeroed() };
    key_event.logical_key = key;
    key_event.physical_key =
        PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    key_event.location = KeyLocation::Standard;
    key_event.state = ElementState::Pressed;
    key_event.repeat = false;
    key_event.text = None;
    let event = WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: key_event,
        is_synthetic: false,
    };
    let mut state = automerge::AutoCommit::new();
    let root_id = automerge::ROOT;
    let mut handled = false;
    let mut focus_target = None;
    host.handle_event(
        &event,
        1.0,
        &Modifiers::from(modifiers),
        &mut state,
        &root_id,
        &mut handled,
        &mut focus_target,
        host_node,
        taffy,
    );
    handled
}

fn open_quick_settings(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    chrome: &shell::shell::chrome_state::Chrome,
) -> (TaffyTree<()>, NodeId) {
    {
        let mut state = chrome.borrow_mut();
        state.overlays.quick_settings = true;
        state.quick_settings.radios.wifi_available = true;
        state.quick_settings.bluetooth_available = true;
        state.bump();
    }
    host.force_quick_settings_settled();
    layout_host(host)
}

#[test]
fn brightness_slider_click_routes_to_set_brightness() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host.with_qs_actions(Box::new(recorder.clone()));

    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, host_node) = layout_host(&mut host);
    let bar = host.quick_settings_slider_geometry(0).0;
    let click_x = bar.x0 + bar.width() / 3.0;
    let click_y = f64::midpoint(bar.y0, bar.y1);
    click_at(&mut host, &taffy, host_node, click_x, click_y);

    let calls = recorder.0.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("set_brightness(")),
        "expected a set_brightness call; got {calls:?}"
    );
}

#[test]
fn volume_slider_click_routes_to_set_volume() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host.with_qs_actions(Box::new(recorder.clone()));

    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, host_node) = layout_host(&mut host);
    let bar = host.quick_settings_slider_geometry(1).0;
    let click_x = bar.x0 + bar.width() / 2.0;
    let click_y = f64::midpoint(bar.y0, bar.y1);
    click_at(&mut host, &taffy, host_node, click_x, click_y);

    let calls = recorder.0.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("set_volume(")),
        "expected a set_volume call; got {calls:?}"
    );
}

/// The root owns keyboard input before ordinary desktop shortcuts. Exercise
/// that complete host path rather than calling `QuickSettingsRoot` directly:
/// Tab establishes paint-order focus, arrows commit the matching live slider,
/// and no app/input child is allowed to consume the key first.
#[test]
fn keyboard_slider_arrows_route_through_the_host() {
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
    // One more Tab moves brightness → volume.
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

/// Enter, Space and reverse-Tab all use the same root actions exposed to
/// pointer users. This guards the route through `ShellOverlayHost`, where
/// overlay ordering and modifier state have historically swallowed keys.
#[test]
fn keyboard_actions_and_reverse_tab_route_through_the_host() {
    // Sliders, live controls, power profile, and Customize precede Settings.
    // Enter on Settings dismisses the popover. The launcher itself is covered by the
    // serial `quick_settings_settings_action_launches_personal_settings_app`
    // test; this route test also guards against reopening the unrelated
    // workspace-sharing settings surface.
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
    for _ in 0..8 {
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
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    ));
    assert!(!chrome.borrow().overlays.quick_settings);
    assert!(
        !chrome.borrow().overlays.ws_settings,
        "Personal Settings must not be conflated with workspace sharing settings"
    );

    // A fresh host: Space on Customize enters edit mode without dismissing.
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = open_quick_settings(&mut host, &chrome);
    for _ in 0..7 {
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

/// Ctrl+Arrow reorders a focused edit row through the production keyboard
/// dispatcher. The injected config is deliberately non-persisting, proving
/// tests/embedders cannot alter a user's real Quick Settings file.
#[test]
fn keyboard_edit_reorder_routes_through_the_host_and_updates_semantics() {
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

    // The control-first root puts Customize after two sliders, three live
    // controls, and Power profile. Space enters edit mode with no hidden
    // selection. Two Tabs then select Bluetooth. Move it below Night Light
    // with Ctrl+Down.
    for _ in 0..7 {
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
        ["Wi-Fi", "Night Light", "Bluetooth"],
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
        ["Wi-Fi", "Bluetooth", "Night Light"],
        "Ctrl+Up must reverse a keyboard reorder"
    );
}

#[test]
fn wifi_toggle_click_routes_to_set_wifi() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host
        .with_qs_actions(Box::new(recorder.clone()))
        .with_quick_settings_config(QuickSettingsConfig::default());

    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.quick_settings.radios.wifi_available = true;
        st.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, host_node) = layout_host(&mut host);
    let card = host.quick_settings_toggle_geometry(0);
    // Slightly off the chevron (which would drill in to the wifi list); the
    // card's bottom-left maps to the toggle hit.
    let click_x = card.x0 + 24.0;
    let click_y = card.y1 - 16.0;
    click_at(&mut host, &taffy, host_node, click_x, click_y);

    let calls = recorder.0.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("set_wifi(")),
        "expected a set_wifi call; got {calls:?}"
    );
}

#[test]
fn bluetooth_toggle_click_routes_to_set_bluetooth() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host
        .with_qs_actions(Box::new(recorder.clone()))
        .with_quick_settings_config(QuickSettingsConfig::default());

    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.quick_settings.radios.wifi_available = true;
        st.quick_settings.bluetooth_available = true;
        st.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, host_node) = layout_host(&mut host);
    let card = host.quick_settings_toggle_geometry(1);
    let click_x = card.x0 + 24.0;
    let click_y = card.y1 - 16.0;
    click_at(&mut host, &taffy, host_node, click_x, click_y);

    let calls = recorder.0.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("set_bluetooth(")),
        "expected a set_bluetooth call; got {calls:?}"
    );
}

#[test]
fn power_profile_chip_click_routes_to_set_power_profile() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host.with_qs_actions(Box::new(recorder.clone()));

    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, host_node) = layout_host(&mut host);
    let pp = host.quick_settings_power_profile_geometry();
    let click_x = pp.x0 + pp.width() / 2.0;
    let click_y = f64::midpoint(pp.y0, pp.y1);
    click_at(&mut host, &taffy, host_node, click_x, click_y);

    let calls = recorder.0.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("set_power_profile(")),
        "expected a set_power_profile call; got {calls:?}"
    );
}

#[test]
fn wifi_drill_in_row_click_prompts_or_marks_an_async_join() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host.with_qs_actions(Box::new(recorder.clone()));

    // A secured network not yet joined and one already connected take
    // different click branches.
    let scanned = vec![
        WifiNetwork {
            ssid: "hallway-2g".into(),
            signal: 71,
            secure: true,
            connected: false,
        },
        WifiNetwork {
            ssid: "studio-5g".into(),
            signal: 92,
            secure: true,
            connected: true,
        },
    ];
    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.quick_settings
            .push_level(shell::shell::quick_settings::QsLevel::Wifi);
        st.quick_settings.wifi_networks = scanned.clone();
        st.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, host_node) = layout_host(&mut host);
    let rect = host.quick_settings_rect_for_tests();
    let click_x = rect.x0 + rect.width() / 2.0;
    // The drill-in header and hotspot affordance reserve 116 px; rows are
    // 56 px, so +28 into a row lands mid-row.
    let row_y =
        |i: usize| rect.y0 + 116.0 + f64::from(u32::try_from(i).unwrap_or(u32::MAX)) * 56.0 + 28.0;

    // Row 0 — secured, not joined: must prompt for a passphrase rather than
    // firing a connect that would fail silently.
    click_at(&mut host, &taffy, host_node, click_x, row_y(0));
    {
        let st = chrome.borrow();
        let prompt = st
            .quick_settings
            .wifi_password
            .as_ref()
            .expect("clicking a secured network must open the passphrase prompt");
        assert_eq!(
            prompt.ssid, scanned[0].ssid,
            "the prompt must name the network that was clicked"
        );
    }
    assert!(
        recorder.0.calls().is_empty(),
        "no connect may be attempted before the passphrase is entered; got {:?}",
        recorder.0.calls()
    );

    // Row 1 is already connected, so it is truthful status rather than a
    // second join action.
    {
        let mut st = chrome.borrow_mut();
        st.quick_settings.wifi_password = None;
        st.bump();
    }
    click_at(&mut host, &taffy, host_node, click_x, row_y(1));
    let calls = recorder.0.calls();
    assert!(
        calls.is_empty(),
        "the compositor must not block in the injected dispatcher; got {calls:?}"
    );
    assert_eq!(
        chrome.borrow().quick_settings.wifi_joining.as_deref(),
        None,
        "an already-connected network must not enter another join lifecycle"
    );
}

#[test]
fn workspace_settings_invite_button_opens_the_signed_composer() {
    use shell::shell::chrome_state::WsTab;

    let mut tree = minimal_tree();
    tree.adopt_ownerless_workspaces_with(shell::shell::state::local_user_id_for_room);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
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
    let _ = &taffy;

    let btn = host.ws_settings_invite_rect_for_tests();
    assert!(
        btn.area() > 0.0,
        "the Create button has no geometry to click"
    );
    assert!(!host.peer_sync_card_visible());

    let c = btn.center();
    click_at(&mut host, &taffy, host_node, c.x, c.y);

    assert!(
        host.peer_sync_card_visible(),
        "clicking Create did not open the signed invite composer"
    );
    assert!(!chrome.borrow().overlays.ws_settings);
}

/// Guards against a hit rect widened until it swallows the rest of the tab.
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

    let btn = host.ws_settings_invite_rect_for_tests();
    // Well left of the button, still inside the modal body.
    let p = Point::new(btn.x0 - 160.0, btn.center().y);
    click_at(&mut host, &taffy, host_node, p.x, p.y);

    assert!(
        !host.peer_sync_card_visible(),
        "a click beside the Create button opened the composer anyway"
    );
}

/// The ribbon's own geometry is unit-tested in `ui`, but geometry that
/// nothing routes is exactly the defect the ws-settings Copy button was
/// — this drives a real click and asserts `active_strip` moved.
#[test]
fn clicking_a_strip_ribbon_segment_moves_focus() {
    let mut tree = minimal_tree();
    let ws_id = tree.workspaces[0].id;
    for n in 0..2u64 {
        tree.workspaces[0].strips.push(Strip {
            id: StripId(700 + n),
            name: format!("S{n}"),
            window_groups: vec![],
        });
    }
    let first = tree.workspaces[0].strips[0].id;
    tree.active_strip.insert(ws_id, first);

    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = layout_host(&mut host);

    let ribbon = host.strip_ribbon_for_tests();
    assert_eq!(ribbon.count, 3, "fixture should present three strips");
    assert_eq!(ribbon.focused, 0, "focus starts on the first strip");

    let target = ribbon.hit_rect(2).expect("third segment has a target");
    let c = target.center();
    click_at(&mut host, &taffy, host_node, c.x, c.y);

    let (_, focused) = chrome.borrow().strip_focus();
    assert_eq!(
        focused, 2,
        "clicking the third ribbon segment must focus the third strip"
    );
}
