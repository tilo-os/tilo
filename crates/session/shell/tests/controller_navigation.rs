//! A game controller drives the desktop through the production path: typed
//! controller events → `ControllerNavMapper` → `ShellOverlayHost::
//! apply_controller_intent`, asserted on the same workspace-tree JSON the
//! control socket's `state-dump` serves.
//!
//! Desktop-layout intents must change that tree; focus intents must come
//! back as the keystroke the compositor hands to the focused surface.

use std::collections::HashMap;
use std::time::Instant;

use os_actions::{ForwardedGamepadEvent, GamepadAxis, GamepadButton};
use shell::compositor::controller_nav::{ControllerNavMapper, NavDirection};
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::{ControllerDelivery, ControllerKey, ShellOverlayHost};
use shell::shell::state::FreshId as _;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use ui::ControllerFamily;
use ui::widget::Widget;

const G_A1: GroupId = 1001;
const G_A2: GroupId = 1002;
const G_B1: GroupId = 2001;
const G_OTHER: GroupId = 3001;

struct Desk {
    tree: WorkspaceTree,
    strip_a: StripId,
    strip_b: StripId,
    home: WorkspaceId,
    other: WorkspaceId,
}

fn group(id: GroupId, tab: u64, label: &str) -> WindowGroup {
    WindowGroup {
        id,
        tabs: vec![Tab {
            id: tab,
            title: label.into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: label.into(),
            },
            app_title: None,
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// Two windows side by side on strip A, one on strip B below, and a second
/// workspace to switch to.
fn desk() -> Desk {
    let home = WorkspaceId::new();
    let other = WorkspaceId::new();
    let strip_a = StripId::new();
    let strip_b = StripId::new();
    let strip_other = StripId::new();
    let workspace = {
        let mut workspace = Workspace::new(home, "Home");
        workspace.strips = vec![
            Strip {
                id: strip_a,
                name: "A".into(),
                window_groups: vec![group(G_A1, 11, "A1"), group(G_A2, 12, "A2")],
            },
            Strip {
                id: strip_b,
                name: "B".into(),
                window_groups: vec![group(G_B1, 21, "B1")],
            },
        ];
        workspace
    };
    let second = {
        let mut workspace = Workspace::new(other, "Other");
        workspace.strips = vec![Strip {
            id: strip_other,
            name: "Other".into(),
            window_groups: vec![group(G_OTHER, 31, "Other")],
        }];
        workspace
    };
    let tree = WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![workspace, second],
            active: home,
            active_strip: HashMap::from([(home, strip_a), (other, strip_other)]),
            active_group: HashMap::from([(strip_a, G_A1), (strip_b, G_B1), (strip_other, G_OTHER)]),
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    };
    Desk {
        tree,
        strip_a,
        strip_b,
        home,
        other,
    }
}

fn tick(host: &mut ShellOverlayHost) {
    let doc = automerge::AutoCommit::new();
    host.update(&doc, &automerge::ROOT);
}

/// The `workspace_tree` half of `state-dump`, built the way the compositor
/// builds it.
fn state_dump(chrome: &Chrome) -> serde_json::Value {
    serde_json::to_value(&*chrome.borrow().tree()).unwrap()
}

fn key<T: serde::Serialize>(id: &T) -> String {
    match serde_json::to_value(id).unwrap() {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    }
}

fn active_group(dump: &serde_json::Value, strip: StripId) -> u64 {
    dump["active_group"][key(&strip)].as_u64().unwrap()
}

fn active_strip(dump: &serde_json::Value, workspace: WorkspaceId) -> String {
    key(&dump["active_strip"][key(&workspace)])
}

fn strip_groups(dump: &serde_json::Value, strip: StripId) -> Vec<u64> {
    let strip_key = key(&strip);
    dump["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|workspace| workspace["strips"].as_array().unwrap())
        .find(|candidate| key(&candidate["id"]) == strip_key)
        .unwrap()["window_groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| group["id"].as_u64().unwrap())
        .collect()
}

/// A person holding the controller, pressing through the production mapper
/// into the shell.
struct Player {
    mapper: ControllerNavMapper,
    host: ShellOverlayHost,
    chrome: Chrome,
}

impl Player {
    fn new(tree: &WorkspaceTree, family: ControllerFamily) -> Self {
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(tree);
        tick(&mut host);
        Self {
            mapper: ControllerNavMapper::new(family),
            host,
            chrome,
        }
    }

    fn send(&mut self, event: ForwardedGamepadEvent) -> Vec<ControllerDelivery> {
        let deliveries = self
            .mapper
            .feed(event, Instant::now())
            .into_iter()
            .map(|intent| self.host.apply_controller_intent(intent))
            .collect();
        tick(&mut self.host);
        deliveries
    }

    fn tap(&mut self, button: GamepadButton) -> Vec<ControllerDelivery> {
        let mut out = self.send(ForwardedGamepadEvent::Button {
            button,
            pressed: true,
        });
        out.extend(self.send(ForwardedGamepadEvent::Button {
            button,
            pressed: false,
        }));
        out
    }

    fn hold_guide(&mut self, pressed: bool) {
        self.send(ForwardedGamepadEvent::Button {
            button: GamepadButton::Guide,
            pressed,
        });
    }

    fn d_pad(&mut self, x: i8, y: i8) -> Vec<ControllerDelivery> {
        let mut out = self.send(ForwardedGamepadEvent::Hat { x, y });
        out.extend(self.send(ForwardedGamepadEvent::Hat { x: 0, y: 0 }));
        out
    }

    fn dump(&self) -> serde_json::Value {
        state_dump(&self.chrome)
    }
}

#[test]
fn guide_and_the_d_pad_walk_windows_then_strips() {
    let desk = desk();
    let mut player = Player::new(&desk.tree, ControllerFamily::Xbox);
    assert_eq!(active_group(&player.dump(), desk.strip_a), G_A1);

    player.hold_guide(true);
    assert_eq!(player.d_pad(1, 0), vec![ControllerDelivery::Handled]);
    player.hold_guide(false);
    assert_eq!(
        active_group(&player.dump(), desk.strip_a),
        G_A2,
        "Guide + right focuses the next window on the strip"
    );

    player.hold_guide(true);
    player.d_pad(0, 1);
    player.hold_guide(false);
    let layout = player.dump();
    drop(player);
    assert_eq!(
        active_strip(&layout, desk.home),
        key(&desk.strip_b),
        "Guide + down moves to the strip below"
    );
}

#[test]
fn guide_and_a_bumper_carry_the_window_along_the_strip() {
    let desk = desk();
    let mut player = Player::new(&desk.tree, ControllerFamily::PlayStation);
    player.hold_guide(true);
    player.tap(GamepadButton::RightBumper);
    player.hold_guide(false);
    let layout = player.dump();
    drop(player);
    assert_eq!(
        strip_groups(&layout, desk.strip_a),
        vec![G_A2, G_A1],
        "the focused window moved one place right"
    );
}

#[test]
fn triggers_switch_workspaces() {
    let desk = desk();
    let mut player = Player::new(&desk.tree, ControllerFamily::Xbox);
    player.send(ForwardedGamepadEvent::Axis {
        axis: GamepadAxis::RightTrigger,
        value: i16::MAX,
    });
    player.send(ForwardedGamepadEvent::Axis {
        axis: GamepadAxis::RightTrigger,
        value: 0,
    });
    assert_eq!(
        player.dump()["active"].clone(),
        serde_json::to_value(desk.other).unwrap()
    );
    player.send(ForwardedGamepadEvent::Axis {
        axis: GamepadAxis::LeftTrigger,
        value: i16::MAX,
    });
    let active = player.dump()["active"].clone();
    drop(player);
    assert_eq!(active, serde_json::to_value(desk.home).unwrap());
}

#[test]
fn start_select_and_y_open_the_guide_overview_and_search() {
    let desk = desk();
    let mut player = Player::new(&desk.tree, ControllerFamily::Xbox);

    player.tap(GamepadButton::Select);
    assert!(player.chrome.borrow().overlays.overview);
    player.tap(GamepadButton::Select);
    assert!(!player.chrome.borrow().overlays.overview);

    player.tap(GamepadButton::Start);
    assert!(player.chrome.borrow().overlays.shortcut_help);
    player.tap(GamepadButton::Start);
    assert!(!player.chrome.borrow().overlays.shortcut_help);

    assert!(!player.host.palette_visible());
    player.tap(GamepadButton::North);
    let palette_open = player.host.palette_visible();
    drop(player);
    assert!(palette_open, "Y opens search");
}

#[test]
fn focus_moves_and_confirm_become_keys_for_the_focused_surface() {
    let desk = desk();
    let mut player = Player::new(&desk.tree, ControllerFamily::Nintendo);
    let before = player.dump();

    assert_eq!(
        player.d_pad(1, 0),
        vec![ControllerDelivery::Key(ControllerKey::Move(
            NavDirection::Right
        ))]
    );
    assert_eq!(
        player.send(ForwardedGamepadEvent::Axis {
            axis: GamepadAxis::LeftY,
            value: i16::MIN,
        }),
        vec![ControllerDelivery::Key(ControllerKey::Move(
            NavDirection::Up
        ))]
    );
    // Nintendo prints A on the east button.
    assert_eq!(
        player.tap(GamepadButton::East),
        vec![ControllerDelivery::Key(ControllerKey::Activate)]
    );
    assert_eq!(
        player.tap(GamepadButton::South),
        vec![ControllerDelivery::Key(ControllerKey::Back)]
    );
    let after = player.dump();
    drop(player);
    assert_eq!(
        before, after,
        "moving focus inside an app leaves the desktop layout alone"
    );
}
