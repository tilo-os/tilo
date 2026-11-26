//! The desktop's shortcuts under the tilolang shell: each runs on the desktop the session owns
//! (`desktop:chrome`'s `shortcut` and `fixed-shortcut`, the actions a chord, the palette and an
//! agent call), and the shell shows what changed. No Rust chrome draws or decides anything here:
//! the shell is the tilolang one, and every check reads the desktop the session holds.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{GroupId, Layout, StripId, TabId};
use tilo_session::{Options, Session};
use ui::shortcut_table::{FixedAction, ShortcutAction};

use crate::common::{HEIGHT, PERSON, TITLES, WIDTH, notebook as notes, until};

pub(crate) const CHROME: &str = "desktop:chrome";
/// A frame of the session.
const REFRESH: std::time::Duration = std::time::Duration::from_millis(16);

/// A session with the tilolang shell as the whole screen and the notes a test places.
pub(crate) fn session() -> (Session, Vec<String>) {
    let (notebook, notes) = notes(&TITLES);
    let session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            person: PERSON.to_owned(),
            collab: false,
            services: vec![Box::new(notebook)],
            ..Options::default()
        },
    );
    (session, notes)
}

/// Run the shortcut `action` as its chord does.
fn shortcut(session: &mut Session, action: ShortcutAction) {
    let id = serde_json::to_value(action).expect("a shortcut has an id");
    let arguments: Map<String, Value> = json!({ "action": id }).as_object().unwrap().clone();
    session
        .invoke(CHROME, "shortcut", &arguments)
        .unwrap_or_else(|error| panic!("{action:?}: {error:?}"));
    settle(session);
}

/// Run the fixed shortcut `action` as its chord does.
fn fixed(session: &mut Session, action: FixedAction) {
    let arguments: Map<String, Value> = json!({ "action": action.id() })
        .as_object()
        .unwrap()
        .clone();
    session
        .invoke(CHROME, "fixed-shortcut", &arguments)
        .unwrap_or_else(|error| panic!("{action:?}: {error:?}"));
    settle(session);
}

/// Let the session apply what it was asked.
pub(crate) fn settle(session: &mut Session) {
    for _ in 0..3 {
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Read the desktop the session holds.
pub(crate) fn tree<R>(session: &Session, read: impl FnOnce(&Layout) -> R) -> R {
    session.handle().read(read)
}

/// The windows of the shown strip, in order: each window's id and the tab it shows.
pub(crate) fn windows(session: &Session) -> Vec<(GroupId, TabId)> {
    tree(session, |tree| {
        let workspace = tree.active_workspace().expect("a workspace in front");
        let strip = tree.active_strip.get(&workspace.id()).copied();
        workspace
            .strips
            .iter()
            .find(|s| Some(s.id) == strip)
            .map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .map(|group| (group.id, group.active_tab))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The window the keyboard is on in the shown strip.
pub(crate) fn focused(session: &Session) -> Option<GroupId> {
    tree(session, |tree| {
        let workspace = tree.active_workspace()?;
        let strip = tree.active_strip.get(&workspace.id()).copied()?;
        tree.active_group_for(strip)
    })
}

/// The strips of the workspace in front, and the one shown.
pub(crate) fn strips(session: &Session) -> (Vec<StripId>, Option<StripId>) {
    tree(session, |tree| {
        let workspace = tree.active_workspace().expect("a workspace in front");
        (
            workspace.strips.iter().map(|strip| strip.id).collect(),
            tree.active_strip.get(&workspace.id()).copied(),
        )
    })
}

/// The strip the window `group` stands in.
pub(crate) fn strip_holding(session: &Session, group: GroupId) -> Option<StripId> {
    tree(session, |tree| {
        tree.active_workspace()?
            .strips
            .iter()
            .find(|strip| strip.window_groups.iter().any(|g| g.id == group))
            .map(|strip| strip.id)
    })
}

/// The window `group`, read with `read`.
pub(crate) fn group<R>(
    session: &Session,
    group: GroupId,
    read: impl FnOnce(&tilo_desktop_model::WindowGroup) -> R,
) -> R {
    tree(session, |tree| {
        let found = tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .find(|g| g.id == group)
            .unwrap_or_else(|| panic!("the window {group} stands somewhere"));
        read(found)
    })
}

/// The workspace in front.
pub(crate) fn workspace(session: &Session) -> String {
    tree(session, |tree| tree.active.to_string())
}

/// Place each of the first `count` notes in a window of its own on the shown strip; the windows in
/// order, with the keyboard on the last.
pub(crate) fn open(session: &mut Session, notes: &[String], count: usize) -> Vec<(GroupId, TabId)> {
    until(session, "the shell", |session| {
        crate::common::on_screen(session, "Showing Strips").is_some()
    });
    for note in notes.iter().take(count) {
        let before = windows(session).len();
        let arguments: Map<String, Value> = json!({
            "resource": note,
            "x": 80,
            "y": 60,
            "width": 480,
            "height": 360,
        })
        .as_object()
        .unwrap()
        .clone();
        session
            .invoke("shell:layout", "place", &arguments)
            .unwrap_or_else(|error| panic!("place {note}: {error:?}"));
        until(session, "the window is placed", |session| {
            windows(session).len() > before
        });
    }
    windows(session)
}

/// A strip made at the end of the workspace, as the strip switcher's New strip makes it.
pub(crate) fn new_strip(session: &mut Session) -> StripId {
    let before = strips(session).0;
    session
        .invoke(
            CHROME,
            "new-strip",
            json!({ "name": "" }).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("a strip is made: {error:?}"));
    until(session, "the strip made", |session| {
        strips(session).0.len() > before.len()
    });
    strips(session)
        .0
        .into_iter()
        .find(|strip| !before.contains(strip))
        .expect("the new strip")
}

/// Focus previous/next window: the keyboard steps along the shown strip and stops at its ends.
#[test]
fn focus_previous_and_next_window_step_along_the_strip() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 3);
    assert_eq!(
        focused(&session),
        Some(shown[2].0),
        "the window placed last has the keyboard"
    );

    shortcut(&mut session, ShortcutAction::FocusPreviousWindow);
    assert_eq!(focused(&session), Some(shown[1].0));
    shortcut(&mut session, ShortcutAction::FocusPreviousWindow);
    assert_eq!(focused(&session), Some(shown[0].0));
    shortcut(&mut session, ShortcutAction::FocusPreviousWindow);
    assert_eq!(
        focused(&session),
        Some(shown[0].0),
        "the first window is the end"
    );
    shortcut(&mut session, ShortcutAction::FocusNextWindow);
    assert_eq!(focused(&session), Some(shown[1].0));
}

/// Focus window 1–9: the keyboard goes to the strip's window by its place; past the last window
/// it stays.
#[test]
fn focus_window_by_number_puts_the_keyboard_on_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 3);
    shortcut(&mut session, ShortcutAction::FocusGroup1);
    assert_eq!(focused(&session), Some(shown[0].0));
    shortcut(&mut session, ShortcutAction::FocusGroup3);
    assert_eq!(focused(&session), Some(shown[2].0));
    shortcut(&mut session, ShortcutAction::FocusGroup2);
    assert_eq!(focused(&session), Some(shown[1].0));
    shortcut(&mut session, ShortcutAction::FocusGroup9);
    assert_eq!(
        focused(&session),
        Some(shown[1].0),
        "no ninth window: the keyboard stays"
    );
}

/// Attach tab left/right, previous/next tab and detach tab: a window's tab joins its neighbour,
/// the tabs cycle and wrap, and the tab leaves for a window of its own again.
#[test]
fn tabs_attach_cycle_and_detach_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 3);
    let (first, _) = shown[0];
    let (_, second_tab) = shown[1];

    shortcut(&mut session, ShortcutAction::FocusGroup2);
    shortcut(&mut session, ShortcutAction::AttachTabLeft);
    until(&mut session, "the tab joins the first window", |session| {
        group(session, first, |g| {
            g.tabs.iter().any(|tab| tab.id == second_tab)
        })
    });
    assert_eq!(windows(&session).len(), 2, "its own window went with it");
    let tabs = group(&session, first, |g| {
        g.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>()
    });
    assert_eq!(tabs.len(), 2);

    let showing = |session: &Session| group(session, first, |g| g.active_tab);
    shortcut(&mut session, ShortcutAction::FocusGroup1);
    let before = showing(&session);
    shortcut(&mut session, ShortcutAction::NextTab);
    let after = showing(&session);
    assert_ne!(after, before, "the next tab is shown");
    shortcut(&mut session, ShortcutAction::NextTab);
    assert_eq!(showing(&session), before, "the tabs wrap");
    shortcut(&mut session, ShortcutAction::PreviousTab);
    assert_eq!(showing(&session), after, "and back");

    shortcut(&mut session, ShortcutAction::DetachTab);
    until(&mut session, "the tab has a window of its own", |session| {
        windows(session).len() == 3
    });
    assert_eq!(group(&session, first, |g| g.tabs.len()), 1);

    // The window to the right takes the tab in front as its last.
    shortcut(&mut session, ShortcutAction::FocusGroup1);
    let (_, first_tab) = windows(&session)[0];
    let right = windows(&session)[1].0;
    shortcut(&mut session, ShortcutAction::AttachTabRight);
    until(
        &mut session,
        "the tab joins the window to its right",
        |session| {
            group(session, right, |g| {
                g.tabs.last().map(|tab| tab.id) == Some(first_tab)
            })
        },
    );
}

/// Previous/next strip and strip 1–9: the shown strip steps, and stepping past an end stays.
#[test]
fn strips_step_and_go_by_number() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let first = strips(&session).1.expect("a strip shown");
    let second = new_strip(&mut session);
    let third = new_strip(&mut session);

    shortcut(&mut session, ShortcutAction::FocusStrip1);
    assert_eq!(strips(&session).1, Some(first));
    shortcut(&mut session, ShortcutAction::PreviousStrip);
    assert_eq!(
        strips(&session).1,
        Some(first),
        "the first strip is the end"
    );
    shortcut(&mut session, ShortcutAction::NextStrip);
    assert_eq!(strips(&session).1, Some(second));
    shortcut(&mut session, ShortcutAction::FocusStrip3);
    assert_eq!(strips(&session).1, Some(third));
    shortcut(&mut session, ShortcutAction::NextStrip);
    assert_eq!(strips(&session).1, Some(third), "the last strip is the end");
    shortcut(&mut session, ShortcutAction::FocusStrip9);
    assert_eq!(
        strips(&session).1,
        Some(third),
        "no ninth strip: the shown one stays"
    );
    shortcut(&mut session, ShortcutAction::PreviousStrip);
    assert_eq!(strips(&session).1, Some(second));
}

/// Previous/next workspace: the workspace in front steps, and stays at either end.
#[test]
fn workspaces_step_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let first = workspace(&session);
    session
        .invoke(CHROME, "new-workspace", &Map::new())
        .unwrap_or_else(|error| panic!("a workspace is made: {error:?}"));
    until(&mut session, "the workspace made", |session| {
        tree(session, |tree| tree.workspaces.len()) == 2
    });
    let made = workspace(&session);
    assert_ne!(made, first);

    shortcut(&mut session, ShortcutAction::NextWorkspace);
    assert_eq!(workspace(&session), made, "the last workspace is the end");
    shortcut(&mut session, ShortcutAction::PreviousWorkspace);
    until(&mut session, "the first workspace", |session| {
        workspace(session) == first
    });
    shortcut(&mut session, ShortcutAction::PreviousWorkspace);
    assert_eq!(workspace(&session), first, "the first workspace is the end");
    shortcut(&mut session, ShortcutAction::NextWorkspace);
    until(&mut session, "the workspace made again", |session| {
        workspace(session) == made
    });
}

/// Maximize, restore and toggle maximize: the window in front fills its strip and gives the room
/// back, as its menu's rows do.
#[test]
fn maximize_restore_and_toggle_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 2);
    let front = shown[1].0;
    let maximized = |session: &Session| group(session, front, |g| g.maximized);

    shortcut(&mut session, ShortcutAction::MaximizeWindow);
    until(&mut session, "maximized", |session| maximized(session));
    shortcut(&mut session, ShortcutAction::RestoreWindow);
    until(&mut session, "restored", |session| !maximized(session));
    shortcut(&mut session, ShortcutAction::ToggleMaximize);
    until(&mut session, "toggled to maximized", |session| {
        maximized(session)
    });
    shortcut(&mut session, ShortcutAction::ToggleMaximize);
    until(&mut session, "toggled back", |session| !maximized(session));
}

/// Toggle fullscreen: the window in front covers the display, and gives it back.
#[test]
fn fullscreen_toggles_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 1);
    let front = shown[0].0;
    let fullscreen = |session: &Session| tree(session, Layout::fullscreen_group);

    shortcut(&mut session, ShortcutAction::ToggleFullscreen);
    until(&mut session, "fullscreen", |session| {
        fullscreen(session) == Some(front)
    });
    shortcut(&mut session, ShortcutAction::ToggleFullscreen);
    until(&mut session, "fullscreen ends", |session| {
        fullscreen(session).is_none()
    });
}

/// Toggle float: the window in front floats over its strip, and snaps back into it.
#[test]
fn float_toggles_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 2);
    let front = shown[1].0;
    let floating = |session: &Session| {
        tree(session, |tree| {
            tilo_session_core::desktop::state::window_menu::floating(tree, front)
        })
    };

    shortcut(&mut session, ShortcutAction::ToggleFloat);
    until(&mut session, "floating", |session| floating(session));
    shortcut(&mut session, ShortcutAction::ToggleFloat);
    until(&mut session, "snapped back", |session| !floating(session));
}

/// Move window left/right: the window in front changes place along its strip, keeping the
/// keyboard; at an end it stays.
#[test]
fn windows_move_along_the_strip_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 3);
    let moved = shown[2].0;
    let at = |session: &Session| windows(session).iter().position(|(g, _)| *g == moved);

    shortcut(&mut session, ShortcutAction::MoveGroupLeft);
    until(&mut session, "one place left", |session| {
        at(session) == Some(1)
    });
    shortcut(&mut session, ShortcutAction::MoveGroupLeft);
    until(&mut session, "first", |session| at(session) == Some(0));
    shortcut(&mut session, ShortcutAction::MoveGroupLeft);
    assert_eq!(at(&session), Some(0), "the first place is the end");
    shortcut(&mut session, ShortcutAction::MoveGroupRight);
    until(&mut session, "one place right", |session| {
        at(session) == Some(1)
    });
    assert_eq!(focused(&session), Some(moved), "the keyboard went with it");
}

/// Move window to the previous/next strip: the window in front lands in the strip beside, past the
/// last strip in a new one, and nowhere when it is alone in the last strip.
#[test]
fn windows_move_between_strips_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 2);
    let moved = shown[1].0;
    let first = strips(&session).1.expect("a strip shown");

    shortcut(&mut session, ShortcutAction::MoveGroupToNextStrip);
    until(&mut session, "a new strip holds it", |session| {
        strip_holding(session, moved).is_some_and(|strip| strip != first)
    });
    assert_eq!(
        strips(&session).0.len(),
        2,
        "past the last strip is a new one"
    );
    let second = strip_holding(&session, moved).expect("the window stands in a strip");

    // Alone in the last strip it has nowhere to go: no other strip is made.
    shortcut(&mut session, ShortcutAction::FocusStrip2);
    shortcut(&mut session, ShortcutAction::MoveGroupToNextStrip);
    settle(&mut session);
    assert_eq!(strip_holding(&session, moved), Some(second));
    assert_eq!(strips(&session).0.len(), 2, "no empty strip is left behind");

    shortcut(&mut session, ShortcutAction::MoveGroupToPreviousStrip);
    until(&mut session, "back in the first strip", |session| {
        strip_holding(session, moved) == Some(first)
    });
    assert!(strips(&session).0.contains(&second));
    assert_eq!(
        windows(&session).last().map(|(g, _)| *g),
        Some(moved),
        "it lands last there"
    );
}

/// Narrower/wider: the window in front steps through the strip's widths.
#[test]
fn windows_resize_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 2);
    let front = shown[1].0;
    let width = |session: &Session| group(session, front, |g| g.width.permille());

    let before = width(&session);
    shortcut(&mut session, ShortcutAction::ResizeWider);
    until(&mut session, "wider", |session| width(session) > before);
    let wider = width(&session);
    shortcut(&mut session, ShortcutAction::ResizeNarrower);
    until(&mut session, "narrower", |session| width(session) < wider);
}

/// Close tab and reopen closed tab: the window in front goes, as its menu's Close does, and the
/// shortcut brings it back where it was.
#[test]
fn close_and_reopen_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 2);
    let resource = |session: &Session| crate::common::placed(session, &notes[1]);
    assert_eq!(resource(&session), 1);

    fixed(&mut session, FixedAction::CloseTab);
    until(&mut session, "the window in front closed", |session| {
        windows(session).len() == 1
    });
    assert_eq!(windows(&session)[0].0, shown[0].0, "the other window stays");

    shortcut(&mut session, ShortcutAction::ReopenClosedTab);
    until(&mut session, "the closed window is back", |session| {
        windows(session).len() == 2 && resource(session) == 1
    });
}

/// The overview shortcut opens the shell's overview and closes it again.
#[test]
fn the_overview_shortcut_opens_the_shells_overview() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let overview = tilo_session_core::desktop::overlays::Overlay::Overview.name();
    shortcut(&mut session, ShortcutAction::WorkspaceOverview);
    until(&mut session, "the overview", |session| {
        crate::common::shell_here(session) == overview
    });
    shortcut(&mut session, ShortcutAction::WorkspaceOverview);
    until(&mut session, "the overview closed", |session| {
        crate::common::shell_here(session) != overview
    });
}

/// The workspace history shortcuts open what happened here, in the workspace in front.
#[test]
fn workspace_history_opens_by_shortcut() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    fixed(&mut session, FixedAction::WorkspaceHistory);
    until(&mut session, "the workspace's history", |session| {
        tree(session, |tree| {
            tilo_session_core::desktop::navigation::window_of_app(
                tree,
                tilo_session_core::desktop::state::ACTIVITY_APP,
            )
            .is_some()
        })
    });
}

/// The emoji picker shortcut opens the shell's picker, the one the session's overlays publish, and
/// closes it again.
#[test]
fn the_emoji_shortcut_opens_the_shells_picker() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    shortcut(&mut session, ShortcutAction::EmojiPicker);
    until(&mut session, "the emoji picker", |session| {
        crate::common::shell_here(session) == "emoji"
    });
    shortcut(&mut session, ShortcutAction::EmojiPicker);
    until(&mut session, "the picker closed", |session| {
        crate::common::shell_here(session) != "emoji"
    });
}

/// New window opens the shell's launcher, to pick the app the window shows.
#[test]
fn new_window_opens_the_shells_launcher() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let launcher = tilo_session_core::desktop::overlays::Overlay::Launcher.name();
    fixed(&mut session, FixedAction::NewWindow);
    until(&mut session, "the launcher", |session| {
        crate::common::shell_here(session) == launcher
    });
}
