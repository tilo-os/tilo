//! Every chord of the person's table on the real chrome: each one is
//! answered where it belongs — the desk moves its windows, tabs, groups,
//! strips and displays, the session is asked for the rest, and none is
//! dropped — and the rows a chord reaches show the chord the table binds now.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desk_verbs::chrome_resource;
use tilo_desktop::desktop_host::{DesktopAsk, DesktopChrome, DesktopRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use ui::headless::FrameHarness;
use ui::shortcut_table::ShortcutHome;
use ui::synthetic_input::key_press;
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
        view: String::new(),
        pinned: false,
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

fn desk_tree(strips: Vec<Strip>) -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = strips;
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    let first = tree.workspaces[0].strips[0].id;
    tree.active_strip.insert(WorkspaceSlot(1), first);
    if let Some(group) = tree.workspaces[0].strips[0].window_groups.first() {
        tree.active_group.insert(first, group.id);
    }
    tree
}

/// One strip with two windows of two tabs each.
fn two_windows() -> DesktopTree {
    desk_tree(vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![
            group(
                3,
                vec![
                    window(20, "Notes", "so.tilo.notes"),
                    window(21, "Mail", "so.tilo.mail"),
                ],
            ),
            group(4, vec![window(30, "Chat", "so.tilo.chat")]),
        ],
    }])
}

/// A desk every place a chord reaches exists on: nine strips, the shown one
/// holding nine windows, so numbering a strip or a window lands somewhere
/// and moving to the strip above or below does too.
fn every_place() -> DesktopTree {
    let groups: Vec<WindowGroup> = (0..9)
        .map(|index| {
            let id = u64::from(index) + 1;
            group(
                id,
                vec![
                    window(100 + u64::from(index) * 2, "Left", "so.tilo.notes"),
                    window(101 + u64::from(index) * 2, "Right", "so.tilo.mail"),
                ],
            )
        })
        .collect();
    let mut strips: Vec<Strip> = (1..9)
        .map(|index| Strip {
            id: StripId(index + 1),
            name: format!("Strip {index}"),
            window_groups: vec![group(
                20 + u64::from(index),
                vec![window(200 + u64::from(index), "Window", "so.tilo.notes")],
            )],
        })
        .collect();
    strips[0].window_groups = groups;
    strips[0].id = StripId(2);
    desk_tree(strips)
}

/// The desk with `tree` on it and the chrome that carries the desktop's
/// chords over it.
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

/// The chrome's own reading of itself, and what it asked its host for.
fn take_asks(frame: &mut FrameHarness) -> Vec<DesktopAsk> {
    frame
        .widget_mut::<DesktopChrome>()
        .expect("the chrome is the frame's root")
        .take_asks()
}

fn hold(frame: &mut FrameHarness, mods: ModifiersState) {
    frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(mods)));
}

fn press(frame: &mut FrameHarness, key: Key) {
    frame.send_host_event(&key_press(key, None));
}

/// The winit key a chord names.
fn key(name: &str) -> Key {
    match name {
        "left" => Key::Named(NamedKey::ArrowLeft),
        "right" => Key::Named(NamedKey::ArrowRight),
        "up" => Key::Named(NamedKey::ArrowUp),
        "down" => Key::Named(NamedKey::ArrowDown),
        "pageup" => Key::Named(NamedKey::PageUp),
        "pagedown" => Key::Named(NamedKey::PageDown),
        "tab" => Key::Named(NamedKey::Tab),
        "enter" => Key::Named(NamedKey::Enter),
        "escape" => Key::Named(NamedKey::Escape),
        "space" => Key::Named(NamedKey::Space),
        "f10" => Key::Named(NamedKey::F10),
        "delete" => Key::Named(NamedKey::Delete),
        other => Key::Character(other.into()),
    }
}

/// The modifiers a chord names, as the host reports them.
fn modifiers(chord: &KeyChord) -> ModifiersState {
    let mut state = ModifiersState::empty();
    for name in &chord.modifiers {
        state |= match name.as_str() {
            "Super" => ModifiersState::SUPER,
            "Ctrl" => ModifiersState::CONTROL,
            "Alt" => ModifiersState::ALT,
            "Shift" => ModifiersState::SHIFT,
            other => panic!("a modifier the test does not know: {other}"),
        };
    }
    state
}

/// Press the chord the person's table binds `action` to, and let the desk
/// carry out what it asked for.
fn press_chord(desk: &mut FrameHarness, frame: &mut FrameHarness, action: ShortcutAction) {
    let chord = ShortcutConfig::default()
        .bindings
        .get(&action)
        .unwrap_or_else(|| panic!("{action:?} is bound"))
        .clone();
    hold(frame, modifiers(&chord));
    press(frame, key(&chord.key));
    hold(frame, ModifiersState::empty());
    for _ in 0..4 {
        desk.relayout();
    }
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

/// What the desk has in front: the active window of the shown strip.
fn in_front(handle: &DesktopHandle) -> Option<u64> {
    handle
        .snapshot("g", vec![])
        .active
        .and_then(|route| route.tab.parse().ok())
}

/// The group in front, and the strip it stands in.
fn in_front_group(handle: &DesktopHandle) -> Option<(u64, u64)> {
    let route = handle.snapshot("g", vec![]).active?;
    Some((route.group.parse().ok()?, route.strip.parse().ok()?))
}

/// The groups of the shown strip, left to right.
fn groups_of_shown_strip(handle: &DesktopHandle) -> Vec<u64> {
    let (_, strip) = in_front_group(handle).expect("a window in front");
    handle.read(|tree| {
        tree.workspaces[0]
            .strips
            .iter()
            .find(|shown| shown.id.0 == strip)
            .map(|shown| shown.window_groups.iter().map(|group| group.id).collect())
            .unwrap_or_default()
    })
}

/// The width of the window in front, in permille of its strip.
fn width_in_front(handle: &DesktopHandle) -> Option<u32> {
    let (group, _) = in_front_group(handle)?;
    handle.read(|tree| {
        tree.workspaces[0]
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .find(|shown| shown.id == group)
            .map(|shown| shown.width.permille())
    })
}

/// Every app of the desk's windows, in reading order.
fn apps(handle: &DesktopHandle) -> Vec<String> {
    handle.read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| match &tab.tile {
                TileContent::LocalApp { app_id, .. } => Some(app_id.clone()),
                _ => None,
            })
            .collect()
    })
}

/// Every shortcut of the table is carried out by one side or handed to the
/// other: the desk answers the shortcuts of its own windows and asks its host
/// for nothing, and the session's are passed on whole — none falls between
/// them.
#[test]
fn the_table_hands_every_shortcut_to_the_desk_or_the_session() {
    let tree = every_place();
    for action in ShortcutAction::ALL {
        let (_desk, mut frame, _handle) = desk_and_chrome(&tree);
        let chrome = frame
            .widget_mut::<DesktopChrome>()
            .expect("the chrome is the frame's root");
        let taken = chrome.shortcut(action);
        let asks = chrome.take_asks();
        match (action, action.home()) {
            // Reporting a problem is the desk's picker, which asks its host
            // where to send it: the one session surface a chord reaches
            // through the desk's own layer.
            (ShortcutAction::ReportProblem, _) => {
                assert!(taken, "{action:?} keeps its own chord");
                assert_eq!(asks, [DesktopAsk::ReportProblem], "{action:?}");
            }
            (_, ShortcutHome::Desk) => {
                assert!(taken, "{action:?} is the desk's");
                assert!(asks.is_empty(), "{action:?} is not the session's: {asks:?}");
            }
            (_, ShortcutHome::Session) => {
                assert!(!taken, "{action:?} is the session's");
                assert_eq!(
                    asks,
                    [DesktopAsk::Shortcut(action)],
                    "{action:?} reaches the session once"
                );
            }
        }
    }
}

/// The chords of the table move what they name: the tab in front within its
/// window, a tab into a window of its own, that window along its strip and
/// into the strip below, and the window's share of the strip.
#[test]
fn the_chords_move_tabs_windows_and_strips() {
    let (mut desk, mut frame, handle) = desk_and_chrome(&two_windows());
    assert_eq!(in_front(&handle), Some(20));

    press_chord(&mut desk, &mut frame, ShortcutAction::NextTab);
    assert_eq!(in_front(&handle), Some(21), "the next tab of its window");
    press_chord(&mut desk, &mut frame, ShortcutAction::NextTab);
    assert_eq!(
        in_front(&handle),
        Some(20),
        "and round the ring of its window's tabs"
    );
    press_chord(&mut desk, &mut frame, ShortcutAction::PreviousTab);
    assert_eq!(in_front(&handle), Some(21), "back the other way");

    // A tab into a window of its own, right after the one it left: its own
    // window holds the tab in front, and the window it left keeps the other.
    press_chord(&mut desk, &mut frame, ShortcutAction::DetachTab);
    assert_eq!(windows_of(&handle, "3"), ["20"]);
    let split = groups_of_shown_strip(&handle);
    assert_eq!(split.first(), Some(&3), "the window it left stands first");
    assert_eq!(
        windows_of(&handle, split[1].to_string().as_str()),
        ["21"],
        "and the tab is in the window after it: {split:?}"
    );

    // Attaching it to the window before it puts the tab back.
    press_chord(&mut desk, &mut frame, ShortcutAction::AttachTabLeft);
    assert_eq!(windows_of(&handle, "3"), ["20", "21"]);

    // A window moves one place along its strip.
    let before = groups_of_shown_strip(&handle);
    press_chord(&mut desk, &mut frame, ShortcutAction::MoveGroupRight);
    let after = groups_of_shown_strip(&handle);
    assert_ne!(before, after, "the window moved along its strip");
    assert_eq!(after, [4, 3], "one place right: {after:?}");

    // Nowhere to go keeps the window where it is, and the desk says so
    // rather than passing the chord on.
    let shown = in_front_group(&handle);
    press_chord(&mut desk, &mut frame, ShortcutAction::MoveGroupToNextStrip);
    assert_eq!(
        in_front_group(&handle),
        shown,
        "the one strip was already the last"
    );

    // A share of the strip, narrower and wider again.
    let normal = width_in_front(&handle).expect("a window in front");
    press_chord(&mut desk, &mut frame, ShortcutAction::ResizeNarrower);
    let narrower = width_in_front(&handle).expect("a window in front");
    assert!(narrower < normal, "{narrower} is narrower than {normal}");
    press_chord(&mut desk, &mut frame, ShortcutAction::ResizeWider);
    assert_eq!(width_in_front(&handle), Some(normal), "and wider again");
}

/// The window in front takes the strip below it, and the person goes with it.
#[test]
fn a_window_moves_to_the_strip_below() {
    let tree = desk_tree(vec![
        Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: vec![group(3, vec![window(20, "Notes", "so.tilo.notes")])],
        },
        Strip {
            id: StripId(5),
            name: "Below".into(),
            window_groups: vec![group(6, vec![window(40, "Chat", "so.tilo.chat")])],
        },
    ]);
    let (mut desk, mut frame, handle) = desk_and_chrome(&tree);

    press_chord(&mut desk, &mut frame, ShortcutAction::MoveGroupToNextStrip);

    let strip = in_front_group(&handle).map(|(_, strip)| strip);
    assert_eq!(strip, Some(5), "the window is in the strip below");
    assert!(
        windows_of(&handle, "3").is_empty(),
        "its old strip is empty"
    );
}

/// The windows a chord opens are the app's, beside the ones already shown.
#[test]
fn the_chords_open_the_session_s_own_apps() {
    let (mut desk, mut frame, handle) = desk_and_chrome(&two_windows());
    press_chord(&mut desk, &mut frame, ShortcutAction::OpenFiles);
    assert!(
        apps(&handle).contains(&"so.tilo.file-explorer".to_owned()),
        "the files app opened: {:?}",
        apps(&handle)
    );
    press_chord(&mut desk, &mut frame, ShortcutAction::OpenTerminal);
    assert!(
        apps(&handle).contains(&"so.tilo.terminal".to_owned()),
        "the terminal opened: {:?}",
        apps(&handle)
    );
}

/// A row a chord reaches shows the chord the person's table binds now, and a
/// rebinding reads there too: one table, no copy of a default.
#[test]
fn a_row_a_chord_reaches_shows_the_chord_the_table_binds() {
    let table = ShortcutConfig::default();
    let shown = |table: &ShortcutConfig| -> Vec<(String, String)> {
        chrome_resource(table)
            .actions
            .iter()
            .filter_map(|action| {
                action
                    .state
                    .shortcut
                    .as_ref()
                    .map(|chord| (action.id.clone(), chord.clone()))
            })
            .collect()
    };

    let mut expected: Vec<(String, String)> = [
        ("reopen-closed", ShortcutAction::ReopenClosedTab),
        ("maximize", ShortcutAction::ToggleMaximize),
        ("fullscreen", ShortcutAction::ToggleFullscreen),
        ("split-right", ShortcutAction::DetachTab),
    ]
    .into_iter()
    .map(|(verb, action)| (verb.to_owned(), table.bindings[&action].display()))
    .collect();
    expected.sort();
    let mut reads = shown(&table);
    reads.sort();
    assert_eq!(
        reads, expected,
        "every row a chord reaches shows its chord, as the table spells it"
    );

    // The same row, after the person binds the action elsewhere.
    let mut rebound = ShortcutConfig::default();
    rebound
        .set(
            ShortcutAction::ReopenClosedTab,
            KeyChord::parse("Super+Ctrl+R").unwrap(),
        )
        .expect("a free chord");
    let reads = shown(&rebound);
    assert!(
        reads.contains(&("reopen-closed".to_owned(), "Super+Ctrl+R".to_owned())),
        "the row reads the new chord: {reads:?}"
    );
    assert!(
        !reads.contains(&(
            "reopen-closed".to_owned(),
            table.bindings[&ShortcutAction::ReopenClosedTab].display()
        )),
        "and nothing of the old one: {reads:?}"
    );
}
