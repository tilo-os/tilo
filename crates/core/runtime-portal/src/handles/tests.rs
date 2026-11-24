#![allow(clippy::unwrap_used, reason = "test fixtures")]

use std::io::{Read as _, Write as _};
use std::os::windows::io::AsHandle as _;
use std::process::Command;

use tilo_test_env::SpawnUnderEnvLock as _;

use super::*;

/// Set on the copy of this test binary a test starts as a process it did not start through
/// `process_link`: it waits until it is ended.
const WAITS: &str = "TILO_TEST_HANDLES_WAITS";

#[test]
fn waits() {
    if std::env::var_os(WAITS).is_some() {
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
}

/// A process ended when the test does, however it ends.
struct Ended(std::process::Child);

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn waiting() -> Ended {
    Ended(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "handles::tests::waits", "--test-threads=1"])
            .env(WAITS, "1")
            .spawn_locked()
            .unwrap(),
    )
}

/// The system's process, another account's, and this process itself are no peers: nothing is
/// pushed into them or pulled out of them.
#[test]
fn another_accounts_process_and_this_one_are_refused() {
    for pid in [4, std::process::id()] {
        let refused = Peer::of(pid).unwrap_err();
        assert_eq!(
            refused.kind(),
            io::ErrorKind::PermissionDenied,
            "{pid}: {refused}"
        );
        assert!(
            refused.to_string().contains(&format!("process {pid}")),
            "names the process: {refused}"
        );
    }
}

/// A process that ended is refused too, though this process started it and still holds it.
#[test]
fn a_started_process_that_ended_is_refused() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "handles::tests::waits", "--test-threads=1"]);
    let (_link, mut child) = crate::process_link::spawn(command).unwrap();
    assert!(child.wait().unwrap().success());
    let refused = Peer::of(child.id()).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    assert!(refused.to_string().contains("ended"), "{refused}");
}

/// A handle pushed into a program of this account's and pulled back out is the same pipe, and
/// the pull closes it there: once the pulled handle goes, nothing holds that end any more.
#[test]
fn a_handle_goes_into_a_peer_and_back_out() {
    let waiting = waiting();
    let peer = Peer::of(waiting.0.id()).unwrap();
    let (ours, theirs) = tilo_socket::link::pair().unwrap();
    let value = peer.push(theirs.as_handle()).unwrap();
    drop(theirs);
    let back = tilo_socket::Stream::from_handle(peer.pull(value).unwrap()).unwrap();
    (&back).write_all(b"round trip").unwrap();
    let mut read = [0_u8; 10];
    (&ours).read_exact(&mut read).unwrap();
    assert_eq!(&read, b"round trip");
    drop(back);
    let mut after = [0_u8; 1];
    assert_eq!(
        (&ours).read(&mut after).unwrap(),
        0,
        "the peer still holds the end it was pulled out of"
    );
}

/// What a started process gives up comes back to its starter, and goes on to another started
/// process: the starter relays it, as the daemon relays between apps.
#[test]
fn a_starter_relays_a_handle_from_one_started_process_to_another() {
    let (first, second) = (waiting(), waiting());
    let (first, second) = (
        Peer::of(first.0.id()).unwrap(),
        Peer::of(second.0.id()).unwrap(),
    );
    let (ours, theirs) = tilo_socket::link::pair().unwrap();
    // As if the first had sent it: its value there.
    let given = first.push(theirs.as_handle()).unwrap();
    drop(theirs);
    let relayed = first.pull(given).unwrap();
    let there = second.push(relayed.as_handle()).unwrap();
    drop(relayed);
    let back = tilo_socket::Stream::from_handle(second.pull(there).unwrap()).unwrap();
    (&ours).write_all(b"relayed").unwrap();
    let mut read = [0_u8; 7];
    (&back).read_exact(&mut read).unwrap();
    assert_eq!(&read, b"relayed");
}

/// A process of another sign-in session is refused for that, before it is even opened: one of
/// the system's, or another person's.
#[test]
fn a_process_of_another_session_is_refused() {
    let here = session_of(std::process::id()).unwrap();
    let elsewhere = (4..=0x0010_0000_u32)
        .step_by(4)
        .find(|pid| session_of(*pid).is_ok_and(|there| there != here))
        .expect("a Windows runs processes in a session besides this one: the system's");
    let refused = Peer::of(elsewhere).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    assert!(
        refused.to_string().contains("another sign-in session"),
        "{refused}"
    );
}
