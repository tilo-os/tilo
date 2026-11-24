//! A session started as the shell of its device's daemon, its apps' wire up: the seat's providers
//! are served on the shell link while the session's own loop reads and writes on it, and the
//! session gets through its start and its passes and answers. A read the loop waits for must never
//! wait on something only the loop itself, or a task nobody polls, can answer.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_session::{AppSocket, Options};

#[test]
fn a_session_on_its_daemon_serving_the_seat_starts_and_answers() {
    let root = crate::common::private_home();
    let socket = root.path().join("app_wire.sock");
    let (started, answered) = std::sync::mpsc::channel();
    // The session runs on a thread of its own: a hang in its start or its loop fails this test
    // with what it waited for, where it would otherwise stop the runner.
    std::thread::spawn(move || {
        let mut session = crate::common::on_its_daemon(
            &[OutputConfig::new("HEADLESS-1", 1280, 800)],
            Options {
                app_socket: AppSocket::At(socket),
                collab: false,
                ..Options::default()
            },
        );
        for _ in 0..3 {
            session.step(Duration::from_millis(1)).expect("a frame");
        }
        let workspaces = session.handle().read(|tree| tree.workspaces.len());
        started
            .send(workspaces)
            .expect("the test waits for the session");
        // The session ends with the test's process: it is not dropped here, so a teardown of its
        // own never reads as the start's answer.
        std::mem::forget(session);
    });
    let workspaces = tilo_test_env::recv(
        "the session on its daemon to start and take its first passes",
        &answered,
    );
    assert!(
        workspaces >= 1,
        "the session shows the person's workspaces it read through the daemon"
    );
}
