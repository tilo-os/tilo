//! The chat binary's record mode: what search finds in a record it reads on a
//! device that never ran Chat's window.
#![allow(
    clippy::expect_used,
    reason = "a record that cannot be read fails the test loudly"
)]

use std::io::Write as _;
use std::process::{Command, Stdio};

use chat::demo::Workspace;
use chat::messages::chat_part;
use chat::parts::ChatPart;
use ui::tilo_resource::{Id, NonBlank, Replica};
use ui::tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

/// What the chat binary answers search about `doc`: its entries, as the
/// session's index takes them.
fn read(doc: &[u8]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_chat"))
        .arg("--index")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the chat binary runs");
    child
        .stdin
        .take()
        .expect("the record is written to the reader")
        .write_all(doc)
        .expect("the reader takes its record");
    let out = child.wait_with_output().expect("the reader ends");
    assert!(
        out.status.success(),
        "the record is read: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("the reader answers entries")
}

/// The title of the entry opening the item `anchor` of a record, when the
/// reader found one: an anchor names its kind, its field and its id.
fn found(entries: &[serde_json::Value], anchor: &str) -> Option<String> {
    entries
        .iter()
        .find(|entry| {
            entry["resource"]
                .as_str()
                .is_some_and(|resource| resource.ends_with(anchor))
        })
        .map(|entry| entry["title"].as_str().unwrap_or_default().to_owned())
}

/// The anchor of the one message entry whose text is `text`.
fn text_hit(entries: &[serde_json::Value], text: &str) -> Option<String> {
    entries
        .iter()
        .find(|entry| entry["title"] == text)
        .map(|entry| entry["resource"].as_str().unwrap_or_default().to_owned())
}

#[test]
fn a_conversation_and_its_messages_are_found_from_the_records() {
    let mut workspace = Workspace::demo().expect("the demo workspace holds");
    let posted = workspace
        .say("u_aria", "c_general", "the wheel is loose again")
        .expect("a message is posted");
    let chat = workspace.chat.save();
    let part = workspace.part_bytes(0).expect("the messages are a part");

    let conversations = read(&chat);
    assert_eq!(
        found(&conversations, "/channels/c_general").as_deref(),
        Some("general"),
        "a conversation is found by its name"
    );

    let messages = read(&part);
    assert!(
        text_hit(&messages, "the wheel is loose again")
            .is_some_and(|resource| resource.ends_with(&format!("/messages/{posted}"))),
        "a message is found by its text, opening it where it lives"
    );
}

#[test]
fn an_agent_is_found_from_the_workspace_record() {
    let mut agents = Replica::<WorkspaceAgents>::new().as_author("u_ada");
    agents
        .apply(
            WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                name: NonBlank::parse("Ada").expect("a name"),
                keeper: None,
                id: None,
                model: None,
            }),
        )
        .expect("an agent is made");

    let entries = read(&agents.save());
    let hits: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|entry| entry["title"] == "Ada")
        .collect();
    assert_eq!(hits.len(), 1, "the agent is found by its name");
    assert!(
        hits[0]["resource"]
            .as_str()
            .is_some_and(|resource| resource.contains("/agents/")),
        "a hit opens the agent at its item of the record: {}",
        hits[0]["resource"]
    );
}

#[test]
fn a_deleted_message_is_not_found() {
    let mut workspace = Workspace::demo().expect("the demo workspace holds");
    let posted = workspace
        .say("u_aria", "c_general", "the wheel is loose again")
        .expect("a message is posted");
    workspace
        .apply_part_as(
            "u_aria",
            ChatPart::DELETE_MESSAGE.with(chat_part::DeleteMessage {
                message: Id::from_raw(posted),
            }),
        )
        .expect("its author deletes it");

    let messages = read(&workspace.part_bytes(0).expect("the messages are a part"));
    assert_eq!(
        text_hit(&messages, "the wheel is loose again"),
        None,
        "what the record no longer holds is not found"
    );
}
