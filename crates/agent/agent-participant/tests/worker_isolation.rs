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

/// A record a host creates under the room's default route is the record
/// every member's first window opens: its genesis names that id, not one
/// of its own, or a member's store refuses it as not matching its binding.
#[test]
fn a_confined_record_created_under_a_room_route_names_that_route() {
    assert_seed_route("so.tilo.scratchpad", ui::record::RecordId::room_default());
}

#[test]
fn a_confined_room_record_is_created_under_the_package_primary_route() {
    assert_seed_route("so.tilo.notes", ui::record::RecordId::package_primary());
}

fn assert_seed_route(app: &str, default: ui::record::RecordId) {
    let catalogue = catalogue();
    let source = ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "host",
        &tilo_agent_participant::ReplicaActor::Owner,
        Seed::NewAt(default.clone()),
    )
    .unwrap();
    assert_eq!(source.record(), default.as_str());
    let Some(DocumentMutation::Snapshot(genesis)) = source.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let doc = automerge::AutoCommit::load(&genesis).unwrap();
    let kind = catalogue.get(app).unwrap().record_kind.clone();
    if default.is_package_primary() {
        // A package primary uses its store namespace, not a minted identity.
        assert_eq!(ui::record::identity(&doc), None);
    } else {
        assert_eq!(ui::record::room_scope(&doc).as_deref(), Some(ROOM));
        assert_eq!(
            ui::record::identity(&doc),
            Some(ui::record::RecordCheckout::new(default, kind))
        );
    }
    // Both route shapes must reopen through the actual confined factory.
    ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "member",
        &tilo_agent_participant::ReplicaActor::Owner,
        Seed::Existing {
            record: source.record_id().clone(),
            changes: vec![genesis],
        },
    )
    .unwrap();
}

fn edit(text: &str) -> serde_json::Map<String, serde_json::Value> {
    common::scratchpad_body(text).1
}

#[test]
fn a_confined_record_is_edited_in_its_worker_and_converges_with_a_page() {
    let catalogue = catalogue();
    let app = "so.tilo.scratchpad";
    let note = common::scratchpad_body("").0;
    let mut page = Replica::open(
        &catalogue,
        app,
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation() else {
        panic!("a new record is published whole");
    };
    let agent = ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        &tilo_agent_participant::ReplicaActor::Agent,
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
        &tilo_agent_participant::ReplicaActor::Agent,
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
        &tilo_agent_participant::ReplicaActor::Agent,
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

/// The appearances the context entries of `change`'s changes name; a
/// snapshot, which an edit never is, names none.
fn appearances(change: &DocumentMutation) -> Vec<Option<tilo_actions::Appearance>> {
    let DocumentMutation::Incremental(bytes) = change else {
        return Vec::new();
    };
    collab_types::parse_changes(bytes)
        .unwrap()
        .iter()
        .filter_map(automerge::Change::message)
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter_map(|context| context.invoker)
        .map(|invoker| invoker.appearance)
        .collect()
}

#[test]
fn an_appearance_given_to_the_worker_is_recorded_on_the_change() {
    let catalogue = catalogue();
    let app = "so.tilo.scratchpad";
    let note = common::scratchpad_body("").0;
    let mut page = Replica::open(
        &catalogue,
        app,
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation() else {
        panic!("a new record is published whole");
    };
    let agent = ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        &tilo_agent_participant::ReplicaActor::Agent,
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![snapshot],
        },
    )
    .unwrap();
    let alice = tilo_actions::Appearance::new("alice", None, None).unwrap();
    agent
        .invoke_appearing(&note, &edit("From Discord"), false, Some(&alice))
        .unwrap();
    let shown = appearances(&agent.take_mutation().unwrap().unwrap());
    assert!(!shown.is_empty(), "the change names its invoker");
    assert!(
        shown.iter().all(|shown| shown.as_ref() == Some(&alice)),
        "{shown:?}"
    );

    // A plain invocation shows none.
    agent.invoke(&note, &edit("Plain"), false).unwrap();
    let shown = appearances(&agent.take_mutation().unwrap().unwrap());
    assert!(!shown.is_empty(), "the change names its invoker");
    assert!(shown.iter().all(Option::is_none), "{shown:?}");
}
