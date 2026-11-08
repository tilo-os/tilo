//! The REPL at the session's agent door, on the headless session with the tilolang shell as the
//! whole screen: `tilolang repl --door` asks to act as the person, the person admits it, and the
//! lines it sends are the agent's own calls (`tilo_ls`, `tilo`), run as the person.
//!
//! The person admits it either on the sheet the door's question opens, pressed as they press it,
//! or ahead of time, by naming the program when they start the session (`--admit-repl`). A note
//! placed on the arrangement by a `tilo` program stands on the shell's screen, read from what the
//! shell lays out for assistive technology, and is the person's step, which `undo` takes back.

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

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_service::{Questions, Service as _, Started};
use tilo_session::{ActionPlane, Options, Session};
use tilo_sheets::portal::PortalEffect;
use tilolang_tilo::records_service::RecordService;

use common::{
    CANVAS_ENTRY, HEIGHT, MAIL, NOTE_TITLE, NOTES, PERSON, Repl, SUBJECT, WIDTH, control,
    on_screen, place_the_inbox, placed, placed_resources, press_sheet, private_home, repl_session,
    seeded_session, show_preset, until,
};

/// A notebook kept by the notes app, published as the session publishes a workspace's records,
/// and one note in it titled [`NOTE_TITLE`], made through the app's own actions.
fn notebook() -> (RecordService, String) {
    let (service, notes) = common::notebook(&[NOTE_TITLE]);
    (service, notes.into_iter().next().expect("a note"))
}

/// The headless session with the shell as the screen, the notebook published, its agent door
/// at `root`, admitting the programs `admit_repl` to act as the person.
fn session(
    root: &std::path::Path,
    questions: &Questions,
    admit_repl: Vec<std::path::PathBuf>,
) -> (Session, String) {
    let (notebook, note) = notebook();
    let session = repl_session(root, questions, admit_repl, notebook);
    (session, note)
}

/// An action read as a value, rather than called: what it says of how it is
/// called.
fn reading_an_action_says_how_it_is_called(session: &mut Session, repl: &mut Repl) {
    let (named, failed) = repl.call(session, "tilo", &json!({"program": "session.overview"}));
    assert!(failed, "{named}");
    assert!(
        named.contains("call it: `session.overview(open: Option<Bool> = .none)`"),
        "{named}"
    );
}

/// Lists the apps, places the note through a `tilo` program, sees it on the shell's screen as
/// the person's step, and takes it back with `undo`.
fn the_repl_works_as_the_person(session: &mut Session, repl: &mut Repl, note: &str) {
    let tools = repl.opened(session);
    assert!(
        tools.iter().any(|tool| tool == "tilo"),
        "the door runs programs: {tools:?}"
    );

    // `ls`: the apps, the session's own resources among them.
    let (apps, failed) = repl.call(session, "tilo_ls", &json!({}));
    assert!(!failed, "{apps}");
    assert!(
        apps.lines().any(|line| line.starts_with("session")),
        "the session's own resources are listed: {apps}"
    );
    let (desk, failed) = repl.call(session, "tilo_ls", &json!({"path": "session"}));
    assert!(!failed, "{desk}");
    assert!(
        desk.contains("shell:arrangement") || desk.contains("arrangement"),
        "the arrangement is among the session's resources: {desk}"
    );

    // `ls` lists each action as a program calls it, what it may go without included. The
    // chrome by its key: a window's control reads under the window's name, and the desk's
    // window is another `Desktop`.
    let (chrome, failed) = repl.call(
        session,
        "tilo_ls",
        &json!({"path": "session/desktop:chrome"}),
    );
    assert!(!failed, "{chrome}");
    assert!(
        chrome
            .lines()
            .any(|line| line.starts_with("session.overview(open: Option<Bool> = .none)  ")),
        "the overview's action, as it is typed: {chrome}"
    );
    let (arranging, failed) =
        repl.call(session, "tilo_ls", &json!({"path": "session/arrangement"}));
    assert!(!failed, "{arranging}");
    let place = arranging
        .lines()
        .find(|line| line.starts_with("session.place("))
        .unwrap_or_else(|| panic!("`place`, as it is typed: {arranging}"));
    assert!(
        place.contains("resource: host.Resource") && place.contains("x: Option<Int> = .none"),
        "it takes the resource as a value, and may go without the rest: {place}"
    );
    // A resource that is a value goes by a reference, as an answer's do.
    // The notebook by its key: the launchpad's Notes card is another resource of that name.
    let book = note
        .split('/')
        .next()
        .and_then(|book| book.rsplit(':').next())
        .unwrap();
    let (notebook, failed) = repl.call(
        session,
        "tilo_ls",
        &json!({"path": format!("session/{book}")}),
    );
    assert!(!failed, "{notebook}");
    // The note's own line: a reference, then the name it goes by. The
    // listing's first line is the path as it resolves, which names the
    // notebook by the title it shares with its note.
    let reference = notebook
        .lines()
        .find_map(|line| {
            let mut words = line.split_whitespace();
            let reference = words.next()?;
            (words.next() == Some(NOTE_TITLE)
                && reference.strip_prefix('n').is_some_and(|number| {
                    !number.is_empty() && number.chars().all(|digit| digit.is_ascii_digit())
                }))
            .then(|| reference.to_owned())
        })
        .unwrap_or_else(|| panic!("the note goes by a reference: {notebook}"));
    reading_an_action_says_how_it_is_called(session, repl);

    // The framed canvas, as the person shows it.
    show_preset(session, "Frames");
    until(session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });

    // A `tilo` program places the note on the arrangement, as the person.
    let (placed_it, failed) = repl.call(
        session,
        "tilo",
        &json!({"program": format!("session.place(resource: {reference}, x: 80, y: 60)")}),
    );
    assert!(!failed, "the program ran: {placed_it}");
    assert!(placed_it.contains("step 1"), "{placed_it}");
    until(session, "the note's title on the canvas", |session| {
        on_screen(session, NOTE_TITLE).is_some()
    });
    assert_eq!(placed(session, note), 1, "one place shows the note");
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the arrangement's history");
    assert_eq!(
        (who.as_str(), verb.as_str()),
        (PERSON, "place"),
        "the person's step, not an agent's"
    );

    // `undo` takes the step back.
    let (undone, failed) = repl.call(session, "tilo", &json!({"program": "undo"}));
    assert!(!failed, "{undone}");
    until(session, "the note off the canvas", |session| {
        placed(session, note) == 0 && on_screen(session, NOTE_TITLE).is_none()
    });
}

/// The person admits the REPL on the sheet the door's question opens.
#[test]
fn the_person_admits_the_repl_on_the_sheet_and_it_works_as_them() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let (mut session, note) = session(root.path(), &questions, Vec::new());
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });

    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    until(&mut session, "the question", |_| {
        questions.pending().len() == 1
    });
    let shown = session
        .portal()
        .and_then(tilo_sheets::portal::PortalSeat::dialog)
        .expect("the question stands on the portal's sheet");
    assert_eq!(shown.title, "Act as you in “Desk”", "what it asks");
    assert!(
        shown.heading.starts_with("tilolang repl ("),
        "who asks: {}",
        shown.heading
    );
    press_sheet(&mut session, PortalEffect::SELECT.id, &json!({"index": 0}));
    press_sheet(&mut session, PortalEffect::CONFIRM.id, &json!({}));

    the_repl_works_as_the_person(&mut session, &mut repl, &note);
}

/// The person named the REPL when they started the session: it is admitted without a question.
#[test]
fn a_repl_named_when_the_session_starts_is_admitted_without_asking() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    // The REPL here is this test's own program: what the kernel names the client by.
    let program = std::env::current_exe().expect("this test's program");
    let (mut session, note) = session(root.path(), &questions, vec![program]);
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });

    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    the_repl_works_as_the_person(&mut session, &mut repl, &note);
    assert!(questions.pending().is_empty(), "nobody was asked");
}

/// Placing what a text finds, twice, through the door: each place is the person's step, and the
/// session keeps answering, the connection that asked and a new one alike.
#[test]
fn placing_what_text_finds_twice_keeps_the_session_answering() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let (mut session, note) = session(root.path(), &questions, vec![program]);
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    for placed_before in 0..2 {
        let (answer, failed) = repl.call(
            &mut session,
            "tilo",
            &json!({"program": format!(
                "session.place_found(text: \"{NOTE_TITLE}\", view: .none, x: .some({}), y: .some(60))",
                80 + placed_before * 400
            )}),
        );
        assert!(!failed, "the program ran: {answer}");
        until(&mut session, "the note placed once more", |session| {
            placed(session, &note) == usize::try_from(placed_before).expect("a count") + 1
        });
    }

    // The connection that asked is still answered.
    let (apps, failed) = repl.call(&mut session, "tilo_ls", &json!({}));
    assert!(!failed, "{apps}");
    // And so is a new one.
    let mut second = Repl::connect(&root.path().join("agent.sock"));
    let tools = second.opened(&mut session);
    assert!(tools.iter().any(|tool| tool == "tilo"), "{tools:?}");
}

/// What the session's services publish (the seeded apps' records) is listed through the door
/// past the first page of the session's rows, page by page as its cursor reads on, and a note
/// is found by its title.
#[test]
fn a_record_the_sessions_services_publish_is_found_through_the_door() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let apps = common::seeded_apps();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(apps)],
            questions: Some(questions),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    // Every page of the session's listing, as its cursors read on.
    let mut listed = String::new();
    let mut cursor = String::new();
    for _ in 0..20 {
        let (page, failed) = repl.call(
            &mut session,
            "tilo_ls",
            &json!({"path": "session", "cursor": cursor}),
        );
        assert!(!failed, "{page}");
        listed.push_str(&page);
        listed.push('\n');
        let Some(next) = page
            .rsplit_once("cursor \"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(next, _)| next.to_owned())
        else {
            break;
        };
        cursor = next;
    }
    // The session's records are listed, each row naming its type the way a program types it
    // (`tasks.Tasks`, `chat.Conversations`): the name this listing is for, and the name a path
    // walks back by. Never the kind the app publishes it under (`tl.<app>.<Type>`), which no
    // program writes.
    let row_of = |listed: &str, form: &str| -> String {
        listed
            .lines()
            .find(|line| line.split("  ").any(|column| column.trim() == form))
            .unwrap_or_else(|| panic!("the listing names {form}: {listed}"))
            .to_owned()
    };
    // A row's columns are two spaces apart: the reference a program passes it by, the name,
    // then the type.
    let name_of = |row: &str| -> String {
        row.split("  ")
            .map(str::trim)
            .nth(1)
            .map(|name| name.trim_end_matches('/').to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| panic!("the row names its record: {row}"))
    };
    // No record is named by the kind its app publishes it under (`tl.<app>.<Type>`): a row's
    // type column is a program form. An app's own row names the app itself (`Chat
    // tl.so.tilo.lang.chat`), an id whose last part is no capitalised type.
    assert!(
        !listed.lines().any(|line| {
            line.split("  ").map(str::trim).any(|column| {
                column
                    .strip_prefix("tl.")
                    .and_then(|rest| rest.rsplit_once('.'))
                    .is_some_and(|(_, ty)| ty.starts_with(|c: char| c.is_ascii_uppercase()))
            })
        }),
        "no record is named by its published kind: {listed}"
    );
    // The app's own record, at the session's top level, named by its type as a program types it.
    let tasks = name_of(&row_of(&listed, "tasks.Tasks"));
    // A record of the app's, a level down: listed the same way, and its name walks back to it.
    let (inside, failed) = repl.call(
        &mut session,
        "tilo_ls",
        &json!({"path": format!("session/{tasks}")}),
    );
    assert!(!failed, "tasks at session/{tasks}: {inside}");
    let task = name_of(&row_of(&inside, "tasks.Task"));
    let task_at_path = format!("session/{tasks}/{task}");
    let (task_at, failed) = repl.call(&mut session, "tilo_ls", &json!({"path": task_at_path}));
    assert!(!failed, "tasks.Task at {task_at_path}: {task_at}");
    assert!(
        task_at
            .lines()
            .next()
            .is_some_and(|header| header.ends_with("(tasks.Task)")),
        "the row's own name reads back as its type: {task_at}"
    );
    // The type name itself reads the records of that kind: a note, a channel and a mailbox,
    // each under its app's record, resolve by what the listing prints.
    for (form, under) in [
        ("notes.Note", "notes.Notebook"),
        ("chat.TextChannel", "chat.Conversations"),
        ("mail.Mailbox", "mail.Mail"),
    ] {
        let root = name_of(&row_of(&listed, under));
        let (record, failed) = repl.call(
            &mut session,
            "tilo_ls",
            &json!({"path": format!("session/{root}/{form}")}),
        );
        assert!(
            !failed || !record.contains(&format!("no {form} in")),
            "a type name read off a row resolves in a path: {record}"
        );
    }

    let (found, failed) = repl.call(&mut session, "tilo_find", &json!({"query": NOTE_TITLE}));
    assert!(!failed, "{found}");
    assert!(
        found.lines().any(|line| line.contains(NOTE_TITLE)),
        "the note is found by its title: {found}"
    );
}

/// The resources `session` publishes, at any depth, as its action model holds them.
fn everything(session: &Session) -> Vec<tilo_actions::ResourceSpec> {
    fn walk(resources: &[tilo_actions::ResourceSpec], found: &mut Vec<tilo_actions::ResourceSpec>) {
        for resource in resources {
            found.push(resource.clone());
            walk(&resource.children, found);
        }
    }
    let mut found = Vec::new();
    walk(&session.resources(), &mut found);
    found
}

/// The resources `session` publishes under `label`, at any depth, as its action model holds them.
fn labelled(session: &Session, label: &str) -> Vec<tilo_actions::ResourceSpec> {
    everything(session)
        .into_iter()
        .filter(|resource| resource.label == label)
        .collect()
}

/// A seeded app's catalogue is the app's words on the desk: with the interface in German, the
/// session labels the notes app's notebook and its actions in German, and its search matches the
/// German word for what it holds.
#[test]
fn a_seeded_apps_catalogue_gives_a_german_session_its_words() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    home.env()
        .set("LC_ALL", "de_DE.UTF-8")
        .set("LANG", "de_DE.UTF-8")
        .set("LANGUAGE", "de_DE");
    // The apps come from the run's store, which the session reads as a device reads its own.
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the notebook's German label", |session| {
        !labelled(session, "Notizen").is_empty()
    });
    let nouns: Vec<tilo_actions::Noun> = everything(&session)
        .into_iter()
        .filter_map(|resource| resource.resource_type.map(|kind| kind.noun))
        .collect();
    for noun in [
        tilo_actions::Noun::new("Notizbuch", "Notizbücher"),
        tilo_actions::Noun::new("Notiz", "Notizen"),
    ] {
        assert!(
            nouns.contains(&noun),
            "the notebook's nouns are the catalogue's, in the reader's language: {nouns:?}"
        );
    }
    let offered: Vec<String> = everything(&session)
        .into_iter()
        .flat_map(|resource| {
            resource
                .actions
                .into_iter()
                .map(|action| action.label.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        offered.iter().any(|label| label == "Neue Notiz"),
        "the notes app's actions say what they do in German: {offered:?}"
    );

    // Search matches the reader's words: the German plural finds the notebook on the desk, and
    // the English one finds nothing, because the desk's words are German.
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": "session.place_found(text: \"notizbücher\", view: .none, x: .some(80), y: .some(60))"}),
    );
    assert!(!failed, "the program ran: {answer}");
    until(
        &mut session,
        "the notebook placed by its German name",
        |session| {
            common::places(session)
                .iter()
                .any(|place| place.record.contains(NOTES))
        },
    );
    let placed_by_german = noted(&session);
    // The English word finds nothing, because the desk's words are German: it
    // is a sentence to the agent rather than a found resource, so it places no
    // second notebook — it asks (shell.md §8.1, "Asking from the canvas").
    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": "session.place_found(text: \"notebooks\", view: .none, x: .some(80), y: .some(60))"}),
    );
    assert!(!failed, "the program ran: {answer}");
    assert_eq!(
        noted(&session),
        placed_by_german,
        "the English word finds no notebook on a German desk"
    );
    until(&mut session, "the ask conversation", |session| {
        common::places(session)
            .iter()
            .any(|place| place.record.contains("chat.workspace/channels/"))
    });
}

/// How many of the seeded notes app's resources stand on the arrangement.
fn noted(session: &Session) -> usize {
    session
        .arrangement()
        .borrow()
        .reading()
        .places
        .iter()
        .filter(|place| place.record.contains(NOTES))
        .count()
}

/// The overview shown from the REPL is the shell's own on its screen, and showing it is no step:
/// `undo` after it takes back the place before it. A step whose undo the session refuses (an
/// outside action, which no history holds) stays, and `undo` goes on to the step before it.
#[test]
fn showing_the_overview_is_no_step_and_a_refused_undo_holds_no_other_step() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let (mut session, note) = session(root.path(), &questions, vec![program]);
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
    let mut run = |session: &mut Session, program: &str| {
        let (answer, failed) = repl.call(session, "tilo", &json!({"program": program}));
        (answer, failed)
    };

    let (answer, failed) = run(
        &mut session,
        &format!(
            "session.place_found(text: \"{NOTE_TITLE}\", view: .none, x: .some(80), y: .some(60))"
        ),
    );
    assert!(!failed && answer.contains("step 1"), "{answer}");
    until(&mut session, "the note placed", |session| {
        placed(session, &note) == 1
    });

    let (answer, failed) = run(&mut session, "session.overview(open: .some(true))");
    assert!(!failed, "the overview opens: {answer}");
    assert!(
        !answer.contains("step"),
        "showing the overview is no step: {answer}"
    );
    until(&mut session, "the shell's overview", |session| {
        on_screen(session, "Overview").is_some()
    });

    let (answer, failed) = run(&mut session, "undo");
    assert!(!failed && answer.contains("undone: step 1"), "{answer}");
    until(&mut session, "the note taken off", |session| {
        placed(session, &note) == 0
    });

    let (answer, failed) = run(&mut session, "session.overview(open: .some(false))");
    assert!(!failed, "the overview closes: {answer}");
    until(&mut session, "the overview closed", |session| {
        on_screen(session, "Overview").is_none()
    });

    // A place, then an outside action no history takes back.
    let (answer, failed) = run(
        &mut session,
        &format!(
            "session.place_found(text: \"{NOTE_TITLE}\", view: .none, x: .some(80), y: .some(60))"
        ),
    );
    assert!(!failed && answer.contains("step 2"), "{answer}");
    until(&mut session, "the note placed again", |session| {
        placed(session, &note) == 1
    });
    let (answer, failed) = run(&mut session, "session.notifications(open: false)");
    assert!(!failed && answer.contains("step 3"), "{answer}");
    let (answer, failed) = run(&mut session, "undo");
    assert!(failed, "the outside step is not taken back: {answer}");
    assert!(answer.contains("undo` takes back step 2 next"), "{answer}");
    let (answer, failed) = run(&mut session, "undo");
    assert!(!failed && answer.contains("undone: step 2"), "{answer}");
    until(&mut session, "the note taken off again", |session| {
        placed(session, &note) == 0
    });
}

/// What the seeded apps keep, placed through the door, is drawn on the canvas by its own app's
/// view, its content and not its title alone: a note found by its title shows its text. And a
/// text finds the resource it names even where many actions name it too: "Inbox" places the
/// inbox.
#[test]
fn a_placed_record_is_drawn_through_its_own_apps_view() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let apps = common::seeded_apps();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(apps)],
            questions: Some(questions),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": format!(
            "session.place_found(text: \"{NOTE_TITLE}\", view: .none, x: .some(80), y: .some(60))"
        )}),
    );
    assert!(!failed, "the program ran: {answer}");
    until(&mut session, "the note's text on the canvas", |session| {
        session.shell_says("Oat milk, eggs, sourdough")
    });

    // A text naming a resource that many actions name too finds the resource: "Inbox" is the
    // mailbox, under every conversation's "Move to inbox".
    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": "session.place_found(text: \"Inbox\", view: .none, x: .some(640), y: .some(60))"}),
    );
    assert!(!failed, "the program ran: {answer}");
    let mailbox = |session: &Session| {
        let resources = session.resources();
        session
            .arrangement()
            .borrow()
            .reading()
            .places
            .iter()
            .filter_map(|place| {
                tilo_actions::door::locate(
                    &resources,
                    &tilo_actions::ActionAddress::new(&place.record, "open"),
                )
                .map(|(resource, _)| (resource.kind.clone(), resource.label.clone()))
            })
            .find(|(kind, _)| kind == "tl.so.tilo.lang.mail.Mailbox")
    };
    until(&mut session, "the inbox placed", |session| {
        mailbox(session).is_some()
    });
    assert_eq!(
        mailbox(&session).map(|(_, label)| label).as_deref(),
        Some("Inbox"),
        "the mailbox placed is the inbox"
    );
}

/// A text naming what something is, not what it is called, places the one that kind means:
/// "my mails" is mail's Inbox, its mailboxes' declared primary, though no word of it says
/// "Inbox" and "my" names nothing.
#[test]
fn my_mails_on_the_canvas_places_mails_root() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let apps = common::seeded_apps();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(apps)],
            questions: Some(questions),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": "session.place_found(text: \"my mails\", view: .none, x: .some(80), y: .some(60))"}),
    );
    assert!(!failed, "the program ran: {answer}");
    let placed = |session: &Session| {
        let resources = session.resources();
        session
            .arrangement()
            .borrow()
            .reading()
            .places
            .iter()
            .filter_map(|place| {
                tilo_actions::door::locate(
                    &resources,
                    &tilo_actions::ActionAddress::new(&place.record, "open"),
                )
                .map(|(resource, _)| (resource.kind.clone(), resource.label.clone()))
            })
            .collect::<Vec<_>>()
    };
    until(&mut session, "what \"my mails\" names placed", |session| {
        !placed(session).is_empty()
    });
    let found = placed(&session);
    assert_eq!(
        found
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        ["tl.so.tilo.lang.mail.Mail"],
        "\"my mails\" places Mail's root, which Mail draws as its inbox: {found:?}"
    );
}

/// How many pixels of `shot` in the logical rectangle `(x, y, w, h)` (scale 1) differ from
/// the colour most of it is: what is drawn there over its background, in either theme.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a rectangle on a 1280x800 screen"
)]
fn inked(shot: &image::RgbaImage, (x, y, w, h): (f64, f64, f64, f64)) -> usize {
    let mut counts: std::collections::HashMap<[u8; 4], usize> = std::collections::HashMap::new();
    let mut pixels = Vec::new();
    for py in (y.max(0.0) as u32)..((y + h) as u32).min(shot.height()) {
        for px in (x.max(0.0) as u32)..((x + w) as u32).min(shot.width()) {
            let pixel = shot.get_pixel(px, py).0;
            let count = counts.entry(pixel).or_default();
            *count = count.saturating_add(1);
            pixels.push(pixel);
        }
    }
    let Some((ground, _)) = counts.iter().max_by_key(|(_, count)| **count) else {
        return 0;
    };
    let far = |a: [u8; 4], b: [u8; 4]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| u32::from(x.abs_diff(y)))
            .sum::<u32>()
            > 96
    };
    pixels.iter().filter(|pixel| far(**pixel, *ground)).count()
}

/// The seeded inbox placed through the door stands on the canvas as the mail app's own view
/// of it: a seeded conversation's subject is laid out in it, and drawn where it is laid out in
/// the snapshot `tilo-control` takes. Mail's package is loaded because the place shows it.
#[test]
fn the_seeded_inbox_placed_through_the_door_is_drawn_as_mails_inbox() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    assert!(
        session.shell_apps().is_empty(),
        "no app's package is the shell's before a place shows it: {:?}",
        session.shell_apps()
    );
    place_the_inbox(&mut session, root.path());
    assert_eq!(session.shell_apps(), vec![MAIL.to_owned()]);
    // As it stands once the frames after it settled, not only on the first.
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let subject = session
        .shell_text_bounds(SUBJECT)
        .expect("the conversation's row is laid out");
    assert!(
        subject.2 > 0.0 && subject.3 > 0.0,
        "the conversation's row has room: {subject:?}"
    );
    let png = root.path().join("shot.png");
    let answer = control(
        &mut session,
        &root.path().join("test-control.sock"),
        &json!({"op": "snapshot", "path": png.to_str().unwrap()}),
    );
    assert_eq!(
        answer,
        json!({"ok": {"frame": 0, "locked": false}}),
        "the snapshot is taken, of a session that is not locked"
    );
    let shot = image::open(&png).expect("a PNG").into_rgba8();
    assert!(
        inked(&shot, subject) > 20,
        "the conversation's row is drawn where it is laid out, {subject:?}"
    );
}

/// An app's package is the shell's only while a place shows one of its resources: closing the
/// inbox's place unloads mail's package.
#[test]
fn an_app_no_place_shows_is_unloaded() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    place_the_inbox(&mut session, root.path());
    assert_eq!(session.shell_apps(), vec![MAIL.to_owned()]);
    let places: Vec<u64> = common::places(&session)
        .iter()
        .map(|place| common::number(&place.key))
        .collect();
    for place in places {
        session
            .invoke(
                &tilo_session::arrangement::Item::Place.resource(&place.to_string()),
                "close",
                &serde_json::Map::new(),
            )
            .expect("the place closes");
    }
    until(&mut session, "mail's package unloaded", |session| {
        session.shell_apps().is_empty()
    });
    assert!(
        !session.shell_says(SUBJECT),
        "nothing shows the conversation any more"
    );
}

/// The resource of the seeded mail of the type `ty` whose label is `label`.
fn seeded(session: &Session, ty: &str, label: &str) -> tilo_actions::ResourceSpec {
    fn find(
        resources: &[tilo_actions::ResourceSpec],
        kind: &str,
        label: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.kind == kind && resource.label == label {
                Some(resource.clone())
            } else {
                find(&resource.children, kind, label)
            }
        })
    }
    find(&session.resources(), &format!("tl.{MAIL}.{ty}"), label)
        .unwrap_or_else(|| panic!("the seeded {ty} {label:?}"))
}

/// A change to one conversation the inbox shows reads that conversation's reading again, and
/// no other: not the mailbox, the mail's root, its account or the other conversations.
#[test]
fn a_change_to_one_thread_rereads_only_that_threads_reading() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    place_the_inbox(&mut session, root.path());
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let before = session.shell_reads();
    let thread = seeded(&session, "Thread", "Dinner on Friday?");
    assert!(
        before.contains_key(thread.id.as_str()),
        "the inbox read the conversation: {before:?}"
    );
    let flag = thread
        .actions
        .iter()
        .find(|action| action.label == "Flag")
        .expect("a conversation can be flagged")
        .id
        .clone();
    session
        .invoke(thread.id.as_str(), &flag, &Map::new())
        .expect("the conversation is flagged");
    until(&mut session, "the conversation read again", |session| {
        session.shell_reads().get(thread.id.as_str()) != before.get(thread.id.as_str())
    });
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let after = session.shell_reads();
    let read_again: Vec<&String> = after
        .iter()
        .filter(|(id, count)| before.get(*id) != Some(*count))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        read_again,
        vec![thread.id.as_str()],
        "only the conversation that changed is read again"
    );
}

/// The start of the minute the test runs in, as the session's clock reads it: what a test
/// watching a session at rest holds the wall clock at, so the bar's time cannot turn over
/// inside the window it watches.
fn this_minute() -> ui::time::SystemTime {
    ui::time::wall()
        .checked_sub(Duration::from_secs(
            u64::try_from(ui::clock::local_now().timestamp().rem_euclid(60))
                .expect("a second of the minute"),
        ))
        .expect("the minute the test runs in")
}

/// A placed inbox settles: once its conversations are drawn, a session left alone runs the
/// shell's views no more and draws no frame. A list's rows asked of the shell's own runtime go
/// out on the turn they are asked, rather than the list taking frames until they would.
#[test]
fn a_placed_inbox_settles_and_draws_nothing_while_idle() {
    let _settled = ui::motion::testing::settled();
    // The bar draws the time, and a minute turning over is a change, not idle work: the test
    // holds the clock at the minute it runs in, as a capture does, so what it watches at rest
    // is the shell and nothing else.
    ui::time::testing::with_wall(this_minute(), || {
        let root = private_home();
        let mut session = seeded_session(root.path());
        place_the_inbox(&mut session, root.path());
        for _ in 0..200 {
            session.step(Duration::from_millis(1)).expect("step");
        }
        let settled = session.shell_work().expect("the shell runs");
        for _ in 0..200 {
            session.step(Duration::from_millis(5)).expect("step");
        }
        let idle = session.shell_work().expect("the shell runs");
        assert_eq!(
            (idle.turns, idle.drawn),
            (settled.turns, settled.drawn),
            "an idle session runs and draws nothing"
        );
    });
}

/// The inbox's conversations come in together: the session reads what one run of the views
/// read in one turn, so a placed inbox fills in as many turns as its views reach records deep
/// (the mailbox, the mail's root, its account, the conversations), however many conversations
/// it lists.
#[test]
fn a_placed_inbox_fills_in_a_turn_per_level_not_per_conversation() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let before = session.shell_reading_rounds();
    place_the_inbox(&mut session, root.path());
    for _ in 0..200 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let threads = session
        .shell_reads()
        .keys()
        .filter(|id| id.contains(&format!("{MAIL}:")) && seeded_thread(&session, id))
        .count();
    let rounds = session.shell_reading_rounds() - before;
    assert!(
        threads > 4,
        "the inbox read its conversations: {:?}",
        session.shell_reads()
    );
    assert!(
        rounds <= 5,
        "{threads} conversations came in {rounds} turns, not one each"
    );
}

/// Whether `id` is a seeded conversation of the mail app.
fn seeded_thread(session: &Session, id: &str) -> bool {
    fn find(resources: &[tilo_actions::ResourceSpec], id: &str, kind: &str) -> bool {
        resources
            .iter()
            .any(|r| (r.id.as_str() == id && r.kind == kind) || find(&r.children, id, kind))
    }
    find(&session.resources(), id, &format!("tl.{MAIL}.Thread"))
}

/// A service publishing the seeded apps' records, but withholding the readings of every
/// resource of the kind `withheld` (the app that publishes it never does, or the session's
/// watch has not seen it): what a place whose data never arrives meets.
struct Withholding {
    inner: RecordService,
    withheld: &'static str,
    /// The ids of the resources of that kind, found once.
    ids: std::cell::RefCell<Option<std::collections::BTreeSet<String>>>,
}

impl Withholding {
    fn withheld_ids(&self) -> std::cell::Ref<'_, std::collections::BTreeSet<String>> {
        {
            let mut ids = self.ids.borrow_mut();
            if ids.is_none() {
                fn walk(
                    resources: &[tilo_actions::ResourceSpec],
                    kind: &str,
                    out: &mut std::collections::BTreeSet<String>,
                ) {
                    for resource in resources {
                        if resource.kind == kind {
                            out.insert(resource.id.as_str().to_owned());
                        }
                        walk(&resource.children, kind, out);
                    }
                }
                let mut found = std::collections::BTreeSet::new();
                walk(&self.inner.resources(), self.withheld, &mut found);
                *ids = Some(found);
            }
        }
        std::cell::Ref::map(self.ids.borrow(), |ids| {
            ids.as_ref().expect("the kind's ids were found")
        })
    }
}

impl tilo_service::Service for Withholding {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.inner.resources()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if self.withheld_ids().contains(resource) {
            None
        } else {
            self.inner.read(resource)
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, tilo_actions::DoorError> {
        self.inner.invoke(resource, action, arguments)
    }

    fn settle(&mut self) {
        self.inner.settle();
    }

    fn moved(&mut self) -> Option<Vec<String>> {
        self.inner.moved()
    }

    fn module(&self, app: &str) -> Option<std::sync::Arc<[u8]>> {
        self.inner.module(app)
    }
}

/// A place whose data never arrives does not freeze the screen. A task is placed while the
/// session withholds every task's reading: its window has nothing to draw and refuses nothing,
/// and the rest of the shell keeps working — a note placed after it arrives and shows on
/// screen, and the overview opens and closes. One place's missing reading never fails the whole
/// screen's view.
#[test]
fn a_place_whose_reading_never_arrives_leaves_the_rest_of_the_screen_working() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let apps = common::seeded_apps();
    let program = std::env::current_exe().expect("this test's program");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(Withholding {
                inner: apps,
                // Every task's reading never comes: the window of the one placed has no
                // record to draw.
                withheld: "tl.so.tilo.lang.tasks.Task",
                ids: std::cell::RefCell::new(None),
            })],
            questions: Some(Questions::new()),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": "session.place_found(text: \"Pack the books\", view: .none, x: .some(80), y: .some(60))"}),
    );
    assert!(!failed, "the task is placed: {answer}");
    // The place stands with nothing to draw, and the shell refuses nothing: the reading it is
    // missing is not the screen's fault.
    until(&mut session, "the place", |session| {
        on_screen(session, "Pack the books — Tasks").is_some()
    });
    assert!(
        session.shell_refusals().is_empty(),
        "the place without its reading is no refusal: {:?}",
        session.shell_refusals()
    );

    // The rest of the shell works: a note placed after it shows on the canvas.
    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &json!({"program": "session.place_found(text: \"Groceries\", view: .none, x: .some(600), y: .some(60))"}),
    );
    assert!(!failed, "the note is placed: {answer}");
    until(&mut session, "the note's title on the canvas", |session| {
        session.shell_says(NOTE_TITLE)
    });

    // And the overview opens and closes, as it does on a screen all of whose places have
    // arrived.
    then_the_overview_opens_and_closes(&mut session, &mut repl);
}

/// The overview opens over the desk and closes again, through the door as an agent asks it.
fn then_the_overview_opens_and_closes(session: &mut Session, repl: &mut Repl) {
    let (answer, failed) = repl.call(
        session,
        "tilo",
        &json!({"program": "session.overview(open: .some(true))"}),
    );
    assert!(!failed, "the overview opens: {answer}");
    until(session, "the shell's overview", |session| {
        on_screen(session, "Overview").is_some()
    });
    let (answer, failed) = repl.call(
        session,
        "tilo",
        &json!({"program": "session.overview(open: .some(false))"}),
    );
    assert!(!failed, "the overview closes: {answer}");
    until(session, "the overview closed", |session| {
        on_screen(session, "Overview").is_none()
    });
}

/// The REPL's `find` is the agent's `tilo_find`, and answers by the one search the palette and
/// text typed on the canvas run: over the same sources, with the same ranking. A text naming
/// what something is ("my mails"), what it is called, or both, finds there what
/// `session.place_found` places — never a second ranking that knows a note's title but not
/// Mail's inbox.
#[test]
fn the_repl_finds_what_place_found_places() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let questions = Questions::new();
    let program = std::env::current_exe().expect("this test's program");
    let apps = common::seeded_apps();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(apps)],
            questions: Some(questions),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

    // Each text, and a word the resource it names must carry (its kind, or
    // the app it belongs to).
    for (text, wanted) in [
        ("my mails", "mail.Mail"),
        ("notes", "notes."),
        ("Groceries", "notes.Note"),
        ("mail archive", "mail.Mailbox"),
    ] {
        // What the canvas places for `text`: the one search's first result,
        // typed on the canvas as a person would.
        let (answer, failed) = repl.call(
            &mut session,
            "tilo",
            &json!({"program": format!(
                "session.place_found(text: {text:?}, view: .none, x: .some(80), y: .some(60))"
            )}),
        );
        assert!(!failed, "{text:?} is placed: {answer}");
        until(&mut session, &format!("{text:?} placed"), |session| {
            !placed_resources(session).is_empty()
        });
        let shown = placed_resources(&session);
        assert_eq!(shown.len(), 1, "{text:?} places one thing: {shown:?}");
        let (placed_kind, _) = shown[0].clone();
        assert!(
            placed_kind.contains(wanted),
            "{text:?} places a {placed_kind}, not a {wanted}: {shown:?}"
        );

        // And `find` names the same rows the one search ranks, each by a
        // path `tilo_ls` reads: the row of the place the desk just made leads
        // where the search puts it, and a row of the kind the search sought
        // stands among them. `find` walks the containers it can read, so a
        // record placed out of a notebook is reached through its notebook.
        let (found, failed) = repl.call(&mut session, "tilo_find", &json!({"query": text}));
        assert!(!failed, "{text:?} answers: {found}");
        let rows: Vec<&str> = found
            .lines()
            .filter(|line| !line.starts_with('…'))
            .collect();
        let first = rows
            .first()
            .unwrap_or_else(|| panic!("{text:?} finds nothing: {found}"));
        let path = first
            .split("  ")
            .next()
            .unwrap_or_default()
            .split('#')
            .next()
            .unwrap_or_default();
        let (parent, row) = path.rsplit_once('/').unwrap_or_default();
        assert!(
            !parent.is_empty() && !row.is_empty(),
            "{text:?}: find leads with a row of a directory: {first}"
        );
        let (listed, failed) = repl.call(&mut session, "tilo_ls", &json!({"path": parent}));
        assert!(!failed, "{text:?}: {parent} lists: {listed}");
        assert!(
            listed.contains(row),
            "{text:?}: find leads with {first}, and {parent} lists no row {row}: {listed}"
        );
        assert!(
            rows.iter().any(|row| row
                .split("  ")
                .nth(1)
                .is_some_and(|label| label.contains(wanted))),
            "{text:?}: find names a row of a {wanted}, {shown:?}: {found}"
        );

        // Each `place_found` is a step of the person's: take it back, so the
        // next text places on its own.
        let (undone, failed) = repl.call(&mut session, "tilo", &json!({"program": "undo"}));
        assert!(!failed, "{undone}");
        until(&mut session, "the canvas cleared", |session| {
            placed_resources(session).is_empty()
        });
    }
}
