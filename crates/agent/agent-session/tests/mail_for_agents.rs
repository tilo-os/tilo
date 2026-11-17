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

use collab_types::DocumentMutation;
use serde_json::{Value, json};
use tilo_actions::MemberId;
use tilo_actions::OperationKind;
use tilo_actions::api::{ActionAddress, ActionDescriptor};
use tilo_agent_session::apps::readable;
use tilo_agent_session::{AppCatalogue, ConfinedReplica, Replica, ReplicaActor, Seed};
use tilo_content::ContentStore;

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const MAIL: &str = "so.tilo.mail";
/// The resource every mailbox-level operation is addressed to.
const MAILBOX: &str = "mail.mailbox";

fn arguments(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
}

/// The address the mailbox's messages are published under, beside it.
fn messages(mailbox: &ConfinedReplica) -> String {
    ui::resource::chain_resource_id(
        &<mail::part::MessagePart as ui::framework::Chained>::chain_beside(mailbox.document_id()),
    )
}

/// The operation `action` the mailbox offers an agent.
fn operation(mailbox: &ConfinedReplica, action: &str) -> ActionDescriptor {
    operation_on(mailbox, MAILBOX, action)
}

/// The operation `action` the resource `resource` offers an agent.
fn operation_on(mailbox: &ConfinedReplica, resource: &str, action: &str) -> ActionDescriptor {
    let offered = mailbox.survey();
    offered
        .iter()
        .find(|descriptor| {
            descriptor.address.resource == resource && descriptor.address.action == action
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
fn step(mailbox: &ConfinedReplica, action: &str, args: Value) -> tilo_actions::ActionOutput {
    step_on(mailbox, MAILBOX, action, args)
}

/// Invoke `action` on the resource `resource` with `args`.
fn step_on(
    mailbox: &ConfinedReplica,
    resource: &str,
    action: &str,
    args: Value,
) -> tilo_actions::ActionOutput {
    mailbox
        .invoke(
            &ActionAddress::new(resource, action),
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

/// The content an export's answer names, as `workspace` keeps it.
fn produced(workspace: &ContentStore, output: &tilo_actions::ActionOutput) -> String {
    let address = output
        .value
        .as_ref()
        .and_then(|value| value["content"].as_str().map(str::to_owned))
        .or_else(|| output.resources.first().cloned())
        .unwrap_or_else(|| panic!("an export names its content: {output:?}"));
    let hash = tilo_actions::content_hash(&address).expect("a content address");
    String::from_utf8(
        workspace
            .read(hash)
            .expect("the workspace keeps what was exported"),
    )
    .unwrap()
}

fn subjects(listing: &Value) -> Vec<String> {
    listing["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["subject"].as_str().unwrap().to_owned())
        .collect()
}

/// The held messages a read of the mailbox's messages lists, by name, newest
/// first.
fn held_names(listing: &Value) -> Vec<String> {
    listing["held"]
        .as_array()
        .map(|held| {
            held.iter()
                .map(|message| message["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn mail_is_in_the_agents_catalogue_and_the_browsers() {
    assert!(AppCatalogue::native().get(MAIL).is_some());
    assert!(
        tilo_agent_session::component_apps()
            .iter()
            .any(|definition| definition.app_id == MAIL),
        "a browser page mounts Mail"
    );
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn an_agent_reads_searches_drafts_and_exports_mail_delivered_without_a_server() {
    let workspace = ContentStore::memory();
    let catalogue = AppCatalogue::native();
    let agent = ConfinedReplica::open(
        &catalogue,
        MAIL,
        WORKSPACE,
        &MemberId::agent("assistant"),
        &ReplicaActor::Agent {
            person: MemberId::person("maker"),
        },
        Seed::New,
    )
    .expect("a Mail record is held in a confined worker")
    .lending(workspace.clone());
    assert_ne!(agent.worker_id(), Some(std::process::id()));
    let genesis = agent.take_mutation().unwrap().expect("a new record");

    // What the mailbox offers an agent: drafting on the mailbox, delivering
    // on the messages kept beside it.
    let beside = messages(&agent);
    assert_eq!(
        operation(&agent, "draft-message").kind,
        OperationKind::Document
    );
    // A chain's verbs are carried out on the part they change, through the
    // chain: offered all the same.
    operation_on(&agent, &beside, "deliver-message");
    // A reply is drafted from the message wherever it is kept, fetched or
    // held, so the view answers it and records the draft.
    assert_eq!(operation(&agent, "draft-reply").kind, OperationKind::Effect);
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
    let request = step_on(
        &agent,
        &beside,
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
    step_on(
        &agent,
        &beside,
        "deliver-message",
        json!({
            "from": "billing@example.test",
            "subject": "Invoice 4411",
            "body": "Your invoice is attached.",
            "date": "2026-10-12T10:00:00Z",
        }),
    );

    // Reading and searching.
    let listing = step_on(&agent, &beside, "read", json!({}))
        .value
        .expect("read answers with what the messages hold");
    assert_eq!(
        held_names(&listing),
        ["Invoice 4411", "Offsite venue"],
        "newest first: {listing}"
    );
    let item = listing["held"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|held| {
            held["item"]
                .as_str()
                .filter(|item| item.ends_with(&request_id))
                .map(str::to_owned)
        })
        .expect("the request is listed");
    let one = step_on(&agent, &item, "read", json!({})).value.unwrap();
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

    // Drafting a reply: a draft of its own each time, never sent.
    let reply = step(
        &agent,
        "draft-reply",
        json!({
            "message": request_id,
            "body": "Booked for 14 on 3 November.",
            "notes": "Check the harbour venue's catering first.",
        }),
    )
    .value
    .expect("a reply answers with its draft");
    assert_eq!(
        reply["draft"]["to"], "Dana Ruiz <dana@example.test>",
        "{reply}"
    );
    assert_eq!(reply["draft"]["subject"], "Re: Offsite venue", "{reply}");
    let drafted = agent.take_mutation().unwrap().expect("the draft");

    // Exporting as content the workspace keeps, the draft with the mail.
    let exported = step(&agent, "export-content", json!({ "format": "md" }));
    let markdown = produced(&workspace, &exported);
    for expected in [
        "## Offsite venue",
        "## Invoice 4411",
        "(draft)",
        "Booked for 14 on 3 November.",
    ] {
        assert!(markdown.contains(expected), "{expected}: {markdown}");
    }
    assert!(
        !markdown.contains("catering"),
        "a draft's notes are private: {markdown}"
    );
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
        WORKSPACE,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::Existing {
            record: agent.document_id().clone(),
            changes: vec![snapshot],
        },
    )
    .unwrap();
    page.apply(&drafted).unwrap();
    let drafts = page
        .read(|doc| {
            let mail = <mail::resource::Mail as ui::tilo_resource::Resource>::read(doc).unwrap();
            mail.drafts
                .values()
                .map(|draft| (draft.body.clone(), draft.notes.clone()))
                .collect::<Vec<(String, String)>>()
        })
        .unwrap();
    let [(body, notes)] = drafts.as_slice() else {
        panic!("one draft: {drafts:?}");
    };
    assert!(body.contains("Booked for 14"), "{body}");
    assert!(notes.contains("catering"), "{notes}");
}

/// Adding a moved account is offered to an agent as an action reaching
/// outside the record, which an agent's grants put to its person before it
/// runs, unless the person allowed it.
#[test]
fn an_agent_is_offered_adding_an_account_its_person_is_asked_for() {
    let agent = ConfinedReplica::open(
        &AppCatalogue::native(),
        MAIL,
        WORKSPACE,
        &MemberId::agent("assistant"),
        &ReplicaActor::Agent {
            person: MemberId::person("maker"),
        },
        Seed::New,
    )
    .expect("a Mail record is held in a confined worker");
    let add = operation(&agent, "add-account");
    assert_eq!(add.kind, OperationKind::Effect);
    assert_eq!(
        add.contract.kind,
        tilo_action_policy::ActionKind::Outside,
        "{add:?}"
    );
    assert_eq!(
        tilo_action_policy::agent_answers::default_answer(add.contract.kind, false),
        tilo_action_policy::Answer::Ask,
        "an agent asks its person first"
    );
}
