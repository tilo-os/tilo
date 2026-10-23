use serde_json::json;
use ui::framework::testing::TestApp;

use super::*;
use crate::{MessagesEffect, MessagesShell};

fn app() -> TestApp<MessagesShell> {
    TestApp::new().expect("Messages opens")
}

fn deliver(app: &mut TestApp<MessagesShell>, arguments: serde_json::Value) -> serde_json::Value {
    app.call("deliver-message", arguments)
        .expect("the delivery is admitted")
        .expect("a delivery answers")
}

#[test]
fn conversation_keys_normalise_numbers_names_and_item_addresses() {
    assert_eq!(
        conversation_key("+1 555 0100").as_deref(),
        Some("+15550100")
    );
    assert_eq!(conversation_key("+15550100").as_deref(), Some("+15550100"));
    assert_eq!(
        conversation_key("Ada Lovelace").as_deref(),
        Some("ada-lovelace")
    );
    assert_eq!(
        conversation_key("tilo.messages/conversations/ada-lovelace").as_deref(),
        Some("ada-lovelace")
    );
    assert_eq!(conversation_key("  ").as_deref(), None);
    assert_eq!(conversation_key("+").as_deref(), None);
}

#[test]
fn dates_read_as_rfc_3339_with_or_without_a_zone_and_print_in_utc() {
    let at = parse_date("2026-10-11T09:30:00Z").expect("a date");
    assert_eq!(format_date(at), "2026-10-11T09:30:00Z");
    assert_eq!(parse_date("2026-10-11T09:30"), Some(at));
    assert_eq!(parse_date("2026-10-11T11:30:00+02:00"), Some(at));
    assert_eq!(parse_date("2026-10-11T09:30:00.250Z"), Some(at + 250));
    assert_eq!(
        parse_date("2026-10-11").map(format_date).as_deref(),
        Some("2026-10-11T00:00:00Z")
    );
    assert_eq!(parse_date("2026-02-30"), None);
    assert_eq!(parse_date("yesterday"), None);
}

#[test]
fn a_delivered_message_lands_in_the_senders_conversation_under_a_derived_id() {
    let mut app = app();
    let answer = deliver(
        &mut app,
        json!({"from": "+1 555 0100", "body": "Running late, start without me",
               "date": "2026-10-11T09:30:00Z"}),
    );
    assert_eq!(
        answer["conversation"],
        "tilo.messages/conversations/+15550100"
    );
    let message = answer["message"].as_str().expect("the message's id");
    assert!(message.starts_with('m'), "{answer}");

    // The same delivery again changes nothing and names no new message.
    let again = deliver(
        &mut app,
        json!({"from": "+1 555 0100", "body": "Running late, start without me",
               "date": "2026-10-11T09:30:00Z"}),
    );
    assert!(again.get("message").is_none(), "{again}");
    let read = app
        .call("read", json!({"item": "+15550100"}))
        .unwrap()
        .unwrap();
    assert_eq!(read["messages"].as_array().map(Vec::len), Some(1));
    assert_eq!(read["messages"][0]["id"], message);
    assert_eq!(read["messages"][0]["direction"], "in");
    assert_eq!(read["messages"][0]["date"], "2026-10-11T09:30:00Z");
}

#[test]
fn deliveries_read_in_time_order_whatever_order_they_arrive_in() {
    let mut app = app();
    deliver(
        &mut app,
        json!({"from": "Ada", "body": "second", "date": "2026-10-11T10:00:00Z"}),
    );
    deliver(
        &mut app,
        json!({"from": "Ada", "body": "first", "date": "2026-10-11T09:00:00Z"}),
    );
    let read = app.call("read", json!({"item": "ada"})).unwrap().unwrap();
    let bodies: Vec<&str> = read["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["first", "second"]);
}

#[test]
fn a_delivery_without_a_sender_or_words_is_refused() {
    let mut app = app();
    assert!(
        app.call("deliver-message", json!({"from": " ", "body": "hi"}))
            .is_err()
    );
    assert!(
        app.call("deliver-message", json!({"from": "Ada", "body": " "}))
            .is_err()
    );
    assert!(
        app.call(
            "deliver-message",
            json!({"from": "Ada", "body": "hi", "date": "soon"})
        )
        .is_err()
    );
}

#[test]
fn send_message_and_reply_only_queue_and_say_so() {
    let mut app = app();
    let sent = app
        .call(
            "send-message",
            json!({"to": "Grace Hopper", "body": "Lunch at noon?"}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        sent["conversation"],
        "tilo.messages/conversations/grace-hopper"
    );
    assert_eq!(sent["status"], "queued");
    let replied = app
        .call(
            "reply",
            json!({"conversation": "grace-hopper", "body": "Or one?"}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(replied["status"], "queued");
    let read = app
        .call("read", json!({"item": sent["conversation"]}))
        .unwrap()
        .unwrap();
    let messages = read["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .all(|message| message["status"] == "queued" && message["direction"] == "out")
    );
    assert_eq!(messages[0]["to"], "Grace Hopper");
}

#[test]
fn a_reply_to_a_conversation_that_is_not_here_is_refused() {
    let mut app = app();
    assert!(
        app.call("reply", json!({"conversation": "nobody", "body": "hello?"}))
            .is_err()
    );
}

#[test]
fn read_lists_conversations_latest_first_and_reads_one_message() {
    let mut app = app();
    deliver(
        &mut app,
        json!({"from": "Ada", "body": "older", "date": "2026-10-10T09:00:00Z"}),
    );
    let newer = deliver(
        &mut app,
        json!({"from": "Grace", "body": "newer", "date": "2026-10-11T09:00:00Z"}),
    );
    let listed = app.call("read", json!({})).unwrap().unwrap();
    let withs: Vec<&str> = listed["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|conversation| conversation["with"].as_str().unwrap())
        .collect();
    assert_eq!(withs, ["Grace", "Ada"]);
    assert_eq!(listed["conversations"][0]["last"]["snippet"], "newer");
    let one = app
        .call("read", json!({"item": newer["message"]}))
        .unwrap()
        .unwrap();
    assert_eq!(one["body"], "newer");
    assert!(app.call("read", json!({"item": "nobody"})).is_err());
}

#[test]
fn search_messages_finds_every_word_in_any_case_newest_first() {
    let mut app = app();
    deliver(
        &mut app,
        json!({"from": "Ada", "body": "The invoice is attached", "date": "2026-10-10T09:00:00Z"}),
    );
    deliver(
        &mut app,
        json!({"from": "Grace", "body": "Invoice paid", "date": "2026-10-11T09:00:00Z"}),
    );
    deliver(
        &mut app,
        json!({"from": "Grace", "body": "See you", "date": "2026-10-12T09:00:00Z"}),
    );
    let found = app
        .effect(&MessagesEffect::SearchMessages {
            query: "INVOICE".into(),
        })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(found["total"], 2);
    assert_eq!(found["messages"][0]["body"], "Invoice paid");
    let narrowed = app
        .effect(&MessagesEffect::SearchMessages {
            query: "invoice grace".into(),
        })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(narrowed["total"], 1);
    assert!(
        app.effect(&MessagesEffect::SearchMessages { query: " ".into() })
            .is_err()
    );
}

#[test]
fn export_content_keeps_a_transcript_as_content_in_markdown_or_json() {
    let mut app = app();
    deliver(
        &mut app,
        json!({"from": "Ada", "body": "Are we on for Friday?", "date": "2026-10-10T09:00:00Z"}),
    );
    app.call("reply", json!({"conversation": "ada", "body": "Yes"}))
        .unwrap();
    let output = app
        .effect(&MessagesEffect::ExportContent {
            format: None,
            conversation: Some("ada".into()),
        })
        .unwrap();
    let value = output.value.unwrap();
    assert_eq!(value["format"], "md");
    assert_eq!(value["messages"], 2);
    let address = value["content"].as_str().unwrap().to_owned();
    assert_eq!(output.resources, vec![address.clone()]);
    let hash = ui::tilo_actions::content_hash(&address).unwrap();
    let markdown = String::from_utf8(app.content().read(hash).unwrap()).unwrap();
    assert!(markdown.starts_with("# Ada\n"), "{markdown}");
    assert!(markdown.contains("**Ada** (2026-10-10T09:00:00Z): Are we on for Friday?"));
    assert!(markdown.contains("**You**"));
    assert!(markdown.contains("(queued): Yes"));

    let json_output = app
        .effect(&MessagesEffect::ExportContent {
            format: Some("json".into()),
            conversation: None,
        })
        .unwrap()
        .value
        .unwrap();
    let hash = ui::tilo_actions::content_hash(json_output["content"].as_str().unwrap())
        .unwrap()
        .to_owned();
    let exported: serde_json::Value =
        serde_json::from_slice(&app.content().read(&hash).unwrap()).unwrap();
    assert_eq!(
        exported["conversations"][0]["messages"][1]["status"],
        "queued"
    );
    assert!(
        app.effect(&MessagesEffect::ExportContent {
            format: Some("pdf".into()),
            conversation: None,
        })
        .is_err()
    );
}

#[test]
fn the_conversations_are_read_only_to_field_edits() {
    use ui::tilo_actions::door::survey;
    let mut app = app();
    deliver(&mut app, json!({"from": "Ada", "body": "hi"}));
    let resources = app.harness().resources();
    let offered = survey(&resources);
    assert!(
        offered
            .iter()
            .filter(|descriptor| descriptor.address.action == "field-edit")
            .all(|descriptor| !descriptor.address.resource.contains("conversations")),
        "no field edit reaches the conversations"
    );
}

#[test]
fn the_agent_operations_are_offered_to_agents_as_reads_and_verbs() {
    use ui::tilo_actions::ActionKind;
    let specs = <MessagesEffect as ui::framework::Effect>::specs();
    for id in ["search-messages", "export-content"] {
        let spec = specs.iter().find(|spec| spec.id == id).expect(id);
        assert_eq!(spec.contract.kind, ActionKind::Read, "{id} only reads");
        assert!(spec.contract.reaches_agents(), "{id} is for agents");
    }
}
