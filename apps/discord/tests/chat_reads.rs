#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

//! What the bridge reads of a real Chat record: the ids a verb created come
//! back in its answer, and Chat's `read-room` is the JSON the planners
//! read.

use chat::effects::ChatEffect;
use chat::record::Chat;
use chat::resource::ChatShell;
use discord::tilo::{self, RoomChat};
use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_actions::ActionOutput;
use ui::tilo_resource::Id;
use ui::tilo_resource::Resource as _;

fn begun() -> (TestApp<ChatShell>, String) {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    app.call(
        "begin-chat",
        json!({"name": "Ada", "initials": "A", "color": 1}),
    )
    .unwrap();
    let state = app.state();
    let (channel, _) = state.channels.iter().next().unwrap();
    (app, channel.to_string())
}

/// The chat's messages, as the window publishes them.
fn chain() -> String {
    ui::framework::chain_resource_id(&chat::parts::chain())
}

/// The id of the one item a call created, from the resources it answered.
fn created(output: Option<ActionOutput>) -> String {
    let output = output.expect("an answer");
    let [resource] = output.resources.as_slice() else {
        panic!("one created item: {output:?}");
    };
    resource.rsplit('/').next().unwrap().to_owned()
}

#[test]
fn a_verb_answers_the_ids_it_created() {
    let (mut app, _) = begun();
    let category = created(
        app.call_on_output(Chat::KIND, "create-category", json!({"name": "chat"}))
            .unwrap(),
    );
    assert!(app.state().categories.contains(&Id::from_raw(&category)));
    let channel = created(
        app.call_on_output(
            Chat::KIND,
            "create-channel",
            json!({"name": "general", "category": category}),
        )
        .unwrap(),
    );
    let message = created(
        app.call_on_output(
            &chain(),
            "post",
            json!({"channel": channel, "text": "hello"}),
        )
        .unwrap(),
    );
    let held = app
        .with_surface::<chat::ChatApp, _>(|surface| surface.held_messages())
        .unwrap();
    assert_eq!(held.get(&Id::from_raw(&message)).unwrap().text, "hello");
}

#[test]
fn read_room_is_what_the_planners_read() {
    let (mut app, channel) = begun();
    let root = created(
        app.call_on_output(
            &chain(),
            "post",
            json!({"channel": channel, "text": "question?"}),
        )
        .unwrap(),
    );
    let reply = created(
        app.call_on_output(
            &chain(),
            "post",
            json!({"channel": channel, "text": "answer", "parent": root}),
        )
        .unwrap(),
    );
    let read = app
        .effect(&ChatEffect::ReadRoom {
            message: Some(reply.clone()),
            channel: None,
        })
        .unwrap()
        .value
        .unwrap();
    let layout = RoomChat::from_chat(&read);
    assert!(layout.channels.iter().any(|c| c.id == channel), "{read}");
    assert!(
        layout.categories.is_empty(),
        "a room opens with no categories"
    );
    let message = tilo::message(&read, &reply).expect("the asked message");
    assert_eq!(message.text, "answer");
    assert_eq!(message.parent.as_deref(), Some(root.as_str()));
    assert_eq!(tilo::message(&read, &root).unwrap().text, "question?");
    let author = tilo::person(&read, &message.author).expect("its author");
    let state = app.state();
    let stored = state
        .people
        .iter()
        .find(|p| p.id.0 == message.author)
        .unwrap();
    assert_eq!(author.name, stored.name);
}

/// A feedback report filed in a channel reads as the message the channel
/// shows: its picks read out as text, its masked picture as the image the
/// bridge uploads.
#[test]
fn a_filed_report_reads_as_its_message_with_its_picture() {
    let (mut app, channel) = begun();
    let report = "0123456789abcdef0123456789abcdef";
    let picture = "cd".repeat(32);
    let submission = json!({
        "id": report,
        "destination": {"room": "room", "provider": "demo-chat", "channel": channel},
        "feedback": {
            "text": "Saving does nothing",
            "context": [],
            "screenshot": {"hash": picture, "width": 320, "height": 200},
            "selections": [{
                "app": "so.tilo.files", "app_name": "Files", "path": "Toolbar[0]/Button[2]",
                "role": "button",
                "label": {"kind": "interface", "parts": [{"text": "Save"}]}
            }]
        }
    });
    app.call_on_output(
        &chat::projection::item_id("channels", &channel),
        "submit-feedback",
        json!({ "submission": submission }),
    )
    .unwrap();
    let id = chat::feedback::message_id(report);
    let read = app
        .effect(&ChatEffect::ReadRoom {
            message: Some(id.clone()),
            channel: None,
        })
        .unwrap()
        .value
        .unwrap();
    let message = tilo::message(&read, &id).expect("the report's message");
    assert_eq!(message.channel, channel);
    assert_eq!(
        message.text,
        "Saving does nothing\n1. The “Save” button in Files"
    );
    let attachment = message.attachment.expect("its picture");
    assert_eq!(attachment.blob, picture);
    assert_eq!(attachment.name, "report.png");
    assert!(tilo::person(&read, &message.author).is_some(), "{read}");
}
