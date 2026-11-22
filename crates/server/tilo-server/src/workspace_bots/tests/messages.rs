//! tl Chat's record as a workspace holds it: the messages of every channel and thread, read
//! through Chat's own types, as the workspace's Chat actions leave them.

use tilo_workspace_service::tl_records::runtime::ResourceRef;

use crate::tl_chat::Message;
use crate::workspaces::Workspaces;

/// Every message of `workspace`'s Chat, in each channel and each channel's threads, with the
/// conversation it is in.
pub(crate) async fn messages(
    workspaces: &Workspaces,
    workspace: &str,
) -> Vec<(ResourceRef, Message)> {
    workspaces
        .with_tl_record(
            workspace,
            &crate::tl_chat::packages().unwrap(),
            crate::tl_chat::viewer(),
            |chat| crate::tl_chat::all_messages(chat),
        )
        .await
        .unwrap()
}

/// The messages of `workspace`'s Chat, in order, as the tests read what the bots posted.
pub(crate) async fn posts(workspaces: &Workspaces, workspace: &str) -> Vec<Message> {
    messages(workspaces, workspace)
        .await
        .into_iter()
        .map(|(_, message)| message)
        .collect()
}
