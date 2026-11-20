//! A member removed ends the agents they kept in the workspace: an agent acts for
//! its keeper, so it does not outlive their place, and a keeper who may not act in
//! the workspace any more is not the one who ends it.

use super::tests::{governor, guest_device, test_host};
use super::*;
use tilo_peer_state::join::Role;
use tilo_server_protocol::directory::{EntryJoin, Listing};
use tilo_server_protocol::workspaces::JoinReply;
use tilo_server_session::workspaces::RemoveMemberRequest;
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::WorkspaceAgents;

/// A member removed ends the agents they kept there: an agent acts for its
/// keeper, so it does not outlive their place, and a keeper who may not act in
/// the workspace any more is not the one who ends it. The server then takes no
/// turn for it.
#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server stays live until its consuming shutdown"
)]
async fn removing_a_member_retires_the_agents_they_kept_there() {
    use tilo_workspace_agent::AgentState;

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Demo").await.unwrap().workspace;
    workspaces
        .publish(
            &workspace,
            &cert,
            Listing {
                description: String::new(),
                default_role: Role::Editor,
            },
        )
        .await
        .unwrap();
    let (_, guest) = guest_device();
    let join = EntryJoin {
        user_public_key: hex::encode(guest.user_public_key),
        display_name: "Cy".into(),
    };
    assert!(matches!(
        super::listing::entry_tests::walk_in_as(workspaces, &workspace, &join, &guest).await,
        Ok(JoinReply::Admitted { .. })
    ));
    let keeper = hex::encode(guest.user_public_key);
    let helper = agent_the_governor_works(
        workspaces,
        &workspace,
        &cert,
        &keeper,
        tilo_app_id::apps::CHAT,
    )
    .await;
    let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let before = workspaces
        .read_resource::<WorkspaceAgents>(&workspace, (tilo_app_id::apps::CHAT, &list))
        .await
        .unwrap()
        .unwrap();
    let agent = before.get(&helper).expect("the keeper's agent");
    assert!(agent.is_active() && agent.on_host);

    workspaces
        .remove_member(
            &workspace,
            &cert,
            RemoveMemberRequest {
                request_id: [7; 32],
                user_public_key: guest.user_public_key,
            },
        )
        .await
        .unwrap();

    let after = workspaces
        .read_resource::<WorkspaceAgents>(&workspace, (tilo_app_id::apps::CHAT, &list))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.get(&helper).expect("the agent stays readable").state,
        AgentState::Retired,
        "a removed keeper's agent does not stay live"
    );
    // No turn is taken for it: its keeper's person is admitted on no device.
    assert!(
        workspaces
            .person_device(
                WorkspaceId::parse(&workspace).expect("a workspace id"),
                guest.user_public_key,
            )
            .await
            .is_none()
    );
    server.shutdown().await;
}

/// A question an agent asks is put under the name the workspace's list gives it, never under the
/// label of the client that connected it or its id: what the person reads at the bell.
#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server stays live until its consuming shutdown"
)]
async fn a_questions_asker_is_named_by_the_workspaces_list() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Demo").await.unwrap().workspace;
    let keeper = hex::encode(cert.user_public_key);
    let helper = agent_the_governor_works(
        workspaces,
        &workspace,
        &cert,
        &keeper,
        tilo_workspace_agent::OWNER,
    )
    .await;
    assert_eq!(
        workspaces
            .agent_name(&workspace, helper.as_str())
            .await
            .as_deref(),
        Some("Cy's helper"),
        "the agent asks under the name its person reads"
    );
    assert_eq!(
        workspaces.agent_name(&workspace, "nobody").await,
        None,
        "an agent the list does not name has no name to be asked under"
    );
    server.shutdown().await;
}

/// The agent `helper` that `keeper` keeps in `workspace`, written into Chat's
/// list of the workspace's agents by their own device and worked by the
/// workspace's governor: the list a member's own device starts, and their own
/// device edits.
async fn agent_the_governor_works(
    workspaces: &Workspaces,
    workspace: &str,
    governor: &DeviceCert,
    keeper: &str,
    app: &'static str,
) -> tilo_resource::Id {
    use tilo_resource::{Id, NonBlank, Resource as _};

    let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let mut start = automerge::AutoCommit::new();
    WorkspaceAgents::initialize(&mut start).unwrap();
    start.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
    workspaces
        .author_workspace_document_from(
            (workspace, governor),
            (app, &list),
            Some(Box::new(start)),
            |doc| {
                let mut replica =
                    tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc));
                replica = replica.as_author(keeper).in_app(app, "Chat");
                replica
                    .apply(WorkspaceAgents::CREATE_AGENT.with(
                        tilo_workspace_agent::workspace_agents::CreateAgent {
                            name: NonBlank::parse("Cy's helper").unwrap(),
                            keeper: None,
                            id: Some("helper".to_owned()),
                            model: None,
                            mcp_client: None,
                        },
                    ))
                    .map_err(|error| error.to_string())?;
                replica = replica.as_author(keeper).in_app(app, "Chat");
                replica
                    .apply(WorkspaceAgents::SET_AGENT_ON_HOST.with(
                        tilo_workspace_agent::workspace_agents::SetAgentOnHost {
                            agent: Id::from_raw("helper".to_owned()),
                            on_host: true,
                        },
                    ))
                    .map_err(|error| error.to_string())?;
                std::mem::swap(doc, replica.doc_mut());
                Ok(())
            },
        )
        .await
        .unwrap();
    Id::from_raw("helper".to_owned())
}
