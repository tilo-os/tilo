//! A workspace whose agents a build before kept under Chat holds the same
//! agents under Assistant once the server governing it opens it again.

use super::super::fixtures::{governor, test_host};
use super::*;
use tilo_server_protocol::WorkspaceId;

/// The list of a workspace's agents as the build before this one kept it
/// (Chat's resource, kind `chat.agent`), saved by a window of Chat in which one
/// person made the agent `scout` (Scout) with the model
/// `anthropic/claude-sonnet-4-5` and told it "Keep the release notes short.",
/// and the agent `reed` (Reed), whose model they then chose as
/// `openai/gpt-5`: the bytes that window sends the workspace.
const CHAT_AGENTS: &str = include_str!("agent_roster_chat_agent.hex");

fn chat_agents() -> AutoCommit {
    let bytes = hex::decode(CHAT_AGENTS.trim()).unwrap();
    AutoCommit::load(&bytes).unwrap()
}

/// Keep [`CHAT_AGENTS`] as `workspace`'s list of its agents under Chat, as
/// the governor holding `key` took it in from a window of Chat.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the workspace stays held through its settling"
)]
async fn keep_under_chat(
    workspaces: &Workspaces,
    workspace: &str,
    key: &ed25519_dalek::SigningKey,
) {
    let id = WorkspaceId::parse(workspace).unwrap();
    let state = workspaces.held(id).await.unwrap();
    let mut governed = state.lock().await;
    let at = tilo_clock::wall();
    let mut candidate = governed.peer().clone();
    candidate
        .publication_mut()
        .publish_document(
            FORMER_OWNER,
            &collab_types::workspace_document_id(FORMER_KIND),
            chat_agents().save(),
            key,
            at,
        )
        .unwrap();
    let collaboration = governed.collaboration().clone();
    governed
        .keep(candidate, key, at, |_, _| Ok(collaboration))
        .unwrap();
    workspaces.settle(&mut governed).await.unwrap();
}

/// Every agent the workspace kept under Chat is in Assistant's list of them
/// once its server opens again, with its name, person, model and instruction,
/// and opening again copies nothing more.
#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server stays live until its consuming shutdown"
)]
async fn a_workspaces_agents_kept_under_chat_are_assistants_after_it_opens_again() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspace = server.workspaces().create("Demo").await.unwrap().workspace;
    // The workspace as the build before left it: its agents in Chat's list.
    keep_under_chat(server.workspaces(), &workspace, &key).await;
    let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let assistants = |workspaces: &Workspaces| {
        let workspaces = workspaces.clone();
        let (workspace, list) = (workspace.clone(), list.clone());
        async move {
            workspaces
                .read_resource::<WorkspaceAgents>(&workspace, (tilo_workspace_agent::OWNER, &list))
                .await
                .unwrap()
        }
    };
    assert!(
        assistants(server.workspaces()).await.is_none(),
        "before it opens again, Assistant holds no agents of the workspace"
    );
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    let agents = assistants(server.workspaces())
        .await
        .expect("Assistant's list of the workspace's agents");
    let mut kept: Vec<_> = agents
        .agents
        .iter()
        .map(|agent| {
            (
                agent.id.0.as_str(),
                agent.name.as_str(),
                agent.acts_for.as_str(),
                agent.model.as_deref(),
                agent.instruction.as_deref(),
                agent.is_active(),
            )
        })
        .collect();
    kept.sort_unstable();
    assert_eq!(
        kept,
        [
            (
                "reed",
                "Reed",
                "test-author",
                Some("openai/gpt-5"),
                None,
                true
            ),
            (
                "scout",
                "Scout",
                "test-author",
                Some("anthropic/claude-sonnet-4-5"),
                Some("Keep the release notes short."),
                true
            ),
        ]
    );
    // The document names itself as Assistant's list of the workspace's agents,
    // as the one a device would have started.
    let bytes = server
        .workspaces()
        .resource_document(&workspace, tilo_workspace_agent::OWNER, &list)
        .await
        .unwrap()
        .unwrap();
    let held = AutoCommit::load(&bytes).unwrap();
    let named = identity::identity(&held).expect("the document names itself");
    assert_eq!(named.id.as_str(), list);
    assert_eq!(named.kind.as_str(), tilo_workspace_agent::KIND);
    assert_eq!(identity::home(&held).as_deref(), Some(workspace.as_str()));

    // Opened again, it holds them already, and copies none.
    assert_eq!(
        server.workspaces().move_agent_roster(&workspace).await,
        Ok(0)
    );
    server.shutdown().await;
}

/// Two servers that copy the same agents onto the same list write the same
/// changes, which merge as one; an agent the list holds already is left as it
/// is.
#[test]
fn copying_is_the_same_change_wherever_it_is_made() {
    let old = chat_agents();
    let mut first = AutoCommit::new();
    let mut second = AutoCommit::new();
    let workspace = "3f2b1c00-0000-4000-8000-0000000000e1";
    assert_eq!(copy_agents(&old, workspace, &mut first), Ok(2));
    assert_eq!(copy_agents(&old, workspace, &mut second), Ok(2));
    assert_eq!(first.get_heads(), second.get_heads());
    assert_eq!(copy_agents(&old, workspace, &mut first), Ok(0));
    assert_eq!(
        WorkspaceAgents::read(&first).unwrap().agents.iter().count(),
        2
    );
}
