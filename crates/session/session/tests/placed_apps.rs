//! Opening an app whose model the session runs itself — a tilolang app — stands its root on the
//! canvas as a place of the shell's arrangement, and starts no window process of its own
//! (`docs/designs/app-language/one-app-model.md` §3, "Windows belong to the shell";
//! `docs/architecture/apps.md` §8 step 2).
//!
//! The place names the root the session's record service publishes
//! (`tilolang_tilo::records_service::RecordService::hosted`), which is the resource the app's
//! views read their state from, and the shell draws it through the app's own default view from
//! the session's reading. The launcher is a stand-in here, so a window process of the app would
//! leave a line in the file it writes: an empty file is a place and nothing else.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session outlives every call"
)]

mod common;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use tilo_app_launch::SessionEnvironment;
use tilo_session::mount::apps::Apps;
use tilo_session::{AppActions, Options, Session};

use common::PERSON;

/// The notebook, as a package names it.
const NOTES: &str = "so.tilo.lang.notes";
/// The root the notes app's views read: what the session's record service publishes for it.
const ROOT: &str = "tl:so.tilo.lang.notes:root-v7cub7aw";

/// A launcher that records each start it was handed, and holds, so a process a launch started
/// says it did. Nothing this test expects to be placed starts one.
fn launcher_recording(home: &tilo_test_env::TestHome, handed: &Path) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf 'started\\n' >> '{handed}'\nhold\n",
            handed = handed.display()
        ),
    )
    .unwrap()
}

/// Whether a launch reached the launcher: a process the session started for an app.
fn launched(handed: &Path) -> bool {
    std::fs::read_to_string(handed).is_ok_and(|said| said.contains("started"))
}

/// The apps this session starts, on its own display, through `launcher`.
fn apps_at(home: &tilo_test_env::TestHome, launcher: PathBuf) -> Rc<Apps> {
    let mut environment = SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    Rc::new(Apps::with_launcher(environment, None, Some(launcher)))
}

/// A session showing `preset`, whose notes app the record service serves, its apps started
/// through `launcher`.
fn session(
    _home: &tilo_test_env::TestHome,
    preset: &str,
    apps: &Rc<Apps>,
    served: bool,
) -> Session {
    let services: Vec<Box<dyn tilo_service::Service>> = if served {
        vec![Box::new(common::notebook(&["Groceries"]).0)]
    } else {
        Vec::new()
    };
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services,
            apps: Some(Rc::clone(apps) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(apps)),
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            common::WIDTH,
            common::HEIGHT,
        )],
    )
    .expect("a session");
    common::show_preset(&mut session, preset);
    session
}

/// Opening the notes app in `preset` stands its root on the canvas as a place, and starts no
/// window process of it: the app's process would be a launch the stand-in launcher recorded.
fn opening_a_served_app_places_its_root(preset: &str) {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(&home, preset, &apps, true);
    // The record service publishes the notes; the disposition of the connection from the desk
    // to the app's door is the shell's, not this test's.
    common::until(&mut session, "the notes' root is published", |session| {
        session
            .resources()
            .iter()
            .any(|resource| resource.id.as_str() == ROOT)
    });

    // A place is an arranging step of the person's, taken on the desk the session draws: the
    // next pass lays the place out, and the one after draws it.
    apps.open(&tilo_app_launch::Launch::Package {
        app: NOTES.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    common::until(&mut session, "the app's root on the canvas", |session| {
        common::places(session)
            .iter()
            .any(|place| place.record == ROOT)
    });
    let place = common::places(&session)
        .into_iter()
        .find(|place| place.record == ROOT)
        .expect("the place the app's root stands in");
    assert_eq!(
        place.shows, NOTES,
        "the place shows the app whose root it names"
    );
    assert!(
        !place.shown_as.is_empty(),
        "the place stands through the app's own view: {place:?}"
    );
    assert!(
        !launched(&handed),
        "no window process of the app starts: its place is the shell's"
    );
}

#[test]
fn a_served_app_opened_in_strips_stands_on_the_canvas() {
    opening_a_served_app_places_its_root("Strips");
}

#[test]
fn a_served_app_opened_in_frames_stands_on_the_canvas() {
    opening_a_served_app_places_its_root("Frames");
}

/// A placed Tasks root draws its tasks' titles, not the placeholder a row shows for an item
/// nobody has named: each row reads the title field of its task's own reading, which the
/// session publishes when the composed list asks for it.
#[test]
fn a_placed_tasks_root_draws_its_tasks_titles() {
    const TASKS: &str = "so.tilo.lang.tasks";
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    // The apps a development session seeds, served as the session serves a device's own.
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(common::seeded_apps_in(&store))],
            apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(&apps)),
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            common::WIDTH,
            common::HEIGHT,
        )],
    )
    .expect("a session");
    common::show_preset(&mut session, "Frames");
    // The seeded tasks the place draws, by the title the seeded scenario gave them.
    common::until(&mut session, "the seeded tasks", |session| {
        session
            .resources()
            .iter()
            .flat_map(|resource| resource.children.iter())
            .any(|resource| resource.label == "Take the bins out")
    });

    apps.open(&tilo_app_launch::Launch::Package {
        app: TASKS.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    common::until(&mut session, "the app's root on the canvas", |session| {
        common::places(session)
            .iter()
            .any(|place| place.shows == TASKS)
    });
    // Today lists what is due today or late, and the seeded week's tasks are due then: the
    // place's window opens in Today, and each of its rows names its task as the record says,
    // drawn on the canvas where a person reads it.
    common::until(&mut session, "a task's title in the place", |session| {
        ["Call the landlord about the boiler", "Book a haircut"]
            .iter()
            .any(|title| session.shell_drawn_text_bounds(title).is_some())
    });
    let drawn = session.accessible_labels();
    assert!(
        !drawn.iter().any(|said| said.contains("Untitled")),
        "no row of the place shows the placeholder an unnamed item gets: {drawn:?}"
    );
}

/// A session with the notes app placed on the canvas, its own root drawing from the record the
/// session serves: the apps a development session seeds, the notes root standing as a place, and
/// the frames the place's views read their readings through run to a note's title on screen.
fn placed_notes() -> (tilo_test_env::TestHome, Session) {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(common::seeded_apps_in(&store))],
            apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(&apps)),
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            common::WIDTH,
            common::HEIGHT,
        )],
    )
    .expect("a session");
    common::show_preset(&mut session, "Frames");
    common::until(&mut session, "the seeded notes", |session| {
        session
            .resources()
            .iter()
            .flat_map(|resource| resource.children.iter())
            .any(|resource| resource.label == common::TITLES[0])
    });
    apps.open(&tilo_app_launch::Launch::Package {
        app: NOTES.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    common::until(&mut session, "the app's root on the canvas", |session| {
        common::places(session)
            .iter()
            .any(|place| place.shows == NOTES)
    });
    common::until(&mut session, "a note's title in the place", |session| {
        session.shell_drawn_text_bounds(common::TITLES[0]).is_some()
    });
    (home, session)
}

/// A placed Notes root draws its notebook's name and each note's title, not the id a note falls
/// back to when its title field reads empty and not "Note", the type's name, when the note's
/// type reads as a type nobody named.
#[test]
fn a_placed_notes_root_draws_its_notes_titles_and_its_notebooks_name() {
    let (_home, session) = placed_notes();
    let notebook = notebook_title(&session);
    assert_eq!(
        notebook, "Notes",
        "the notebook stands under its app's name, not its type's id"
    );
    // The list's title over the rows reads "All notes": the type's noun, from the notes app's
    // catalogue, not the type's id.
    let drawn = session.accessible_labels();
    assert!(
        drawn.iter().any(|said| said == "All notes"),
        "the place's list is named by the type's noun: {drawn:?}"
    );
    assert!(
        !drawn.iter().any(|said| said.contains("Mavvhka7")),
        "no part of the place shows the type's id: {drawn:?}"
    );
    assert!(
        !drawn.iter().any(|said| said.trim() == "Note"),
        "no row of the place shows the type's name for a note that has a title: {drawn:?}"
    );
}

/// The title the placed notebook stands under: its sidebar's heading, as the place draws it.
fn notebook_title(session: &Session) -> String {
    session
        .accessible_labels()
        .into_iter()
        .find(|said| said.contains("Mavvhka7") || said.contains("Notes"))
        .unwrap_or_default()
}

/// A retitle through the session changes the title the place draws: the notes app's own
/// `retitle` on the note's published resource moves the reading the place composes, and the
/// row's title is read from it again. Nothing of the place writes the note's field itself.
///
/// It is the turn an agent's `invoke` runs and a window's field edit asks for, so the rename a
/// person (or their agent) makes is the one the place draws.
#[test]
fn a_retitle_through_the_session_changes_the_drawn_title() {
    let (_home, mut session) = placed_notes();
    let note = common::wait_for(&mut session, "the seeded note", |session| {
        session
            .resources()
            .iter()
            .flat_map(|resource| resource.children.iter())
            .find(|resource| resource.label == common::TITLES[0])
            .map(|resource| resource.id.as_str().to_owned())
    });
    // The note's own `retitle`, as the notes app declares it, invoked on its published resource:
    // the same action an agent's `invoke` runs and a window's field edit asks for.
    session
        .invoke(
            &note,
            "zvgl2tzt",
            serde_json::json!({ "title": "A week of shopping" })
                .as_object()
                .expect("an object of arguments"),
        )
        .expect("the note retitles");
    // The note stands published under the title it was given: the value an agent's `invoke`
    // writes and a window's field edit asks for, and the reading the place draws its row from.
    let named = common::wait_for(&mut session, "the retitled note", |session| {
        session
            .resources()
            .iter()
            .flat_map(|resource| resource.children.iter())
            .find(|resource| resource.id.as_str() == note)
            .map(|resource| resource.label.clone())
    });
    assert_eq!(named, "A week of shopping", "the retitle took");
    // The place draws the note under the title it now has, and its window is named for it too.
    common::until(&mut session, "the retitled note in the place", |session| {
        session
            .shell_drawn_text_bounds("A week of shopping")
            .is_some()
    });
}

/// An app no service runs launches as every app did: the launcher is handed it, and no place
/// stands for it. What proves this test is real is the launcher line a window process leaves.
#[test]
fn an_app_no_service_runs_launches_its_window() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(&home, "Strips", &apps, false);

    apps.open(&tilo_app_launch::Launch::Package {
        app: NOTES.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    common::until(&mut session, "the launcher is handed the app", |_| {
        launched(&handed)
    });
    assert_eq!(
        common::places(&session)
            .iter()
            .filter(|place| place.record == ROOT)
            .count(),
        0,
        "no place stands for an app no service runs"
    );
}
