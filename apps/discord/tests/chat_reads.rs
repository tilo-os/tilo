#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

//! What the bridge reads of a real Chat record: the ids a verb created come
//! back in its answer, and Chat's `read-workspace` is the JSON the planners
//! read.

use chat::effects::ChatEffect;
use chat::record::Chat;
use chat::resource::ChatShell;
use discord::tilo::{self, Workspace};
use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_actions::ActionOutput;
use ui::tilo_resource::Id;
use ui::tilo_resource::items::item_resource_id;

fn begun() -> (TestApp<ChatShell>, String, String) {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    app.call(
        "begin-chat",
        json!({"workspace_name": "Tilo", "name": "Ada", "initials": "A", "color": 1}),
    )
    .unwrap();
    let state = app.state();
    let (workspace, _) = state.workspaces.iter().next().unwrap();
    let (channel, _) = state
        .channels
        .iter()
        .find(|(_, c)| c.workspace == *workspace)
        .unwrap();
    (app, workspace.to_string(), channel.to_string())
}

/// The chat's messages, as the window publishes them.
fn chain() -> String {
    ui::framework::chain_resource_id(&chat::parts::chain())
}

fn on(field: &str, id: &str) -> String {
    item_resource_id::<Chat>(field, &Id::from_raw(id))
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
    let (mut app, workspace, _) = begun();
    let category = created(
        app.call_on_output(
            &on("workspaces", &workspace),
            "create-category",
            json!({"name": "chat"}),
        )
        .unwrap(),
    );
    assert!(app.state().categories.contains(&Id::from_raw(&category)));
    let channel = created(
        app.call_on_output(
            &on("workspaces", &workspace),
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
fn read_workspace_is_what_the_planners_read() {
    let (mut app, workspace, channel) = begun();
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
        .effect(&ChatEffect::ReadWorkspace {
            workspace: workspace.clone(),
            message: Some(reply.clone()),
            channel: None,
        })
        .unwrap()
        .value
        .unwrap();
    let layout = Workspace::from_chat(&read, &workspace);
    assert!(layout.channels.iter().any(|c| c.id == channel), "{read}");
    assert!(!layout.categories.is_empty());
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
