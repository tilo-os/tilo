//! A workspace's Chat as a test reaches it: a workspace with the chat and its
//! `#general` channel, posts in it as a member's page makes them, and the
//! messages its parts hold.

use automerge::AutoCommit;
use chat::projection::ChatProjection as _;
use collab_types::DocumentMutation;
use tilo_actions::MemberId;
use tilo_agent_session::{AppCatalogue, Replica, ReplicaActor, Seed};
use tilo_app_id::apps::CHAT;
use tilo_user_identity::DeviceCert;

use crate::workspaces::Workspaces;

/// The chat's first part of messages, as a document of the workspace.
pub fn first_part() -> String {
    chat::parts::part(0).as_str().to_owned()
}

/// The messages of the chat's first part, as the workspace holds it.
pub async fn posts(workspaces: &Workspaces, workspace: &str) -> Vec<chat::chat::Message> {
    workspaces
        .resource_document(workspace, CHAT, &first_part())
        .await
        .unwrap()
        .map(|bytes| chat::messages::read_part(&AutoCommit::load(&bytes).unwrap()))
        .map(|part| part.messages.values().cloned().collect())
        .unwrap_or_default()
}

/// The message ids `page` holds in the parts beside its chat's document.
pub fn message_ids(page: &Replica) -> Vec<String> {
    page.resources_beside()
        .iter()
        .filter_map(|document| page.read_resource(document, chat::messages::read_part))
        .flat_map(|part| {
            part.messages
                .ids()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Post `text` in `channel` of `workspace` as `author`, as their page does: a
/// view of the chat record and the parts of its messages, posting through
/// the chain into the head (starting the first part), its change carried
/// to the workspace on the part's own route. The new message's id.
pub async fn post_in(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    (author, channel, text): (&str, &str, &str),
) -> String {
    post_with(workspaces, workspace, cert, (author, channel, text), None).await
}

/// Introduce `person` to `workspace` as an editor, as a server admits a
/// member before their page writes: a member's change is decided as a
/// member's, never as the server's device alone.
pub async fn introduce(workspaces: &Workspaces, workspace: &str, cert: &DeviceCert, person: &str) {
    let Some(key) = tilo_failure::ignored!(
        "an author no root key names is a test's own name, which nobody admits",
        hex::decode(person)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    ) else {
        return;
    };
    let Ok((_, members)) = workspaces.membership(workspace).await else {
        return;
    };
    if members.contains_key(person) {
        return;
    }
    workspaces
        .add_member(
            workspace,
            cert,
            crate::workspaces::AddMemberRequest {
                request_id: *blake3::hash(person.as_bytes()).as_bytes(),
                user_public_key: key,
                place: crate::workspaces::MemberPlace::Role(tilo_peer_state::join::Role::Editor),
                name: None,
            },
        )
        .await
        .unwrap();
}

/// [`post_in`] with `attachment`, a file another action put in the store.
pub async fn post_with(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    (author, channel, text): (&str, &str, &str),
    attachment: Option<serde_json::Value>,
) -> String {
    introduce(workspaces, workspace, cert, author).await;
    let catalogue = AppCatalogue::native();
    let chat = workspaces
        .resource_document(workspace, CHAT, "")
        .await
        .unwrap()
        .unwrap();
    let mut parts = Vec::new();
    for (app, record) in workspaces.document_routes(workspace).await.unwrap() {
        let Some(part) = collab_types::document::DocumentId::parse(&record)
            .filter(|part| app == CHAT && chat::parts::part_index(part).is_some())
        else {
            continue;
        };
        let bytes = workspaces
            .resource_document(workspace, CHAT, &record)
            .await
            .unwrap()
            .unwrap();
        parts.push((part, bytes));
    }
    // The page lives on this thread only; what it changed leaves with it.
    let (id, taken) = {
        let mut page = Replica::open(
            &catalogue,
            CHAT,
            workspace,
            &MemberId::person(author),
            ReplicaActor::Owner,
            Seed::Existing {
                record: collab_types::document::DocumentId::package_primary(),
                changes: vec![chat],
            },
        )
        .unwrap();
        for (part, bytes) in parts {
            page.apply_document(&part, &DocumentMutation::Snapshot(bytes))
                .unwrap();
        }
        let before = message_ids(&page);
        let mut arguments = serde_json::json!({ "channel": channel, "text": text });
        if let Some(attachment) = attachment {
            arguments["attachments"] = serde_json::json!([attachment]);
        }
        page.invoke(
            &tilo_actions::api::ActionAddress::new(
                collab_types::document::chain_resource_id(&chat::parts::chain()),
                "post",
            ),
            arguments.as_object().unwrap(),
            false,
        )
        .unwrap();
        let id = message_ids(&page)
            .into_iter()
            .find(|id| !before.contains(id))
            .expect("the post is in a part");
        (id, page.take_document_mutations().unwrap())
    };
    for (record, mutation) in taken {
        match mutation {
            DocumentMutation::Snapshot(bytes) => workspaces
                .author_workspace_document(workspace, cert, (CHAT, record.as_str()), |doc| {
                    tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                        .map(drop)
                        .map_err(|error| error.to_string())
                })
                .await
                .unwrap(),
            DocumentMutation::Incremental(bytes) => workspaces
                .author_application(workspace, cert, &catalogue, CHAT, record.as_str(), |doc| {
                    tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                        .map(drop)
                        .map_err(|error| error.to_string())
                })
                .await
                .unwrap(),
        }
    }
    id
}

/// A workspace with the chat, and `#general`'s id.
pub async fn chat_workspace(workspaces: &Workspaces, cert: &DeviceCert) -> (String, String) {
    let catalogue = AppCatalogue::native();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    workspaces
        .prepare_application(&workspace, cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let general = workspaces
        .author_application(&workspace, cert, &catalogue, CHAT, "", |doc| {
            chat::demo::apply_as(
                doc,
                &hex::encode(cert.user_public_key),
                chat::Chat::CREATE_CHANNEL.with(chat::chat::chat::CreateChannel {
                    name: "general".parse().unwrap(),
                    category: None,
                    kind: None,
                    topic: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            Ok(chat::projection::read(doc)
                .channel_rows()
                .into_iter()
                .find(|channel| channel.kind == chat::model::ChannelKind::Text)
                .expect("the chat has a text channel")
                .id)
        })
        .await
        .unwrap();
    (workspace, general)
}
