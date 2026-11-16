//! Every chord of the person's table on the real chrome: each one is
//! answered where it belongs — the shell moves its windows, tabs, groups,
//! strips and displays, the session is asked for the rest, and none is
//! dropped — and the rows a chord reaches show the chord the table binds now.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::group;
use crate::support::hold;
use crate::support::in_front;
use crate::support::press;
use crate::support::shell_and_chrome;
use crate::support::window;
use crate::support::windows_of;
use tilo_desktop::desktop_host::{DesktopAsk, DesktopChrome};
use tilo_desktop::verbs::chrome_resource;
use tilo_desktop_model::{Layout, Strip, StripId, WindowGroup, Workspace, WorkspaceId};
use tilo_desktop_surface::DesktopHandle;
use ui::headless::FrameHarness;
use ui::shortcut_table::{FixedAction, KeyChord, ShortcutAction, ShortcutConfig, ShortcutHome};
use winit::event::{DeviceId, ElementState, KeyEvent, RawKeyEvent, WindowEvent};
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

fn layout_tree(strips: Vec<Strip>) -> Layout {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
    workspace.strips = strips;
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    let first = tree.workspaces[0].strips[0].id;
    tree.active_strip.insert(WorkspaceId::from_u128(1), first);
    if let Some(group) = tree.workspaces[0].strips[0].window_groups.first() {
        tree.active_group.insert(first, group.id);
    }
    tree
}

/// One strip with two windows of two tabs each.
fn two_windows() -> Layout {
    layout_tree(vec![Strip {
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

/// A shell every place a chord reaches exists on: nine strips, the shown one holding nine windows,
/// so numbering a strip or a window lands somewhere and moving to the strip above or below does
/// too.
fn every_place() -> Layout {
    let groups: Vec<WindowGroup> = (0..9_u64)
        .map(|index| {
            let id = index + 1;
            group(
                id,
                vec![
                    window(100 + index * 2, "Left", "so.tilo.notes"),
                    window(101 + index * 2, "Right", "so.tilo.mail"),
                ],
            )
        })
        .collect();
    // Nine strips: the shown one holds the nine windows, and the eight below it one window each.
    let mut strips: Vec<Strip> = (1..9_u64)
        .map(|index| Strip {
            id: StripId(10 + index),
            name: format!("Strip {index}"),
            window_groups: vec![group(
                20 + index,
                vec![window(200 + index, "Window", "so.tilo.notes")],
            )],
        })
        .collect();
    strips.insert(
        0,
        Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: groups,
        },
    );
    layout_tree(strips)
}

/// A key press as a browser page reports it: the letter is the one the held Shift and Alt type,
/// while the physical place it stands on is what the page's stand-in for Super reads.
fn press_here(frame: &mut FrameHarness, code: KeyCode, logical: Key, mods: ModifiersState) {
    hold(frame, mods);
    frame.send_host_event(&WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event: KeyEvent::synthetic(
            RawKeyEvent {
                physical_key: PhysicalKey::Code(code),
                state: ElementState::Pressed,
            },
            logical.clone(),
            None,
            KeyLocation::Standard,
            false,
            None,
            logical,
        ),
        is_synthetic: true,
    });
    hold(frame, ModifiersState::empty());
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

/// Press the chord the person's table binds `action` to, and let the shell carry out what it asked
/// for.
fn press_chord(shell: &mut FrameHarness, frame: &mut FrameHarness, action: ShortcutAction) {
    let chord = ShortcutConfig::default()
        .bindings
        .get(&action)
        .unwrap_or_else(|| panic!("{action:?} is bound"))
        .clone();
    hold(frame, modifiers(&chord));
    press(frame, key(&chord.key));
    hold(frame, ModifiersState::empty());
    for _ in 0..4 {
        shell.relayout();
    }
}

/// The group in front, and the strip it stands in.
fn in_front_group(handle: &DesktopHandle) -> Option<(u64, u64)> {
    let route = handle.snapshot("g", vec![]).active?;
    Some((route.group.parse().ok()?, route.strip.parse().ok()?))
}

/// The groups of the shown strip, left to right.
fn groups_of_shown_strip(handle: &DesktopHandle) -> Vec<u64> {
    let Some((_, strip)) = in_front_group(handle) else {
        return Vec::new();
    };
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

/// Every shortcut of the table is carried out by one side or handed to the other: the shell answers
/// the shortcuts of its own windows and asks its host for nothing, and the session's are passed on
/// whole — none falls between them.
#[test]
fn the_table_hands_every_shortcut_to_the_shell_or_the_session() {
    let tree = every_place();
    for action in ShortcutAction::ALL {
        let (_shell, mut frame, _handle) = shell_and_chrome(&tree);
        let chrome = frame
            .widget_mut::<DesktopChrome>()
            .expect("the chrome is the frame's root");
        let taken = chrome.shortcut(action);
        let asks = chrome.take_asks();
        match (action, action.home()) {
            // Reporting a problem is the shell's picker, which asks its host where to send it: the
            // one session surface a chord reaches through the shell's own layer.
            (ShortcutAction::ReportProblem, _) => {
                assert!(taken, "{action:?} keeps its own chord");
                assert_eq!(asks, [DesktopAsk::ReportProblem], "{action:?}");
            }
            (_, ShortcutHome::Shell) => {
                assert!(taken, "{action:?} is the shell's");
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

/// The chords of the table move what they name: the tab in front within its window, a tab into a
/// window of its own, that window along its strip and into the strip below, and the window's share
/// of the strip.
#[test]
fn the_chords_move_tabs_windows_and_strips() {
    let (mut shell, mut frame, handle) = shell_and_chrome(&two_windows());
    assert_eq!(in_front(&handle), Some(20));

    press_chord(&mut shell, &mut frame, ShortcutAction::NextTab);
    assert_eq!(in_front(&handle), Some(21), "the next tab of its window");
    press_chord(&mut shell, &mut frame, ShortcutAction::NextTab);
    assert_eq!(
        in_front(&handle),
        Some(20),
        "and round the ring of its window's tabs"
    );
    press_chord(&mut shell, &mut frame, ShortcutAction::PreviousTab);
    assert_eq!(in_front(&handle), Some(21), "back the other way");

    // A tab into a window of its own, right after the one it left: its own window holds the tab in
    // front, and the window it left keeps the other.
    press_chord(&mut shell, &mut frame, ShortcutAction::DetachTab);
    assert_eq!(windows_of(&handle, "3"), ["20"]);
    let split = groups_of_shown_strip(&handle);
    assert_eq!(split.first(), Some(&3), "the window it left stands first");
    assert_eq!(
        windows_of(&handle, split[1].to_string().as_str()),
        ["21"],
        "and the tab is in the window after it: {split:?}"
    );

    // Attaching it to the window before it puts the tab back.
    press_chord(&mut shell, &mut frame, ShortcutAction::AttachTabLeft);
    assert_eq!(windows_of(&handle, "3"), ["20", "21"]);

    // A window moves one place along its strip.
    let before = groups_of_shown_strip(&handle);
    press_chord(&mut shell, &mut frame, ShortcutAction::MoveGroupRight);
    let after = groups_of_shown_strip(&handle);
    assert_ne!(before, after, "the window moved along its strip");
    assert_eq!(after, [4, 3], "one place right: {after:?}");
    press_chord(&mut shell, &mut frame, ShortcutAction::MoveGroupLeft);
    assert_eq!(
        groups_of_shown_strip(&handle),
        before,
        "and back the other way"
    );

    // Nowhere to go keeps the window where it is, and the shell says so rather than passing the
    // chord on.
    let shown = in_front_group(&handle);
    press_chord(&mut shell, &mut frame, ShortcutAction::MoveGroupToNextStrip);
    assert_eq!(
        in_front_group(&handle),
        shown,
        "the one strip was already the last"
    );

    // A share of the strip, narrower and wider again.
    let normal = width_in_front(&handle).expect("a window in front");
    press_chord(&mut shell, &mut frame, ShortcutAction::ResizeNarrower);
    let narrower = width_in_front(&handle).expect("a window in front");
    assert!(narrower < normal, "{narrower} is narrower than {normal}");
    press_chord(&mut shell, &mut frame, ShortcutAction::ResizeWider);
    assert_eq!(width_in_front(&handle), Some(normal), "and wider again");
}

/// The fixed chords find their commands: the window in front closes, another window of its app
/// opens beside it, and what the session owns is asked for whole, so the same action answers a menu
/// row, the palette and an agent.
#[test]
fn the_fixed_chords_close_duplicate_and_ask_the_session() {
    let (mut shell, mut frame, handle) = shell_and_chrome(&two_windows());
    assert_eq!(in_front(&handle), Some(20));

    let chrome = frame.widget_mut::<DesktopChrome>().expect("the chrome");
    assert!(chrome.fixed_shortcut(FixedAction::CloseTab));
    for _ in 0..4 {
        shell.relayout();
    }
    assert_eq!(in_front(&handle), Some(21), "the window in front closed");

    // Another window of the app in front is the shell's to open; it asks the session for nothing.
    let chrome = frame.widget_mut::<DesktopChrome>().expect("the chrome");
    assert!(chrome.fixed_shortcut(FixedAction::DuplicateTab));
    assert!(chrome.take_asks().is_empty(), "the shell opens the window");

    // A new window is the app picker: search over the apps opens on the shell.
    assert!(!handle.search_open());
    let chrome = frame.widget_mut::<DesktopChrome>().expect("the chrome");
    assert!(chrome.fixed_shortcut(FixedAction::NewWindow));
    assert!(chrome.take_asks().is_empty(), "the shell opens the picker");
    shell.relayout();
    assert!(handle.search_open(), "Super+N opens the app picker");

    // The capture studio, the guide and the remote control are the session's: the shell asks for
    // them rather than doing nothing.
    let chrome = frame.widget_mut::<DesktopChrome>().expect("the chrome");
    for action in [
        FixedAction::CaptureScreenshot,
        FixedAction::PictureInPicture,
        FixedAction::ShortcutGuide,
        FixedAction::StopRemoteInput,
    ] {
        assert!(
            !chrome.fixed_shortcut(action),
            "{action:?} is the session's"
        );
        assert_eq!(
            chrome.take_asks(),
            [DesktopAsk::FixedShortcut(action)],
            "{action:?} reaches the session once"
        );
    }
}

/// On a shell with no window, Super+T has no app to open another window of: it opens the app
/// picker, as Super+N does.
#[test]
fn a_new_tab_on_an_empty_shell_opens_the_app_picker() {
    let (mut shell, mut frame, handle) = shell_and_chrome(&layout_tree(vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: Vec::new(),
    }]));
    let chrome = frame.widget_mut::<DesktopChrome>().expect("the chrome");
    assert!(chrome.fixed_shortcut(FixedAction::DuplicateTab));
    shell.relayout();
    assert!(handle.search_open(), "the app picker is open");
}

/// A browser page runs the fixed chords the shell shares with the native
/// session, through the page's stand-in for the Super a browser may keep,
/// and asks for the ones over the system rather than dropping them.
#[test]
fn a_page_closes_a_window_and_asks_for_what_it_has_no_surface_for() {
    let (mut shell, mut frame, handle) = shell_and_chrome(&two_windows());
    assert_eq!(in_front(&handle), Some(20));

    // Alt+Shift is Super where the browser keeps it: the window in front closes, as the native
    // desktop's Super+W does.
    press_here(
        &mut frame,
        KeyCode::KeyW,
        Key::Character("W".into()),
        ModifiersState::ALT | ModifiersState::SHIFT,
    );
    for _ in 0..4 {
        shell.relayout();
    }
    assert_eq!(in_front(&handle), Some(21), "the window in front closed");

    // Super+Shift+S is a capture, taken by the session over the whole screen: the page has none, so
    // its chrome hands the chord on.
    press_here(
        &mut frame,
        KeyCode::KeyS,
        Key::Character("S".into()),
        ModifiersState::SUPER | ModifiersState::SHIFT,
    );
    let chrome = frame.widget_mut::<DesktopChrome>().expect("the chrome");
    assert_eq!(
        chrome.take_asks(),
        [DesktopAsk::FixedShortcut(FixedAction::CaptureScreenshot)],
        "the page is told what it cannot do here"
    );
}

/// The window in front takes the strip below it, and the person goes with it.
#[test]
fn a_window_moves_to_the_strip_below() {
    let tree = layout_tree(vec![
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
    let (mut shell, mut frame, handle) = shell_and_chrome(&tree);

    press_chord(&mut shell, &mut frame, ShortcutAction::MoveGroupToNextStrip);

    let strip = in_front_group(&handle).map(|(_, strip)| strip);
    assert_eq!(strip, Some(5), "the window is in the strip below");
    let strips = handle.read(|tree| {
        tree.workspaces[0]
            .strips
            .iter()
            .map(|strip| {
                (
                    strip.id.0,
                    strip
                        .window_groups
                        .iter()
                        .map(|group| group.id)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        strips,
        [(2, vec![]), (5, vec![6, 3])],
        "the window left its strip for the one below"
    );
}

/// A row a chord reaches shows the chord the person's table binds now, and a rebinding reads there
/// too: one table, no copy of a default.
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
    let rebound_chord = KeyChord::parse("Super+Ctrl+R").expect("a chord").display();
    assert!(
        reads.contains(&("reopen-closed".to_owned(), rebound_chord)),
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

/// The display chords go by where the displays stand, not by the order the outputs were listed
/// in: with the first output on the right, the arrow towards the other display moves the window
/// there, and the arrow away from it moves nothing.
#[test]
fn the_display_chords_follow_where_the_displays_stand() {
    let (mut shell, mut frame, handle) = shell_and_chrome(&two_windows());
    let display = |id: u32, monitor: &str, x: f64| tilo_desktop_model::Display {
        id,
        monitor: monitor.into(),
        x,
        y: 0.0,
        width: 640.0,
        height: 800.0,
        primary: false,
    };
    // The first output, where the windows start, stands right of the second.
    assert!(handle.set_displays(&[display(0, "right", 640.0), display(1, "left", 0.0)]));
    for _ in 0..4 {
        shell.relayout();
    }
    let (group, _) = in_front_group(&handle).unwrap();
    assert_eq!(
        handle.display_of(group),
        Some(0),
        "it starts on the right display"
    );

    press_chord(
        &mut shell,
        &mut frame,
        ShortcutAction::MoveGroupToNextDisplay,
    );
    assert_eq!(
        handle.display_of(group),
        Some(0),
        "nothing stands right of the right display"
    );
    press_chord(
        &mut shell,
        &mut frame,
        ShortcutAction::MoveGroupToPreviousDisplay,
    );
    assert_eq!(
        handle.display_of(group),
        Some(1),
        "the left arrow takes it to the display on the left"
    );
    press_chord(
        &mut shell,
        &mut frame,
        ShortcutAction::MoveGroupToNextDisplay,
    );
    assert_eq!(
        handle.display_of(group),
        Some(0),
        "and the right arrow back"
    );
}
