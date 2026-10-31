//! Alt+Tab and Alt with the key above it on the real chrome: the windows of
//! the workspace, the windows of the app in front, and the focus they leave
//! behind.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
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
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use ui::headless::FrameHarness;
use ui::kurbo::Affine;
use ui::synthetic_input::key_press;
use ui::widget::{TileHost, Widget};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

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

fn hold_alt(frame: &mut FrameHarness, mods: ModifiersState) {
    frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(mods)));
}

fn press(frame: &mut FrameHarness, key: Key) {
    frame.send_host_event(&key_press(key, None));
}

/// What the desk has in front: the active window of the active strip.
fn in_front(handle: &DesktopHandle) -> Option<u64> {
    handle.read(|tree| {
        tree.active_workspace().and_then(|workspace| {
            let strip = tree.active_strip.get(&workspace.id)?;
            let group = tree.active_group_for(*strip)?;
            workspace
                .strips
                .iter()
                .find(|candidate| candidate.id == *strip)?
                .window_groups
                .iter()
                .find(|candidate| candidate.id == group)
                .map(|group| group.active_tab)
        })
    })
}

/// Run `visit` on the chrome the frame mounts, as the session's chrome layer
/// holds it.
fn with_chrome<R>(frame: &mut FrameHarness, visit: impl FnOnce(&mut DesktopChrome) -> R) -> R {
    let mut chrome = frame.root_mut::<DesktopChrome>();
    visit(&mut chrome)
}

/// Draw one frame of the chrome, as the session's chrome layer does.
fn draw_chrome(frame: &mut FrameHarness) {
    let theme = ui::Theme::dark();
    let mut text = ui::TextRenderer::default();
    let presence = ui::presence::PresenceCtx::default();
    let mut scene = ui::vello::Scene::new();
    with_chrome(frame, |chrome| {
        chrome.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
    });
}

/// Alt+Tab walks the workspace's windows and Alt up goes to the one it
/// stopped on.
#[test]
fn alt_tab_walks_the_workspaces_windows_and_alt_up_goes_there() {
    let tree = desk_tree(vec![
        group(3, vec![window(20, "Notes", "so.tilo.notes")]),
        group(4, vec![window(30, "Chat", "so.tilo.chat")]),
    ]);
    let (mut desk, mut frame, handle) = desk_and_chrome(&tree);
    assert_eq!(in_front(&handle), Some(20), "the first window is in front");

    let start = ui::time::now();
    ui::time::testing::with_monotonic_time(start, || {
        hold_alt(&mut frame, ModifiersState::ALT);
        press(&mut frame, Key::Named(NamedKey::Tab));
    });

    with_chrome(&mut frame, |chrome| {
        assert!(chrome.switcher_active(), "Alt+Tab opens the switcher");
        assert_eq!(chrome.switcher_selected(), Some(30), "on the other window");
        assert!(
            chrome.wants_keyboard(),
            "the keys are the switcher's while it is up"
        );
    });
    assert_eq!(
        in_front(&handle),
        Some(20),
        "nothing moves until Alt comes up"
    );

    // A held Alt keeps the panel up; it draws over the desk with a card for
    // each window.
    let later = start + ui::time::Duration::from_millis(300);
    ui::time::testing::with_monotonic_time(later, || {
        frame.relayout();
        draw_chrome(&mut frame);
        with_chrome(&mut frame, |chrome| {
            assert!(chrome.switcher_revealed(), "the panel is up once shown");
            let panel = chrome.switcher_panel();
            assert!(
                panel.width() > 0.0 && panel.height() > 0.0,
                "the panel has an area over the desk: {panel:?}"
            );
        });
    });

    hold_alt(&mut frame, ModifiersState::empty());

    with_chrome(&mut frame, |chrome| {
        assert!(!chrome.switcher_active(), "Alt up closes the switcher");
        assert!(!chrome.wants_keyboard());
    });
    desk.relayout();
    assert_eq!(in_front(&handle), Some(30), "and goes to that window");
}

/// The next Alt+Tab starts from where the last one left off: it goes straight
/// back to the window that was in front a moment ago.
#[test]
fn alt_tab_again_returns_to_where_the_desktop_was() {
    let tree = desk_tree(vec![
        group(3, vec![window(20, "Notes", "so.tilo.notes")]),
        group(4, vec![window(30, "Chat", "so.tilo.chat")]),
    ]);
    let (mut desk, mut frame, handle) = desk_and_chrome(&tree);

    hold_alt(&mut frame, ModifiersState::ALT);
    press(&mut frame, Key::Named(NamedKey::Tab));
    hold_alt(&mut frame, ModifiersState::empty());
    desk.relayout();
    assert_eq!(in_front(&handle), Some(30));

    hold_alt(&mut frame, ModifiersState::ALT);
    press(&mut frame, Key::Named(NamedKey::Tab));
    with_chrome(&mut frame, |chrome| {
        assert_eq!(
            chrome.switcher_selected(),
            Some(20),
            "back to the window that was in front before"
        );
    });
    hold_alt(&mut frame, ModifiersState::empty());
    desk.relayout();
    assert_eq!(in_front(&handle), Some(20));
}

/// Alt with the key above Tab stays in the app in front: only its own
/// windows are offered.
#[test]
fn alt_grave_walks_the_windows_of_the_app_in_front() {
    let tree = desk_tree(vec![
        group(
            3,
            vec![
                window(20, "Notes", "so.tilo.notes"),
                window(21, "Notes — plan", "so.tilo.notes"),
            ],
        ),
        group(4, vec![window(30, "Chat", "so.tilo.chat")]),
    ]);
    let (mut desk, mut frame, handle) = desk_and_chrome(&tree);

    hold_alt(&mut frame, ModifiersState::ALT);
    press(&mut frame, Key::Character("`".into()));

    with_chrome(&mut frame, |chrome| {
        assert!(chrome.switcher_active());
        assert_eq!(
            chrome.switcher_selected(),
            Some(21),
            "the app's other window, not the other app's"
        );
    });
    hold_alt(&mut frame, ModifiersState::empty());
    desk.relayout();
    assert_eq!(in_front(&handle), Some(21));
}

/// A lone window leaves Alt+Tab to the window: there is nothing to switch to.
#[test]
fn a_lone_window_keeps_alt_tab() {
    let tree = desk_tree(vec![group(3, vec![window(20, "Notes", "so.tilo.notes")])]);
    let (_desk, mut frame, _handle) = desk_and_chrome(&tree);

    hold_alt(&mut frame, ModifiersState::ALT);
    press(&mut frame, Key::Named(NamedKey::Tab));
    with_chrome(&mut frame, |chrome| {
        assert!(!chrome.switcher_active(), "no switcher without two windows");
        assert!(!chrome.wants_keyboard());
    });
}
