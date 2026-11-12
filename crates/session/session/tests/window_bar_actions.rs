//! Every shell's window bar shows the window's own actions: what the resource the place shows
//! publishes, in the same list the agent and the REPL see for it, so a press runs the same
//! broker call an agent runs. Frames, Strips and the Desktop each draw a placed note's bar with
//! that list, and pressing one changes the note — read back from its record, never from pixels.
//!
//! The list is the shell's, not the app's: the window's own header row keeps its title and the
//! shell's controls alone (`ui.WindowFrame(lists_actions:)`), so an action never stands twice
//! and the chrome is the system's (one-app-model.md §3, "Windows belong to the shell").

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use serde_json::Value;
use tilo_session::Session;

use common::{Repl, on_screen, until};

/// The menu of the window's own actions, as the shell's bar carries it.
const WINDOW_ACTIONS: &str = "Window actions";

/// The session with the shell as the whole screen and the seeded apps: the framed canvas is in
/// front of it, and the seeded notes stand on the desk as its windows.
fn session(root: &std::path::Path) -> (Session, Vec<String>) {
    let session = common::seeded_session(root);
    let notes = note_ids(&session);
    assert!(notes.len() >= 2, "the seed keeps two notes: {notes:?}");
    (session, notes.into_iter().take(2).collect())
}

/// The ids of the notes the seeded notes app holds, in the order it lists them.
fn note_ids(session: &Session) -> Vec<String> {
    fn find(resources: &[tilo_actions::ResourceSpec], out: &mut Vec<String>) {
        for resource in resources {
            if resource.kind.ends_with(".Note") {
                out.push(resource.id.as_str().to_owned());
            }
            find(&resource.children, out);
        }
    }
    let mut out = Vec::new();
    find(&session.resources(), &mut out);
    out
}

/// Show the desk style `preset` the way a person does: the three canvas presets through the
/// bar's own style switch (the shell's `show_style`), the desktop through the preset it is
/// applied as (`chrome.tl`'s `apply`).
fn show(session: &mut Session, root: &std::path::Path, preset: &str) {
    if preset == "Desktop" {
        session
            .run_shell_setting("apply", vec![Value::from("windows_desktop")])
            .unwrap_or_else(|why| panic!("the desk shows {preset}: {why}"));
    } else {
        let mut repl = Repl::connect(&root.join("agent.sock"));
        repl.opened(session);
        let (answer, failed) = repl.run(
            session,
            &format!(
                "shell.show_style(shell.viewing, shown: \"so.tilo.shell:{}\")",
                preset.to_lowercase()
            ),
        );
        assert!(
            !failed || answer.contains("shown that way already"),
            "the desk shows {preset}: {answer}"
        );
    }
    if preset == "Desktop" {
        until(session, "the desktop", |session| {
            common::preset_menu(session).is_none() && on_screen(session, "Dock").is_some()
        });
    } else {
        until(session, &format!("the {preset} desk"), |session| {
            common::preset_menu(session).as_deref() == Some(&format!("Showing {preset}"))
        });
    }
}

/// Place `resource` on the canvas through the arrangement's published `place`, as the canvas's
/// own typed line and an agent do. Its place's key.
fn place(session: &mut Session, resource: &str) -> String {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.slot.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments = serde_json::json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": place,
        "resource": resource,
        "x": 100,
        "y": 120,
        "width": 480,
        "height": 360,
    })
    .as_object()
    .expect("an object")
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place.to_string()
}

/// Whether the note `record` is pinned in its own app's record, read through the session's door
/// as the record service publishes it: the note's one flag field, whichever key the app keeps it
/// under (the ids lock keeps that key stable across builds).
fn note_pinned(session: &Session, record: &str) -> Option<bool> {
    session
        .read(record)?
        .as_object()?
        .values()
        .find_map(Value::as_bool)
}

/// Press the control named `name` as a person on the desk does: activate the accessibility node
/// a screen reader reaches, at the surface it stands on. A name the shell lays out more than once
/// (the window's own menu row, and the placeholder a menu of another surface keeps at its own
/// hidden corner) is taken at the one that stands with a size on screen.
fn press_shown(session: &mut Session, name: &str) {
    let (place, node) = common::wait_for(
        session,
        format!("the shell's {name} to stand on the display"),
        |session| {
            session
                .a11y_windows()
                .into_iter()
                .find_map(|(place, _, tree)| {
                    tree.nodes
                        .iter()
                        .find(|(_, node)| {
                            node.label() == Some(name)
                                && node
                                    .bounds()
                                    .is_some_and(|rect| rect.width() > 0.0 && rect.height() > 0.0)
                        })
                        .map(|(id, _)| (place, *id))
                })
        },
    );
    assert!(
        session.a11y_perform(place, node, &ui::a11y::Action::Activate),
        "the desk takes the press on {name:?}"
    );
    session
        .step(std::time::Duration::from_millis(16))
        .expect("a frame");
}

/// A placed note's bar shows its actions, and pressing the pin runs the note's own action: the
/// record flips, as it does when an agent runs the same action. Frames, Strips and the Desktop
/// each keep that one list of what the window can do now.
#[test]
fn every_shells_window_bar_lists_the_notes_actions_and_runs_one() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let (mut session, notes) = session(root.path());
    let note = notes.first().expect("a note").clone();
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let key = place(&mut session, &note);
    let name = common::window_name_of(&session, &note);

    for preset in ["Frames", "Strips", "Desktop"] {
        // The note's own flag, read before the menu opens: running anything through the door may
        // dismiss an open menu, so the record is read while the desk is at rest.
        let before = note_pinned(&session, &note).unwrap_or_else(|| {
            panic!(
                "the note {note} reads its pinned field: {:?}",
                session.read(&note)
            )
        });
        let label = if before { "Unpin" } else { "Pin" };

        show(&mut session, root.path(), preset);
        until(
            &mut session,
            &format!("the note's window on the {preset} desk"),
            |session| on_screen(session, &name).is_some(),
        );

        // The window's own actions, in the bar's one list: the menu named for the window in the
        // Desktop's top bar (`action_bar` with its subject), the bar's own actions menu in the
        // two canvas frames' windows. The window in front is the note just placed.
        if preset == "Desktop" {
            press_shown(&mut session, &name);
        } else {
            press_shown(&mut session, WINDOW_ACTIONS);
        }
        until(
            &mut session,
            &format!("the note's actions in the {preset} menu"),
            |session| session.shell_says("Retitle") && session.shell_says("Tag"),
        );

        // The action the list offers runs the note's own action on the note: its pin flips, and
        // the label the menu offered says which way. Read back from the record.
        press_shown(&mut session, label);
        let seen = common::wait_maybe(&mut session, |session| {
            note_pinned(session, &note).filter(|now| *now != before)
        });
        assert_eq!(
            seen,
            Some(!before),
            "the {label} pressed from the {preset} bar changes the note's record: pinned went \
             {before} -> {:?}; the session says: {:?}",
            note_pinned(&session, &note),
            session.accessible_labels()
        );
        assert!(
            common::place(&session, common::number(&key)).is_some(),
            "the note stays where it was placed in the {preset} desk"
        );
    }
}
