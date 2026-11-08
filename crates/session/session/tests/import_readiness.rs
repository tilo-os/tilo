//! Add folders belongs to the session the person is logged into: it answers
//! whether first use is over on the socket named for that session's display,
//! and it opens the review the person left to be continued once it is over.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

mod common;

use std::ffi::OsStr;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use tilo_app_launch::SessionEnvironment;
use tilo_compositor::OutputConfig;
use tilo_session::mount::apps::Apps;
use tilo_session::{Options, Session};

/// A session that starts the person's apps, on the display it binds, through a launcher that
/// records what it was asked and runs as an app would until the test is over.
fn session(
    home: &tilo_test_env::TestHome,
    records: &Path,
) -> (Session, Rc<Apps>, tilo_test_env::StandIn) {
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$*\" >> '{records}'\nhold\n",
            records = records.display()
        ),
    )
    .expect("a launcher");
    let launch = Rc::new(Apps::with_launcher(
        SessionEnvironment::from_process(),
        None,
        Some(launcher.path().to_owned()),
    ));
    let session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
        Options {
            collab: false,
            launch: Some(Rc::clone(&launch)),
            ..Options::default()
        },
    );
    (session, launch, launcher)
}

/// The installed folder importer on this login: its desktop entry starts the script at `program` ,
/// and it keeps its review where the login reads it.
fn importer(home: &tilo_test_env::TestHome, program: &str) -> PathBuf {
    let executable = home.home().join("tilo-import-folders");
    tilo_test_env::script(&executable, program).expect("the importer's program");
    let applications = home.data().join("applications");
    fs::create_dir_all(&applications).expect("this login's applications");
    fs::write(
        applications.join("tilo-import-folders.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=Add folders to Tilo\nExec={} --import-folders\n",
            executable.display()
        ),
    )
    .expect("the importer's entry");
    desktop_entry::cache::invalidate();
    let review = tilo_paths::login::importer_continuation().expect("the importer's review");
    fs::create_dir_all(review.parent().expect("the importer's directory"))
        .expect("the importer's directory");
    review
}

/// The review the person left: whether it asks to be opened when their first use is over.
fn leave_review(review: &Path, asks_to_open: bool) {
    fs::write(
        review,
        serde_json::json!({ "show_on_login": asks_to_open }).to_string(),
    )
    .expect("the review");
    fs::set_permissions(review, fs::Permissions::from_mode(0o600))
        .expect("private to this account");
}

/// Where the installed folder importer reads this session's answer.
fn readiness_socket(runtime: &Path, display: &str) -> PathBuf {
    let wayland = fs::symlink_metadata(runtime.join(display)).expect("the display");
    tilo_paths::login::import_readiness_socket(
        Some(runtime.as_os_str().to_owned()),
        OsStr::new(display),
        wayland.ino(),
    )
    .expect("the session's readiness path")
}

/// Run the session until `done` holds: a launched app starts on a worker of its own, so the loop
/// keeps drawing while it gets there.
fn until(session: &mut Session, what: &str, mut done: impl FnMut() -> bool) {
    common::wait_for(session, what, |_| done().then_some(()));
}

/// The installed folder importer reads the answer from this session and no other: ready once first
/// use is over, and only this account may ask.
#[test]
fn the_session_answers_add_folders_on_the_socket_named_for_its_display() {
    let home = tilo_test_env::TestHome::new().expect("this login's home");
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
        Options {
            collab: false,
            ..Options::default()
        },
    );
    let display = session.compositor().listen().expect("a display");
    session
        .serve_folder_import(OsStr::new(&display))
        .expect("the session answers Add folders");

    let socket = readiness_socket(home.runtime(), &display);
    assert_eq!(
        fs::symlink_metadata(&socket)
            .expect("the socket is served")
            .mode()
            & 0o777,
        0o600,
        "only this account may ask"
    );

    // The answer comes from this session's state: no setup runs here, so its first use is over.
    assert!(!session.setup_up(), "this session runs no setup");
    let mut client = UnixStream::connect(&socket).expect("the session answers");
    client
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .expect("a bounded wait");
    session.step(Duration::from_millis(1)).expect("step");
    let mut answer = [0; 21];
    client.read_exact(&mut answer).expect("one answer");
    assert_eq!(&answer, b"tilo-import-ready-v1\n");

    // The answers are this session's: they go when it does.
    drop(session);
    assert!(
        fs::symlink_metadata(&socket).is_err(),
        "the readiness socket goes with the session"
    );
}

/// The review the person left opens once, when the login it belongs to starts: the Add folders
/// click they meant to come back to, without one they must remember and without a second window of
/// it.
#[test]
fn the_review_a_person_left_opens_the_importer_once_after_login() {
    let home = tilo_test_env::TestHome::new().expect("this login's home");
    let records = home.home().join("launches");
    let review = importer(&home, "#!/bin/sh\nexit 0\n");
    leave_review(&review, true);

    let (mut session, apps, _launcher) = session(&home, &records);
    let display = session.compositor().listen().expect("a display");
    apps.set_displays(home.runtime().join(&display), None, None);
    session
        .serve_folder_import(OsStr::new(&display))
        .expect("the session serves Add folders");

    let opens = || {
        fs::read_to_string(&records)
            .map(|opened| opened.lines().count())
            .unwrap_or_default()
    };
    until(&mut session, "the saved review opens", || opens() > 0);
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(opens(), 1, "one login opens the saved review once");
    assert!(
        fs::read_to_string(&records)
            .expect("the launch")
            .contains("--desktop-id tilo-import-folders"),
        "the importer's own entry is what opens"
    );
}

/// A review the person already finished, or put aside, opens nothing: the login leaves Add folders
/// to them.
#[test]
fn a_review_that_asks_for_nothing_opens_nothing() {
    let home = tilo_test_env::TestHome::new().expect("this login's home");
    let records = home.home().join("launches");
    let review = importer(&home, "#!/bin/sh\nexit 0\n");
    leave_review(&review, false);

    let (mut session, apps, _launcher) = session(&home, &records);
    let display = session.compositor().listen().expect("a display");
    apps.set_displays(home.runtime().join(&display), None, None);
    session
        .serve_folder_import(OsStr::new(&display))
        .expect("the session serves Add folders");
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert!(
        !records.exists(),
        "a review that asks for nothing opens nothing"
    );
}
