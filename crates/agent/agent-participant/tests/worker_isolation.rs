//! App code the host runs for agents and room actions runs in confined
//! worker processes, out of reach of the host's state and keys, and what a
//! worker returns is admitted before the host would sign it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

// Records are held in confined app workers (this binary, started again);
// their changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use common::{Body as _, catalogue};
use tilo_agent_participant::worker::{ProbeReport, probe_confinement};
use tilo_agent_participant::{ConfinedReplica, Replica, Seed};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

#[test]
fn the_app_worker_cannot_read_the_hosts_state_directory() {
    let state = tempfile::tempdir().unwrap();
    let agents = state.path().join("agents.json");
    std::fs::write(&agents, br#"{"agents":[{"key":"secret"}]}"#).unwrap();
    let report = probe_confinement(&agents).expect("a worker starts and confines itself");
    assert_eq!(
        report,
        ProbeReport::default(),
        "a confined worker opens no file, creates none, reaches no network and starts nothing"
    );
    assert!(
        !state.path().join("agents.probe").exists(),
        "the worker created a file beside the host's state"
    );
}

fn edit(text: &str) -> serde_json::Map<String, serde_json::Value> {
    common::scratchpad_body(text).1
}

#[test]
fn a_confined_record_is_edited_in_its_worker_and_converges_with_a_page() {
    let catalogue = catalogue();
    let app = "so.tilo.scratchpad";
    let note = common::scratchpad_body("").0;
    let mut page = Replica::open(&catalogue, app, ROOM, "ada", false, Seed::New).unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation() else {
        panic!("a new record is published whole");
    };
    let agent = ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        true,
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![snapshot],
        },
    )
    .unwrap();
    let worker = agent.worker_id().expect("the record is held in a worker");
    assert_ne!(
        worker,
        std::process::id(),
        "the app runs in another process"
    );

    assert!(
        agent.survey().iter().any(|action| action.address == note),
        "the worker publishes the record's operations"
    );
    agent
        .invoke(&note, &edit("Drafted in the worker"), false)
        .unwrap();
    let change = agent
        .take_mutation()
        .unwrap()
        .expect("the edit is the agent's own, admitted change");
    assert!(matches!(change, DocumentMutation::Incremental(_)));
    page.apply(&change).unwrap();
    assert!(
        page.body().contains("Drafted in the worker"),
        "{}",
        page.body()
    );
    assert!(
        agent.body().contains("Drafted in the worker"),
        "the host's admitted copy holds what it would sign"
    );
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "nothing is taken twice"
    );

    // And back: the page's edit reaches the worker, and is never taken as
    // the agent's own.
    page.invoke(&note, &edit("Ada's revision"), false).unwrap();
    agent.apply(&page.take_mutation().unwrap()).unwrap();
    assert!(agent.body().contains("Ada's revision"), "{}", agent.body());
    assert!(agent.take_mutation().unwrap().is_none());
}

#[test]
fn bytes_that_are_not_automerge_never_reach_the_worker() {
    let catalogue = catalogue();
    let agent = ConfinedReplica::open(
        &catalogue,
        "so.tilo.scratchpad",
        ROOM,
        "writer",
        true,
        Seed::New,
    )
    .unwrap();
    let worker = agent.worker_id();
    assert!(
        agent
            .apply(&DocumentMutation::Incremental(b"not automerge".to_vec()))
            .is_err(),
        "the host refuses the bytes"
    );
    assert_eq!(agent.worker_id(), worker, "the worker never saw them");
    assert!(matches!(
        agent.take_mutation().unwrap(),
        Some(DocumentMutation::Snapshot(_))
    ));
}

#[test]
fn a_dead_worker_is_replaced_from_what_the_host_holds() {
    let catalogue = catalogue();
    let note = common::scratchpad_body("").0;
    let agent = ConfinedReplica::open(
        &catalogue,
        "so.tilo.scratchpad",
        ROOM,
        "writer",
        true,
        Seed::New,
    )
    .unwrap();
    agent.take_mutation().unwrap();
    agent
        .invoke(&note, &edit("Before the crash"), false)
        .unwrap();
    agent.take_mutation().unwrap().unwrap();
    let first = agent.worker_id().unwrap();
    kill(first);
    // The next request finds the worker gone and is refused; the one after
    // runs in a new worker seeded from the host's admitted copy.
    let _ = agent.survey();
    assert!(
        !agent.survey().is_empty(),
        "a new worker answers for the record"
    );
    let second = agent.worker_id().unwrap();
    assert_ne!(first, second);
    agent
        .invoke(&note, &edit("After the crash"), false)
        .unwrap();
    agent.take_mutation().unwrap().unwrap();
    assert!(agent.body().contains("After the crash"), "{}", agent.body());
}

fn kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    std::thread::sleep(std::time::Duration::from_millis(200));
}
