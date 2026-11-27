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
    put_for(
        collab_types::EVERY_MEMBER,
        (kind, id, parent),
        fields,
        invoker,
    )
}

/// [`put`], read by `readers` alone.
fn put_for(
    readers: &str,
    (kind, id, parent): (&str, &str, Option<&str>),
    fields: &[(&str, &str)],
    invoker: &str,
) -> Command {
    Command::Put {
        kind: kind.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: readers.to_owned(),
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

/// A file kept for its sender alone.
const KEPT: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

/// A file another workspace's message carries.
const ELSEWHERE: &str = "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3";

/// The resource `id` of [`ATTACHED`], as the workspace's index finds it by what it names.
fn attached(id: &str) -> Vec<NamingResource> {
    vec![NamingResource {
        app: APP.to_owned(),
        kind: ATTACHED.to_owned(),
        id: id.to_owned(),
    }]
}

/// `content` as an attachment names it.
fn blob(content: &str) -> String {
    format!("{}{content}", tilo_content::BLOB_SCHEME)
}

/// `person`'s message in `workspace` carrying [`SENT`], their file [`KEPT`] sealed to them alone,
/// and a message in `other` carrying [`ELSEWHERE`].
async fn sent_kept_and_elsewhere(
    workspaces: &Workspaces,
    (workspace, other): (&str, &str),
    person: &str,
) {
    write(workspaces, workspace, send(person))
        .await
        .expect("the message is sent");
    write(
        workspaces,
        workspace,
        put_for(
            person,
            (ATTACHED, "kept-1", Some("says")),
            &[("name", "mine.md"), ("blob", &blob(KEPT))],
            person,
        ),
    )
    .await
    .expect("the sealed file is kept");
    write(
        workspaces,
        other,
        Command::Batch(vec![
            put((CHANNEL, "general", None), &[("name", "General")], person),
            put(
                (MESSAGE, "there", Some("general")),
                &[("body", "Over here.")],
                person,
            ),
            put(
                (ATTACHED, "there-1", Some("there")),
                &[("name", "there.md"), ("blob", &blob(ELSEWHERE))],
                person,
            ),
        ]),
    )
    .await
    .expect("the other workspace's message is sent");
}

/// The resources that name a content are found by the index of what each names, as each of their
/// readers reads them: a file a message carries by every member, one sealed to its sender by them
/// alone, one only another workspace names by none of this one. Once the last resource that named
/// it names other content or is gone, nothing names it.
#[tokio::test]
async fn content_is_named_by_the_resources_its_reader_reads_as_writes_leave_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    crate::workspaces::fixtures::declare_app(&server, APP, &surface());
    let workspace = workspaces.create("Chat").await.unwrap().workspace;
    let other = workspaces.create("Elsewhere").await.unwrap().workspace;
    let person = hex::encode(workspaces.signing_device().cert.user_public_key);
    let stranger = "d4".repeat(32);
    sent_kept_and_elsewhere(&workspaces, (&workspace, &other), &person).await;
    let naming = |workspace: &str, reader: &str, address: &str| {
        let (workspaces, workspace, reader, address) = (
            workspaces.clone(),
            workspace.to_owned(),
            reader.to_owned(),
            address.to_owned(),
        );
        async move {
            workspaces
                .store_naming_content(&workspace, &reader, &address)
                .await
        }
    };

    assert_eq!(
        naming(&workspace, &person, SENT).await,
        attached("file-1"),
        "the file a message carries is named by its attachment"
    );
    assert_eq!(
        naming(&workspace, &person, KEPT).await,
        attached("kept-1"),
        "its sender reads the file sealed to them"
    );
    assert!(
        naming(&workspace, &stranger, KEPT).await.is_empty(),
        "nobody else reads the sealed file's attachment"
    );
    assert!(
        naming(&workspace, &person, ELSEWHERE).await.is_empty(),
        "what only another workspace names is named by nothing of this one"
    );
    assert_eq!(
        naming(&other, &person, ELSEWHERE).await,
        attached("there-1")
    );
    assert!(
        naming(&workspace, &person, &"e5".repeat(32))
            .await
            .is_empty(),
        "content nothing names is named by nothing"
    );

    // The attachment names other content, and the sealed one is removed: nothing names either.
    let mut edit = put(
        (ATTACHED, "file-1", Some("sends")),
        &[("name", "from Bo.md"), ("blob", &blob(ELSEWHERE))],
        &person,
    );
    if let Command::Put { stamp, .. } = &mut edit {
        "edit-file-1".clone_into(&mut stamp.invocation);
    }
    write(&workspaces, &workspace, edit)
        .await
        .expect("the attachment is changed");
    write(
        &workspaces,
        &workspace,
        Command::Remove {
            kind: ATTACHED.to_owned(),
            id: "kept-1".to_owned(),
            stamp: tilo_resource_store::Stamp {
                verb: "send".to_owned(),
                invoker: person.clone(),
                invocation: "remove-kept-1".to_owned(),
                at: SENT_AT.to_owned(),
                ..tilo_resource_store::Stamp::default()
            },
        },
    )
    .await
    .expect("the sealed file is removed");
    assert!(
        naming(&workspace, &person, SENT).await.is_empty(),
        "the attachment that named the file names other content now"
    );
    assert!(
        naming(&workspace, &person, KEPT).await.is_empty(),
        "the removed attachment names nothing"
    );
    assert_eq!(
        naming(&workspace, &person, ELSEWHERE).await,
        attached("file-1")
    );
}
