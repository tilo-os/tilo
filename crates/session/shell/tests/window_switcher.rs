//! Alt+Tab end to end through the real overlay host: windows come in the
//! order they were last used, a held Alt raises the switcher and its release
//! switches, a quick Alt+Tab goes straight back without showing anything,
//! Escape stays put, and Alt+Grave walks one app's windows.

use std::collections::HashMap;
use std::time::Duration;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, FreshId as _, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    WorkspaceTree,
};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, Role};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Tabs 1..=4 in single-tab groups 10..=40 along one strip. 1 and 3 are
/// both terminals; 2 is a browser and 4 a file manager.
fn desktop() -> (WorkspaceTree, StripId) {
    let tab = |id: u64, kind: AppKind| Tab {
        id,
        title: format!("{} {id}", kind.as_str()),
        tile: TileContent::Stub {
            label: kind.as_str().to_string(),
            kind,
        },
    };
    let group = |id: GroupId, tab: Tab| WindowGroup {
        id,
        active_tab: tab.id,
        tabs: vec![tab],
        width: WindowWidth::Normal,
        maximized: false,
    };
    let mut tree = WorkspaceTree::bootstrap_default();
    let strip = StripId::new();
    let workspace = tree.desktop.workspaces[0].id;
    tree.desktop.workspaces[0].strips = vec![Strip {
        id: strip,
        name: String::new(),
        window_groups: vec![
            group(10, tab(1, AppKind::Terminal)),
            group(20, tab(2, AppKind::Browser)),
            group(30, tab(3, AppKind::Terminal)),
            group(40, tab(4, AppKind::FileExplorer)),
        ],
    }];
    tree.desktop.active_strip.insert(workspace, strip);
    tree.desktop.set_active_group(strip, 10);
    tree.seed_doc_from_cache();
    (tree, strip)
}

struct Harness {
    host: ShellOverlayHost,
    chrome: Chrome,
    taffy: TaffyTree<()>,
    node: NodeId,
    strip: StripId,
    _home: tilo_test_env::TestHome,
}

impl Harness {
    fn new() -> Self {
        let home = tilo_test_env::TestHome::new().expect("isolated home");
        let (tree, strip) = desktop();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let (taffy, node) = lay_out(&mut host);
        Self {
            host,
            chrome,
            taffy,
            node,
            strip,
            _home: home,
        }
    }

    fn event(&mut self, event: &WindowEvent, mods: ModifiersState) -> bool {
        let mut doc = automerge::AutoCommit::new();
        let mut handled = false;
        let mut focus = None;
        self.host.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::from(mods),
                state: ui::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: self.node,
                taffy: &self.taffy,
            },
        );
        handled
    }

    /// `mods` is the modifier state with this key applied, as the
    /// compositor hands it on: Alt's own release arrives with Alt up.
    fn key(&mut self, key: Key, state: ElementState, mods: ModifiersState) -> bool {
        let key_event = ui::synthetic_input::key_event(key, None, state);
        self.event(
            &WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event: key_event,
                is_synthetic: false,
            },
            mods,
        )
    }

    fn hold_alt(&mut self) {
        self.key(
            Key::Named(NamedKey::Alt),
            ElementState::Pressed,
            ModifiersState::ALT,
        );
    }

    fn tab_with_alt(&mut self) {
        let mut both = |state| {
            assert!(
                self.key(Key::Named(NamedKey::Tab), state, ModifiersState::ALT),
                "Alt+Tab never reaches a window"
            );
        };
        both(ElementState::Pressed);
        both(ElementState::Released);
    }

    fn release_alt(&mut self) {
        assert!(
            self.key(
                Key::Named(NamedKey::Alt),
                ElementState::Released,
                ModifiersState::empty(),
            ),
            "the release that ends a switch belongs to the shell"
        );
    }

    /// Focus tab `tab` (in group `tab * 10`) the way a click on it does.
    fn focus(&mut self, tab: u64) {
        let _ = self
            .host
            .focus_window_on_strip(self.strip, tab.saturating_mul(10), tab);
        assert_eq!(self.focused(), tab);
    }

    fn focused(&self) -> u64 {
        let state = self.chrome.borrow();
        let tree = state.tree();
        let group = tree.desktop.active_group_for(self.strip).expect("a focus");
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .find(|candidate| candidate.id == group)
            .map(|group| group.active_tab)
            .expect("the focused group exists")
    }

    fn switcher_on_screen(&self) -> Option<AccessNode> {
        self.host
            .accessibility()
            .expect("the host publishes a semantic tree")
            .children
            .into_iter()
            .find(|node| {
                node.role == Role::Dialog && node.name.as_deref() == Some("Switch windows")
            })
    }
}

fn lay_out(host: &mut ShellOverlayHost) -> (TaffyTree<()>, NodeId) {
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    let mut text = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1600.0),
                height: AvailableSpace::Definite(900.0),
            },
            |known, available, id, _, _| match node_map.get(&id) {
                Some(&widget) => unsafe { (*widget).measure(known, available, &mut text) },
                None => Size::ZERO,
            },
        )
        .expect("layout computes");
    host.apply_layout(&taffy, node, Point::ZERO);
    (taffy, node)
}

/// Titles of the switcher's windows in order, and the selected one.
fn listed(switcher: &AccessNode) -> (Vec<String>, Option<String>) {
    let list = switcher
        .children
        .iter()
        .find(|node| node.role == Role::List)
        .expect("the switcher lists its windows");
    let titles = list
        .children
        .iter()
        .filter_map(|item| item.name.clone())
        .collect();
    let selected = list
        .children
        .iter()
        .find(|item| item.state.selected && item.state.focused)
        .and_then(|item| item.name.clone());
    (titles, selected)
}

#[test]
fn holding_alt_through_two_tabs_then_releasing_focuses_the_third_most_recent_window() {
    let mut h = Harness::new();
    for tab in [2, 4, 1] {
        h.focus(tab);
    }

    h.hold_alt();
    h.tab_with_alt();
    h.tab_with_alt();
    assert_eq!(h.focused(), 1, "nothing switches while Alt is held");
    h.host
        .window_switcher_mut()
        .elapse(Duration::from_millis(200));
    let switcher = h
        .switcher_on_screen()
        .expect("a held Alt raises the switcher");
    let (titles, selected) = listed(&switcher);
    assert_eq!(
        titles,
        ["Terminal 1", "Files 4", "Browser 2", "Terminal 3"],
        "most recently used first; the never-focused window last"
    );
    assert_eq!(selected.as_deref(), Some("Browser 2"));

    h.release_alt();
    let focused = h.focused();
    let on_screen = h.switcher_on_screen().is_some();
    let active = h.host.window_switcher_active();
    drop(h);
    assert_eq!(focused, 2, "releasing Alt goes to the third most recent");
    assert!(!on_screen);
    assert!(!active);
}

#[test]
fn a_quick_alt_tab_goes_back_to_the_previous_window_without_showing_the_switcher() {
    let mut h = Harness::new();
    h.focus(3);
    h.focus(1);

    h.hold_alt();
    h.tab_with_alt();
    assert!(
        h.switcher_on_screen().is_none(),
        "a quick Alt+Tab never flashes the switcher"
    );
    h.release_alt();
    assert_eq!(h.focused(), 3, "Alt+Tab is \"back to the last window\"");

    h.hold_alt();
    h.tab_with_alt();
    h.release_alt();
    let focused = h.focused();
    drop(h);
    assert_eq!(focused, 1, "and again goes back once more");
}

#[test]
fn escape_cancels_and_the_later_alt_release_changes_nothing() {
    let mut h = Harness::new();
    h.focus(4);
    h.focus(2);

    h.hold_alt();
    h.tab_with_alt();
    h.tab_with_alt();
    assert!(h.key(
        Key::Named(NamedKey::Escape),
        ElementState::Pressed,
        ModifiersState::ALT
    ));
    assert!(!h.host.window_switcher_active());
    h.key(
        Key::Named(NamedKey::Alt),
        ElementState::Released,
        ModifiersState::empty(),
    );
    let focused = h.focused();
    drop(h);
    assert_eq!(focused, 2, "Escape stays on the window it began from");
}

#[test]
fn alt_grave_cycles_the_focused_apps_windows() {
    let mut h = Harness::new();
    h.focus(2);
    h.focus(1);

    let grave = || Key::Character("`".into());
    h.hold_alt();
    assert!(h.key(grave(), ElementState::Pressed, ModifiersState::ALT));
    h.host
        .window_switcher_mut()
        .elapse(Duration::from_millis(200));
    let (titles, _) = listed(
        &h.host
            .accessibility()
            .unwrap()
            .children
            .into_iter()
            .find(|node| node.role == Role::Dialog)
            .expect("the app switcher is on screen"),
    );
    assert_eq!(titles, ["Terminal 1", "Terminal 3"], "only the terminals");
    h.release_alt();
    assert_eq!(h.focused(), 3);

    h.hold_alt();
    h.key(grave(), ElementState::Pressed, ModifiersState::ALT);
    h.release_alt();
    assert_eq!(h.focused(), 1, "Alt+` again returns to the other terminal");

    h.focus(4);
    h.hold_alt();
    h.key(grave(), ElementState::Pressed, ModifiersState::ALT);
    assert!(
        !h.host.window_switcher_active(),
        "an app with one window has nothing to switch to"
    );
    h.key(
        Key::Named(NamedKey::Alt),
        ElementState::Released,
        ModifiersState::empty(),
    );
    let focused = h.focused();
    drop(h);
    assert_eq!(focused, 4);
}

#[test]
fn clicking_a_window_in_the_switcher_goes_there() {
    let mut h = Harness::new();
    h.focus(1);

    h.hold_alt();
    h.tab_with_alt();
    h.host
        .window_switcher_mut()
        .elapse(Duration::from_millis(200));
    let order: Vec<u64> = {
        let state = h.chrome.borrow();
        h.host
            .window_switcher()
            .ordered_windows(
                &state.tree().desktop,
                &shell::shell::window_previews::ShellWindows,
            )
            .iter()
            .map(|window| window.address.tab)
            .collect()
    };
    let target = order.iter().position(|tab| *tab == 4).expect("listed");
    let card = h
        .host
        .window_switcher()
        .card_rect(target)
        .expect("every window fits on screen");
    let at = card.center();
    h.event(
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(at.x, at.y),
        },
        ModifiersState::ALT,
    );
    assert_eq!(
        h.host
            .window_switcher()
            .selected()
            .map(|window| window.address.tab),
        Some(4),
        "hovering a window selects it"
    );
    assert!(h.event(
        &WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        ModifiersState::ALT,
    ));
    let focused = h.focused();
    let active = h.host.window_switcher_active();
    let release_absorbed = h.event(
        &WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
        ModifiersState::ALT,
    );
    drop(h);
    assert_eq!(focused, 4);
    assert!(!active);
    assert!(
        release_absorbed,
        "the release of the click that chose a window never reaches that window"
    );
}

#[test]
fn closing_another_window_while_alt_is_held_keeps_the_intended_selection() {
    let mut h = Harness::new();
    h.focus(3);
    h.focus(1);
    h.hold_alt();
    h.tab_with_alt();
    h.host
        .window_switcher_mut()
        .elapse(Duration::from_millis(200));
    assert!(h.host.close_tab_in_group(20, Some(2)));
    h.event(
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(0.0, 0.0),
        },
        ModifiersState::ALT,
    );
    let (titles, selected) = listed(&h.switcher_on_screen().expect("surviving session"));
    assert!(!titles.iter().any(|title| title == "Browser 2"));
    assert_eq!(selected.as_deref(), Some("Terminal 3"));
    h.release_alt();
    let focused = h.focused();
    drop(h);
    assert_eq!(focused, 3);
}

#[test]
fn closing_the_armed_window_cancels_before_a_later_alt_release() {
    let mut h = Harness::new();
    h.focus(4);
    h.focus(1);
    h.hold_alt();
    h.tab_with_alt();
    h.host
        .window_switcher_mut()
        .elapse(Duration::from_millis(200));
    assert!(h.host.close_tab_in_group(40, Some(4)));
    h.event(
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(0.0, 0.0),
        },
        ModifiersState::ALT,
    );
    assert!(!h.host.window_switcher_active());
    assert!(h.switcher_on_screen().is_none());
    h.key(
        Key::Named(NamedKey::Alt),
        ElementState::Released,
        ModifiersState::empty(),
    );
    assert_eq!(h.focused(), 1);
    h.hold_alt();
    h.tab_with_alt();
    h.release_alt();
    let focused = h.focused();
    drop(h);
    assert_ne!(focused, 4, "reopening cannot rearm a closed window");
}
