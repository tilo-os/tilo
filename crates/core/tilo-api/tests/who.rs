//! Logging this computer in with a ticket, as `tilo login` does: the wait at this endpoint
//! follows the daemon's call until the person answers it.

use tilo_api::daemon::who::log_in;
use tilo_device_identity::IdentityPaths;
use tilo_device_identity::fixture::{DeviceDaemon, Login};

/// A code in the form `tilo device add` prints it, and the daemon below answers.
const CODE: &str = "tilo1thecode";

/// This account's computer, whose daemon answers a login with `login`.
#[expect(
    clippy::expect_used,
    reason = "a test's own machine that cannot be made is the test's setup failing"
)]
fn machine(login: Login) -> (tilo_test_env::TestHome, DeviceDaemon) {
    let mut home = tilo_test_env::TestHome::new().expect("a machine of this test's own");
    home.env()
        .remove("TILO_RUNTIME_PROFILE")
        .remove("TILO_IDENTITY_ROOT");
    let paths = IdentityPaths::under(home.root().join("device"));
    let daemon = DeviceDaemon::logging_in(&paths, login, false).expect("the daemon's socket");
    (home, daemon)
}

/// The login ends with the person's answer, after the daemon reported them still deciding:
/// logged in once they approve it, and their words once they decline it.
#[test]
fn a_login_ends_with_the_persons_answer() {
    let (home, daemon) = machine(Login::Pending);
    let who = log_in(CODE, "Codex", "the desk").expect("the person approved the login");
    assert_eq!(who.device, daemon.device_key());
    let home_device = who.home.expect("the login enters a workspace");
    assert_eq!(home_device.cert.device_public_key, daemon.device_key());
    assert_eq!(who.person, Some(home_device.cert.user_public_key));
    drop((home, daemon));

    let (home, daemon) = machine(Login::PendingRefused(
        "your person did not add this computer",
    ));
    let why = log_in(CODE, "Codex", "the desk").expect_err("the person declined the login");
    assert_eq!(why, "your person did not add this computer");
    drop((home, daemon));
}

/// A person who never answers the login leaves the caller waiting: the wait is theirs, not a
/// clock's, and it ends only when nothing answers it any more, saying so.
#[test]
fn a_login_nothing_answers_ends_instead_of_hanging() {
    let (home, daemon) = machine(Login::PendingForever);
    let waiting = std::thread::spawn(|| log_in(CODE, "Codex", "the desk"));
    drop(daemon);
    let why = waiting
        .join()
        .expect("the login ends")
        .expect_err("nothing answered the login");
    // Nothing answers the login any more: it ends there, saying what it found, rather than on a
    // clock of its own — no endpoint to connect to, or a connection that ended.
    let found = ["daemon", "running", "Connection"];
    assert!(
        found.iter().any(|said| why.contains(said)),
        "the wait ended, saying what it found: {why}"
    );
    drop(home);
}
