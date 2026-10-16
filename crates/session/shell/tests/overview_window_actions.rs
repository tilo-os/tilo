//! The overview's window previews act on their windows, end to end through
//! the real overlay host: a click or a tap goes to the window and closes
//! the overview, a middle click closes the window, dragging a preview onto
//! another workspace moves the window there, and the arrow keys and Enter
//! do the same without a pointer.

use std::sync::Arc;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, FreshId as _, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree,
};
use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, Role};
use ui::synthetic_input::{cursor_moved, key_event, keyboard_input, mouse_input};
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, Touch, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::support::host::{dispatch, layout_host};

/// The client window: an app process the sandbox of "Home" launched.
const CLIENT_TAB: u64 = 4;
const CLIENT_SURFACE: u64 = 4_404;
const CLIENT_APP: &str = "org.example.Notes";

/// "Home" holds tabs 1..=4 in single-tab groups 10..=40 along one strip:
/// three windows the shell draws and one client. "Elsewhere" is a second
/// workspace with an empty strip.
fn desktop() -> (WorkspaceTree, StripId, WorkspaceId) {
    let stub = |id: u64, kind: AppKind| Tab {
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
        focus_history: Vec::new(),
    };
    let mut tree = WorkspaceTree::bootstrap_default();
    let strip = StripId::new();
    let home = tree.desktop.workspaces[0].id;
    tree.desktop.workspaces[0].strips = vec![Strip {
        id: strip,
        name: String::new(),
        window_groups: vec![
            group(10, stub(1, AppKind::Terminal)),
            group(20, stub(2, AppKind::Browser)),
            group(30, stub(3, AppKind::FileExplorer)),
            group(
                40,
                Tab {
                    id: CLIENT_TAB,
                    title: "Notes".into(),
                    tile: TileContent::Foreign {
                        app_id: CLIENT_APP.into(),
                        surface_id: CLIENT_SURFACE,
                    },
                },
            ),
        ],
    }];
    tree.desktop.active_strip.insert(home, strip);
    tree.desktop.set_active_group(strip, 10);
    let elsewhere = WorkspaceId::new();
    let mut other = Workspace::new(elsewhere, "Elsewhere");
    let other_strip = StripId::new();
    other.strips.push(Strip::new(other_strip, "Main"));
    tree.desktop.workspaces.push(other);
    tree.desktop.active_strip.insert(elsewhere, other_strip);
    tree.seed_doc_from_cache();
    (tree, strip, elsewhere)
}

struct Harness {
    host: ShellOverlayHost,
    chrome: Chrome,
    taffy: TaffyTree<()>,
    node: NodeId,
    strip: StripId,
    elsewhere: WorkspaceId,
    _home: tilo_test_env::TestHome,
}

impl Harness {
    /// The desktop with the overview up and a preview on hand for every
    /// window, the way the compositor leaves it once a round has run.
    fn new() -> Self {
        let home = tilo_test_env::TestHome::new().expect("isolated home");
        let (tree, strip, elsewhere) = desktop();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let (taffy, node) = layout_host(&mut host);
        let mut h = Self {
            host,
            chrome,
            taffy,
            node,
            strip,
            elsewhere,
            _home: home,
        };
        for tab in [3_u64, 2, 1] {
            let _ = h
                .host
                .focus_window_on_strip(h.strip, tab.saturating_mul(10), tab);
        }
        for tab in 1..=CLIENT_TAB {
            let pixels: Arc<[u8]> = vec![0x80; 16 * 10 * 4].into();
            h.host.store_window_preview(tab, pixels, 16, 10, None);
        }
        {
            let mut state = h.chrome.borrow_mut();
            state.overlays.overview = true;
            state.bump();
        }
        h.host.force_overview_settled(true);
        h
    }

    fn event(&mut self, event: &WindowEvent) -> bool {
        let modifiers = Modifiers::default();
        dispatch(&mut self.host, &self.taffy, self.node, modifiers, event)
    }

    fn key(&mut self, key: Key) -> bool {
        self.event(&keyboard_input(key_event(key, None, ElementState::Pressed)))
    }

    fn cursor_to(&mut self, at: Point) {
        self.event(&cursor_moved(at.x, at.y));
    }

    fn button(&mut self, button: MouseButton, state: ElementState) -> bool {
        self.event(&mouse_input(state, button))
    }

    fn touch(&mut self, id: u64, phase: TouchPhase, at: Point) -> bool {
        self.event(&WindowEvent::Touch(Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: PhysicalPosition::new(at.x, at.y),
            force: None,
            id,
        }))
    }

    /// Press, travel in steps and let go with the primary button.
    fn mouse_drag(&mut self, from: Point, to: Point) {
        self.cursor_to(from);
        assert!(self.button(MouseButton::Left, ElementState::Pressed));
        for step in 1..=8 {
            let t = f64::from(step) / 8.0;
            self.cursor_to(from.lerp(to, t));
        }
        assert!(self.button(MouseButton::Left, ElementState::Released));
    }

    fn overview_open(&self) -> bool {
        self.chrome.borrow().overlays.overview
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

    /// Every tab in workspace `workspace`, in tree order.
    fn tabs_in(&self, workspace: WorkspaceId) -> Vec<Tab> {
        let state = self.chrome.borrow();
        let tree = state.tree();
        tree.desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .map(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter().cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn home(&self) -> WorkspaceId {
        self.chrome.borrow().tree().desktop.workspaces[0].id
    }

    fn overview_node(&self) -> AccessNode {
        self.host
            .accessibility()
            .expect("the host publishes a semantic tree")
            .children
            .into_iter()
            .find(|node| {
                node.role == Role::Dialog && node.name.as_deref() == Some("Workspace overview")
            })
            .expect("the overview is on screen")
    }

    /// The overview's window row: each window's title, bounds and whether
    /// the keyboard is on it.
    fn window_row(&self) -> Vec<(String, Rect, bool)> {
        self.overview_node()
            .children
            .into_iter()
            .find(|node| node.role == Role::List)
            .expect("the overview lists the room's windows")
            .children
            .into_iter()
            .map(|item| {
                (
                    item.name.clone().unwrap_or_default(),
                    item.bounds.expect("each window is on screen").to_rect(),
                    item.state.selected && item.state.focused,
                )
            })
            .collect()
    }

    fn preview_of(&self, title: &str) -> Point {
        self.window_row()
            .into_iter()
            .find(|(name, _, _)| name == title)
            .map_or_else(
                || panic!("{title} has a preview in the overview"),
                |(_, bounds, _)| bounds.center(),
            )
    }

    fn selected_window(&self) -> Option<String> {
        self.window_row()
            .into_iter()
            .find(|(_, _, selected)| *selected)
            .map(|(name, _, _)| name)
    }

    fn room_row(&self, name: &str) -> Point {
        self.overview_node()
            .children
            .into_iter()
            .find(|node| node.role == Role::Button && node.name.as_deref() == Some(name))
            .and_then(|node| node.bounds.map(ui::a11y::AccessBounds::to_rect))
            .map_or_else(
                || panic!("{name} is listed in the overview"),
                |bounds| bounds.center(),
            )
    }
}

#[test]
fn clicking_a_preview_goes_to_its_window_and_closes_the_overview() {
    let mut h = Harness::new();
    assert_eq!(h.focused(), 1);
    let titles: Vec<String> = h.window_row().into_iter().map(|(name, ..)| name).collect();
    assert_eq!(
        titles,
        ["Terminal 1", "Browser 2", "Files 3", "Notes"],
        "the room's windows, most recently used first"
    );

    let browser = h.preview_of("Browser 2");
    h.cursor_to(browser);
    assert!(h.button(MouseButton::Left, ElementState::Pressed));
    assert!(h.overview_open(), "a press alone may still become a drag");
    assert!(h.button(MouseButton::Left, ElementState::Released));
    let focused = h.focused();
    let overview_open = h.overview_open();
    drop(h);
    assert_eq!(focused, 2, "the click went to the window");
    assert!(!overview_open, "and closed the overview");
}

#[test]
fn a_middle_click_closes_the_window_and_the_overview_stays() {
    let mut h = Harness::new();
    let files = h.preview_of("Files 3");
    h.cursor_to(files);
    assert!(h.button(MouseButton::Middle, ElementState::Pressed));
    let home: Vec<u64> = h.tabs_in(h.home()).iter().map(|tab| tab.id).collect();
    assert_eq!(
        home,
        [1, 2, CLIENT_TAB],
        "only the middle-clicked window closed"
    );
    assert!(h.overview_open());
    assert_eq!(h.focused(), 1, "focus did not move");
    let titles: Vec<String> = h.window_row().into_iter().map(|(name, ..)| name).collect();
    drop(h);
    assert_eq!(titles, ["Terminal 1", "Browser 2", "Notes"]);
}

#[test]
fn dragging_a_preview_onto_another_workspace_moves_the_window_there() {
    let mut h = Harness::new();
    let elsewhere = h.elsewhere;
    let from = h.preview_of("Files 3");
    let to = h.room_row("Elsewhere");
    h.mouse_drag(from, to);

    let home: Vec<u64> = h.tabs_in(h.home()).iter().map(|tab| tab.id).collect();
    assert_eq!(home, [1, 2, CLIENT_TAB], "the window left its workspace");
    let moved = h.tabs_in(elsewhere);
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].id, 3, "the same tab, not a copy");
    assert!(
        matches!(
            moved[0].tile,
            TileContent::Stub {
                kind: AppKind::FileExplorer,
                ..
            }
        ),
        "a window the shell draws moves as it is"
    );
    assert!(h.overview_open(), "the overview stays up to show it leave");
    assert_eq!(
        h.chrome.borrow().tree().desktop.active,
        h.home(),
        "moving a window does not take you with it"
    );

    // Let go over the room you are in, or over nothing: nothing moves.
    let from = h.preview_of("Browser 2");
    h.mouse_drag(from, Point::new(from.x, from.y + 40.0));
    let stayed = h.tabs_in(h.home()).iter().any(|tab| tab.id == 2);
    let overview_open = h.overview_open();
    drop(h);
    assert!(stayed);
    assert!(overview_open, "a drag that lands nowhere is not a click");
}

#[test]
fn dragging_a_client_window_asks_what_crosses_before_anything_moves() {
    let mut h = Harness::new();
    let elsewhere = h.elsewhere;
    let queue = shell::shell::portal_dialog::PortalDialogQueue::default();
    let profiles = tempfile::tempdir().unwrap();
    {
        let mut state = h.chrome.borrow_mut();
        state.window_transfers.set_dialog_queue(queue.clone());
        state.window_transfers.set_attended(|| true);
        state
            .window_transfers
            .set_data_home(profiles.path().to_path_buf());
        let _ = state.take_surface_close_requests();
    }
    let from = h.preview_of("Notes");
    let to = h.room_row("Elsewhere");
    h.mouse_drag(from, to);

    assert!(
        h.tabs_in(elsewhere).is_empty(),
        "nothing moves before the answer"
    );
    assert!(h.tabs_in(h.home()).iter().any(|tab| {
        tab.id == CLIENT_TAB
            && matches!(tab.tile, TileContent::Foreign { surface_id, .. } if surface_id == CLIENT_SURFACE)
    }));
    let mut sheet = shell::shell::portal_dialog::PortalDialogController::new(queue);
    assert!(sheet.pump(), "the drop puts up the transfer sheet");
    let view = sheet.view().expect("the transfer sheet");
    assert!(view.title().contains("Notes") && view.title().contains("Elsewhere"));
    // This client saved no record that can travel, so the only offer is a
    // new window there; its own window is never asked to close.
    assert_eq!(view.choices().len(), 1);
    assert!(sheet.confirm());
    shell::shell::window_transfer::tick(&mut h.chrome.borrow_mut());
    let opened = h.tabs_in(elsewhere);
    let close_requests = h.chrome.borrow_mut().take_surface_close_requests();
    let still_home = h.tabs_in(h.home()).iter().any(|tab| tab.id == CLIENT_TAB);
    drop(h);
    assert!(matches!(
        opened.as_slice(),
        [tab] if matches!(&tab.tile, TileContent::LocalApp { app_id, note, .. }
            if app_id == CLIENT_APP && note.is_empty())
    ));
    assert!(still_home);
    assert!(close_requests.is_empty());
}

#[test]
fn arrows_walk_the_previews_and_enter_goes_to_the_selected_window() {
    let mut h = Harness::new();
    assert_eq!(h.selected_window(), None, "the keyboard starts on the room");

    assert!(h.key(Key::Named(NamedKey::ArrowRight)));
    assert_eq!(h.selected_window().as_deref(), Some("Terminal 1"));
    assert!(h.key(Key::Named(NamedKey::ArrowRight)));
    assert_eq!(h.selected_window().as_deref(), Some("Browser 2"));
    assert!(h.key(Key::Named(NamedKey::ArrowLeft)));
    assert!(h.key(Key::Named(NamedKey::ArrowLeft)));
    assert_eq!(
        h.selected_window().as_deref(),
        Some("Notes"),
        "a ring, like the room list"
    );
    assert!(h.key(Key::Named(NamedKey::ArrowLeft)));
    assert_eq!(h.selected_window().as_deref(), Some("Files 3"));
    assert!(h.overview_open());

    assert!(h.key(Key::Named(NamedKey::Enter)));
    let focused = h.focused();
    let overview_open = h.overview_open();
    drop(h);
    assert_eq!(focused, 3, "Enter went to the selected window");
    assert!(!overview_open);
}

#[test]
fn up_and_down_leave_the_previews_for_the_rooms() {
    let mut h = Harness::new();
    assert!(h.key(Key::Named(NamedKey::ArrowRight)));
    assert!(h.selected_window().is_some());
    assert!(h.key(Key::Named(NamedKey::ArrowDown)));
    assert_eq!(h.selected_window(), None);
    assert!(h.key(Key::Named(NamedKey::Enter)));
    let active = h.chrome.borrow().tree().desktop.active;
    let elsewhere = h.elsewhere;
    let focused = h.focused();
    drop(h);
    assert_eq!(
        active, elsewhere,
        "Enter goes to the room the keyboard moved to"
    );
    assert_eq!(focused, 1, "and focuses no window in the room it left");
}

#[test]
fn a_tap_goes_to_the_window_and_a_finger_drag_moves_it() {
    let mut h = Harness::new();
    let elsewhere = h.elsewhere;

    let from = h.preview_of("Files 3");
    let to = h.room_row("Elsewhere");
    assert!(h.touch(1, TouchPhase::Started, from));
    for step in 1..=8 {
        let t = f64::from(step) / 8.0;
        assert!(h.touch(1, TouchPhase::Moved, from.lerp(to, t)));
    }
    assert!(h.touch(1, TouchPhase::Ended, to));
    assert_eq!(
        h.tabs_in(elsewhere)
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        [3],
        "the finger carried the window to the other workspace"
    );
    assert!(h.overview_open());

    let browser = h.preview_of("Browser 2");
    assert!(h.touch(2, TouchPhase::Started, browser));
    assert!(h.touch(2, TouchPhase::Ended, browser));
    let focused = h.focused();
    let overview_open = h.overview_open();
    drop(h);
    assert_eq!(focused, 2, "the tap went to the window");
    assert!(!overview_open);
}
