//! A chat's messages: reading a part, and the ids a page holds beside its
//! chat's document.

use super::super::Workspaces;
use automerge::AutoCommit;
use tilo_agent_session::Replica;
use tilo_app_id::apps::CHAT;

/// The chat's first part of messages, as a document of the workspace.
pub(crate) fn first_part() -> String {
    chat::parts::part(0).as_str().to_owned()
}

/// The messages of the chat's first part, as the workspace holds it.
pub(crate) async fn posts(workspaces: &Workspaces, workspace: &str) -> Vec<chat::chat::Message> {
    workspaces
        .resource_document(workspace, CHAT, &first_part())
        .await
        .unwrap()
        .map(|bytes| chat::messages::read_part(&AutoCommit::load(&bytes).unwrap()))
        .map(|part| part.messages.values().cloned().collect())
        .unwrap_or_default()
}

/// The message ids `page` holds in the parts beside its chat's document.
pub(crate) fn message_ids(page: &Replica) -> Vec<String> {
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
