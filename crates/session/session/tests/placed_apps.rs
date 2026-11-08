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
