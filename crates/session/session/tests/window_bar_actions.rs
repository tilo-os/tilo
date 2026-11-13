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

use serde_json::Value;
use tilo_session::Session;

use crate::common::{Repl, on_screen, until};

/// The menu of the window's own actions, as the shell's bar carries it.
const WINDOW_ACTIONS: &str = "Window actions";

/// The pill at the trailing end of a strip's window bar: the window's own width, focus and pin
/// commands.
const WINDOW_MENU: &str = "This window";

/// The session with the shell as the whole screen and the seeded apps: the framed canvas is in
/// front of it, and the seeded notes stand on the desk as its windows.
fn session(root: &std::path::Path) -> (Session, Vec<String>) {
    let session = crate::common::seeded_session(root);
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
                "shell.show_style(shell.viewing, style: \"so.tilo.shell:{}\")",
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
            crate::common::preset_menu(session).is_none() && on_screen(session, "Dock").is_some()
        });
    } else {
        until(session, &format!("the {preset} desk"), |session| {
            crate::common::preset_menu(session).as_deref() == Some(&format!("Showing {preset}"))
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
    let (place, node) = crate::common::wait_for(
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

/// Close the menu the last press opened without choosing a row: Escape, as a person leaves a menu
/// they did not mean.
fn dismiss_menu(session: &mut Session) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(tilo_compositor::InputEvent::Key {
                keycode: KEY_ESCAPE,
                pressed,
            })
            .expect("the key");
        session
            .step(std::time::Duration::from_millis(16))
            .expect("a frame");
    }
}

/// The evdev code of Escape.
const KEY_ESCAPE: u32 = 1;

/// The strip holding the place that shows `record`.
fn strip_of(session: &Session, record: &str) -> String {
    crate::common::places(session)
        .into_iter()
        .find(|place| place.record == record)
        .map_or_else(
            || panic!("the desk holds a place for {record}"),
            |place| place.strip,
        )
}

/// Show the strip `strip` as the one in front, as a press on its window's "Bring to front" does:
/// the shell's own `focus_strip`, through the agent door.
fn focus_strip(session: &mut Session, root: &std::path::Path, strip: &str) {
    let mut repl = Repl::connect(&root.join("agent.sock"));
    repl.opened(session);
    let (answer, failed) = repl.run(
        session,
        &format!(r#"shell.focus_strip(shell.viewing, strip: "{strip}")"#),
    );
    assert!(!failed, "the shell focuses strip {strip}: {answer}");
    session
        .step(std::time::Duration::from_millis(16))
        .expect("a frame");
}

/// Press the pill of the window named `window`: the "This window" menu at the trailing end of that
/// window's bar. Every strip's window carries a pill under the same name — and each bar also keeps
/// a placeholder pill off screen — so the pill taken is the one on the same bar row as the window's
/// own name, at the trailing side of it, as the bar that names the window carries it.
fn press_window_menu(session: &mut Session, window: &str) {
    let (place, node) = crate::common::wait_for(
        session,
        format!("{window:?}'s own window menu"),
        |session| {
            let nodes: Vec<(ui::accesskit::NodeId, ui::accesskit::Node)> = session
                .a11y_windows()
                .into_iter()
                .flat_map(|(_, _, tree)| tree.nodes)
                .collect();
            let named = nodes
                .iter()
                .find(|(_, node)| node.label() == Some(window))
                .and_then(|(_, node)| node.bounds())?;
            let place = session
                .a11y_windows()
                .into_iter()
                .find_map(|(place, _, tree)| {
                    tree.nodes
                        .iter()
                        .any(|(_, node)| node.label() == Some(window))
                        .then_some(place)
                })?;
            let trailing = nodes
                .iter()
                .filter(|(_, node)| node.label() == Some(WINDOW_MENU))
                .filter_map(|(id, node)| node.bounds().map(|rect| (*id, rect)))
                .filter(|(_, rect)| {
                    rect.width() > 0.0
                        && rect.height() > 0.0
                        && rect.y0 < named.y1
                        && rect.y1 > named.y0
                        && rect.x0 >= named.x0
                })
                .min_by(|(_, a), (_, b)| a.x0.total_cmp(&b.x0))
                .map(|(id, _)| id)?;
            Some((place, trailing))
        },
    );
    assert!(
        session.a11y_perform(place, node, &ui::a11y::Action::Activate),
        "the desk takes the press on {window:?}'s window menu"
    );
    until(
        session,
        &format!("{window:?}'s window menu to open"),
        |session| session.shell_says("Narrow") && session.shell_says("Normal"),
    );
}

/// In Strips only a window behind offers "Bring to front": the window in front's own menu never
/// does, so a person reading it is offered nothing that would do nothing. `window_controls` takes
/// `in_front` for that one row alone, and passing it `in_front and lists` offered the row on every
/// window whenever the desk's bar listed no commands (`lists` false), which is Strips' way.
#[test]
fn only_a_window_behind_offers_bring_to_front_in_strips() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    let one = notes.first().expect("a note").clone();
    let other = notes.get(1).expect("a second note").clone();
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    show(&mut session, root.path(), "Strips");

    // Two notes placed, each into a strip of its own: two windows on the desk.
    place(&mut session, &one);
    place(&mut session, &other);
    let one_strip = strip_of(&session, &one);
    let other_strip = strip_of(&session, &other);
    assert_ne!(one_strip, other_strip, "each note stands in its own strip");
    let one_name = crate::common::window_name_of(&session, &one);
    let other_name = crate::common::window_name_of(&session, &other);
    until(
        &mut session,
        "both notes' windows on the Strips desk",
        |session| {
            on_screen(session, &one_name).is_some() && on_screen(session, &other_name).is_some()
        },
    );

    for (front, back) in [(&one_name, &other_name), (&other_name, &one_name)] {
        let front_strip = strip_of(&session, if front == &one_name { &one } else { &other });
        focus_strip(&mut session, root.path(), &front_strip);

        // The window in front: its own menu lists the window's commands and no way to come to
        // the front it is already in front of.
        press_window_menu(&mut session, front);
        assert!(
            !session.shell_says("Bring to front"),
            "{front:?} is in front, so its own menu offers no way to come to the front of itself: \
             {:?}",
            session.accessible_labels()
        );
        dismiss_menu(&mut session);

        // The window behind: its own menu offers that way, and it is the row the person presses.
        press_window_menu(&mut session, back);
        assert!(
            session.shell_says("Bring to front"),
            "{back:?} stands behind, so its own menu offers \"Bring to front\": {:?}",
            session.accessible_labels()
        );
        dismiss_menu(&mut session);
    }
}

/// A placed note's bar shows its actions, and pressing the pin runs the note's own action: the
/// record flips, as it does when an agent runs the same action. Frames, Strips and the Desktop
/// each keep that one list of what the window can do now.
#[test]
fn every_shells_window_bar_lists_the_notes_actions_and_runs_one() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    let note = notes.first().expect("a note").clone();
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let key = place(&mut session, &note);
    let name = crate::common::window_name_of(&session, &note);

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
            |session| session.shell_says("Rename…") && session.shell_says("Tag…"),
        );

        // The action the list offers runs the note's own action on the note: its pin flips, and
        // the label the menu offered says which way. Read back from the record.
        press_shown(&mut session, label);
        let seen = crate::common::wait_maybe(&mut session, |session| {
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
            crate::common::place(&session, crate::common::number(&key)).is_some(),
            "the note stays where it was placed in the {preset} desk"
        );
    }
}
