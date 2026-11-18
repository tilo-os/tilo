//! Enabling the workspace's moderation bot: a person's decision, and the
//! grant it reaches.

use super::bot_server;
use crate::workspace_bots::MODERATION_INSTALLATION;
use crate::workspaces::fixtures::governor;
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, Party, TreePath};

/// The bot's answer for an undoable change at `node` in `workspace`.
async fn bot_answer(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    node: &TreePath,
) -> Answer {
    let grants = workspaces.workspace_grants(workspace).await.unwrap();
    let party = Party::new(vec![Grantee::bot(MODERATION_INSTALLATION)], grants.iter());
    party
        .decide(node, ActionKind::Undoable, &Context::at_secs(2000))
        .answer
}

fn chat_node(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .app(tilo_app_id::apps::CHAT)
        .kind("chat.messages")
        .resource(collab_types::document::chain_resource_id(
            &chat::parts::chain(),
        ))
        .action("post")
}

fn outside_node(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .app("so.tilo.tasks")
        .kind("tasks.issues")
        .resource("chain:issues")
        .action("create")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_enables_the_moderation_bot_in_chat_and_nothing_outside_it() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;

    // No person enabled it: it holds nothing, so its changes are refused.
    assert!(
        !crate::workspace_bots::enabled(&workspaces, &workspace)
            .await
            .unwrap(),
        "a workspace nobody enabled has no moderation bot"
    );
    assert_eq!(
        bot_answer(&workspaces, &workspace, &chat_node(&workspace)).await,
        Answer::Deny
    );

    // The person enables it: Chat, and nothing outside Chat.
    crate::workspace_bots::enable(&workspaces, &workspace)
        .await
        .unwrap();
    assert!(
        crate::workspace_bots::enabled(&workspaces, &workspace)
            .await
            .unwrap()
    );
    assert_eq!(
        bot_answer(&workspaces, &workspace, &chat_node(&workspace)).await,
        Answer::Allow
    );
    assert_eq!(
        bot_answer(&workspaces, &workspace, &outside_node(&workspace)).await,
        Answer::Deny
    );
}
