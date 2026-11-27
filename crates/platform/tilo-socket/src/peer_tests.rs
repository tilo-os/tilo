#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in these process fixtures a failed unwrap is the test assertion"
)]

use super::*;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Where a child this test starts connects: set only in the child.
const CHILD: &str = "TILO_SOCKET_PEER_TEST_CONNECT";

/// Not a test of its own: the process the tests start, which connects to the socket `CHILD`
/// names and holds the connection until its input closes. Run as a test, it does nothing.
#[test]
fn connecting_child() {
    let Some(path) = std::env::var_os(CHILD) else {
        return;
    };
    let mut stream = crate::connect(Path::new(&path)).expect("the child connects");
    stream.write_all(b"!").expect("the child says it is there");
    let _ = std::io::stdin().read(&mut [0_u8; 1]);
}

/// A process that connects to the listener at `path`, and the server's end of its connection.
fn connected(listener: &crate::Listener, path: &Path) -> (Child, crate::Stream) {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "peer::tests::connecting_child",
            "--exact",
            "--test-threads=1",
            "-q",
        ])
        .env(CHILD, path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("the test starts a child");
    let (mut stream, _) = listener.accept().expect("the child's connection");
    let mut said = [0_u8; 1];
    stream.read_exact(&mut said).expect("the child is there");
    (child, stream)
}

fn listener() -> (tempfile::TempDir, std::path::PathBuf, crate::Listener) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peer.sock");
    let listener = crate::bind(&path).unwrap();
    (dir, path, listener)
}

#[test]
fn the_pin_is_the_process_that_connected_until_it_ends() {
    let (_dir, path, listener) = listener();
    let (mut child, stream) = connected(&listener, &path);
    let pinned = Pinned::of(&stream).expect("the child's process is pinned");
    assert_eq!(
        pinned.pid(),
        child.id(),
        "the pin names the child, not this process"
    );
    assert!(pinned.alive());
    let start = imp::start_of(child.id()).expect("the child's start");
    assert!(pinned.is(child.id(), start));
    assert!(
        !pinned.is(child.id(), start.wrapping_add(1)),
        "another start under the same pid is another process"
    );

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!pinned.alive(), "an ended process is not taken as running");
    assert!(
        !pinned.is(child.id(), start),
        "nor named by its pid any more"
    );
}

#[test]
fn another_process_that_connects_is_not_the_pinned_one() {
    let (_dir, path, listener) = listener();
    let (mut first, first_stream) = connected(&listener, &path);
    let (mut second, second_stream) = connected(&listener, &path);
    let pinned = Pinned::of(&first_stream).unwrap();
    let other = Pinned::of(&second_stream).unwrap();
    assert_ne!(pinned.pid(), other.pid());
    let other_start = imp::start_of(other.pid()).unwrap();
    assert!(
        !pinned.is(other.pid(), other_start),
        "a second process on the same socket is not the first"
    );
    let own = std::process::id();
    assert!(
        imp::start_of(own).is_none_or(|start| !pinned.is(own, start)),
        "nor is the process that accepted it"
    );
    for child in [&mut first, &mut second] {
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

#[cfg(feature = "tokio")]
#[test]
fn exited_resolves_once_the_pinned_process_ends() {
    let (_dir, path, listener) = listener();
    let (mut child, stream) = connected(&listener, &path);
    let pinned = Pinned::of(&stream).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    runtime.block_on(async {
        let ended = tokio::spawn({
            let pinned = pinned.clone();
            async move { pinned.exited().await }
        });
        tokio::task::yield_now().await;
        assert!(!ended.is_finished(), "a running process has not exited");
        child.kill().unwrap();
        child.wait().unwrap();
        ended.await.unwrap();
    });
}
