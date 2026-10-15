#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::json;
use ui::framework::testing::TestApp;

use super::{conversation_key, derived_id};
use crate::{MessagesEffect, MessagesShell};

fn app() -> TestApp<MessagesShell> {
    TestApp::<MessagesShell>::for_person("messages-fixture-owner").expect("headless app")
}

/// Deliver an SMS from `from` at `date`, answering what the record said.
fn deliver(
    app: &mut TestApp<MessagesShell>,
    from: &str,
    body: &str,
    date: &str,
) -> serde_json::Value {
    app.call(
        "deliver-message",
        json!({ "from": from, "body": body, "date": date }),
    )
    .unwrap()
    .expect("a delivery answers with its conversation")
}

fn exported(app: &mut TestApp<MessagesShell>, effect: &MessagesEffect) -> String {
    let output = app.effect(effect).expect("exports");
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = ui::tilo_actions::content_hash(address).expect("a content address");
    String::from_utf8(app.content().read(hash).expect("kept")).unwrap()
}

#[test]
fn conversation_ids_are_derived_from_who_it_is_with() {
    assert_eq!(
        conversation_key("+1 (555) 010-0199").as_deref(),
        Some("+15550100199")
    );
    assert_eq!(
        conversation_key("+15550100199").as_deref(),
        Some("+15550100199")
    );
    assert_eq!(
        conversation_key("Ada Lovelace").as_deref(),
        Some("ada-lovelace")
    );
    assert_eq!(
        conversation_key("messages.conversation:ada-lovelace").as_deref(),
        Some("ada-lovelace")
    );
    assert_eq!(conversation_key("  Zoë  & co. ").as_deref(), Some("zoë-co"));
    assert_eq!(conversation_key("--- ").as_deref(), None);
    assert_ne!(derived_id(&["a", "b"]), derived_id(&["ab", ""]));
}

#[test]
fn the_record_publishes_reading_delivery_queueing_search_and_export() {
    let mut app = app();
    let surface = app.agent_surface().to_string();
    for action in [
        "\"deliver-message\"",
        "\"send-message\"",
        "\"reply\"",
        "\"read\"",
        "\"search-messages\"",
        "\"export-content\"",
    ] {
        assert!(surface.contains(action), "{action}: {surface}");
    }
}

#[test]
fn delivered_sms_land_in_one_conversation_per_sender_in_time_order() {
    let mut app = app();
    let later = deliver(
        &mut app,
        "+1 555 0100",
        "See you at 7?",
        "2026-10-11T18:05:00Z",
    );
    assert_eq!(later["conversation"], "messages.conversation:+15550100");
    assert!(
        later["message"]
            .as_str()
            .is_some_and(|id| id.starts_with("messages.message:m")),
        "{later}"
    );
    // Delivered out of order, it still reads first.
    deliver(
        &mut app,
        "+15550100",
        "Dinner tonight?",
        "2026-10-11T18:00:00+00:00",
    );
    deliver(
        &mut app,
        "Ada Lovelace",
        "The museum opens at 10.",
        "2026-10-11T08:00:00Z",
    );

    let listing = app.call("read", json!({})).unwrap().unwrap();
    let conversations = listing["conversations"].as_array().unwrap();
    assert_eq!(conversations.len(), 2, "{listing}");
    assert_eq!(conversations[0]["id"], "messages.conversation:+15550100");
    assert_eq!(conversations[0]["messages"], 2);
    assert_eq!(conversations[0]["last"]["snippet"], "See you at 7?");
    assert_eq!(conversations[1]["with"], "Ada Lovelace");

    let thread = app
        .call("read", json!({ "item": "messages.conversation:+15550100" }))
        .unwrap()
        .unwrap();
    let bodies: Vec<&str> = thread["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["Dinner tonight?", "See you at 7?"]);
    assert_eq!(thread["messages"][0]["date"], "2026-10-11T18:00:00Z");
    assert_eq!(thread["messages"][0]["direction"], "in");

    let one = app
        .call("read", json!({ "item": later["message"] }))
        .unwrap()
        .unwrap();
    assert_eq!(one["body"], "See you at 7?");
}

#[test]
fn delivering_the_same_message_twice_changes_nothing() {
    let mut app = app();
    deliver(&mut app, "Ada", "Hello", "2026-10-11T09:00:00Z");
    let again = deliver(&mut app, "Ada", "Hello", "2026-10-11T09:00:00Z");
    assert!(again.get("unchanged").is_some(), "{again}");
    assert_eq!(
        app.state()
            .conversations
            .values()
            .map(|conversation| conversation.messages.len())
            .sum::<usize>(),
        1
    );
}

#[test]
fn a_delivery_without_a_sender_words_or_a_readable_date_is_refused() {
    let mut app = app();
    for args in [
        json!({ "from": " ", "body": "Hi" }),
        json!({ "from": "Ada", "body": "  " }),
        json!({ "from": "Ada", "body": "Hi", "date": "yesterday" }),
        json!({ "from": "Ada", "body": "Hi", "conversation": "!!" }),
    ] {
        assert!(app.call("deliver-message", args.clone()).is_err(), "{args}");
    }
    assert!(app.state().conversations.is_empty());
}

#[test]
fn a_reply_is_queued_in_its_conversation_and_undo_takes_it_back() {
    let mut app = app();
    deliver(
        &mut app,
        "+15550100",
        "Dinner tonight?",
        "2026-10-11T18:00:00Z",
    );
    let queued = app
        .call(
            "reply",
            json!({ "conversation": "messages.conversation:+15550100", "body": "Yes, at 7." }),
        )
        .unwrap()
        .expect("a reply answers with its message");
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["conversation"], "messages.conversation:+15550100");
    let thread = app
        .call("read", json!({ "item": "+15550100" }))
        .unwrap()
        .unwrap();
    let reply = &thread["messages"][1];
    assert_eq!(reply["body"], "Yes, at 7.");
    assert_eq!(reply["direction"], "out");
    assert_eq!(reply["status"], "queued");
    assert_eq!(reply["to"], "+15550100");

    assert!(app.undo(), "a queued reply is an undo step");
    let thread = app
        .call("read", json!({ "item": "+15550100" }))
        .unwrap()
        .unwrap();
    assert_eq!(thread["messages"].as_array().unwrap().len(), 1);

    assert!(
        app.call("reply", json!({ "conversation": "nobody", "body": "Hi" }))
            .is_err(),
        "a reply needs a conversation to answer"
    );
}

#[test]
fn send_message_starts_a_conversation_and_queues_without_sending() {
    let mut app = app();
    let sent = app
        .call(
            "send-message",
            json!({ "to": "Grace Hopper", "body": "Are you coming?" }),
        )
        .unwrap()
        .unwrap();
    assert_eq!(sent["conversation"], "messages.conversation:grace-hopper");
    assert_eq!(sent["status"], "queued");
    let listing = app.call("read", json!({})).unwrap().unwrap();
    assert_eq!(listing["conversations"][0]["with"], "Grace Hopper");
    assert_eq!(listing["conversations"][0]["queued"], 1);
}

#[test]
fn search_finds_every_word_in_any_case_newest_first() {
    let mut app = app();
    deliver(
        &mut app,
        "Ada",
        "The museum opens at 10",
        "2026-10-11T08:00:00Z",
    );
    deliver(
        &mut app,
        "Bob",
        "Museum tickets are booked",
        "2026-10-11T09:00:00Z",
    );
    deliver(&mut app, "Bob", "Lunch after?", "2026-10-11T10:00:00Z");
    let found = app
        .effect(&MessagesEffect::SearchMessages {
            query: "MUSEUM".into(),
        })
        .unwrap()
        .value
        .unwrap();
    let bodies: Vec<&str> = found["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["body"].as_str().unwrap())
        .collect();
    assert_eq!(
        bodies,
        ["Museum tickets are booked", "The museum opens at 10"]
    );
    let found = app
        .effect(&MessagesEffect::SearchMessages {
            query: "bob lunch".into(),
        })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(found["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        found["messages"][0]["conversation"],
        "messages.conversation:bob"
    );
}

#[test]
fn export_keeps_a_transcript_as_content_in_each_format() {
    let mut app = app();
    deliver(
        &mut app,
        "Ada",
        "First line\nsecond line",
        "2026-10-11T08:00:00Z",
    );
    deliver(&mut app, "Bob", "Hi", "2026-10-11T09:00:00Z");
    app.call("reply", json!({ "conversation": "ada", "body": "Thanks" }))
        .unwrap();

    let markdown = exported(
        &mut app,
        &MessagesEffect::ExportContent {
            format: "md".into(),
            conversation: Some("messages.conversation:ada".into()),
        },
    );
    assert!(
        markdown.contains("## Ada (messages.conversation:ada)"),
        "{markdown}"
    );
    assert!(
        markdown.contains("**Ada**: First line\n  second line"),
        "{markdown}"
    );
    assert!(markdown.contains("**You** (queued): Thanks"), "{markdown}");
    assert!(
        !markdown.contains("Bob"),
        "only the conversation named: {markdown}"
    );

    let all: serde_json::Value = serde_json::from_str(&exported(
        &mut app,
        &MessagesEffect::ExportContent {
            format: "json".into(),
            conversation: None,
        },
    ))
    .unwrap();
    assert_eq!(all["conversations"].as_array().unwrap().len(), 2);

    assert!(
        app.effect(&MessagesEffect::ExportContent {
            format: "pdf".into(),
            conversation: None,
        })
        .is_err()
    );
}
