//! Messages is an app an agent can use: its records hold the conversations
//! delivered into them, held in a confined app worker like every other
//! app's, and an agent reads them, searches them, queues replies and exports
//! them as content — through the operations the record publishes, as
//! written steps name them, none of them asking its person first. Nothing
//! is sent from the worker.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

// Records are held in confined app workers (this binary, started again);
// their changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use common::catalogue;
use serde_json::{Value, json};
use tilo_actions::OperationKind;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_agent_participant::apps::readable;
use tilo_agent_participant::{ConfinedReplica, Replica, ReplicaActor, Seed};
use tilo_content::ContentStore;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const MESSAGES: &str = "so.tilo.messages";
/// The resource every record-level operation is addressed to.
const RECORD: &str = "tilo.messages";
/// The conversation two SMS from one number land in.
const CONVERSATION: &str = "tilo.messages/conversations/+15550100";

fn arguments(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
}

/// The operation `action` the record offers an agent at its root.
fn operation(record: &ConfinedReplica, action: &str) -> ActionDescriptor {
    let offered = record.survey();
    offered
        .iter()
        .find(|descriptor| {
            descriptor.address.resource == RECORD && descriptor.address.action == action
        })
        .cloned()
        .unwrap_or_else(|| {
            let listed: Vec<String> = offered
                .iter()
                .map(|d| format!("{}#{}", d.address.resource, d.address.action))
                .collect();
            panic!("the record offers {action}: {listed:?}")
        })
}

/// Run `action` on the record with `args`, as a written step
/// `{"app": "so.tilo.messages", "resource": "tilo.messages", "action", "arguments"}`
/// does — unconfirmed — and answer with its output.
fn step(record: &ConfinedReplica, action: &str, args: Value) -> tilo_actions::ActionOutput {
    record
        .invoke(&ActionAddress::new(RECORD, action), &arguments(args), false)
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// What `read` answers with `args`.
fn read(record: &ConfinedReplica, args: Value) -> Value {
    step(record, "read", args)
        .value
        .expect("read answers with what the record holds")
}

/// The content `output` names, as `room` keeps it.
fn produced(room: &ContentStore, output: &tilo_actions::ActionOutput) -> String {
    let address = output
        .resources
        .first()
        .cloned()
        .or_else(|| {
            output
                .value
                .as_ref()
                .and_then(|value| value["content"].as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| panic!("one content resource: {output:?}"));
    let hash = tilo_actions::content_hash(&address).expect("a content address");
    String::from_utf8(room.read(hash).expect("the room keeps what was exported")).unwrap()
}

fn bodies(conversation: &Value) -> Vec<String> {
    conversation["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["body"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn messages_is_in_the_agents_catalogue() {
    assert!(catalogue().get(MESSAGES).is_some());
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn an_agent_reads_searches_answers_and_exports_sms_delivered_into_a_record() {
    let room = ContentStore::memory();
    let catalogue = catalogue();
    let agent = ConfinedReplica::open(
        &catalogue,
        MESSAGES,
        ROOM,
        "assistant",
        &ReplicaActor::Agent,
        Seed::New,
    )
    .expect("a Messages record is held in a confined worker")
    .lending(room.clone());
    assert_ne!(agent.worker_id(), Some(std::process::id()));
    let genesis = agent.take_mutation().unwrap().expect("a new record");

    // What the record offers an agent.
    for action in ["deliver-message", "send-message", "reply"] {
        assert_eq!(operation(&agent, action).kind, OperationKind::Document);
    }
    assert!(
        readable(&operation(&agent, "read")),
        "a planner may read the record while it plans"
    );
    assert!(
        readable(&operation(&agent, "search-messages")),
        "and search it"
    );
    let export = operation(&agent, "export-content");
    assert_eq!(export.kind, OperationKind::Effect);

    // Two SMS from one number, delivered as a harness event delivers them.
    let first = step(
        &agent,
        "deliver-message",
        json!({
            "from": "+1 555 0100",
            "body": "Are we still on for the museum on Saturday?",
            "date": "2026-10-11T09:00:00Z",
        }),
    );
    let answer = first
        .value
        .expect("a delivery answers with where it landed");
    assert_eq!(answer["conversation"], CONVERSATION, "{answer}");
    step(
        &agent,
        "deliver-message",
        json!({
            "from": "+15550100",
            "to": "+15550199",
            "body": "Tickets are 18 EUR each, I can book two.",
            "date": "2026-10-11T09:05:00Z",
        }),
    );
    let seeded = agent
        .take_mutation()
        .unwrap()
        .expect("a delivery changes the record, so it is news to a running task");

    // Reading: the conversations, then one whole.
    let listing = read(&agent, json!({}));
    assert_eq!(
        listing["conversations"].as_array().unwrap().len(),
        1,
        "{listing}"
    );
    assert_eq!(listing["conversations"][0]["id"], CONVERSATION);
    assert_eq!(listing["conversations"][0]["messages"], 2);
    let conversation = read(&agent, json!({ "item": CONVERSATION }));
    assert_eq!(
        bodies(&conversation),
        [
            "Are we still on for the museum on Saturday?",
            "Tickets are 18 EUR each, I can book two."
        ]
    );
    assert_eq!(conversation["messages"][1]["to"], "+15550199");
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "reading changes nothing"
    );

    // Searching.
    let found = step(&agent, "search-messages", json!({ "query": "TICKETS eur" }))
        .value
        .unwrap();
    assert_eq!(found["messages"].as_array().unwrap().len(), 1, "{found}");

    // Replying: queued in the conversation, never sent from here.
    step(
        &agent,
        "reply",
        json!({ "conversation": CONVERSATION, "body": "Yes! Please book two." }),
    );
    let conversation = read(&agent, json!({ "item": CONVERSATION }));
    let reply = &conversation["messages"][2];
    assert_eq!(reply["body"], "Yes! Please book two.", "{conversation}");
    assert_eq!(reply["direction"], "out");
    let replying = agent.take_mutation().unwrap().expect("the reply");

    // Exporting as content the room keeps, for a grader or a replay.
    let markdown = produced(
        &room,
        &step(
            &agent,
            "export-content",
            json!({ "format": "md", "conversation": CONVERSATION }),
        ),
    );
    for expected in [
        "Are we still on for the museum on Saturday?",
        "Tickets are 18 EUR each",
        "Yes! Please book two.",
    ] {
        assert!(markdown.contains(expected), "{expected}: {markdown}");
    }
    let exported: Value = serde_json::from_str(&produced(
        &room,
        &step(&agent, "export-content", json!({ "format": "json" })),
    ))
    .unwrap();
    assert_eq!(
        exported["conversations"][0]["messages"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "{exported}"
    );
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "an export changes nothing"
    );

    // A person's window on the same record holds the same conversation.
    let DocumentMutation::Snapshot(snapshot) = genesis else {
        panic!("a new record is published whole");
    };
    let mut page = Replica::open(
        &catalogue,
        MESSAGES,
        ROOM,
        "ada",
        ReplicaActor::Owner,
        Seed::Existing {
            record: agent.record_id().clone(),
            changes: vec![snapshot],
        },
    )
    .unwrap();
    page.apply(&seeded).unwrap();
    page.apply(&replying).unwrap();
    let held = page
        .read(|doc| {
            <messages::conversations::Messages as ui::tilo_resource::Resource>::read(doc)
                .unwrap()
                .conversations
                .values()
                .map(|conversation| conversation.messages.len())
                .sum::<usize>()
        })
        .unwrap();
    assert_eq!(held, 3, "two delivered, one queued");
}
