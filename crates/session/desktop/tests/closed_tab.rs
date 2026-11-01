//! The desktop's reopen chord on the real chrome: a window the person closed
//! comes back where it was, and the close is undone once.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desktop_host::{DesktopChrome, DesktopRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_desktop_surface::navigation::Command;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use ui::headless::FrameHarness;
use ui::synthetic_input::key_press;
use ui::widget::TileHost;
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState};

/// The windows of the test desk, with their content as plain notices.
struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

struct Rules {
    handle: DesktopHandle,
}

impl DesktopRules for Rules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        "Ada".into()
    }

    fn may(&self, _: &DesktopCommand) -> Result<(), String> {
        Ok(())
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        Ok(())
    }
}

fn window(id: u64, title: &str, app: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
    }
}

fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
    let active_tab = tabs[0].id;
    WindowGroup {
        id,
        tabs,
        active_tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

fn desk_tree(groups: Vec<WindowGroup>) -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: groups,
    }];
    let mut tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The desk with `tree` on it and the chrome that carries the desktop's
/// chords over it: the desk and the chrome are the two layers the session
/// hosts, both driving the one desktop.
fn desk_and_chrome(tree: &DesktopTree) -> (FrameHarness, FrameHarness, DesktopHandle) {
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(Rules {
            handle: handle.clone(),
        }),
        TopBarChrome::from_tree(tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::Theme::dark(),
    );
    frame.relayout();
    (desk, frame, handle)
}

fn hold(frame: &mut FrameHarness, mods: ModifiersState) {
    frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(mods)));
}

fn press(frame: &mut FrameHarness, key: Key) {
    frame.send_host_event(&key_press(key, None));
}

/// The windows of group `group`, in the order the strip shows them.
fn windows_of(handle: &DesktopHandle, group: &str) -> Vec<String> {
    handle
        .snapshot("g", vec![])
        .entries
        .into_iter()
        .filter(|entry| entry.route.group == group)
        .map(|entry| entry.route.tab)
        .collect()
}

/// What the desk has in front: the active window of the active strip.
fn in_front(handle: &DesktopHandle) -> Option<u64> {
    handle
        .snapshot("g", vec![])
        .active
        .and_then(|route| route.tab.parse().ok())
}

/// A window closed from its ✕ comes back where it was when the person asks
/// for it back: at its old index in its group, in front, and only the once.
#[test]
fn the_reopen_chord_brings_a_closed_window_back_where_it_was() {
    let tree = desk_tree(vec![
        group(
            3,
            vec![
                window(20, "Notes", "so.tilo.notes"),
                window(30, "Chat", "so.tilo.chat"),
                window(40, "Files", "so.tilo.files"),
            ],
        ),
        group(4, vec![window(50, "Mail", "so.tilo.mail")]),
    ]);
    let (mut desk, mut frame, handle) = desk_and_chrome(&tree);
    assert_eq!(windows_of(&handle, "3"), ["20", "30", "40"]);

    // The window's ✕ asks the desktop to close it, as a page's does.
    let closed = handle.snapshot("g", vec![]).entries[1].route.clone();
    assert_eq!(closed.group, "3");
    handle
        .dispatch(
            &Command::Close {
                generation: "g".into(),
                route: closed,
            },
            "g",
        )
        .unwrap();
    for _ in 0..4 {
        desk.relayout();
    }
    assert_eq!(windows_of(&handle, "3"), ["20", "40"]);

    hold(&mut frame, ModifiersState::SUPER | ModifiersState::SHIFT);
    press(&mut frame, Key::Character("t".into()));
    for _ in 0..4 {
        desk.relayout();
    }
    assert_eq!(
        windows_of(&handle, "3"),
        ["20", "30", "40"],
        "back at its old index in its group"
    );
    assert_eq!(in_front(&handle), Some(30), "and in front");

    // The close has been undone: the chord brings back nothing a second
    // time, and the desktop still answers it.
    press(&mut frame, Key::Character("t".into()));
    for _ in 0..4 {
        desk.relayout();
    }
    assert_eq!(windows_of(&handle, "3"), ["20", "30", "40"]);
    assert_eq!(windows_of(&handle, "4"), ["50"]);
}
