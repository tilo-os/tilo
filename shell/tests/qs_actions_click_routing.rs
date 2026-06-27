//! Click-routing wire-up for the quick-settings panel.
//!
//! The goal of this file is *not* to repeat the parser-level unit tests
//! in `shell/src/shell/qs_actions.rs#mod tests` — those already pin the
//! brightnessctl/wpctl/nmcli/bluetoothctl stdout shapes the panel parses.
//! What this file covers is the *click-routing*: when a user clicks the
//! brightness slider (or volume slider, or Wi-Fi toggle, …) inside the
//! popover, the host pushes the new value through the
//! [`QsActions`](shell::shell::qs_actions::QsActions) dispatcher exactly
//! once and with the expected arguments.
//!
//! Strategy: build a real [`ShellOverlayHost`], swap in a
//! [`RecordingDispatcher`] via [`ShellOverlayHost::with_qs_actions`],
//! force the QS popover open, then drive the host's `handle_event`
//! with a synthetic `CursorMoved` + `MouseInput::Pressed` pair targeted
//! at the slider's bar rect. Read the dispatcher's call log and assert
//! the expected operation landed.

use std::cell::RefCell;
use std::collections::HashMap;

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::qs_actions::{PowerProfile, QsActions};
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use taffy::prelude::TaffyMaxContent;
use taffy::{NodeId, Size, TaffyTree, AvailableSpace};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};

/// Records every QS action call so the test can assert on the call log.
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
}

/// Shared recording dispatcher so the test can read calls back after
/// the dispatcher is moved into the host. The host owns a
/// `Box<dyn QsActions>`, so we wrap the recorder in an `Rc` and hand
/// out a separate clone for read-back.
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

/// One-strip, one-group, one-tab tree — the smallest topology the
/// chrome can render. The test never actually hits the workspace
/// content; the QS popover sits over it.
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
        id: ws_id,
        name: "W".into(),
        strips: vec![strip],
        roles: vec![],
        workspace_files: vec![],
        collaborators: vec![],
        grants: HashMap::new(),
        shared_surfaces: HashMap::new(),
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

/// Lay out the host into a 1600x900 viewport, returning the taffy
/// node id + tree so the test can call `handle_event` afterwards. The
/// host's `self.rect` is set as a side-effect of `apply_layout`, so
/// geometry helpers like `quick_settings_slider_geometry` can be
/// called after this returns.
fn layout_host(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
) -> (TaffyTree<()>, NodeId) {
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

/// Drive `CursorMoved` then `MouseInput::Pressed` at `(x, y)` through
/// the host's `handle_event`. Must be called after [`layout_host`].
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

    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    let pressed = WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    host.handle_event(
        &pressed,
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

#[test]
fn brightness_slider_click_routes_to_set_brightness() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
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
    let click_y = (bar.y0 + bar.y1) / 2.0;
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
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
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
    let click_y = (bar.y0 + bar.y1) / 2.0;
    click_at(&mut host, &taffy, host_node, click_x, click_y);

    let calls = recorder.0.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("set_volume(")),
        "expected a set_volume call; got {calls:?}"
    );
}

#[test]
fn wifi_toggle_click_routes_to_set_wifi() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host.with_qs_actions(Box::new(recorder.clone()));

    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, host_node) = layout_host(&mut host);
    let card = host.quick_settings_toggle_geometry(0);
    // Click slightly off the chevron (which would drill-in to wifi
    // list); the bottom-left corner of the card maps to the toggle hit.
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
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host.with_qs_actions(Box::new(recorder.clone()));

    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
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
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
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
    let click_y = (pp.y0 + pp.y1) / 2.0;
    click_at(&mut host, &taffy, host_node, click_x, click_y);

    let calls = recorder.0.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("set_power_profile(")),
        "expected a set_power_profile call; got {calls:?}"
    );
}

#[test]
fn wifi_drill_in_row_click_routes_to_connect_wifi() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host.with_qs_actions(Box::new(recorder.clone()));

    // Drill into the Wi-Fi list.
    {
        let mut st = chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.quick_settings
            .push_level(shell::shell::quick_settings::QsLevel::Wifi);
        st.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, host_node) = layout_host(&mut host);
    let rect = host.quick_settings_rect_for_tests();
    let click_x = rect.x0 + rect.width() / 2.0;
    let click_y = rect.y0 + 56.0 + 28.0;
    click_at(&mut host, &taffy, host_node, click_x, click_y);

    let calls = recorder.0.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("connect_wifi(")),
        "expected a connect_wifi call; got {calls:?}"
    );
}
