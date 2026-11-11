#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

//! What the bridge reads of a real Chat record: the ids a verb created come
//! back in its answer, and Chat's records as the record door reads them are
//! what the planners read.

use chat::record::Chat;
use chat::resource::ChatShell;
use discord::tilo::{self, WorkspaceChat};
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
        .with_surface::<chat::ChatWindow, _>(|window| window.state().held_messages())
        .unwrap();
    assert_eq!(held.get(&Id::from_raw(&message)).unwrap().text, "hello");
}

/// Chat's records as the bridge's program reads them through the record door:
/// its record of the workspace, and the newest part of its messages' chain.
fn read(app: &TestApp<ChatShell>) -> tilo::ChatRead {
    use ui::tilo_actions::remote::{Caller, HeldCopies, RecordAddress, RecordAsk};
    let own = app.record().clone();
    let mut held = HeldCopies::default();
    held.0
        .insert(String::new(), app.record_document(&own).unwrap());
    for part in (0..).map(chat::parts::part) {
        let Some(bytes) = app.record_document(&part) else {
            break;
        };
        held.0.insert(part.route().to_owned(), bytes);
    }
    let door = |record: String| {
        let address = RecordAddress {
            app: "so.tilo.chat".into(),
            record,
        };
        let ask = RecordAsk {
            address,
            joined: false,
        };
        held.readable(&Caller::local_agent("bot"), &[ask]).unwrap()
    };
    let newest = ui::tilo_actions::record_sync::chain_head(chat::parts::chain().route());
    tilo::ChatRead::of(&json!({
        "chat": door(String::new()),
        "messages": door(newest),
        "changed": null,
    }))
}

#[test]
fn the_record_door_reads_what_the_planners_read() {
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
    let read = read(&app);
    let layout = WorkspaceChat::from_chat(&read);
    assert!(layout.channels.iter().any(|c| c.id == channel), "{read:?}");
    assert!(
        layout.categories.is_empty(),
        "a workspace opens with no categories"
    );
    assert_eq!(layout.busy, [channel]);
    let message = tilo::message(&read, &reply).expect("the asked message");
    assert_eq!(message.text, "answer");
    assert_eq!(message.parent.as_deref(), Some(root.as_str()));
    assert_eq!(tilo::message(&read, &root).unwrap().text, "question?");
    assert_eq!(
        tilo::messages(&read)
            .into_iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        [root, reply],
        "in the order they were posted"
    );
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
        "destination": {"room": "workspace", "provider": "demo-chat", "channel": channel},
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
    let read = read(&app);
    let message = tilo::message(&read, &id).expect("the report's message");
    assert_eq!(message.channel, channel);
    assert_eq!(
        message.text,
        "Saving does nothing\n1. The “Save” button in Files"
    );
    let attachment = message.attachments.first().expect("its picture");
    assert_eq!(attachment.blob, picture);
    assert_eq!(attachment.name, "report.png");
    assert!(tilo::person(&read, &message.author).is_some(), "{read:?}");
}

/// A message reads with the files it carries and the reactions people
/// answered it with, as the bridge sends them across.
#[test]
fn a_message_reads_with_its_files_and_reactions() {
    let (mut app, channel) = begun();
    let blob = "ab".repeat(32);
    let posted = app.call_on_output(
        &chain(),
        "post",
        json!({"channel": channel, "text": "file",
            "attachments": [{"kind": "file", "name": "a.txt", "blob": blob, "size": 3}]}),
    );
    let message = created(posted.unwrap());
    app.call_on_output(
        &chain(),
        "field-edit",
        json!({"edit": {"path": [{"field": "messages"}, {"item": message}, {"field": "reactions"}],
            "op": "react", "emoji": "👍"}}),
    )
    .unwrap();
    let read = tilo::message(&read(&app), &message).expect("the message");
    let [attachment] = read.attachments.as_slice() else {
        panic!("one file: {read:?}");
    };
    assert_eq!(
        (
            attachment.blob.as_str(),
            attachment.name.as_str(),
            attachment.size
        ),
        (blob.as_str(), "a.txt", Some(3))
    );
    assert_eq!(
        read.reactions
            .iter()
            .map(|reaction| reaction.emoji.as_str())
            .collect::<Vec<_>>(),
        ["👍"]
    );
}
