//! A chat's messages read and listed through the action API, whichever part of the
//! chain keeps them.
//!
//! A chat keeps its messages in numbered parts: a part takes the messages
//! posted while it is the chain's head, and once it is full the posts after it
//! start the next part. A caller naming the chat's messages — an agent's
//! `tilo_read` and `tilo_ls`, a page's catalogue — names the chain, not the
//! part holding the newest message: the action API answers with the messages of the
//! parts this host holds, the newest part first, a page at a time, and each
//! message reads on its own from the part keeping it.
//!
//! The record is held the way a host holds it for the action API, in a confined app
//! worker: the same replica a workspace's host asks for an action or a page.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard, and one scenario runs in order"
)]

use collab_types::DocumentMutation;
use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{CatalogueQuery, CatalogueRow};
use tilo_agent_session::{ConfinedReplica, Replica, ReplicaActor, Seed};

/// The address the chat publishes its messages under.
fn messages() -> String {
    ui::resource::chain_resource_id(&chat::parts::chain())
}

/// A chat a page founded: its record, with the channel the posts go in.
fn founded() -> Replica {
    let mut page = crate::common::open(
        "so.tilo.chat",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    page.invoke(
        &ActionAddress::new("chat.workspace", "create-channel-with-id"),
        serde_json::json!({"id": "c_chat", "name": "chat", "category": null,
            "kind": null, "topic": null, "members": null})
        .as_object()
        .unwrap(),
        false,
    )
    .expect("the chat takes a channel");
    page
}

/// The chat `page` founded, held as a host holds it for the action API.
fn held(page: &mut Replica) -> ConfinedReplica {
    let Some(DocumentMutation::Snapshot(changes)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    crate::common::confined(
        "so.tilo.chat",
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![changes],
        },
    )
    .expect("the chat opens in its worker")
}

/// Post `text` in the chat, through the action API the host serves.
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

/// Read the chat, through the action API, with `arguments`.
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

/// The messages one page of a read answers, by their text.
fn names(page: &serde_json::Value) -> Vec<String> {
    page.get("messages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message["value"]["text"].as_str())
        .map(str::to_owned)
        .collect()
}

/// The rows `query` lists of `surveyed`, as the action API pages them.
fn rows_of(surveyed: &[CatalogueRow], query: &CatalogueQuery) -> Vec<CatalogueRow> {
    let take = query.page_size();
    Replica::page_of(surveyed, query, (true, (0, take))).1
}

#[test]
fn a_chat_reads_and_lists_the_messages_of_every_part_its_host_holds() {
    let mut record = founded();
    let held = held(&mut record);
    // One part takes the messages posted while it is the head, as long as
    // they fit in it: the post that would overflow it starts the next part,
    // which is the head from then on.
    let mut filled = 0;
    while filled * chat::chat::MAX_MESSAGE_TEXT_BYTES <= ui::framework::PART_BYTES {
        let text = format!(
            "{filled}: {}",
            "x".repeat(chat::chat::MAX_MESSAGE_TEXT_BYTES.saturating_sub(4))
        );
        post(&held, &text);
        filled = filled.saturating_add(1);
    }
    post(&held, "first of the newest part");
    post(&held, "second of the newest part");

    // What is inside the chat, through the action API: the messages of the part the
    // head is, and the messages of the part before it, which the newest part
    // was started after. The newest part comes first.
    let surveyed = held.survey_rows();
    let listed: Vec<&CatalogueRow> = surveyed
        .iter()
        .filter(|row| row.parent_resource == messages())
        .collect();
    assert!(
        !listed.is_empty(),
        "the action API lists the chat's messages"
    );
    let at = |name: &str| {
        listed
            .iter()
            .position(|row| row.resource.label.starts_with(name))
            .unwrap_or_else(|| panic!("the action API lists {name}"))
    };
    // The part the head is was started once the part before it was full: its
    // messages are listed first, and then those of the part before.
    let newest = at("second of the newest part");
    let newest_part = at("first of the newest part");
    let oldest_part = at("0: ");
    let filled_part = at(&format!("{}: ", filled.saturating_sub(2)));
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
            panic!("the action API lists {name}");
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

/// How many documents `observed` read whole for the content they name.
fn documents_read_for_content(observed: &[ui::tilo_resource::profiling::Sample]) -> usize {
    observed
        .iter()
        .filter(|sample| sample.phase == "app_worker.content_named")
        .map(|sample| sample.calls)
        .sum()
}

#[test]
fn the_content_a_chain_names_is_not_read_again_for_the_workers_own_writes() {
    let mut page = founded();
    let held = held(&mut page).lending(tilo_content::ContentStore::memory());
    let mut filled = 0;
    while filled * chat::chat::MAX_MESSAGE_TEXT_BYTES <= ui::framework::PART_BYTES {
        let text = format!(
            "{filled}: {}",
            "x".repeat(chat::chat::MAX_MESSAGE_TEXT_BYTES.saturating_sub(4))
        );
        post(&held, &text);
        filled = filled.saturating_add(1);
    }
    post(&held, "first of the newest part");
    taken(&held);

    // An effect is given the content its document and every part name.
    let ((), first) = ui::tilo_resource::profiling::observe(|| held.put_held_content().unwrap());
    assert!(documents_read_for_content(&first) >= 1, "{first:?}");
    // Nothing moved: nothing is read again, however much the chain holds.
    let ((), again) = ui::tilo_resource::profiling::observe(|| held.put_held_content().unwrap());
    assert_eq!(documents_read_for_content(&again), 0, "{again:?}");
    // A post the worker made itself names only content it made and holds:
    // the part it wrote waits unapplied, and nothing is read again for it.
    post(&held, "second of the newest part");
    taken(&held);
    let ((), moved) = ui::tilo_resource::profiling::observe(|| held.put_held_content().unwrap());
    assert_eq!(documents_read_for_content(&moved), 0, "{moved:?}");
}

/// Take what the document and its parts changed, as the server does after each
/// action it saves.
fn taken(held: &ConfinedReplica) {
    held.take_mutation().unwrap();
    held.take_document_mutations().unwrap();
}

/// The resources a page under the chat's messages lists, in order.
fn listed_ids(held: &ConfinedReplica, window: (usize, usize)) -> (Vec<String>, bool) {
    let query = CatalogueQuery {
        provider: Some("host".into()),
        parent_resource: Some(messages()),
        ..CatalogueQuery::default()
    };
    let (_, rows, arriving) = held.survey_page_arriving(&query, window);
    let mut ids: Vec<String> = Vec::new();
    for row in rows {
        if ids.last() != Some(&row.resource.id) {
            ids.push(row.resource.id.clone());
        }
    }
    (ids, arriving)
}

/// A host that holds only a chat's newest part — one that just started, sent nothing more by
/// the workspace — lists a page reaching past that part only once it holds the part before:
/// the page says it reads a part not here yet, and asks the workspace's hosts for it. It
/// answered the newest part's messages as the whole chat, and a caller paging through it read
/// that the chat ended there.
#[test]
fn a_page_reaching_past_the_parts_a_host_holds_asks_for_the_one_before() {
    let mut record = founded();
    let writer = held(&mut record);
    let mut filled = 0;
    while filled * chat::chat::MAX_MESSAGE_TEXT_BYTES <= ui::framework::PART_BYTES {
        let text = format!(
            "{filled}: {}",
            "x".repeat(chat::chat::MAX_MESSAGE_TEXT_BYTES.saturating_sub(4))
        );
        post(&writer, &text);
        filled = filled.saturating_add(1);
    }
    post(&writer, "first of the newest part");
    post(&writer, "second of the newest part");
    taken(&writer);
    let whole = (0, 1_000_000);
    let (every, arriving) = listed_ids(&writer, whole);
    assert!(!arriving, "the writer holds every part");
    assert!(
        every.len() > filled,
        "both parts are listed: {}",
        every.len()
    );
    let saved = |id: &ui::resource::DocumentId| {
        writer
            .read_resource(id, |doc| doc.clone().save())
            .unwrap_or_else(|| panic!("the writer holds {}", id.as_str()))
    };
    let (oldest, newest) = (saved(&chat::parts::part(0)), saved(&chat::parts::part(1)));

    // A host holding the record and the newest part, as one that just started does.
    let fresh = ConfinedReplica::open(
        &crate::common::catalogue(),
        "so.tilo.chat",
        crate::common::WORKSPACE,
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: record.document_id().clone(),
            changes: vec![writer.read(|doc| doc.clone().save()).unwrap()],
        },
        true,
    )
    .expect("the chat opens in its worker");
    fresh
        .apply_document(&chat::parts::part(1), &DocumentMutation::Snapshot(newest))
        .unwrap();
    // A first page the newest part fills is answered from it.
    let (first, arriving) = listed_ids(&fresh, (0, 20));
    assert!(!arriving, "a page the newest part fills reads nothing more");
    assert_eq!(
        first,
        listed_ids(&writer, (0, 20)).0,
        "the first page is the chat's"
    );
    // A page reaching past it reads the part before, which it asks for.
    let (short, arriving) = listed_ids(&fresh, whole);
    assert!(
        arriving,
        "a page reaching past the newest part says it reads one not here: {} of {}",
        short.len(),
        every.len()
    );
    assert!(
        fresh.wanted_resources().contains(&chat::parts::part(0)),
        "the part before is asked for: {:?}",
        fresh.wanted_resources()
    );
    // Handed it, as the host hands what was asked for, the page is the chat's.
    fresh
        .apply_document(&chat::parts::part(0), &DocumentMutation::Snapshot(oldest))
        .unwrap();
    let (after, arriving) = listed_ids(&fresh, whole);
    assert!(!arriving, "every part is here");
    assert_eq!(
        after, every,
        "the page lists both parts, as the writer's does"
    );
}
