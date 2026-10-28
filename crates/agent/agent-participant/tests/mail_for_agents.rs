//! Mail is an app an agent can use: its records are held in a confined app
//! worker like every other app's, mail is delivered into one with no server
//! or account, and an agent reads it, searches it, drafts a reply and
//! exports it — the draft too — as content, through the operations the
//! record publishes, as written steps name them, none of them asking its
//! person first. Nothing is sent from the worker.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

// Records are held in confined app workers (this binary, started again);
// their changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use serde_json::{Value, json};
use tilo_actions::OperationKind;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_agent_participant::apps::readable;
use tilo_agent_participant::{AppCatalogue, ConfinedReplica, Replica, ReplicaActor, Seed};
use tilo_content::ContentStore;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const MAIL: &str = "so.tilo.mail";
/// The resource every mailbox-level operation is addressed to.
const MAILBOX: &str = "mail.mailbox";

fn arguments(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
}

/// The operation `action` the mailbox offers an agent.
fn operation(record: &ConfinedReplica, action: &str) -> ActionDescriptor {
    let offered = record.survey();
    offered
        .iter()
        .find(|descriptor| {
            descriptor.address.resource == MAILBOX && descriptor.address.action == action
        })
        .cloned()
        .unwrap_or_else(|| {
            let listed: Vec<String> = offered
                .iter()
                .map(|d| format!("{}#{}", d.address.resource, d.address.action))
                .collect();
            panic!("the mailbox offers {action}: {listed:?}")
        })
}

/// Run `action` on the mailbox with `args`, as a written step
/// `{"app": "so.tilo.mail", "resource": "mail.mailbox", "action", "arguments"}`
/// does — unconfirmed — and answer with its output.
fn step(record: &ConfinedReplica, action: &str, args: Value) -> tilo_actions::ActionOutput {
    record
        .invoke(
            &ActionAddress::new(MAILBOX, action),
            &arguments(args),
            false,
        )
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// What `read` answers with `args`.
fn read(record: &ConfinedReplica, args: Value) -> Value {
    step(record, "read", args)
        .value
        .expect("read answers with what the mailbox holds")
}

/// The content an export's answer names, as `room` keeps it.
fn produced(room: &ContentStore, output: &tilo_actions::ActionOutput) -> String {
    let address = output
        .value
        .as_ref()
        .and_then(|value| value["content"].as_str().map(str::to_owned))
        .or_else(|| output.resources.first().cloned())
        .unwrap_or_else(|| panic!("an export names its content: {output:?}"));
    let hash = tilo_actions::content_hash(&address).expect("a content address");
    String::from_utf8(room.read(hash).expect("the room keeps what was exported")).unwrap()
}

fn subjects(listing: &Value) -> Vec<String> {
    listing["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["subject"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn mail_is_in_the_agents_catalogue_but_not_the_browsers() {
    assert!(AppCatalogue::native().get(MAIL).is_some());
    assert!(
        tilo_agent_participant::component_apps()
            .iter()
            .all(|definition| definition.app_id != MAIL),
        "a browser page still cannot mount Mail"
    );
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn an_agent_reads_searches_drafts_and_exports_mail_delivered_without_a_server() {
    let room = ContentStore::memory();
    let catalogue = AppCatalogue::native();
    let agent = ConfinedReplica::open(
        &catalogue,
        MAIL,
        ROOM,
        "assistant",
        &ReplicaActor::Agent,
        Seed::New,
    )
    .expect("a Mail record is held in a confined worker")
    .lending(room.clone());
    assert_ne!(agent.worker_id(), Some(std::process::id()));
    let genesis = agent.take_mutation().unwrap().expect("a new record");

    // What the mailbox offers an agent.
    for action in ["deliver-message", "draft-message", "draft-reply"] {
        assert_eq!(operation(&agent, action).kind, OperationKind::Document);
    }
    assert!(
        readable(&operation(&agent, "read")),
        "a planner may read the mailbox while it plans"
    );
    assert!(
        readable(&operation(&agent, "search-messages")),
        "and search it"
    );
    assert_eq!(
        operation(&agent, "export-content").kind,
        OperationKind::Effect
    );

    // A scenario's mail, seeded with no network or account: a request and
    // an unrelated message.
    let request = step(
        &agent,
        "deliver-message",
        json!({
            "from": "Dana Ruiz <dana@example.test>",
            "to": "team@example.test",
            "subject": "Offsite venue",
            "body": "Please book the harbour venue for 14 people on 3 November.",
            "date": "2026-10-12T09:00:00Z",
        }),
    )
    .value
    .expect("a delivery answers with the message it kept");
    let request_id = request["id"].as_str().unwrap().to_owned();
    step(
        &agent,
        "deliver-message",
        json!({
            "from": "billing@example.test",
            "subject": "Invoice 4411",
            "body": "Your invoice is attached.",
            "date": "2026-10-12T10:00:00Z",
        }),
    );
    let seeded = agent
        .take_mutation()
        .unwrap()
        .expect("a delivery changes the mailbox, so it is news to a running task");

    // Reading and searching.
    let listing = read(&agent, json!({}));
    assert_eq!(
        subjects(&listing),
        ["Invoice 4411", "Offsite venue"],
        "newest first: {listing}"
    );
    let one = read(
        &agent,
        json!({ "item": format!("mail.message:{request_id}") }),
    );
    assert!(
        one["message"]["body"]
            .as_str()
            .unwrap()
            .contains("harbour venue"),
        "{one}"
    );
    let found = step(
        &agent,
        "search-messages",
        json!({ "query": "HARBOUR people" }),
    )
    .value
    .unwrap();
    assert_eq!(subjects(&found), ["Offsite venue"], "{found}");
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "reading changes nothing"
    );

    // Drafting a reply: written into the mailbox's draft, never sent.
    step(
        &agent,
        "draft-reply",
        json!({ "message": request_id, "body": "Booked for 14 on 3 November." }),
    );
    let drafted = agent.take_mutation().unwrap().expect("the draft");

    // Exporting as content the room keeps, the draft with the mail.
    let exported = step(&agent, "export-content", json!({ "format": "md" }));
    let markdown = produced(&room, &exported);
    for expected in [
        "## Offsite venue",
        "## Invoice 4411",
        "(draft)",
        "Booked for 14 on 3 November.",
    ] {
        assert!(markdown.contains(expected), "{expected}: {markdown}");
    }
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "an export changes nothing"
    );

    // A person's window on the same mailbox holds the same mail and draft.
    let DocumentMutation::Snapshot(snapshot) = genesis else {
        panic!("a new record is published whole");
    };
    let mut page = Replica::open(
        &catalogue,
        MAIL,
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
    page.apply(&drafted).unwrap();
    let (held, draft) = page
        .read(|doc| {
            let mail = <mail::resource::Mail as ui::tilo_resource::Resource>::read(doc).unwrap();
            (mail.held.len(), mail.draft.body)
        })
        .unwrap();
    assert_eq!(held, 2, "both messages are held");
    assert!(draft.contains("Booked for 14"), "{draft}");
}
