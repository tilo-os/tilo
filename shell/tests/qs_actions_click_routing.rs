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
use shell::shell::quick_settings::WifiNetwork;
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
        strips: vec![strip],
        // Everything else — including the workspace's durable
        // collaboration room — comes from the builder, so a field added
        // to `Workspace` does not break this fixture.
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

/// Lay out the host into a 1600x900 viewport, returning the taffy
/// node id + tree so the test can call `handle_event` afterwards. The
/// host's `self.rect` is set as a side-effect of `apply_layout`, so
/// geometry helpers like `quick_settings_slider_geometry` can be
/// called after this returns.
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

/// Drive a whole click — `CursorMoved`, `Pressed`, `Released` — at
/// `(x, y)` through the host's `handle_event`. Must be called after
/// [`layout_host`].
///
/// The release matters. Toggle cards and the power chip dispatch on
/// press, but the sliders deliberately hold their value until the button
/// comes up: while a drag is live only the fill follows the pointer, and
/// the value is pushed to the OS once, on release, so dragging the knob
/// across the bar cannot spawn one `brightnessctl`/`wpctl` per frame. A
/// press without a release is a grab the user never let go of, and the
/// panel is right not to commit it.
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
fn wifi_drill_in_row_click_routes_by_whether_a_passphrase_is_needed() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let recorder = SharedRecorder::default();
    host = host.with_qs_actions(Box::new(recorder.clone()));

    // Two rows: a secured network we are not on, and the one we are
    // already connected to. They take different branches, and this
    // previously asserted only that clicking the first called
    // `connect_wifi` — which stopped being true when the pane grew a
    // passphrase prompt, and the test had been red since.
    let scanned = vec![
        WifiNetwork { ssid: "hallway-2g".into(), signal: 71, secure: true, connected: false },
        WifiNetwork { ssid: "studio-5g".into(), signal: 92, secure: true, connected: true },
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
    // Header is 56 px, rows are 56 px, so +28 into a row lands mid-row.
    let row_y = |i: usize| rect.y0 + 56.0 + i as f64 * 56.0 + 28.0;

    // Row 0 — secured, not joined. Joining needs a passphrase, so the
    // pane must ask for one rather than firing a connect that would
    // fail silently.
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

    // Dismiss the prompt, then click row 1 — already connected, so no
    // passphrase is needed and the click connects directly.
    {
        let mut st = chrome.borrow_mut();
        st.quick_settings.wifi_password = None;
        st.bump();
    }
    click_at(&mut host, &taffy, host_node, click_x, row_y(1));
    let calls = recorder.0.calls();
    assert!(
        calls.contains(&format!("connect_wifi({})", scanned[1].ssid)),
        "clicking a network that needs no passphrase must connect to it \
         ({}); got {calls:?}",
        scanned[1].ssid
    );
}

#[test]
fn workspace_settings_copy_button_copies_the_invite_link() {
    use shell::shell::chrome_state::WsTab;

    let tree = minimal_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = WsTab::General;
        st.bump();
    }
    let (taffy, host_node) = layout_host(&mut host);
    host.force_ws_settings_settled();
    // Paint once so the modal's geometry is populated the way it is in
    // a real frame before any click arrives.
    let _ = &taffy;

    let btn = host.ws_settings_copy_rect_for_tests();
    assert!(btn.area() > 0.0, "the Copy button has no geometry to click");
    assert!(
        host.ws_invite_copied_at_for_tests().is_none(),
        "nothing should have been copied before the click"
    );

    let c = btn.center();
    click_at(&mut host, &taffy, host_node, c.x, c.y);

    assert!(
        host.ws_invite_copied_at_for_tests().is_some(),
        "clicking Copy did not run the copy path — the button is painted but not routed"
    );
}

/// A click that misses the Copy button does not copy. Guards against a
/// hit rect widened until it swallows the rest of the tab.
#[test]
fn a_click_beside_the_copy_button_does_not_copy() {
    use shell::shell::chrome_state::WsTab;

    let tree = minimal_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = WsTab::General;
        st.bump();
    }
    let (taffy, host_node) = layout_host(&mut host);
    host.force_ws_settings_settled();

    let btn = host.ws_settings_copy_rect_for_tests();
    // Well left of the button, still inside the modal body.
    let p = Point::new(btn.x0 - 160.0, btn.center().y);
    click_at(&mut host, &taffy, host_node, p.x, p.y);

    assert!(
        host.ws_invite_copied_at_for_tests().is_none(),
        "a click beside the Copy button copied anyway"
    );
}

/// A workspace with a single strip offers no strip ribbon at all —
/// nothing painted, and nothing to click.
///
/// This is the shape of the empty desktop, and the segment it used to
/// draw was the brightest mark on the screen while standing for a choice
/// between strips that did not exist. Driven through the real host, so
/// the assertion cannot pass by disagreeing with the geometry the shell
/// actually builds.
#[test]
fn a_single_strip_workspace_offers_no_strip_ribbon() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = layout_host(&mut host);

    let ribbon = host.strip_ribbon_for_tests();
    assert_eq!(ribbon.count, 1, "fixture should present a single strip");
    assert!(!ribbon.is_visible(), "a lone strip still presents a ribbon");
    assert_eq!(ribbon.segment_rect(0), None, "a lone strip still has segment geometry");

    // Sweep the gutter the ribbon would have occupied: no segment may
    // answer a hit-test anywhere along it.
    let x = ribbon.content.x1
        - ui::chrome::shell::RIBBON_MARGIN
        - ui::chrome::shell::RIBBON_SEG_W / 2.0;
    let mut y = ribbon.content.y0;
    while y < ribbon.content.y1 {
        assert_eq!(ribbon.hit(Point::new(x, y)), None, "still hittable at ({x}, {y})");
        y += 8.0;
    }

    // And a real click through that gutter changes nothing.
    let before = chrome.borrow().strip_focus();
    click_at(&mut host, &taffy, host_node, x, ribbon.content.center().y);
    assert_eq!(
        chrome.borrow().strip_focus(),
        before,
        "a click in the empty ribbon gutter moved strip focus"
    );
}

/// Clicking a strip-ribbon segment moves strip focus through the
/// production click path.
///
/// The ribbon's own geometry is unit-tested in `ui`, but geometry that
/// nothing routes is exactly the defect the ws-settings Copy button was.
/// This drives a real click at a real segment and asserts the tree's
/// `active_strip` moved.
#[test]
fn clicking_a_strip_ribbon_segment_moves_focus() {
    let mut tree = minimal_tree();
    let ws_id = tree.workspaces[0].id;
    for n in 0..2u64 {
        tree.workspaces[0]
            .strips
            .push(Strip { id: StripId(700 + n), name: format!("S{n}"), window_groups: vec![] });
    }
    let first = tree.workspaces[0].strips[0].id;
    tree.active_strip.insert(ws_id, first);

    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
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
