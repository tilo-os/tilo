//! The shell's acceptance path (`~/work/shell-goal-acceptance.md`), one test per step, from the
//! headless session that starts with the tilolang shell as its whole screen to a theme switch
//! that changes how it looks and nothing of where it stands.
//!
//! Every step drives the production seams only: the session's own loop, its agent door, the
//! arrangement and desk it publishes, and the tree the shell lays out for assistive technology
//! (never pixels, but where a step is about looks, the frame the compositor drew).
//!
//! A step whose behaviour is not built yet keeps its real test and is marked `#[ignore]` with the
//! piece that is missing, so the acceptance path is the same list before and after the work.

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

mod common;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, json};
use tilo_agent_runs::{AgentRuns, wire};
use tilo_compositor::OutputConfig;
use tilo_reference::{ProgramStep, ResourceAt, ResourceRef, StepOutcome};
use tilo_session::{Options, Session};
use tilo_surface_host::shell_part::DESK_RESOURCE;

use common::{
    HEIGHT, MAIL, PERSON, Repl, SUBJECT, WIDTH, on_screen, placed_resources, press, press_sheet,
    private_home, seeded_session, seeded_session_with, until,
};

/// Step 1: the session starts with the tilolang shell as the whole screen — its bar across the
/// top, the switch between the presets, and the dock on its edge — and nothing of the desktop's
/// own bar or strips stands beside it.
#[test]
fn step_1_the_session_starts_with_the_shell_as_the_whole_screen() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");

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

    // The preset switch: the three ways of showing the same places.
    for chip in ["Strips", "Frames", "Spaces"] {
        assert!(
            on_screen(&session, chip).is_some(),
            "the preset switch shows {chip}: {:?}",
            session.accessible_labels()
        );
    }
    assert!(
        on_screen(&session, "Typed").is_none(),
        "the strips stand first, not the canvas"
    );

    // The dock: its items on the person's edge.
    until(&mut session, "the dock", |session| {
        on_screen(session, "Launcher").is_some()
    });
}

/// Step 2: the REPL connects through the agent door and lists the session's own resources, each
/// action as a program calls it.
#[test]
fn step_2_the_repl_connects_and_lists_the_sessions_resources() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, "Notifications").is_some()
    });

    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    let tools = repl.opened(&mut session);
    assert!(
        tools.iter().any(|tool| tool == "tilo") && tools.iter().any(|tool| tool == "tilo_ls"),
        "the REPL reaches the door's programs and its listing: {tools:?}"
    );

    // The apps, the session's own among them.
    let (apps, failed) = repl.call(&mut session, "tilo_ls", &json!({}));
    assert!(!failed, "{apps}");
    assert!(
        apps.lines().any(|line| line.starts_with("session")),
        "the session's own resources are listed: {apps}"
    );

    // The session's own: the arrangement it arranges, the desk it shows.
    let (desk, failed) = repl.call(&mut session, "tilo_ls", &json!({"path": "session"}));
    assert!(!failed, "{desk}");
    assert!(
        desk.contains("shell:arrangement") || desk.contains("arrangement"),
        "the arrangement is among them: {desk}"
    );
    assert!(desk.contains("Desktop"), "the desk is: {desk}");

    // Each action as a program calls it, what it may go without included.
    let (chrome, failed) = repl.call(&mut session, "tilo_ls", &json!({"path": "session/Desktop"}));
    assert!(!failed, "{chrome}");
    assert!(
        chrome
            .lines()
            .any(|line| line.starts_with("session.overview(open: Option<Bool> = .none)")),
        "the overview's action, as it is typed: {chrome}"
    );
    let (arranging, failed) = repl.call(
        &mut session,
        "tilo_ls",
        &json!({"path": "session/arrangement"}),
    );
    assert!(!failed, "{arranging}");
    assert!(
        arranging
            .lines()
            .any(|line| line.starts_with("session.place(")),
        "placing, as it is typed: {arranging}"
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
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    // The framed canvas first: the entry typed at stands, and no strip does.
    assert!(
        on_screen(&session, "Typed").is_some(),
        "the canvas stands: {:?}",
        session.accessible_labels()
    );

    // The strips, through the shell's preset.
    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_preset(shell.viewing, shown: shell.Preset.strips)",
    );
    assert!(!failed, "the REPL switches the preset: {answer}");
    until(&mut session, "the strips", |session| {
        on_screen(session, "Typed").is_none() && on_screen(session, "Strips").is_some()
    });

    // The traditional desktop: no preset switch at all, its own top bar instead.
    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_preset(shell.viewing, shown: shell.Preset.desktop)",
    );
    assert!(!failed, "the REPL switches to the desktop: {answer}");
    until(&mut session, "the traditional desktop", |session| {
        on_screen(session, "Strips").is_none()
    });
}

/// Step 4: "my mails" places Mail's root, drawn as the inbox with its threads.
#[test]
fn step_4_my_mails_places_the_inbox_with_its_threads() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

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
        ["tl.so.tilo.lang.mail.Mail"],
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
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

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

/// Step 6: asking an agent places the conversation on the canvas, and a reference it printed
/// opens on press.
#[test]
#[ignore = "step 6: an ask records the run and the desk publishes its conversation, but nothing places \"agent-run:<id>\" on the arrangement — the ask's run never reaches `shell:arrangement`'s `place`, so the conversation is only a desk surface until a person or an agent places it by hand"]
fn step_6_asking_places_the_conversation_and_a_reference_opens_on_press() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let runs = Arc::new(AgentRuns::new(None));
    let mut session = seeded_session_with(root.path(), Some(Arc::clone(&runs)));
    until(&mut session, "the runs know the workspace", |_| {
        !runs.front().is_empty()
    });
    let note = seeded_note(&session, "Groceries");

    // The person asks through the door, as the composer does, naming the run so the place can be
    // looked for by it.
    session
        .invoke(
            wire::RESOURCE,
            "ask",
            json!({"id": "tidy", "title": "Tidy the launch notes", "agent": "agent:claude"})
                .as_object()
                .unwrap(),
        )
        .expect("the session queues the ask");

    // Asking places the conversation on the canvas.
    until(&mut session, "the conversation placed", |session| {
        session
            .arrangement()
            .borrow()
            .strips()
            .places()
            .any(|place| place.record == "agent-run:tidy")
    });
    until(&mut session, "the conversation drawn", |session| {
        session.shell_says("Tidy the launch notes")
    });

    // Its agent runs a program that prints the note: a reference row.
    runs.record_step(
        "tidy",
        ProgramStep {
            program: "find(notes.Note, \"Groceries\")".into(),
            outcome: StepOutcome::Done,
            answer: vec!["n1 \"Groceries\"".into()],
            refs: vec![ResourceAt {
                app: "so.tilo.lang.notes".into(),
                record: None,
                reference: ResourceRef::whole("notes.Note", note.clone(), "Groceries"),
            }],
            ..ProgramStep::default()
        },
    )
    .expect("the agent records its step");
    until(&mut session, "the printed reference", |session| {
        session.shell_says("Groceries")
    });
    assert!(
        session.arrangement().borrow().strips().places().count() == 1,
        "only the conversation stands on the canvas yet"
    );

    // Pressing the reference places the note beside the conversation.
    press(&mut session, "Groceries");
    until(&mut session, "the reference's note placed", |session| {
        session
            .arrangement()
            .borrow()
            .strips()
            .places()
            .any(|place| place.record == note)
    });
}

/// Step 7: what the REPL types runs the same as the agent's program — the same `tilo` call, the
/// same result, whether the person typed it or an agent ran it.
#[test]
fn step_7_what_the_repl_types_runs_the_same_as_the_agents_program() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, "Notifications").is_some()
    });

    // The REPL, acting as the person.
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    // An agent of theirs, admitted on the sheet the door opens.
    let mut agent = Repl::asking(&root.path().join("agent.sock"), false);
    until(&mut session, "the door's question", |session| {
        session
            .portal()
            .and_then(tilo_sheets::portal::PortalSeat::dialog)
            .is_some()
    });
    press_sheet(
        &mut session,
        tilo_sheets::portal::PortalEffect::SELECT.id,
        &json!({"index": 0}),
    );
    press_sheet(
        &mut session,
        tilo_sheets::portal::PortalEffect::CONFIRM.id,
        &json!({}),
    );
    agent.opened(&mut session);

    // The same program, printed by both: the workspace's notes saying "launch", found and
    // described the same way.
    let program = "find(\"launch\").take(2)";
    let (typed, typed_failed) = repl.run(&mut session, program);
    let (ran, ran_failed) = agent.run(&mut session, program);
    assert!(!typed_failed, "the REPL's program ran: {typed}");
    assert!(!ran_failed, "the agent's program ran: {ran}");
    assert_eq!(
        typed, ran,
        "the same program answers the same, typed and run"
    );
}

/// Step 8: traditional desktop — a press on a dock item opens it, and the top bar shows the
/// focused window's actions. (A dock press already opens its item on the strips desk; what is
/// missing is the traditional desk itself, which this test drives the same way.)
#[test]
fn step_8_a_dock_press_opens_its_item_and_the_bar_shows_the_windows_actions() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    // The traditional desktop, through the shell's own record.
    let (answer, failed) = repl.run(
        &mut session,
        "shell.apply(shell.shell, to: shell.Layout.windows_desktop)",
    );
    assert!(!failed, "the desk is the traditional one: {answer}");
    until(&mut session, "the traditional desktop", |session| {
        on_screen(session, "Strips").is_none() && on_screen(session, "Dock").is_some()
    });

    // A dock press opens what its item stands for.
    press(&mut session, "Launcher");
    until(&mut session, "the launcher the dock opened", |session| {
        session
            .read(DESK_RESOURCE)
            .is_some_and(|desk| desk["here"] == json!("launcher"))
    });

    // The top bar shows the focused window's own commands. The seeded note starts pinned, so
    // its pin control toggles; unpinning it through the note's own published action leaves a
    // note whose bar offers "Pin", as one it would pin does.
    let note = seeded_note(&session, "Groceries");
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
    let pin = offered(&session.resources(), &note, "Pin").expect("the note offers its pin");
    session
        .invoke(&note, &pin, &Map::new())
        .expect("the note unpins");
    session
        .invoke(
            "shell:arrangement",
            "place",
            json!({"resource": note, "x": 80, "y": 60})
                .as_object()
                .unwrap(),
        )
        .expect("the note is placed");
    until(&mut session, "the focused window's actions", |session| {
        on_screen(session, "Pin").is_some()
    });
}

/// Step 9: a theme switch changes how the desk looks and nothing of its layout.
#[test]
fn step_9_a_theme_switch_changes_looks_only() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
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

    // Where the desk's own parts stand, and what the screen looks like.
    let controls = ["Strips", "Frames", "Spaces", "Notifications", "Launcher"];
    let before: Vec<(String, (f64, f64, f64, f64))> = controls
        .iter()
        .filter_map(|name| {
            session
                .shell_control(name)
                .map(|rect| ((*name).to_owned(), rect))
        })
        .collect();
    assert_eq!(before.len(), controls.len(), "the desk's parts stand");
    let places = placed_resources(&session);
    let shot_before = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the frame the desk drew");

    // The person picks another theme, as Settings does, one after another (glass, then liquid
    // glass): each draws the desk anew, and neither moves anything.
    let mut shot = shot_before;
    for theme in ["glass", "liquid-glass"] {
        settings_store::update_section(
            "appearance",
            Map::from_iter([
                ("mode".to_owned(), json!("dark")),
                ("dark_theme".to_owned(), json!(theme)),
            ]),
        )
        .expect("the theme is kept");
        let mut drew = None;
        for _ in 0..200 {
            session.step(Duration::from_millis(1)).expect("step");
            let frame = session
                .compositor()
                .screenshot("HEADLESS-1")
                .expect("a frame");
            if frame.rgba != shot.rgba {
                drew = Some(frame);
                break;
            }
        }
        let drawn = drew.unwrap_or_else(|| panic!("the desk draws in {theme}"));
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

/// The published id of the seeded note titled `title`.
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

/// The window place and node of the field holding `value`, as assistive technology reads it: an
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
