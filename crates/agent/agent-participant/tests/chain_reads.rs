//! A chat's messages read and listed through the door, whichever part of the
//! chain keeps them.
//!
//! A chat keeps its messages in numbered parts: a part takes the messages
//! posted while it is the chain's head, and once it is full the posts after it
//! start the next part. A caller naming the chat's messages — an agent's
//! `tilo_read` and `tilo_ls`, a page's catalogue — names the chain, not the
//! part holding the newest message: the door answers with the messages of the
//! parts this host holds, the newest part first, a page at a time, and each
//! message reads on its own from the part keeping it.
//!
//! The record is held the way a host holds it for the door, in a confined app
//! worker: the same replica a workspace's host asks for an action or a page.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard, and one scenario runs in order"
)]

mod common;

tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{CatalogueQuery, CatalogueRow};
use tilo_agent_participant::{ConfinedReplica, Replica, ReplicaActor, Seed};

/// The address the chat publishes its messages under.
fn messages() -> String {
    ui::record::chain_resource_id(&chat::parts::chain())
}

/// A chat a page founded: its record, with the channel the posts go in.
fn founded() -> Replica {
    let mut page = common::open("so.tilo.chat", "ada", ReplicaActor::Owner, Seed::New).unwrap();
    page.invoke(
        &ActionAddress::new("chat.workspace", "create-channel-with-id"),
        serde_json::json!({"id": "c_chat", "name": "chat", "category": null,
            "kind": null, "topic": null, "participants": null})
        .as_object()
        .unwrap(),
        false,
    )
    .expect("the chat takes a channel");
    page
}

/// The chat `page` founded, held as a host holds it for the door.
fn held(page: &mut Replica) -> ConfinedReplica {
    let Some(DocumentMutation::Snapshot(changes)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    common::confined(
        "so.tilo.chat",
        "host",
        &ReplicaActor::Owner,
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![changes],
        },
    )
    .expect("the chat opens in its worker")
}

/// Post `text` in the chat, through the door the host serves.
fn post(held: &ConfinedReplica, text: &str) {
    held.invoke(
        &ActionAddress::new(messages(), "post"),
        serde_json::json!({"channel": "c_chat", "text": text})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the post is applied");
}

/// Read the chat, through the door, with `arguments`.
fn read(
    held: &ConfinedReplica,
    resource: &str,
    arguments: &serde_json::Value,
) -> serde_json::Value {
    held.invoke(
        &ActionAddress::new(resource, "read"),
        &arguments.as_object().cloned().unwrap_or_default(),
        false,
    )
    .expect("the read is answered")
    .value
    .unwrap_or_else(|| panic!("{resource} answers what it read"))
}

/// The messages one page of a read answers, by the name each is shown under.
fn names(page: &serde_json::Value) -> Vec<String> {
    page.get("messages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// The rows `query` lists of `surveyed`, as the door pages them.
fn rows_of(surveyed: &[CatalogueRow], query: &CatalogueQuery) -> Vec<CatalogueRow> {
    let take = query.page_size();
    Replica::page_of(surveyed, query, (true, (0, take))).1
}

#[test]
fn a_chat_reads_and_lists_the_messages_of_every_part_its_host_holds() {
    let mut record = founded();
    let held = held(&mut record);
    // One part takes the messages posted while it is the head, until it holds
    // a part's worth of text: the posts that fill it, and then the part the
    // chain starts next, which is the head from then on.
    let mut filled = 0;
    while filled * chat::record::MAX_MESSAGE_TEXT_BYTES <= chat::parts::PART_TEXT_BYTES {
        let text = format!(
            "{filled}: {}",
            "x".repeat(chat::record::MAX_MESSAGE_TEXT_BYTES.saturating_sub(4))
        );
        post(&held, &text);
        filled = filled.saturating_add(1);
    }
    post(&held, "first of the newest part");
    post(&held, "second of the newest part");

    // What is inside the chat, through the door: the messages of the part the
    // head is, and the messages of the part before it, which the newest part
    // was started after. The newest part comes first.
    let surveyed = held.survey_rows();
    let listed: Vec<&CatalogueRow> = surveyed
        .iter()
        .filter(|row| row.parent_resource == messages())
        .collect();
    assert!(!listed.is_empty(), "the door lists the chat's messages");
    let at = |name: &str| {
        listed
            .iter()
            .position(|row| row.resource.label.starts_with(name))
            .unwrap_or_else(|| panic!("the door lists {name}"))
    };
    // The part the head is was started once the part before it was full: its
    // messages are listed first, and then those of the part before.
    let newest = at("second of the newest part");
    let newest_part = at("first of the newest part");
    let oldest_part = at("0: ");
    let filled_part = at(&format!("{}: ", filled.saturating_sub(1)));
    assert!(
        newest.min(newest_part) < oldest_part.min(filled_part),
        "the newest part's messages come first, then the part before: \
         {newest}, {newest_part}, {oldest_part}, {filled_part}"
    );
    assert!(
        oldest_part < filled_part,
        "the part before keeps its messages in the order they were posted: \
         {oldest_part} before {filled_part}"
    );

    // What a caller asking for those messages reads: only the rows under the
    // chain, and the words asked for.
    let asked = rows_of(
        &surveyed,
        &CatalogueQuery {
            provider: Some("host".into()),
            parent_resource: Some(messages()),
            max_results: 8,
            needle: "of the newest part".into(),
            ..CatalogueQuery::default()
        },
    );
    assert!(!asked.is_empty(), "the words asked for are listed");
    assert!(
        asked
            .iter()
            .all(|row| row.resource.label.contains("of the newest part")),
        "a page carries what was asked for"
    );

    // Reading the chat answers its messages from the newest, a page at a
    // time: the page after it goes on into the part before.
    let first = read(&held, &messages(), &serde_json::json!({"pageSize": 2}));
    assert_eq!(
        names(&first),
        ["second of the newest part", "first of the newest part"],
        "the newest messages first: {first}"
    );
    let cursor = first
        .get("nextCursor")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("a page follows: {first}"))
        .to_owned();
    let second = read(
        &held,
        &messages(),
        &serde_json::json!({"pageSize": 2, "cursor": cursor}),
    );
    let next = names(&second);
    assert_eq!(next.len(), 2, "a page of two: {second}");
    assert!(
        next[0].starts_with(&format!("{}: ", filled.saturating_sub(1))),
        "the page goes on with the oldest of the newest part: {next:?}"
    );
    assert!(
        next[1].starts_with(&format!("{}: ", filled.saturating_sub(2))),
        "and into the part before, newest first: {next:?}"
    );

    // One message of a part reads on its own, from the part keeping it,
    // whichever part that is.
    for name in ["second of the newest part", "0: "] {
        let Some(item) = listed
            .iter()
            .find(|row| row.resource.label.starts_with(name))
            .map(|row| row.resource.id.clone())
        else {
            panic!("the door lists {name}");
        };
        let message = read(&held, &item, &serde_json::json!({}));
        assert!(
            message
                .get("text")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|said| said.starts_with(name)),
            "{name} reads its content: {message}"
        );
    }
}
