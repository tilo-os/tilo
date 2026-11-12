//! Opening an app hands it to the launcher, which starts its window process: the desk holds a
//! window of it, never a place of the session's own drawing.
//!
//! The launcher is a stand-in here, so the line it writes is what proves the start: a session
//! that placed the app's root instead would write nothing.

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
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Options, Session};

use common::PERSON;

/// The notebook, as a package names it.
const NOTES: &str = "so.tilo.lang.notes";

/// A launcher that records each start it was handed, and holds, so a process a launch started
/// says it did.
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

/// A session showing `preset`, its apps started through `launcher`.
fn session(preset: &str, apps: &Rc<Apps>) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
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

#[test]
fn an_opened_app_is_handed_to_the_launcher() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session("Strips", &apps);

    apps.open(&tilo_app_launch::Launch::Package {
        app: NOTES.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    common::until(&mut session, "the launcher is handed the app", |_| {
        launched(&handed)
    });
}
