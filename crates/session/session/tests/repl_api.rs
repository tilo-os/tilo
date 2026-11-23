//! The person's REPL at the session's API (`tilolang repl --session`), on the headless session with
//! the tilolang shell as the whole screen: the API lists what the session publishes and runs the
//! REPL's programs as the person. A note placed on the layout by a program stands on the shell's
//! screen, read from what the shell lays out for assistive technology, and is the person's step,
//! which `undo` takes back.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_service::Started;
use tilo_session::{Options, Session};

use crate::common::{
    Api, CANVAS, MAIL, NOTE_TITLE, NOTES, PERSON, SUBJECT, api_options, control, on_screen,
    place_the_inbox, placed, placed_resources, private_home, seeded_session, show_preset, until,
};

/// The headless session with the shell as the screen and the seeded apps kept in the workspace,
/// a note titled [`NOTE_TITLE`] among the notes app's; its sockets at `root` ([`api_options`]).
/// Answers the session and that note.
fn session(root: &std::path::Path) -> (Session, String) {
    let session = kept_seeded(root);
    let note = note_titled(&session, NOTE_TITLE);
    (session, note)
}

/// The headless session with the seeded apps, once what they keep is in the workspace: a program
/// finds and places what the workspace keeps, and a record placed before the seed moved there
/// names one nobody shows.
fn kept_seeded(root: &std::path::Path) -> Session {
    let mut session = crate::common::headless(Options {
        services: crate::common::seeded_apps(),
        ..api_options(root)
    });
    crate::common::kept_in_workspace(&mut session);
    session
}

/// The id of the note the session publishes under `title`.
fn note_titled(session: &Session, title: &str) -> String {
    fn find(resources: &[tilo_actions::ResourceSpec], title: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            (resource.kind.ends_with(".Note") && resource.label == title)
                .then(|| resource.id.as_str().to_owned())
                .or_else(|| find(&resource.children, title))
        })
    }
    find(&session.resources(), title).unwrap_or_else(|| panic!("a note titled {title}"))
}

/// An action read as a value, rather than called: what it says of how it is called.
fn reading_an_action_says_how_it_is_called(session: &mut Session, repl: &mut Api) {
    let (named, failed) = repl.run(session, "session.overview");
    assert!(failed, "{named}");
    assert!(
        named.contains("call it: `session.overview(open: Option<Bool> = .none)`"),
        "{named}"
    );
}

/// Lists the apps, places the note through a program, sees it on the shell's screen as the
/// person's step, and takes it back with `undo` .
fn the_repl_works_as_the_person(session: &mut Session, repl: &mut Api, note: &str) {
    // The apps, the session's own resources among them.
    let apps = repl.ls(session, "");
    assert!(
        Api::rows(&apps).any(|row| row["name"] == "session"),
        "the session's own resources are listed: {apps}"
    );
    // The layout is found by its resource id, never by the label the person reads.
    let layout_key = tilo_actions::remote::session_actions::LAYOUT_RESOURCE
        .rsplit(':')
        .next()
        .expect("a resource key");

    // Each action is listed with what it takes, and what it may go without. The chrome by its key:
    // a window's control reads under the window's name, and the shell's window is another
    // `Desktop` .
    let chrome = repl.ls(session, "session/desktop:chrome");
    let overview = Api::action(&chrome, "overview")
        .unwrap_or_else(|| panic!("the overview's action: {chrome}"));
    let open = Api::param(&overview, "open");
    assert!(
        open["ty"] == "boolean" && open["required"] == false,
        "it may go without whether to open: {overview}"
    );
    let placing = repl.ls(session, &format!("session/{layout_key}"));
    let place = Api::action(&placing, "place").unwrap_or_else(|| panic!("`place`: {placing}"));
    let resource = Api::param(&place, "resource");
    assert!(
        resource["ty"] == "resource"
            && resource["required"] == true
            && Api::param(&place, "x")["required"] == false,
        "it takes the resource as a value, and may go without the rest: {place}"
    );
    // A resource that is a value goes by a reference, as a program's answers name it: the lines
    // the model reads carry each resource's reference, its title beside it.
    let found = repl.program_answer(session, "find(notes.Note)");
    assert!(found["status"] != "failed", "{found}");
    let output = found["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let reference = output
        .iter()
        .find(|line| line.contains(&format!("\"{NOTE_TITLE}\"")))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("the note goes by a reference: {found}"))
        .to_owned();
    reading_an_action_says_how_it_is_called(session, repl);

    // The framed canvas, as the person shows it.
    show_preset(session, "so.tilo.shell:frames");
    until(session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });

    // A `tilo` program places the note on the layout, as the person.
    let (placed_it, failed) = repl.run(
        session,
        &format!("session.place(resource: {reference}, x: 80, y: 60)"),
    );
    assert!(!failed, "the program ran: {placed_it}");
    assert!(placed_it.contains("step 1"), "{placed_it}");
    until(session, "the note's title on the canvas", |session| {
        on_screen(session, NOTE_TITLE).is_some()
    });
    assert_eq!(placed(session, note), 1, "one place shows the note");
    let who = crate::common::last_step(session, "place");
    assert_eq!(
        who,
        crate::common::as_person(&tilo_session::identity::own_person(&Options {
            person: PERSON.to_owned(),
            ..Options::default()
        })),
        "the person's step, not an agent's"
    );

    // `undo` takes the step back.
    let (undone, failed) = repl.run(session, "undo");
    assert!(!failed, "{undone}");
    until(session, "the note off the canvas", |session| {
        placed(session, note) == 0 && on_screen(session, NOTE_TITLE).is_none()
    });
}

/// The person's REPL at the session's API works as them, and nobody is asked about it.
#[test]
fn the_persons_repl_at_the_api_works_as_them() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let (mut session, note) = session(root.path());
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Api::open(&mut session, root.path());
    the_repl_works_as_the_person(&mut session, &mut repl, &note);
    assert!(
        session
            .portal()
            .and_then(tilo_sheets::portal::PortalState::dialog)
            .is_none(),
        "nobody was asked"
    );
}

/// The shell package the session runs is one a program names, whether or not the app store holds
/// a copy of it: an image ships `so.tilo.shell` only as the session's built-in stand-in, and
/// `docs/designs/app-language/shell.md` §6 makes it the package the session shows, so a program
/// written for a person's device works on the image's. The REPL shows the frames through it, and
/// the shell draws them.
///
/// This fails on a build whose server compiles programs against the installed packages alone: the
/// image's store holds no shell, and the program is refused as an unknown name.
#[test]
fn a_program_names_the_shell_package_the_session_runs_with_none_installed() {
    let _settled = ui::motion::testing::settled();
    // An app store that holds no shell: what an image's own store holds, where the session's
    // copy stands in for the package a person's device installs.
    let store = tempfile::tempdir().expect("a store of the test's own");
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_APPS_DATA_DIR", store.path());
    // A session that runs no service of a seeded app's: an image's own, whose apps a person opens
    // when they want one, so nothing but the shell itself publishes a package to the API.
    let root = private_home();
    let mut session = crate::common::headless(api_options(root.path()));
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Api::open(&mut session, root.path());

    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_style(shell.viewing, style: \"so.tilo.shell:frames\")",
    );
    assert!(
        !failed,
        "the program names the shell the session runs: {answer}"
    );
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });
}

/// Placing what a text finds, twice, through the API: each place is the person's step, and the
/// session keeps answering, the program session that asked and a new one alike.
#[test]
fn placing_what_text_finds_twice_keeps_the_session_answering() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let (mut session, note) = session(root.path());
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Api::open(&mut session, root.path());

    for placed_before in 0..2 {
        let (answer, failed) = repl.run(&mut session, &format!(
                "session.place_found(text: \"{NOTE_TITLE}\", view: .none, x: .some({}), y: .some(60))",
                80 + placed_before * 400
            ));
        assert!(!failed, "the program ran: {answer}");
        until(&mut session, "the note placed once more", |session| {
            placed(session, &note) == usize::try_from(placed_before).expect("a count") + 1
        });
    }

    // The program session that asked is still answered.
    let (answer, failed) = repl.run(&mut session, "catalogue");
    assert!(!failed, "{answer}");
    // And so is a new one.
    let (answer, failed) = Api::open(&mut session, root.path()).run(&mut session, "catalogue");
    assert!(!failed, "{answer}");
}

/// What the session's services publish (the seeded apps' records) is listed through the API past
/// the first page of the session's rows, page by page as its cursor reads on, and a note is found
/// by its title.
#[test]
fn a_record_the_sessions_services_publish_is_found_through_the_server() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let apps = crate::common::seeded_apps();
    let mut session = crate::common::headless(Options {
        services: apps,
        ..api_options(root.path())
    });
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let repl = Api::open(&mut session, root.path());

    let listed = every_page(&repl, &mut session);
    // The session's records are listed, each row naming its type the way a program types it
    // (`tasks.Tasks`, `chat.Conversations` ): the name this listing is for, and the name a path
    // walks back by. Never the kind the app publishes it under (`tl.<app>.<Type>`), which no
    // program writes; an app's own row names the app itself (`tl.so.tilo.chat`), an id
    // whose last part is no capitalised type.
    assert!(
        !listed.iter().any(|row| {
            row["kind"]
                .as_str()
                .and_then(|kind| kind.strip_prefix("tl."))
                .and_then(|rest| rest.rsplit_once('.'))
                .is_some_and(|(_, ty)| ty.starts_with(|c: char| c.is_ascii_uppercase()))
        }),
        "no record is named by its published kind: {listed:?}"
    );
    // The app's own record, at the session's top level, named by its type as a program types it.
    let tasks = path_of(&listed, "tasks.Tasks");
    // A record of the app's, a level down: listed the same way, and its path walks back to it.
    let inside = repl.ls(&mut session, &tasks);
    let task = path_of(
        &Api::rows(&inside).cloned().collect::<Vec<_>>(),
        "tasks.Task",
    );
    let task_at = repl.ls(&mut session, &task);
    assert_eq!(
        task_at["kind"], "tasks.Task",
        "the row's own path reads back as its type: {task_at}"
    );
    // The type name itself reads the records of that kind: a note, a channel and a mailbox,
    // each under its app's record, resolve by what the listing names.
    for (form, under) in [
        ("notes.Note", "notes.Notebook"),
        ("chat.TextChannel", "chat.Conversations"),
        ("mail.Mailbox", "mail.Mail"),
    ] {
        let root = path_of(&listed, under);
        let record = repl.listing(&mut session, &format!("{root}/{form}"), None);
        assert!(
            record
                .as_ref()
                .err()
                .is_none_or(|why| !why.contains(&format!("no {form} in"))),
            "a type name read off a row resolves in a path: {record:?}"
        );
    }

    let found = repl.find(&mut session, NOTE_TITLE);
    assert!(
        found.iter().any(|hit| hit["label"] == NOTE_TITLE),
        "the note is found by its title: {found:?}"
    );
}

/// Every row of the session's listing, page by page as its cursors read on.
fn every_page(repl: &Api, session: &mut Session) -> Vec<Value> {
    let mut listed = Vec::new();
    let mut cursor = None;
    for _ in 0..20 {
        let page = repl
            .listing(session, "session", cursor.as_deref())
            .unwrap_or_else(|why| panic!("the session lists: {why}"));
        listed.extend(Api::rows(&page).cloned());
        let Some(next) = page["next_cursor"].as_str() else {
            break;
        };
        cursor = Some(next.to_owned());
    }
    listed
}

/// The path of the row of `rows` whose type is `form`: what a program reads the record by.
fn path_of(rows: &[Value], form: &str) -> String {
    rows.iter()
        .find(|row| row["kind"] == form)
        .and_then(|row| row["path"].as_str())
        .unwrap_or_else(|| panic!("the listing names {form}: {rows:?}"))
        .to_owned()
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

/// A seeded app's catalogue is the app's words on the shell: with the interface in German, the
/// session names the notes app's notebook and its actions in German, and its search matches the
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
    // The notebook is called by the app's name, whatever it holds; what it is, is said in the
    // catalogue's words.
    let nouns = |session: &Session| -> Vec<tilo_actions::Noun> {
        everything(session)
            .into_iter()
            .filter_map(|resource| resource.resource_type.map(|kind| kind.noun))
            .collect()
    };
    until(&mut session, "the notebook's German nouns", |session| {
        nouns(session).contains(&tilo_actions::Noun::new("Notizbuch", "Notizbücher"))
    });
    assert!(
        nouns(&session).contains(&tilo_actions::Noun::new("Notiz", "Notizen")),
        "the notes' nouns are the catalogue's, in the reader's language: {:?}",
        nouns(&session)
    );
    let labelled = |session: &Session| -> Vec<String> {
        everything(session)
            .into_iter()
            .map(|resource| resource.label)
            .collect()
    };
    assert!(
        labelled(&session).iter().any(|label| label == "Notizen"),
        "the notebook is labelled in German: {:?}",
        labelled(&session)
    );
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

    // Search matches the reader's words: the German plural finds the notebook on the shell, and the
    // English one finds nothing, because the shell's words are German.
    let mut repl = Api::open(&mut session, root.path());
    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"notizbücher\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    until(
        &mut session,
        "the notebook placed by its German name",
        |session| {
            crate::common::places(session)
                .iter()
                .any(|place| place.resource.contains(NOTES))
        },
    );
    let placed_by_german = noted(&session);
    // The English word finds nothing, because the shell's words are German: it is a sentence to the
    // agent rather than a found resource, so it places no second notebook — it asks (shell.md §8.1,
    // "Asking from the canvas").
    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"notebooks\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    assert_eq!(
        noted(&session),
        placed_by_german,
        "the English word finds no notebook on a German shell"
    );
    let sent = crate::common::last_sent(&mut session);
    assert_eq!(
        sent["text"], "notebooks",
        "the agent is asked the words: {sent}"
    );
}

/// How many of the seeded notes app's resources stand on the layout.
fn noted(session: &Session) -> usize {
    session
        .layout()
        .borrow()
        .reading()
        .places
        .iter()
        .filter(|place| place.resource.contains(NOTES))
        .count()
}

/// The overview shown from the REPL is the shell's own on its screen, and showing it is no step:
/// `undo` after it takes back the place before it. A step whose undo the session refuses (an
/// outside action, which no history holds) stays, and `undo` goes on to the step before it.
#[test]
fn showing_the_overview_is_no_step_and_a_refused_undo_holds_no_other_step() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let (mut session, note) = session(root.path());
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Api::open(&mut session, root.path());
    let mut run = |session: &mut Session, program: &str| {
        let (answer, failed) = repl.run(session, program);
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

/// What the seeded apps keep, placed through the API, is drawn on the canvas by its own app's
/// view, its content and not its title alone: a note found by its title shows its text. And a text
/// finds the resource it names even where many actions name it too: "Inbox" places the inbox.
#[test]
fn a_placed_record_is_drawn_through_its_own_apps_view() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = kept_seeded(root.path());
    show_preset(&mut session, "so.tilo.shell:frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });
    let mut repl = Api::open(&mut session, root.path());

    let (answer, failed) = repl.run(
        &mut session,
        &format!(
            "session.place_found(text: \"{NOTE_TITLE}\", view: .none, x: .some(80), y: .some(60))"
        ),
    );
    assert!(!failed, "the program ran: {answer}");
    until(&mut session, "the note's text on the canvas", |session| {
        session.shell_says("Oat milk, eggs, two lemons.")
    });

    // A text naming a resource that many actions name too finds the resource: "Inbox" is the
    // mailbox, under every conversation's "Move to inbox".
    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"Inbox\", view: .none, x: .some(640), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    let mailbox = |session: &Session| {
        let resources = session.resources();
        session
            .layout()
            .borrow()
            .reading()
            .places
            .iter()
            .filter_map(|place| {
                tilo_actions::api::locate(
                    &resources,
                    &tilo_actions::ActionAddress::new(&place.resource, "open"),
                )
                .map(|(resource, _)| (resource.kind.clone(), resource.label.clone()))
            })
            .find(|(kind, _)| kind == "tl.so.tilo.mail.Mailbox")
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

/// A text naming what something is, not what it is called, places the one that kind means: "my
/// mails" is mail's Inbox, its mailboxes' declared primary, though no word of it says "Inbox" and
/// "my" names nothing.
#[test]
fn my_mails_on_the_canvas_places_mails_root() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let apps = crate::common::seeded_apps();
    let mut session = crate::common::headless(Options {
        services: apps,
        ..api_options(root.path())
    });
    show_preset(&mut session, "so.tilo.shell:frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });
    let mut repl = Api::open(&mut session, root.path());

    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"my mails\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    let placed = |session: &Session| {
        let resources = session.resources();
        session
            .layout()
            .borrow()
            .reading()
            .places
            .iter()
            .filter_map(|place| {
                tilo_actions::api::locate(
                    &resources,
                    &tilo_actions::ActionAddress::new(&place.resource, "open"),
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
        ["tl.so.tilo.mail.Mail"],
        "\"my mails\" places Mail's root, through the inbox's list: {found:?}"
    );
}

/// How many pixels of `shot` in the logical rectangle `(x, y, w, h)` (scale 1) differ from the
/// colour most of it is: what is drawn there over its background, in either theme.
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

/// The seeded inbox placed through the API, once, and what the session shows and does with it
/// from then on:
///
/// - it stands on the canvas as the mail app's own view of it, and mail's package is loaded because
///   the place shows it ([`the_inbox_is_drawn_as_mails_inbox`]);
/// - its conversations come in a turn per level, not one per conversation
///   ([`the_inbox_filled_in_a_turn_per_level`]);
/// - once they are drawn, a session left alone runs and draws nothing
///   ([`the_settled_inbox_draws_nothing_while_idle`]);
/// - a change to one conversation reads that conversation again, and no other
///   ([`a_change_to_one_thread_rereads_only_that_threads_reading`]).
///
/// One session and one placed inbox hold all four, each driven as a test of it alone drives it. The
/// steps that only read come first, and the one that changes a record (the flag) comes last, so
/// nothing a step reads was changed by a step before it.
#[test]
fn the_seeded_inbox_placed_through_the_server_fills_in_settles_is_drawn_and_rereads_one_thread() {
    let _settled = ui::motion::testing::settled();
    // The bar draws the time, and a minute turning over is a change, not idle work: the test holds
    // the clock at the minute it runs in, as a capture does, so what it watches at rest is the
    // shell and nothing else. Held from before the session starts, as the idle watch needs it.
    ui::time::testing::with_wall(this_minute(), || {
        let root = private_home();
        let mut session = seeded_session(root.path());
        assert!(
            session.shell_apps().is_empty(),
            "no app's package is the shell's before a place shows it: {:?}",
            session.shell_apps()
        );
        let rounds_before = session.shell_reading_rounds();
        place_the_inbox(&mut session, root.path());
        assert_eq!(session.shell_apps(), vec![MAIL.to_owned()]);
        for _ in 0..200 {
            session.step(Duration::from_millis(1)).expect("step");
        }
        the_inbox_filled_in_a_turn_per_level(&session, rounds_before);
        the_settled_inbox_draws_nothing_while_idle(&mut session);
        the_inbox_is_drawn_as_mails_inbox(&mut session, root.path());
        a_change_to_one_thread_rereads_only_that_threads_reading(&mut session);
    });
}

/// The seeded inbox placed through the API stands on the canvas as the mail app's own view of it:
/// a seeded conversation's subject is laid out in it, and drawn where it is laid out in the
/// snapshot `tilo-control` takes. (That mail's package is loaded because the place shows it, and
/// was not before, is read where the inbox is placed.)
fn the_inbox_is_drawn_as_mails_inbox(session: &mut Session, root: &std::path::Path) {
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
    let png = root.join("shot.png");
    let answer = control(
        session,
        &root.join("test-control.sock"),
        &json!({"op": "snapshot", "path": png.to_str().unwrap()}),
    );
    assert_eq!(answer, json!({"ok": {"frame": 0}}), "the snapshot is taken");
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
    let places: Vec<u64> = crate::common::places(&session)
        .iter()
        .map(|place| crate::common::number(&place.key))
        .collect();
    for place in places {
        session
            .invoke(
                &tilo_session_core::layout::Item::Place.resource(&place.to_string()),
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

/// The resource of the seeded mail of the type `ty` whose label is `label` .
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

/// A change to one conversation the inbox shows reads that conversation's reading again, and no
/// other: not the mailbox, the mail's root, its account or the other conversations.
fn a_change_to_one_thread_rereads_only_that_threads_reading(session: &mut Session) {
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let before = session.shell_reads();
    let thread = seeded(session, "Thread", "Dinner on Friday?");
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
    until(session, "the conversation read again", |session| {
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

/// The start of the minute the test runs in, as the session's clock reads it: what a test watching
/// a session at rest holds the wall clock at, so the bar's time cannot turn over inside the window
/// it watches.
fn this_minute() -> ui::time::SystemTime {
    ui::time::wall()
        .checked_sub(Duration::from_secs(
            u64::try_from(
                ui::clock::local_now()
                    .timestamp()
                    .as_second()
                    .rem_euclid(60),
            )
            .expect("a second of the minute"),
        ))
        .expect("the minute the test runs in")
}

/// A placed inbox settles: once its conversations are drawn, a session left alone runs the shell's
/// views no more and draws no frame. A list's rows asked of the shell's own runtime go out on the
/// turn they are asked, rather than the list taking frames until they would. Read after the two
/// hundred turns that follow the place, with the clock held since before the session started.
fn the_settled_inbox_draws_nothing_while_idle(session: &mut Session) {
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
}

/// The inbox's conversations come in together: the session reads what one run of the views read in
/// one turn, so a placed inbox fills in as many turns as its views reach records deep (the mailbox,
/// the mail's root, its account, the conversations), however many conversations it lists. Read
/// after the two hundred turns that follow the place; `rounds_before` is the shell's count before
/// the inbox was placed.
fn the_inbox_filled_in_a_turn_per_level(session: &Session, rounds_before: u64) {
    let threads = session
        .shell_reads()
        .keys()
        .filter(|id| id.contains(&format!("{MAIL}:")) && seeded_thread(session, id))
        .count();
    let rounds = session.shell_reading_rounds() - rounds_before;
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

/// A service publishing the seeded apps' records, but withholding the readings of every resource of
/// the kind `withheld` (the app that publishes it never does, or the session's watch has not seen
/// it): what a place whose data never arrives meets.
struct Withholding {
    inner: Box<dyn tilo_service::Service>,
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
    ) -> Result<Started, tilo_actions::ApiError> {
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
    let apps = crate::common::seeded_apps();
    let mut session = crate::common::headless(Options {
        // Every task's reading never comes: the window of the one placed has no record to draw.
        services: apps
            .into_iter()
            .map(|inner| {
                Box::new(Withholding {
                    inner,
                    withheld: "tl.so.tilo.lang.tasks.Task",
                    ids: std::cell::RefCell::new(None),
                }) as Box<dyn tilo_service::Service>
            })
            .collect(),
        ..api_options(root.path())
    });
    show_preset(&mut session, "so.tilo.shell:frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });
    let mut repl = Api::open(&mut session, root.path());

    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"Pack the books\", view: .none, x: .some(80), y: .some(60))",
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
    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"Groceries\", view: .none, x: .some(600), y: .some(60))",
    );
    assert!(!failed, "the note is placed: {answer}");
    until(&mut session, "the note's title on the canvas", |session| {
        session.shell_says(NOTE_TITLE)
    });

    // And the overview opens and closes, as it does on a screen all of whose places have arrived.
    then_the_overview_opens_and_closes(&mut session, &mut repl);
}

/// The overview opens over the shell and closes again, through the API as an agent asks it.
fn then_the_overview_opens_and_closes(session: &mut Session, repl: &mut Api) {
    let (answer, failed) = repl.run(session, "session.overview(open: .some(true))");
    assert!(!failed, "the overview opens: {answer}");
    until(session, "the shell's overview", |session| {
        on_screen(session, "Overview").is_some()
    });
    let (answer, failed) = repl.run(session, "session.overview(open: .some(false))");
    assert!(!failed, "the overview closes: {answer}");
    until(session, "the overview closed", |session| {
        on_screen(session, "Overview").is_none()
    });
}

/// The REPL's `find` is the agent's `tilo_find` , and answers by the one search the palette and
/// text typed on the canvas run: over the same sources, with the same ranking. A text naming what
/// something is ("my mails"), what it is called, or both, finds there what `session.place_found`
/// places — never a second ranking that knows a note's title but not Mail's inbox.
#[test]
fn the_repl_finds_what_place_found_places() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let apps = crate::common::seeded_apps();
    let mut session = crate::common::headless(Options {
        services: apps,
        ..api_options(root.path())
    });
    show_preset(&mut session, "so.tilo.shell:frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });
    let mut repl = Api::open(&mut session, root.path());

    // Each text, and a word the resource it names must carry (its kind, or the app it belongs to).
    for (text, wanted) in [
        ("my mails", "mail.Mail"),
        ("notes", "notes."),
        ("Groceries", "notes.Note"),
        ("mail archive", "mail.Mailbox"),
    ] {
        // What the canvas places for `text`: the one search's first result,
        // typed on the canvas as a person would.
        let (answer, failed) = repl.run(
            &mut session,
            &format!(
                "session.place_found(text: {text:?}, view: .none, x: .some(80), y: .some(60))"
            ),
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

        // And `find` names the same rows the one search ranks, each by a path `tilo_ls` reads: the
        // row of the place the shell just made leads where the search puts it, and a row of the kind
        // the search sought stands among them. `find` walks the containers it can read, so a record
        // placed out of a notebook is reached through its notebook.
        let found = repl.find(&mut session, text);
        let first = found
            .first()
            .unwrap_or_else(|| panic!("{text:?} finds nothing: {found:?}"));
        let path = first["path"].as_str().unwrap_or_default();
        let (parent, row) = path.rsplit_once('/').unwrap_or_default();
        assert!(
            !parent.is_empty() && !row.is_empty(),
            "{text:?}: find leads with a row of a directory: {first}"
        );
        let listed = repl.ls(&mut session, parent);
        assert!(
            Api::rows(&listed).any(|listed| listed["path"] == path || listed["name"] == row),
            "{text:?}: find leads with {first}, and {parent} lists no row {row}: {listed}"
        );
        assert!(
            found.iter().any(|hit| hit["kind"]
                .as_str()
                .is_some_and(|kind| kind.contains(wanted))),
            "{text:?}: find names a row of a {wanted}, {shown:?}: {found:?}"
        );

        // Each `place_found` is a step of the person's: take it back, so the next text places on
        // its own.
        let (undone, failed) = repl.run(&mut session, "undo");
        assert!(!failed, "{undone}");
        until(&mut session, "the canvas cleared", |session| {
            placed_resources(session).is_empty()
        });
    }
}

/// An installed tilolang app the session does not run itself is named by what its package
/// declares: its actions by their names (`notes.new_note`), never the ids they are stored under,
/// and its record's root (`notes.notebook`), which those actions are called on.
#[test]
fn an_installed_apps_actions_and_root_are_named_as_it_declares_them() {
    let _settled = ui::motion::testing::settled();
    crate::common::app_store();
    let root = private_home();
    let mut session = crate::common::headless(Options {
        ..api_options(root.path())
    });
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Api::open(&mut session, root.path());
    let (listed, failed) = repl.run(&mut session, "catalogue");
    assert!(!failed, "{listed}");
    let catalogue: Value = serde_json::from_str(&listed).expect("the catalogue is JSON");
    let notes = catalogue["notes"]["signatures"]
        .as_array()
        .unwrap_or_else(|| panic!("the notes app is listed: {catalogue:#}"));
    let named = |kind: &str, name: &str| {
        notes
            .iter()
            .find(|signature| signature["kind"] == kind && signature["name"] == name)
            .cloned()
    };
    let new_note = named("action", "new_note")
        .unwrap_or_else(|| panic!("notes names its action new_note: {notes:#?}"));
    assert!(
        new_note["call"]
            .as_str()
            .is_some_and(|call| call.starts_with("notes.new_note(")),
        "{new_note:#}"
    );
    assert!(
        named("root", "notebook").is_some(),
        "notes names its root notebook: {notes:#?}"
    );
}
