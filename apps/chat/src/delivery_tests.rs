//! Seeding a conversation through the published door: a channel named by
//! the caller, messages delivered as other people, read back by name and
//! exported as content.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::json;
use ui::framework::testing::TestApp;

use crate::{ChatApp, ChatShell};

const WORKSPACE: &str = "chat.workspace:ws_home";
const GENERAL: &str = "chat.channel:c_general";

/// The starter chat every new record holds: `ws_home` with `c_general`.
fn chat() -> TestApp<ChatShell> {
    TestApp::<ChatShell>::new().unwrap()
}

fn read_messages(app: &mut TestApp<ChatShell>, channel: &str) -> Vec<serde_json::Value> {
    let page = app
        .call_on(channel, "read-messages", json!({ "pageSize": 32 }))
        .unwrap()
        .expect("read-messages answers with a page");
    page["content"]["messages"].as_array().unwrap().clone()
}

fn exported(app: &mut TestApp<ChatShell>, channel: &str, format: &str) -> String {
    let output = app
        .call_by(
            ui::tilo_actions::ActionInvoker::actor("test-author"),
            channel,
            "export-content",
            json!({ "format": format }),
        )
        .unwrap();
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = ui::tilo_actions::content_hash(address).expect("a content address");
    String::from_utf8(app.content().read(hash).expect("kept")).unwrap()
}

fn deliver(app: &mut TestApp<ChatShell>, author: &str, body: &str, ts: &str) -> serde_json::Value {
    app.call_on(
        GENERAL,
        "deliver-message",
        json!({ "author": author, "body": body, "ts": ts }),
    )
    .unwrap()
    .expect("a delivery answers with its message")
}

#[test]
fn new_channel_takes_a_caller_given_id_and_answers_its_address() {
    let mut app = chat();
    let made = app
        .call_on(
            WORKSPACE,
            "new-channel",
            json!({ "value": "Standup", "id": "standup" }),
        )
        .unwrap()
        .expect("new-channel answers with the channel");
    assert_eq!(made["channel"], "chat.channel:standup");
    app.call_on(
        "chat.channel:standup",
        "post",
        json!({ "value": "Morning" }),
    )
    .unwrap();
    app.settle();
    assert_eq!(app.state().messages_of("standup").len(), 1);

    // Naming it again leaves the channel and its messages as they were.
    let again = app
        .call_on(
            WORKSPACE,
            "new-channel",
            json!({ "value": "Other", "id": "standup" }),
        )
        .unwrap()
        .unwrap();
    assert!(again.get("unchanged").is_some(), "{again}");
    app.settle();
    let state = app.state();
    let standup = state.channels.iter().find(|ch| ch.id == "standup").unwrap();
    assert_eq!(standup.name, "Standup");
    assert_eq!(state.messages_of("standup").len(), 1);

    for bad in ["has space", "-leading", "a/b", &"x".repeat(65)] {
        assert!(
            app.call_on(
                WORKSPACE,
                "new-channel",
                json!({ "value": "Bad", "id": bad })
            )
            .is_err(),
            "{bad}"
        );
    }
    // Without an id one is minted, as before.
    let minted = app
        .call_on(WORKSPACE, "new-channel", json!({ "value": "Random" }))
        .unwrap()
        .unwrap();
    assert!(
        minted["channel"]
            .as_str()
            .is_some_and(|id| id.starts_with("chat.channel:c_")),
        "{minted}"
    );
}

#[test]
fn a_delivered_message_is_shown_under_its_authors_name_and_delivered_once() {
    let mut app = chat();
    let delivered = deliver(
        &mut app,
        "Maria Chen",
        "Lunch at noon?",
        "2026-10-11T09:00:00Z",
    );
    let message = delivered["message"].as_str().unwrap().to_owned();
    assert!(message.starts_with("chat.message:m_dlv_"), "{delivered}");
    app.settle();

    let messages = read_messages(&mut app, GENERAL);
    let row = messages
        .iter()
        .find(|row| format!("chat.message:{}", row["id"].as_str().unwrap()) == message)
        .expect("the delivered message reads back");
    assert_eq!(row["author"], "Maria Chen");
    assert!(
        row["authorId"]
            .as_str()
            .is_some_and(|id| id.starts_with("delivered:maria-chen-")),
        "{row}"
    );
    assert_eq!(row["ts"], "1791709200");
    let author = row["authorId"].as_str().unwrap().to_owned();
    assert_eq!(
        app.surface_mut::<ChatApp>().display_name(&author),
        "Maria Chen"
    );

    let again = deliver(
        &mut app,
        "Maria Chen",
        "Lunch at noon?",
        "2026-10-11T09:00:00Z",
    );
    assert!(again.get("unchanged").is_some(), "{again}");
    app.settle();
    assert_eq!(read_messages(&mut app, GENERAL).len(), messages.len());

    for args in [
        json!({ "author": " ", "body": "Hi" }),
        json!({ "author": "Maria", "body": "" }),
        json!({ "author": "Maria", "body": "Hi", "ts": "yesterday" }),
    ] {
        assert!(
            app.call_on(GENERAL, "deliver-message", args.clone())
                .is_err(),
            "{args}"
        );
    }
}

#[test]
fn read_messages_names_every_author_and_keeps_the_id_beside_the_name() {
    let mut app = chat();
    app.call_on(GENERAL, "post", json!({ "value": "Hello" }))
        .unwrap();
    app.settle();
    let state = app.state();
    let messages = read_messages(&mut app, GENERAL);
    let mine = messages.last().unwrap();
    let id = mine["authorId"].as_str().unwrap();
    let roster = state
        .users
        .iter()
        .find(|user| user.id == id)
        .expect("the poster is on the roster");
    assert_eq!(mine["author"], roster.name.as_str());
    assert_ne!(
        mine["author"], mine["authorId"],
        "a name, not a participant id"
    );
}

#[test]
fn export_keeps_the_conversation_as_content_by_name_and_time() {
    let mut app = chat();
    deliver(
        &mut app,
        "Maria Chen",
        "Lunch at noon?\nOr later",
        "2026-10-11T09:00:00Z",
    );
    deliver(&mut app, "Sam", "Noon works", "1791709260");
    app.settle();

    let markdown = exported(&mut app, GENERAL, "md");
    assert!(markdown.starts_with("# #general"), "{markdown}");
    assert!(
        markdown.contains("- 2026-10-11T09:00:00Z **Maria Chen**: Lunch at noon?\n  Or later"),
        "{markdown}"
    );
    assert!(
        markdown.contains("- 2026-10-11T09:01:00Z **Sam**: Noon works"),
        "{markdown}"
    );
    assert!(
        !markdown.contains("delivered:"),
        "names, not ids: {markdown}"
    );

    let json: serde_json::Value =
        serde_json::from_str(&exported(&mut app, GENERAL, "json")).unwrap();
    assert_eq!(json["channel"], GENERAL);
    let sam = json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["author"] == "Sam")
        .unwrap();
    assert_eq!(sam["ts"], "2026-10-11T09:01:00Z");
    assert!(
        sam["authorId"]
            .as_str()
            .unwrap()
            .starts_with("delivered:sam-")
    );

    assert!(
        app.call_by(
            ui::tilo_actions::ActionInvoker::actor("test-author"),
            GENERAL,
            "export-content",
            json!({ "format": "pdf" }),
        )
        .is_err()
    );
}
