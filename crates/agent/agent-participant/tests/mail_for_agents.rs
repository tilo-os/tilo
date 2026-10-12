//! Mail is an app an agent can use: its records are held in a confined app
//! worker like every other app's, mail is delivered into one with no server
//! or account, and an agent reads it, searches it, drafts replies into
//! Drafts and exports it as content — through the operations the record
//! publishes, as written steps name them.

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
use tilo_agent_participant::planner::readable;
use tilo_agent_participant::{AppCatalogue, ConfinedReplica, Replica, Seed};
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

/// The operation `action` the mailbox publishes, effects and views too.
fn operation(record: &ConfinedReplica, action: &str) -> ActionDescriptor {
    record
        .survey_with_effects()
        .into_iter()
        .find(|descriptor| {
            descriptor.address.resource == MAILBOX && descriptor.address.action == action
        })
        .unwrap_or_else(|| {
            let listed: Vec<String> = record
                .survey_with_effects()
                .iter()
                .map(|d| format!("{}#{}", d.address.resource, d.address.action))
                .collect();
            panic!("the mailbox publishes {action}: {listed:?}")
        })
}

/// Run `action` on the mailbox with `args`, as a written step
/// `{"app": "so.tilo.mail", "resource": "mail.mailbox", "action", "arguments"}`
/// does, and answer with its output.
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

/// The content `output` names, as `room` keeps it.
fn produced(room: &ContentStore, output: &tilo_actions::ActionOutput) -> String {
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = tilo_actions::content_hash(address).expect("a content address");
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
    let agent = ConfinedReplica::open(&catalogue, MAIL, ROOM, "assistant", true, Seed::New)
        .expect("a Mail record is held in a confined worker")
        .with_content(room.clone());
    assert_ne!(agent.worker_id(), Some(std::process::id()));
    let genesis = agent.take_mutation().unwrap().expect("a new record");

    // What the mailbox offers an agent.
    let deliver = operation(&agent, "deliver-message");
    assert_eq!(deliver.kind, OperationKind::Document);
    for action in ["draft-message", "draft-reply"] {
        assert_eq!(operation(&agent, action).kind, OperationKind::Document);
    }
    let listing = operation(&agent, "read");
    assert!(
        readable(&listing),
        "a planner may read the mailbox while it plans"
    );
    let search = operation(&agent, "search-messages");
    assert!(readable(&search), "and search it");
    let export = operation(&agent, "export-content");
    assert_eq!(export.kind, OperationKind::Effect);
    assert!(
        !readable(&export),
        "an export is a step of the plan, not a read"
    );

    // A scenario's mail, seeded with no network or account: a request, an
    // unrelated message, then a correction to the request.
    step(
        &agent,
        "deliver-message",
        json!({
            "from": "Ada Lovelace <ada@example.test>",
            "to": "me@example.test",
            "subject": "Budget for the offsite",
            "body": "Hi,\n\nPlease plan with a budget of 40k.\n\nAda",
            "date": "2026-10-11T09:00:00Z",
            "message_id": "budget-1@example.test",
        }),
    );
    step(
        &agent,
        "deliver-message",
        json!({
            "from": "newsletter@example.test",
            "subject": "Weekly digest",
            "body": "Nothing to do here.",
            "date": "2026-10-11T10:00:00Z",
        }),
    );
    let corrected = step(
        &agent,
        "deliver-message",
        json!({
            "from": "Ada Lovelace <ada@example.test>",
            "to": "me@example.test",
            "cc": "Bob <bob@example.test>",
            "subject": "Re: Budget for the offsite",
            "body": "Correction: the budget is 45k, not 40k.",
            "date": "2026-10-11T11:30:00Z",
            "in_reply_to": "budget-1@example.test",
        }),
    );
    if let Some(answer) = &corrected.value {
        assert!(
            answer["message"]
                .as_str()
                .is_some_and(|item| item.starts_with("mail.message:")),
            "{answer}"
        );
    }
    // A seed given twice delivers once.
    step(
        &agent,
        "deliver-message",
        json!({
            "from": "newsletter@example.test",
            "subject": "Weekly digest",
            "body": "Nothing to do here.",
            "date": "2026-10-11T10:00:00Z",
        }),
    );
    let seeded = agent.take_mutation().unwrap().expect("the deliveries");

    // Reading: the mailbox, newest first, with snippets.
    let mailbox = read(&agent, json!({}));
    assert_eq!(
        subjects(&mailbox),
        [
            "Re: Budget for the offsite",
            "Weekly digest",
            "Budget for the offsite"
        ],
        "{mailbox}"
    );
    let correction = &mailbox["messages"][0];
    assert_eq!(correction["folder"], "inbox");
    assert_eq!(correction["read"], false);
    assert_eq!(correction["date"], "2026-10-11T11:30:00Z");
    assert_eq!(
        correction["snippet"],
        "Correction: the budget is 45k, not 40k."
    );
    assert_eq!(mailbox["folders"][0]["unread"], 3, "{mailbox}");
    let correction_item = correction["item"].as_str().unwrap().to_owned();

    // One message whole, with its conversation.
    let opened = read(&agent, json!({ "item": correction_item }));
    assert_eq!(opened["body"], "Correction: the budget is 45k, not 40k.");
    assert_eq!(opened["cc"], "Bob <bob@example.test>");
    assert_eq!(opened["in_reply_to"], "budget-1@example.test");
    assert_eq!(
        opened["conversation"].as_array().unwrap().len(),
        2,
        "{opened}"
    );
    let thread = read(&agent, json!({ "item": opened["thread"] }));
    let bodies: Vec<&str> = thread["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].contains("40k") && bodies[1].contains("45k"));
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "reading changes nothing"
    );

    // Searching.
    let found = step(&agent, "search-messages", json!({ "query": "BUDGET 45k" }))
        .value
        .unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 1, "{found}");
    assert_eq!(found["matches"][0]["item"], correction_item.as_str());
    let none = step(
        &agent,
        "search-messages",
        json!({ "query": "budget", "folder": "archive" }),
    )
    .value
    .unwrap();
    assert!(none["matches"].as_array().unwrap().is_empty());

    // Drafting a reply: into Drafts, to Ada (and Bob, replying to all), in
    // the conversation, quoting her; nothing is sent.
    let drafted = step(
        &agent,
        "draft-reply",
        json!({
            "message": correction_item,
            "body": "Thanks Ada, I will plan with 45k.",
            "reply_all": true,
        }),
    );
    if let Some(answer) = &drafted.value {
        assert!(answer["message"].is_string(), "{answer}");
    }
    step(
        &agent,
        "draft-message",
        json!({
            "to": "bob@example.test",
            "subject": "Venue",
            "body": "Can you book the venue?",
        }),
    );
    let drafts = read(&agent, json!({ "item": "mail.folder:drafts" }));
    let mut drafted_subjects = subjects(&drafts);
    drafted_subjects.sort();
    assert_eq!(
        drafted_subjects,
        ["Re: Budget for the offsite", "Venue"],
        "{drafts}"
    );
    let reply = drafts["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["subject"] == "Re: Budget for the offsite")
        .unwrap();
    assert_eq!(reply["to"], "Ada Lovelace <ada@example.test>");
    assert_eq!(reply["from"], "me@example.test");
    assert_eq!(reply["read"], true);
    let reply = read(&agent, json!({ "item": reply["item"] }));
    assert_eq!(reply["cc"], "Bob <bob@example.test>");
    assert!(
        reply["body"]
            .as_str()
            .unwrap()
            .starts_with("Thanks Ada, I will plan with 45k.\n\nOn 2026-10-11T11:30:00Z"),
        "{reply}"
    );
    let reply_item = reply["item"].as_str().unwrap().to_owned();
    // Revising the draft keeps one draft.
    step(
        &agent,
        "draft-reply",
        json!({
            "message": correction_item,
            "body": "Thanks Ada, 45k it is.",
            "id": reply_item,
        }),
    );
    let revised = read(&agent, json!({ "item": reply_item }));
    assert!(
        revised["body"]
            .as_str()
            .unwrap()
            .starts_with("Thanks Ada, 45k it is.")
    );
    assert_eq!(
        read(&agent, json!({ "item": "mail.folder:drafts" }))["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        read(&agent, json!({})).get("server_messages").is_none(),
        "every message is held whole"
    );
    let drafting = agent.take_mutation().unwrap().expect("the drafts");

    // Exporting as content the room keeps, for a grader or a replay.
    let markdown = produced(
        &room,
        &step(&agent, "export-content", json!({ "format": "md" })),
    );
    assert!(markdown.starts_with("# Mailbox"), "{markdown}");
    for expected in [
        "## Budget for the offsite",
        "Correction: the budget is 45k",
        "- Folder: Drafts",
        "Thanks Ada, 45k it is.",
    ] {
        assert!(markdown.contains(expected), "{expected}: {markdown}");
    }
    let mbox = produced(
        &room,
        &step(
            &agent,
            "export-content",
            json!({ "format": "mbox", "folder": "drafts" }),
        ),
    );
    assert_eq!(mbox.matches("X-Tilo-Folder: drafts").count(), 2, "{mbox}");
    let eml = produced(
        &room,
        &step(
            &agent,
            "export-content",
            json!({ "format": "eml", "message": reply_item }),
        ),
    );
    assert!(eml.contains("In-Reply-To: <"), "{eml}");
    assert!(eml.contains("Subject: Re: Budget for the offsite"), "{eml}");
    let conversation = produced(
        &room,
        &step(
            &agent,
            "export-content",
            json!({ "format": "json", "thread": correction_item }),
        ),
    );
    let conversation: Value = serde_json::from_str(&conversation).unwrap();
    assert_eq!(conversation.as_array().unwrap().len(), 3, "{conversation}");
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "an export changes nothing"
    );

    // A person's window on the same record holds the same mail.
    let DocumentMutation::Snapshot(snapshot) = genesis else {
        panic!("a new record is published whole");
    };
    let mut page = Replica::open(
        &catalogue,
        MAIL,
        ROOM,
        "ada",
        false,
        Seed::Existing {
            record: agent.record_id().clone(),
            changes: vec![snapshot],
        },
    )
    .unwrap();
    page.apply(&seeded).unwrap();
    page.apply(&drafting).unwrap();
    let held = page.read(mail::doc::MailDoc::local_messages).unwrap();
    assert_eq!(held.len(), 5, "three delivered, two drafted");
}

#[test]
fn a_delivery_names_a_folder_and_a_date_mail_can_hold() {
    let agent = ConfinedReplica::open(
        &AppCatalogue::native(),
        MAIL,
        ROOM,
        "seeder",
        true,
        Seed::New,
    )
    .unwrap();
    agent.take_mutation().unwrap();
    let refused = |args: Value| {
        agent
            .invoke(
                &ActionAddress::new(MAILBOX, "deliver-message"),
                &arguments(args),
                false,
            )
            .is_err()
    };
    assert!(refused(json!({
        "from": "ada@example.test", "subject": "s", "body": "b", "date": "next tuesday"
    })));
    assert!(refused(json!({
        "from": "ada@example.test", "subject": "s", "body": "b", "folder": "nowhere"
    })));
    assert!(refused(
        json!({ "from": "nobody", "subject": "s", "body": "b" })
    ));
    assert!(
        agent
            .invoke(
                &ActionAddress::new(MAILBOX, "draft-reply"),
                &arguments(json!({ "message": "mail.message:nope", "body": "b" })),
                false,
            )
            .is_err(),
        "a reply needs a message to answer"
    );
    step(
        &agent,
        "deliver-message",
        json!({
            "from": "ada@example.test", "subject": "Old", "body": "b",
            "date": "Sun, 11 Oct 2026 09:00:00 +0200", "folder": "archive", "read": true,
        }),
    );
    let archived = read(&agent, json!({ "item": "mail.folder:archive" }));
    assert_eq!(archived["messages"][0]["date"], "2026-10-11T07:00:00Z");
    assert_eq!(archived["messages"][0]["read"], true);
}
