//! A window's note names the record its app holds, as the device's daemon publishes it: the broker
//! that knows what a window shows runs there, and the session follows what it publishes on its
//! shell link, so a person's session keeps no broker of its own to name it from.
//!
//! The fixture starts a real tilolang app through the session's launcher, on a real device daemon:
//! the app's window opens its record, and the session notes it beside the window.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::Duration;

use tilo_app_launch::Launch;

use crate::common::linked::{Builder, DEFAULT_APP};
use crate::common::wait_for;

/// The window the app's window shows is noted by the record its app opened.
#[test]
fn a_windows_app_opens_a_record_and_the_sessions_note_names_it() {
    let mut fixture = Builder::installing(DEFAULT_APP).start();
    // The session is taken into the person's workspace a moment after it starts, and an app
    // starts there only once it is: open it as the person does, once the launcher will take it.
    let launch = Launch::Package {
        app: DEFAULT_APP.to_owned(),
        open: None,
    };
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        let workspace = fixture.apps.workspace_named(&fixture.workspace);
        match fixture.apps.start(&launch, &workspace) {
            Ok(()) => break,
            Err(why)
                if format!("{why}").contains("hasn't been admitted")
                    && std::time::Instant::now() < deadline =>
            {
                fixture.pass(Duration::from_millis(100));
            }
            Err(why) => panic!("{DEFAULT_APP} starts: {why}"),
        }
    }
    fixture.until_published(DEFAULT_APP);
    // The tab the app's window fills, as the shell shows it.
    let tab = wait_for(&mut fixture.session, "the app's window", |session| {
        session.handle().read(|tree| {
            tree.tabs()
                .find(|tab| {
                    tilo_session_core::desktop::host::records::app_of(tree, tab.id)
                        .is_some_and(|app| app == DEFAULT_APP)
                })
                .map(|tab| tab.id)
        })
    });
    // The record its window opened, read from the daemon on the shell link.
    let note = wait_for(&mut fixture.session, "the window's note", |session| {
        session.window_notes().get(&tab).cloned()
    });
    assert!(
        !note.is_empty(),
        "the note names the record the app's window opened"
    );
}
