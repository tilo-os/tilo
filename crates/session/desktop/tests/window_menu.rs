//! Super+F10 on the real chrome: the menu of the window in front opens at its
//! tab with the desk's own rows, the keyboard walks it, and choosing a row
//! does what that row says — the same menu a right-click on the tab offers.

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
use ui::synthetic_input::{key_press, named_key};
use ui::widget::TileHost;
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

fn desk_tree() -> DesktopTree {
    let tabs = vec![window(20, "Notes", "so.tilo.notes")];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
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

/// The chrome over a desk with one window, and the desk itself, which carries
/// out the commands the chrome dispatches.
fn chrome_with_window() -> (FrameHarness, FrameHarness, DesktopHandle) {
    ui::i18n::I18nManager::init("en-US");
    let tree = desk_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(Rules {
            handle: handle.clone(),
        }),
        TopBarChrome::from_tree(&tree),
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
    (frame, desk, handle)
}

fn hold(frame: &mut FrameHarness, mods: ModifiersState) {
    frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(mods)));
}

/// Super+F10, the window menu's chord.
fn press_tab_menu(frame: &mut FrameHarness) {
    hold(frame, ModifiersState::SUPER);
    frame.send_host_event(&key_press(Key::Named(NamedKey::F10), None));
    hold(frame, ModifiersState::empty());
}

/// Run `visit` on the chrome the frame mounts, as the session's chrome layer
/// holds it.
fn with_chrome<R>(frame: &mut FrameHarness, visit: impl FnOnce(&mut DesktopChrome) -> R) -> R {
    let mut chrome = frame.root_mut::<DesktopChrome>();
    visit(&mut chrome)
}

/// What the desk offers for the window showing `tab`, in order: the rows the
/// menu must show, as the surface makes them.
fn desk_rows(handle: &DesktopHandle, tab: u64) -> Vec<String> {
    handle
        .window_menu(tab)
        .into_iter()
        .map(|row| row.label)
        .collect()
}

#[test]
fn super_f10_opens_the_window_in_fronts_menu_at_its_window() {
    let (mut frame, mut desk, handle) = chrome_with_window();
    let expected = desk_rows(&handle, 20);
    assert_eq!(
        expected,
        [
            "Name this window…",
            "Full screen",
            "Fill the strip",
            "Wider",
            "Narrower",
            "Close"
        ],
        "the desk offers the window its own rows"
    );
    assert!(
        !handle.shown_tabs().is_empty(),
        "the window's tab is on the strip"
    );
    with_chrome(&mut frame, |chrome| {
        assert!(!chrome.window_menu().is_open(), "nothing is up yet");
        assert!(!chrome.wants_keyboard());
    });

    press_tab_menu(&mut frame);
    frame.relayout();

    let (labels, first_row) = with_chrome(&mut frame, |chrome| {
        assert!(chrome.window_menu().is_open(), "Super+F10 opens the menu");
        assert_eq!(
            chrome.window_menu().tab(),
            Some(20),
            "for the window in front"
        );
        assert!(
            chrome.wants_keyboard(),
            "the keys are the menu's while it is up"
        );
        let labels: Vec<String> = chrome
            .window_menu()
            .labels()
            .into_iter()
            .flatten()
            .collect();
        (labels, chrome.window_menu().item_rect(0))
    });
    assert_eq!(labels, expected, "with the desk's own rows");
    let first_row = first_row.expect("the first row is painted");
    assert!(
        first_row.y0 > 40.0,
        "the rows stand under the window, in the desk, not in the bar: {first_row:?}"
    );
    assert!(
        first_row.y1 < 800.0 && first_row.x0 >= 0.0 && first_row.x1 <= 1280.0,
        "and on the display: {first_row:?}"
    );

    // The keyboard walks the rows: Down past every row to Close, Enter takes
    // it, and the window closes as the same command does from anywhere.
    for _ in 0..expected.len() - 1 {
        frame.send_host_event(&named_key(NamedKey::ArrowDown));
    }
    frame.send_host_event(&named_key(NamedKey::Enter));
    frame.relayout();
    desk.relayout();
    with_chrome(&mut frame, |chrome| {
        assert!(!chrome.window_menu().is_open(), "the menu closes");
        assert!(!chrome.wants_keyboard(), "the desk has the keys back");
    });
    assert!(
        handle.read(|tree| tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .all(|tab| tab.id != 20)),
        "Enter chose Close"
    );
}

#[test]
fn escape_and_the_shortcut_again_close_the_menu_without_choosing() {
    let (mut frame, _desk, handle) = chrome_with_window();

    press_tab_menu(&mut frame);
    assert!(with_chrome(&mut frame, |chrome| chrome
        .window_menu()
        .is_open()));
    frame.send_host_event(&named_key(NamedKey::Escape));
    frame.relayout();
    with_chrome(&mut frame, |chrome| {
        assert!(!chrome.window_menu().is_open(), "Escape closes the menu");
        assert!(!chrome.wants_keyboard());
    });
    assert!(
        handle.read(|tree| tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .any(|tab| tab.id == 20)),
        "and leaves the window alone"
    );

    press_tab_menu(&mut frame);
    assert!(with_chrome(&mut frame, |chrome| chrome
        .window_menu()
        .is_open()));
    press_tab_menu(&mut frame);
    assert!(
        !with_chrome(&mut frame, |chrome| chrome.window_menu().is_open()),
        "the shortcut again closes what it opened"
    );
}

#[test]
fn the_menu_follows_the_window_in_front() {
    let (mut frame, _desk, handle) = chrome_with_window();
    let mut tree = handle.read(Clone::clone);
    if let Some(strip) = tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .find(|strip| strip.id == StripId(2))
    {
        strip.window_groups.push(WindowGroup {
            id: 4,
            active_tab: 21,
            tabs: vec![window(21, "Mail", "so.tilo.mail")],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
    }
    tree.active_group.insert(StripId(2), 4);
    handle.replace_tree(tree);
    frame.relayout();

    press_tab_menu(&mut frame);
    frame.relayout();
    with_chrome(&mut frame, |chrome| {
        assert!(chrome.window_menu().is_open());
        assert_eq!(
            chrome.window_menu().tab(),
            Some(21),
            "the menu is the one in front, not the first"
        );
    });
}
