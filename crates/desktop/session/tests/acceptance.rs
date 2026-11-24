//! The shell's acceptance path, one test per step, from the headless session that starts with the
//! tilolang shell as its whole screen to a theme switch that changes how it looks and nothing of
//! where it stands.
//!
//! Every step drives the production services only: the session's own loop, its MCP server, the
//! layout and session it publishes, and the tree the shell lays out for assistive technology
//! (never pixels, but where a step is about looks, the frame the compositor drew).
//!
//! A step joins the list once its behaviour is built; an ignored step would be a red nobody owns.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_session::shell::part::OVERLAYS;
use tilo_session::{Options, Session};

use crate::common::{
    Agent, Api, CANVAS, HEIGHT, MAIL, PERSON, SUBJECT, WIDTH, on_screen, placed_resources,
    preset_menu, press, private_home, seeded_session, type_text, until,
};

/// Step 1: the session starts with the tilolang shell as the whole screen — its bar across the top,
/// the switch between the presets, and the dock on its edge — and nothing of the desktop's own bar
/// or strips stands beside it.
#[test]
fn step_1_the_session_starts_with_the_shell_as_the_whole_screen() {
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        ..Options::default()
    });

    // The bar: the shell's own parts, drawn over the whole output.
    until(&mut session, "the bar's parts", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let bars = session.shell_parts();
    assert_eq!(
        bars.len(),
        1,
        "one shell part, on the one display: {bars:?}"
    );
    let (_, _, width, height) = bars[0].rect;
    assert_eq!(
        (i64::from(width), i64::from(height)),
        (i64::from(WIDTH), i64::from(HEIGHT)),
        "the shell covers the display"
    );

    // The preset switch: the bar's own menu, saying the preset the session shows, offering the ways of
    // showing the same places.
    let menu = preset_menu(&session).expect("the listing's preset menu");
    press(&mut session, &menu);
    for preset in ["Strips", "Frames", "Frameless", "Desktop"] {
        assert!(
            on_screen(&session, preset).is_some(),
            "the preset switch offers {preset}: {:?}",
            session.accessible_labels()
        );
    }
    press(&mut session, "Strips");
    until(&mut session, "the menu closed", |session| {
        on_screen(session, "Frames").is_none()
    });
    assert!(
        on_screen(&session, CANVAS).is_none(),
        "the strips stand first, not the canvas"
    );

    // The dock: its items on the person's edge.
    until(&mut session, "the dock", |session| {
        on_screen(session, "Launcher").is_some()
    });
}

/// Step 2: the REPL connects through Tilo's API and lists the session's own resources, each
/// action as a program calls it.
#[test]
fn step_2_the_repl_connects_and_lists_the_sessions_resources() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let repl = Api::open(&mut session, root.path());

    // The apps, the session's own among them.
    let apps = repl.ls(&mut session, "");
    assert!(
        Api::rows(&apps).any(|row| row["name"] == "session"),
        "the session's own resources are listed: {apps}"
    );

    // The session's own: the layout it arranges — listed by the word a person reads it by, with
    // its placing among its actions — and the session it shows.
    let listing = repl.ls(&mut session, "session");
    assert!(
        Api::rows(&listing).any(|row| row["label"] == "Layout"
            && row["actions"]
                .as_array()
                .is_some_and(|actions| actions.contains(&json!("place")))),
        "the layout is among them, with its placing: {listing}"
    );
    // A listing names the roots of what the session publishes: the session is a row of its own,
    // by the kind its resource is, and what it holds is read a level down.
    assert!(
        Api::rows(&listing).any(|row| row["kind"] == "desktop"),
        "the listing is: {listing}"
    );

    // Each action with what it takes. The chrome by its key: a window's control reads under the
    // window's name, and the session's window is another `Desktop` .
    let chrome = repl.ls(&mut session, "session/desktop:chrome");
    let overview = Api::action(&chrome, "overview")
        .unwrap_or_else(|| panic!("the overview's action: {chrome}"));
    assert_eq!(
        Api::param(&overview, "open")["required"],
        false,
        "{overview}"
    );
    let layout_key = tilo_actions::remote::session_actions::LAYOUT_RESOURCE
        .rsplit(':')
        .next()
        .expect("a resource key");
    let placing = repl.ls(&mut session, &format!("session/{layout_key}"));
    assert!(
        Api::action(&placing, "place").is_some(),
        "placing: {placing}"
    );
}

/// Step 3: the REPL switches the layout — strips, the framed canvas and the traditional desktop —
/// and the screen changes with it.
#[test]
fn step_3_the_repl_switches_the_layout_and_the_screen_changes() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Api::open(&mut session, root.path());

    // The framed canvas first: the entry typed at stands, and no strip does.
    assert!(
        on_screen(&session, CANVAS).is_some(),
        "the canvas stands: {:?}",
        session.accessible_labels()
    );

    // The strips, through the shell's preset.
    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_style(shell.viewing, style: \"so.tilo.shell:strips\")",
    );
    assert!(!failed, "the REPL switches the preset: {answer}");
    until(&mut session, "the strips", |session| {
        on_screen(session, CANVAS).is_none()
            && preset_menu(session).as_deref() == Some("Showing Strips")
    });

    // The traditional desktop: no preset switch at all, its own top bar instead.
    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_style(shell.viewing, style: \"so.tilo.shell:desktop\")",
    );
    assert!(!failed, "the REPL switches to the desktop: {answer}");
    until(&mut session, "the traditional desktop", |session| {
        preset_menu(session).is_none()
    });
}

/// Step 4: "my mails" places Mail's root, drawn as the inbox with its threads.
#[test]
fn step_4_my_mails_places_the_inbox_with_its_threads() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let mut repl = Api::open(&mut session, root.path());

    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"my mails\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");

    until(&mut session, "what \"my mails\" names placed", |session| {
        !placed_resources(session).is_empty()
    });
    let placed = placed_resources(&session);
    assert_eq!(
        placed
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        ["tl.so.tilo.mail.Mail"],
        "\"my mails\" places Mail's root: {placed:?}"
    );

    // Drawn as the inbox, by mail's own view: a seeded conversation stands on the canvas.
    until(&mut session, "the inbox's threads", |session| {
        session.shell_says(SUBJECT)
    });
    assert_eq!(
        session.shell_apps(),
        vec![MAIL.to_owned()],
        "mail's package is the one the place loads"
    );
}

/// Step 5: an inline edit of a placed record commits, through the session's owner of the record.
#[test]
fn step_5_an_inline_edit_of_a_placed_record_commits() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let note = seeded_note(&session, "Groceries");
    let mut repl = Api::open(&mut session, root.path());

    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"Groceries\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    until(&mut session, "the note's editor on the canvas", |session| {
        session.shell_says("Groceries")
    });

    // The editor's title field, as assistive technology reads it: the one showing the title.
    let (place, field) = title_field(&session, "Groceries").unwrap_or_else(|| {
        panic!(
            "the placed note's title field stands: {:?}",
            session.accessible_labels()
        )
    });
    let edited = "Groceries and lemons";
    assert!(
        session.a11y_perform(place, field, &ui::a11y::Action::SetValue(edited.to_owned())),
        "the place takes the edit its field offers"
    );

    // The record the notes app keeps changed, as its owner committed it.
    until(&mut session, "the note's title read back", |session| {
        session
            .read(&note)
            .is_some_and(|reading| reading.to_string().contains(edited))
    });
    until(&mut session, "the place draws the new title", |session| {
        session.shell_says(edited)
    });
}

/// Step 7: what the REPL types runs the same as the agent's program: the same program finds the
/// same, whether the person typed it or an agent ran it.
#[test]
fn step_7_what_the_repl_types_runs_the_same_as_the_agents_program() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, "Notifications").is_some()
    });

    // The REPL, acting as the person.
    let mut repl = Api::open(&mut session, root.path());

    // An agent of theirs, admitted on the sheet the API opens.
    let mut agent = Agent::admitted(&mut session, &root.path().join("agent.sock"));

    // The same program, printed by both: the workspace's notes saying "launch", found and described
    // the same way. The agent is told first what it may not search on this device, which the person
    // may. The person reads each resource by its title; the agent names each by a reference and
    // the same title beside it.
    let program = "find(\"launch\").take(2)";
    let (typed, typed_failed) = repl.run(&mut session, program);
    let (ran, ran_failed) = agent.run(&mut session, program);
    assert!(!typed_failed, "the REPL's program ran: {typed}");
    assert!(!ran_failed, "the agent's program ran: {ran}");
    let told: Vec<&str> = typed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        !told.is_empty(),
        "the person's program found something: {typed}"
    );
    for found in told {
        assert!(
            ran.contains(&format!("{found:?}")) || ran.contains(found),
            "the same program finds {found:?}, typed by the person and answered by the agent: \
                {typed:?} and {ran:?}"
        );
    }
}

/// Step 8: traditional desktop — a press on a dock item opens it, and the top bar shows the focused
/// window's actions. (A dock press already opens its item on the strips session; what is missing is
/// the traditional session itself, which this test drives the same way.)
#[test]
fn step_8_a_dock_press_opens_its_item_and_the_bar_shows_the_windows_actions() {
    fn offered(resources: &[tilo_actions::ResourceSpec], id: &str, label: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                resource
                    .actions
                    .iter()
                    .find(|offered| offered.label == label)
                    .map(|offered| offered.id.clone())
            } else {
                offered(&resource.children, id, label)
            }
        })
    }
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());

    // The traditional desktop, through the chrome's own preset action.
    session
        .run_shell_action("apply", vec![Value::from("windows_desktop")])
        .unwrap_or_else(|why| panic!("the shell shows the traditional desktop: {why}"));
    until(&mut session, "the traditional desktop", |session| {
        preset_menu(session).is_none() && on_screen(session, "Dock").is_some()
    });

    // A dock press opens what its item stands for.
    press(&mut session, "Launcher");
    until(&mut session, "the launcher the dock opened", |session| {
        session
            .read(OVERLAYS)
            .is_some_and(|listing| listing["here"] == json!("launcher"))
    });

    // The top bar shows the focused window's own commands. The seed pins this note, so its pin
    // control toggles; unpinning it through the note's own published action leaves a note whose
    // bar offers "Pin", as one it would pin does.
    let note = seeded_note(&session, "Q3 plan");
    let pin = offered(&session.resources(), &note, "Pin").expect("the note offers its pin");
    session
        .invoke(&note, &pin, &Map::new())
        .expect("the note unpins");
    session
        .invoke(
            "shell:layout",
            "place",
            json!({"resource": note, "x": 80, "y": 60})
                .as_object()
                .unwrap(),
        )
        .expect("the note is placed");
    // The bar itself carries the window's own commands: the menu it opens lists them, and the menu
    // stands on the bar's row, not on the window's. A note whose body also says "Pin" (its own
    // header) would satisfy a search for the word anywhere; what this step proves is that the bar
    // holds it, so it presses the bar's menu and reads the row it opens.
    let name = crate::common::place_title(&session, &note);
    // The bar draws the window the layout names once the shell has read it: the place is
    // published as the session arranges it, and the next frame carries it to the bar.
    until(
        &mut session,
        "the bar naming the window in front",
        |session| {
            session
                .shell_controls(&name)
                .iter()
                .any(|&(_, y, _, _)| y < 40.0)
        },
    );
    let bar = session
        .shell_controls(&name)
        .into_iter()
        .find(|&(_, y, _, _)| y < 40.0)
        .expect("the bar names the window in front");
    press(&mut session, &name);
    until(&mut session, "the note's actions on the bar", |session| {
        on_screen(session, "Pin").is_some_and(|pin| pin.1 < 40.0 || pin.1 > bar.3)
    });
}

/// Step 9: a theme switch changes how the session looks and nothing of its layout.
#[test]
fn step_9_a_theme_switch_changes_looks_only() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let mut repl = Api::open(&mut session, root.path());
    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"my mails\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    until(&mut session, "the inbox drawn", |session| {
        session.shell_says(SUBJECT)
    });
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).expect("step");
    }

    // Where the session's own parts stand, and what the screen looks like.
    // The canvas is the whole screen: its bar keeps the notices and the person's own menu, and it
    // has no dock (`canvas.md` §7).
    let menu = preset_menu(&session).expect("the listing's preset menu");
    let controls = [menu.as_str(), "Notifications"];
    let before: Vec<(String, (f64, f64, f64, f64))> = controls
        .iter()
        .filter_map(|name| {
            session
                .shell_control(name)
                .map(|rect| ((*name).to_owned(), rect))
        })
        .collect();
    assert_eq!(before.len(), controls.len(), "the listing's parts stand");
    let places = placed_resources(&session);
    let shot_before = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the frame the listing drew");

    // The person picks another theme, as Settings does, one after another (Paper, then Dusk):
    // each draws the session anew, and neither moves anything.
    let mut shot = shot_before;
    for theme in ["paper-dark", "dusk-dark"] {
        settings_store::update_section(
            "appearance",
            Map::from_iter([
                ("mode".to_owned(), json!("dark")),
                ("dark_theme".to_owned(), json!(theme)),
            ]),
        )
        .expect("the theme is kept");
        let drawn = crate::common::wait_for(
            &mut session,
            format!("the listing to draw in {theme}"),
            |session: &mut Session| {
                session.step(Duration::from_millis(1)).expect("step");
                let frame = session
                    .compositor()
                    .screenshot("HEADLESS-1")
                    .expect("a frame");
                (frame.rgba != shot.rgba).then_some(frame)
            },
        );
        shot = drawn;

        // Only how it looks: the same parts in the same places, the same places on the canvas.
        for (name, stand) in &before {
            let after = session.shell_control(name).unwrap_or_else(|| {
                panic!(
                    "{name} still stands under {theme}: {:?}",
                    session.accessible_labels()
                )
            });
            assert_eq!(
                after, *stand,
                "{name} stands where it stood: {theme} changes looks, not layout"
            );
        }
        assert_eq!(
            placed_resources(&session),
            places,
            "the same places show the same resources under {theme}"
        );
    }
}

/// The published id of the seeded note titled `title` .
fn seeded_note(session: &Session, title: &str) -> String {
    fn find(resources: &[tilo_actions::ResourceSpec], title: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.label == title && resource.kind.ends_with(".Note") {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, title)
            }
        })
    }
    find(&session.resources(), title).unwrap_or_else(|| panic!("the seeded note {title:?}"))
}

/// The window place and node of the field holding `value` , as assistive technology reads it: an
/// editable text field (the shell's own places are drawn by the shell, so its window is the one
/// that carries them).
fn title_field(session: &Session, value: &str) -> Option<(usize, ui::accesskit::NodeId)> {
    for (place, _, tree) in session.a11y_windows() {
        for (id, node) in tree.nodes {
            if matches!(
                node.role(),
                ui::accesskit::Role::TextInput | ui::accesskit::Role::MultilineTextInput
            ) && node.value() == Some(value)
            {
                return Some((place, id));
            }
        }
    }
    None
}

/// What the person types at the caret they pointed stands in it as they type: the caret keeps its
/// words while the shell's record moves under it.
#[test]
fn typing_at_the_canvas_caret_shows_what_is_typed() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let at = crate::common::free_point(&mut session);
    crate::common::click(&mut session, at);
    until(
        &mut session,
        "the caret where the person pointed",
        |session| on_screen(session, crate::common::CANVAS_CARET).is_some(),
    );
    type_text(&mut session, "groceries");
    until(&mut session, "the typed words at the caret", |session| {
        session.shell_says("groceries")
    });
}
