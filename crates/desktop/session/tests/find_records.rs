//! A program's `find` over an app's records, from the person's REPL at the session's API and from
//! an agent of theirs admitted at its MCP server.
//!
//! A task's programs read each app's record as the API hands it over
//! (`docs/designs/app-language/agents.md`, "Reading records outside their app"): every tilolang
//! app this session runs keeps its record in the session's own record, so a task reads all of
//! them by their apps, and starts no app to read one. A `find` that hands a record over as
//! nothing — or refuses it for want of an original caller — is what these tests catch.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use crate::common::{Agent, Api, on_screen, private_home, seeded_session, until};

/// A task the tasks app's first scenario seeded, by its title: found through `find(tasks.Task)`,
/// whichever of its fields a program reads.
const SEEDED_TASK: &str = "Take the bins out";

/// A note the notes app's record holds: an app's record a task reads like any other.
const NOTE_TITLE: &str = crate::common::NOTE_TITLE;

#[test]
fn the_persons_repl_finds_the_workspaces_tasks() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut repl = Api::open(&mut session, root.path());

    let (found, failed) = repl.run(&mut session, "find(tasks.Task)");
    assert!(!failed, "the REPL's find ran: {found}");
    assert!(
        found.contains(SEEDED_TASK),
        "the seeded tasks are found: {found}"
    );
    let (notes, failed) = repl.run(&mut session, "find(notes.Note)");
    assert!(!failed, "the notes are found too: {notes}");
    assert!(
        notes.contains(NOTE_TITLE),
        "the seeded note is found: {notes}"
    );
}

#[test]
fn an_agents_run_at_the_api_finds_the_workspaces_tasks() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let mut agent = Agent::admitted(&mut session, &root.path().join("agent.sock"));

    let (found, failed) = agent.run(&mut session, "find(tasks.Task)");
    assert!(!failed, "the agent's find ran: {found}");
    assert!(
        found.contains(SEEDED_TASK),
        "the seeded tasks are found: {found}"
    );
}
