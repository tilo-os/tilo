//! `desktop:chrome`'s verbs under the tilolang shell: each is the session's own, done on the
//! desktop it holds, as a person's click, the palette and an agent ask it. Every check reads that
//! desktop.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::{Value, json};
use tilo_desktop_model::{GroupId, Layout};
use tilo_session::Session;

use crate::common::until;
use crate::layout_shortcuts::{
    CHROME, focused, group, new_strip, open, session, settle, strip_holding, strips, tree, windows,
    workspace,
};

/// Ask `desktop:chrome` for `verb` with `arguments`, as a caller of its published action does.
fn verb(session: &mut Session, verb: &str, arguments: &Value) {
    session
        .invoke(CHROME, verb, arguments.as_object().expect("arguments"))
        .unwrap_or_else(|error| panic!("{verb}: {error:?}"));
    settle(session);
}

/// What `verb` with `arguments` is refused with.
fn refused(session: &mut Session, verb: &str, arguments: &Value) -> String {
    match session.invoke(CHROME, verb, arguments.as_object().expect("arguments")) {
        Ok(done) => panic!("{verb} was done: {done:?}"),
        Err(error) => format!("{error:?}"),
    }
}

/// The names of the strips of the workspace in front, in order.
fn strip_names(session: &Session) -> Vec<String> {
    tree(session, |tree| {
        tree.active_workspace()
            .expect("a workspace in front")
            .strips
            .iter()
            .map(|strip| strip.name.clone())
            .collect()
    })
}

/// The workspaces: their names, in order.
fn workspace_names(session: &Session) -> Vec<String> {
    tree(session, |tree| {
        tree.workspaces.iter().map(|w| w.name.clone()).collect()
    })
}

/// How many tabs the window `id` holds.
fn tabs(session: &Session, id: GroupId) -> usize {
    group(session, id, |g| g.tabs.len())
}

/// New strip, step strip, show strip, move strip and renaming a strip: the strips of the
/// workspace in front change as each asks, and the one shown follows the person.
#[test]
fn strips_are_made_stepped_shown_moved_and_named_by_verb() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let (before, _) = strips(&session);
    let first = before[0];
    let made = new_strip(&mut session);
    assert_eq!(strips(&session).0, [first, made], "the new strip is last");

    verb(&mut session, "step-strip", &json!({ "steps": -1 }));
    until(&mut session, "back on the first strip", |session| {
        strips(session).1 == Some(first)
    });
    verb(
        &mut session,
        "show-strip",
        &json!({ "workspace": workspace(&session), "strip": made.0.to_string() }),
    );
    until(&mut session, "the strip shown", |session| {
        strips(session).1 == Some(made)
    });

    verb(
        &mut session,
        "rename",
        &json!({ "strip": made.0.to_string(), "name": "Reading" }),
    );
    until(&mut session, "the strip named", |session| {
        strip_names(session).last().map(String::as_str) == Some("Reading")
    });
    verb(
        &mut session,
        "move-strip",
        &json!({ "strip": "Reading", "before": first.0.to_string() }),
    );
    until(&mut session, "the strip moved up", |session| {
        strips(session).0 == [made, first]
    });
    assert_eq!(
        strips(&session).1,
        Some(made),
        "the strip shown stays shown"
    );
}

/// Set width, equalize, maximize, fullscreen and float: the window asked about changes, and
/// changes back.
#[test]
fn a_windows_size_follows_its_verbs() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 2);
    let (front, other) = (shown[1].0, shown[0].0);
    let id = front.to_string();
    let width = |session: &Session, g: GroupId| group(session, g, |g| g.width.permille());

    verb(
        &mut session,
        "set-width",
        &json!({ "group": id, "permille": 700 }),
    );
    until(&mut session, "the window takes 70%", |session| {
        width(session, front) == 700
    });
    verb(
        &mut session,
        "set-width",
        &json!({ "group": id, "preset": "narrow" }),
    );
    until(&mut session, "the window narrows", |session| {
        width(session, front) < 700
    });
    let strip = strip_holding(&session, front).expect("the window's strip");
    verb(
        &mut session,
        "equalize",
        &json!({ "strip": strip.0.to_string() }),
    );
    until(&mut session, "the windows share the strip", |session| {
        width(session, front) == width(session, other)
    });

    verb(
        &mut session,
        "maximize",
        &json!({ "group": id, "on": true }),
    );
    until(&mut session, "maximized", |session| {
        group(session, front, |g| g.maximized)
    });
    verb(
        &mut session,
        "maximize",
        &json!({ "group": id, "on": false }),
    );
    until(&mut session, "restored", |session| {
        !group(session, front, |g| g.maximized)
    });

    verb(
        &mut session,
        "fullscreen",
        &json!({ "group": id, "on": true }),
    );
    until(&mut session, "fullscreen", |session| {
        tree(session, Layout::fullscreen_group) == Some(front)
    });
    verb(
        &mut session,
        "fullscreen",
        &json!({ "group": id, "on": false }),
    );
    until(&mut session, "fullscreen ends", |session| {
        tree(session, Layout::fullscreen_group).is_none()
    });

    let floating = |session: &Session| {
        tree(session, |tree| {
            tilo_session_core::desktop::state::window_menu::floating(tree, front)
        })
    };
    verb(&mut session, "float", &json!({ "group": id, "on": true }));
    until(&mut session, "floating", |session| floating(session));
    verb(&mut session, "float", &json!({ "group": id, "on": false }));
    until(&mut session, "back in the strip", |session| {
        !floating(session)
    });
}

/// Move tab into another window, new group, split right and renaming a tab: a window's tabs
/// gather and part as asked, and a tab is called what the person calls it.
#[test]
fn tabs_gather_part_and_are_named_by_verb() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 3);
    let (keeper, joined) = (shown[0].0, shown[1].1);
    let strip = strip_holding(&session, keeper).expect("the strip");

    verb(
        &mut session,
        "move-tab",
        &json!({ "tab": joined.to_string(), "to": { "strip": strip.0.to_string(), "group": keeper.to_string() } }),
    );
    until(&mut session, "the tab joins the first window", |session| {
        tabs(session, keeper) == 2 && windows(session).len() == 2
    });

    verb(
        &mut session,
        "new-group",
        &json!({ "tab": joined.to_string() }),
    );
    until(&mut session, "the tab is a window again", |session| {
        tabs(session, keeper) == 1 && windows(session).len() == 3
    });

    verb(
        &mut session,
        "move-tab",
        &json!({ "tab": joined.to_string(), "to": { "strip": strip.0.to_string(), "group": keeper.to_string() } }),
    );
    until(&mut session, "joined again", |session| {
        tabs(session, keeper) == 2
    });
    verb(
        &mut session,
        "split-right",
        &json!({ "tab": joined.to_string() }),
    );
    until(
        &mut session,
        "split to the right of its window",
        |session| {
            let order: Vec<_> = windows(session).iter().map(|(g, _)| *g).collect();
            tabs(session, keeper) == 1
                && order.len() == 3
                && order.iter().position(|g| *g == keeper).map(|at| at + 1)
                    == windows(session).iter().position(|(_, tab)| *tab == joined)
        },
    );

    verb(
        &mut session,
        "rename",
        &json!({ "tab": joined.to_string(), "name": "  Plans " }),
    );
    until(&mut session, "the tab is named", |session| {
        tree(session, |tree| {
            tree.workspaces
                .iter()
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .flat_map(|g| &g.tabs)
                .any(|tab| tab.id == joined && tab.title == "Plans")
        })
    });
}

/// Close window and reopen closed: the window goes, the others keep the keyboard, and it comes
/// back.
#[test]
fn a_closed_window_reopens_by_verb() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 2);
    verb(
        &mut session,
        "close-window",
        &json!({ "group": shown[1].0.to_string() }),
    );
    until(&mut session, "the window closed", |session| {
        windows(session).len() == 1
    });
    assert_eq!(
        focused(&session),
        Some(shown[0].0),
        "the other window has the keyboard"
    );
    verb(&mut session, "reopen-closed", &json!({}));
    until(&mut session, "the window is back", |session| {
        windows(session).len() == 2 && crate::common::placed(session, &notes[1]) == 1
    });
}

/// New workspace, switch workspace and renaming a workspace: the person goes to the one made,
/// steps back, and it keeps the name they give it.
#[test]
fn workspaces_are_made_stepped_and_named_by_verb() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let home = workspace(&session);
    verb(&mut session, "new-workspace", &json!({ "name": "Taxes" }));
    until(&mut session, "in the new workspace", |session| {
        workspace(session) != home
    });
    let made = workspace(&session);
    assert_eq!(
        workspace_names(&session).last().map(String::as_str),
        Some("Taxes")
    );

    verb(&mut session, "switch-workspace", &json!({ "delta": -1 }));
    until(&mut session, "back home", |session| {
        workspace(session) == home
    });
    verb(
        &mut session,
        "switch-workspace",
        &json!({ "workspace": made }),
    );
    until(&mut session, "in the new one again", |session| {
        workspace(session) == made
    });

    verb(
        &mut session,
        "rename",
        &json!({ "workspace": "Taxes", "name": "Taxes 2026" }),
    );
    until(&mut session, "renamed", |session| {
        workspace_names(session)
            .iter()
            .any(|name| name == "Taxes 2026")
    });

    verb(
        &mut session,
        "new-workspace",
        &json!({ "name": "Later", "background": true }),
    );
    until(&mut session, "made in the background", |session| {
        workspace_names(session).iter().any(|name| name == "Later")
    });
    assert_eq!(
        workspace(&session),
        made,
        "the person stays where they were"
    );
}

/// A verb naming what the desktop doesn't hold is refused and changes nothing.
#[test]
fn a_verb_naming_nothing_here_is_refused() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    let shown = open(&mut session, &notes, 1);
    let before = windows(&session);
    refused(&mut session, "close-window", &json!({ "group": "999999" }));
    refused(&mut session, "move-strip", &json!({ "strip": "999999" }));
    refused(&mut session, "rename", &json!({ "name": "Nothing" }));
    settle(&mut session);
    assert_eq!(windows(&session), before);
    assert_eq!(focused(&session), Some(shown[0].0));
}
