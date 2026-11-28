//! A fresh session's first app open lands in the workspace the shell started with: the one the
//! shell names with its own word until the workspace's governance gives it one, and the one this
//! device is founding when the person opens the app.
//!
//! The session founds that workspace on its own workspace service a moment after the shell shows
//! it. An open asked before the founding landed used to be refused by the service ("this device
//! does not hold that workspace") and its window stayed empty, and the workspace was named by its
//! id until governance answered. Both are races with the founding, so this starts fresh sessions
//! one after another and opens an app as the first thing each of them does.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use serde_json::json;

use crate::common::people::Person;

/// The app each fresh session opens first: a window of the session's own, as a person's first app
/// is.
const APP: &str = "so.tilo.passwords";

/// What the shell calls a workspace nobody has named yet: the name the fresh workspace is founded
/// under, and the one every surface names it by from the first frame.
const WORKSPACE: &str = "Workspace";

/// How many fresh sessions this runs. The founding lands a moment after the shell shows the
/// workspace, and the first open of about one session in three used to arrive before it: these
/// many say the open waits every time.
const SESSIONS: u32 = 20;

#[test]
fn a_fresh_sessions_first_app_opens_in_the_seed_workspace_named_workspace() {
    for at in 0..SESSIONS {
        // Each session is a person's first computer: one let into an account that already has a
        // workspace opens in that one, once its copy arrives, not in one of its own.
        let person = Person::new("Ada");
        let label = format!("fresh session {at}");
        let mut device = person.device(&label);
        device.install_passwords();
        device.start();
        // The first thing this session does is open an app, while its workspace is still being
        // founded: the open waits for the founding and lands in that workspace, and nothing of it
        // is refused.
        device.open(APP);
        device.wait_for_window(APP);
        // The session's own fresh workspace is the one in front, and it is the only one: what
        // opened is what landed in it.
        let seed = device.showing().expect("a workspace in front");
        assert_eq!(
            device.on_shell(),
            vec![seed.clone()],
            "{label}: the shell's one workspace is the one in front"
        );
        let windows = device.ask(&json!({"op": "windows"}));
        let window = windows["answer"]["windows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["app_id"] == APP).cloned())
            .unwrap_or_default();
        assert_eq!(
            window["workspace"],
            json!(WORKSPACE),
            "{label}: {APP} opened in the workspace the shell started with, named by the word the \
             shell gives a workspace nobody has named yet and never by its id: {windows}\n{}",
            device.diagnostic()
        );
        assert!(
            window["rect"].is_array(),
            "{label}: {APP}'s window came and drew: {windows}\n{}",
            device.diagnostic()
        );
        device.stop();
    }
}
