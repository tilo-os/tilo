//! A message as its own record holds it: who sent it and when, and the content of each file it
//! carries, read where the turn the message started names it.

use super::*;
use std::collections::BTreeMap;

use collab_types::store::wire::Command;
use collab_types::store::{Cell, Resource, Value};

/// The app the tests write: a channel, its messages, and the files a message carries.
const APP: &str = "test.sent";
const CHANNEL: &str = "tl.test.sent.Channel";
const MESSAGE: &str = "tl.test.sent.Message";
const ATTACHED: &str = "tl.test.sent.Attached";

/// What the sent file is kept as.
const SENT: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

/// When the message is sent.
const SENT_AT: &str = "2026-09-21T14:13:20Z";

/// The app's surface: a channel holds messages, and a message the files it carries.
fn surface() -> Vec<u8> {
    let verbs = serde_json::json!([{"id": "send", "kind": "undoable"}]);
    collab_types::surface::canonical(&serde_json::json!({
        "resources": [
            {
                "kind": CHANNEL, "label": "Channel", "version": 1, "verbs": verbs,
                "fields": [
                    {"path": "name", "type": "text"},
                    {"path": "messages", "stored": "collection",
                     "type": {"collection": {"resource": MESSAGE}}},
                ],
            },
            {
                "kind": MESSAGE, "label": "Message", "version": 1, "verbs": verbs,
                "fields": [
                    {"path": "body", "type": "text"},
                    {"path": "attachments", "stored": "collection",
                     "type": {"collection": {"resource": ATTACHED}}},
                ],
            },
            {
                "kind": ATTACHED, "label": "Attached", "version": 1, "verbs": verbs,
                "fields": [
                    {"path": "name", "type": "text"},
                    {"path": "blob", "type": "text"},
                ],
            },
        ],
    }))
    .into_bytes()
}

/// The resource `id` of `kind` in `parent`, holding `fields`, written by `invoker`'s send.
fn put(
    (kind, id, parent): (&str, &str, Option<&str>),
    fields: &[(&str, &str)],
    invoker: &str,
) -> Command {
    Command::Put {
        kind: kind.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: collab_types::EVERY_MEMBER.to_owned(),
            parent: parent.map(str::to_owned),
            order: None,
            fields: fields
                .iter()
                .map(|(name, value)| ((*name).to_owned(), Cell::One(Value::from(*value))))
                .collect::<BTreeMap<_, _>>(),
        },
        stamp: tilo_resource_store::Stamp {
            verb: "send".to_owned(),
            invoker: invoker.to_owned(),
            invocation: format!("send-{id}"),
            at: SENT_AT.to_owned(),
            ..tilo_resource_store::Stamp::default()
        },
    }
}

/// The message "sends" in the channel "general", carrying the file [`SENT`], and the message
/// "says" beside it, carrying none: one send by the host's person.
fn send(person: &str) -> Command {
    Command::Batch(vec![
        put((CHANNEL, "general", None), &[("name", "General")], person),
        put(
            (MESSAGE, "sends", Some("general")),
            &[("body", "What does Bo ask for?")],
            person,
        ),
        put(
            (ATTACHED, "file-1", Some("sends")),
            &[
                ("name", "from Bo.md"),
                ("blob", &format!("{}{SENT}", tilo_content::BLOB_SCHEME)),
            ],
            person,
        ),
        put(
            (MESSAGE, "says", Some("general")),
            &[("body", "Thanks.")],
            person,
        ),
    ])
}

/// `command`, drafted and kept as this device's write.
async fn write(workspaces: &Workspaces, workspace: &str, command: Command) -> Result<(), String> {
    let state = workspaces
        .workspace(workspace)
        .await
        .map_err(|error| error.to_string())?;
    let command = &command;
    crate::workspaces::wanted_keys::write_with_keys(&state, move |mut hosted| async move {
        let writes = tilo_resource_store::wire::draft(
            &mut workspaces.store_of(&mut hosted),
            command.clone(),
        )
        .map_err(|refused| refused.error.to_string())?;
        workspaces
            .keep_writes(hosted, workspaces.signing_device(), APP, writes, None)
            .await
            .map(|_| ())
    })
    .await
}

/// A message is read as its record holds it: its sender, when they sent it and the content of
/// each file it carries. One named in another conversation than its own, or not held, is none.
#[tokio::test]
async fn a_message_names_its_sender_and_the_content_it_carries() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    crate::workspaces::fixtures::declare_app(&server, APP, &surface());
    let created = workspaces.create("Chat").await.unwrap();
    let workspace = created.workspace.clone();
    let person = hex::encode(workspaces.signing_device().cert.user_public_key);
    write(&workspaces, &workspace, send(&person))
        .await
        .expect("the message is sent");
    assert_eq!(
        workspaces.store_sent(&workspace, "general", "sends").await,
        Some(SentMessage {
            by: person.clone(),
            at: SENT_AT.to_owned(),
            carries: vec![SENT.to_owned()],
        }),
        "the message names who sent it, when, and the content of the file it carries"
    );
    assert_eq!(
        workspaces.store_sent(&workspace, "general", "says").await,
        Some(SentMessage {
            by: person,
            at: SENT_AT.to_owned(),
            carries: Vec::new(),
        }),
        "a message that carries no file names none"
    );
    assert_eq!(
        workspaces.store_sent(&workspace, "other", "sends").await,
        None,
        "a message named in a conversation it is not in is not the one asked for"
    );
    assert_eq!(
        workspaces.store_sent(&workspace, "general", "gone").await,
        None,
        "a message the workspace does not hold is none"
    );
}
