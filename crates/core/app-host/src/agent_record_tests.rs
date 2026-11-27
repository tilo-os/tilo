#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test failing is its assertion failing"
)]

use serde_json::json;
use tilo_actions::remote::{
    ActionCallError, InvokeRequest, WORKSPACE_AGENT_CREATE, WORKSPACE_AGENT_DELETE,
    WORKSPACE_AGENT_UPDATE,
};
use tilo_peer_state::join::Role;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_agent::WorkspaceAgents;
use tilo_workspace_service::work::{self, Ineligible, NotTaken};
use tilo_workspace_service::workspaces::fixtures::{
    admit, agents_listed as agents, ask_agents as ask, hosting_device, offered_agents as offered,
    test_host,
};
use tilo_workspace_service::workspaces::{SetMemberRoleRequest, Workspaces};

use crate::agents::agent_keys::AgentKeys;
use crate::agents::triggered;

/// The triggers saved in `workspace` as this device's person, by their shortcut ids and the
/// names they go by.
async fn saved_triggers(workspaces: &Workspaces, workspace: &str) -> Vec<(String, String)> {
    workspaces
        .described()
        .await
        .into_iter()
        .find(|held| held.id().to_string() == workspace)
        .map(|held| {
            held.shortcuts
                .iter()
                .filter_map(|saved| {
                    saved
                        .decode()
                        .ok()
                        .map(|shortcut| (shortcut.id, shortcut.name))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The keys a person's device makes for its agents: one fixed key here, which the engine takes
/// as the agent's own. What is proved is whether the key acts, so any key will do.
pub(crate) struct Keyed;

impl AgentKeys for Keyed {
    fn grant(
        &self,
        _workspace: WorkspaceId,
        _agent: &str,
        _name: &str,
        _person: [u8; 32],
    ) -> Result<[u8; 32], String> {
        Ok([7; 32])
    }

    fn sign(
        &self,
        _workspace: WorkspaceId,
        _key: &[u8; 32],
        _request: &mut InvokeRequest,
    ) -> Result<(), String> {
        Ok(())
    }

    fn made(
        &self,
        _key: &[u8; 32],
    ) -> Option<(tilo_user_identity::DeviceCert, ed25519_dalek::SigningKey)> {
        None
    }
}

/// An agent made from the catalogue's create row comes with its triggers; pause holds back the
/// turn its trigger would start, and resuming lets the agent's own key take it again; retiring
/// the agent takes its triggers with it. A member whose grants only let them read is refused
/// making, pausing, resuming and retiring it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one agent's life told whole: made, paused, resumed and retired"
)]
async fn an_agent_installs_with_its_triggers_pauses_resumes_and_leaves_with_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Studio").await.unwrap();
    let workspace = created.workspace.clone();
    let viewer = admit(workspaces, &workspace, &owner, 7).await;
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [41; 32],
                user_public_key: viewer.user_public_key,
                role: Role::Viewer,
            },
        )
        .await
        .unwrap();
    // This device runs the workspace's agents, each under the key the device makes for it.
    let _agents = crate::agents::ServerAgents::on_device(
        workspaces.clone(),
        std::sync::Arc::new(tilo_actions::remote::TestProvider::new()),
        std::sync::Arc::new(Keyed),
    );
    let engine = workspaces
        .agent_keys()
        .expect("this device runs its workspace's agents");
    let create = |name: &str| (WORKSPACE_AGENT_CREATE, json!({ "name": name }));

    // The catalogue lists the create row; invoking it is how an agent is installed.
    assert!(
        offered(workspaces, &workspace, &owner)
            .await
            .contains(&WORKSPACE_AGENT_CREATE.to_owned()),
        "the workspace's catalogue lists adding an agent"
    );
    let made = ask(workspaces, &workspace, &owner, create("Scout"))
        .await
        .expect("the owner installs the agent");
    let scout = made["agent"].as_str().expect("the agent's id").to_owned();
    let shortcut = tilo_workspace_service::agent_shortcuts::shortcut_id(&scout);
    assert!(
        saved_triggers(workspaces, &workspace)
            .await
            .contains(&(shortcut.clone(), "Scout".to_owned())),
        "the agent comes with the triggers its person's device saved for it"
    );

    // A viewer may not pause, resume or retire it.
    let pause = |agent: &str| {
        (
            WORKSPACE_AGENT_UPDATE,
            json!({ "action": WorkspaceAgents::PAUSE_AGENT.id, "arguments": { "agent": agent } }),
        )
    };
    let resume = |agent: &str| {
        (
            WORKSPACE_AGENT_UPDATE,
            json!({ "action": WorkspaceAgents::RESUME_AGENT.id, "arguments": { "agent": agent } }),
        )
    };
    let retire = |agent: &str| (WORKSPACE_AGENT_DELETE, json!({ "agent": agent }));
    for change in [pause(&scout), resume(&scout), retire(&scout)] {
        let refused = ask(workspaces, &workspace, &viewer, change).await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "a member without the grant is refused: {refused:?}"
        );
    }
    assert!(
        agents(workspaces, &workspace)
            .await
            .contains(&(scout.clone(), "Scout".to_owned(), true)),
        "the refusals changed nothing"
    );

    // Its key acts while it works.
    assert!(
        engine.acts_as(&workspace, &scout).await,
        "the agent's own key takes its turn"
    );
    let place = |item: &str| work::Where {
        app: "so.tilo.lang.chat".to_owned(),
        kind: tilo_workspace_service::agent_shortcuts::MESSAGE.to_owned(),
        record: "general".to_owned(),
        item: item.to_owned(),
        change: item.to_owned(),
    };

    // Paused, its trigger is still saved, and the turn that trigger starts is refused: the
    // agent's key does not act.
    ask(workspaces, &workspace, &owner, pause(&scout))
        .await
        .expect("the owner pauses it");
    assert!(
        agents(workspaces, &workspace)
            .await
            .contains(&(scout.clone(), "Scout".to_owned(), false)),
        "the record says it does not work now"
    );
    assert!(
        saved_triggers(workspaces, &workspace)
            .await
            .contains(&(shortcut.clone(), "Scout".to_owned())),
        "a paused agent keeps its triggers, so resuming needs nothing else"
    );
    assert!(
        !engine.acts_as(&workspace, &scout).await,
        "a paused agent's key takes no turn"
    );
    let paused = triggered::started_here(workspaces, &scout, &place("m-paused"));
    assert!(
        matches!(
            work::takes_turn(workspaces, &workspace, &paused).await,
            Err(NotTaken::Ineligible(Ineligible::CannotActAs))
        ),
        "a paused agent's trigger starts no turn"
    );

    // Resumed, the same saved trigger starts the turn, taken by the agent's own key.
    ask(workspaces, &workspace, &owner, resume(&scout))
        .await
        .expect("the owner resumes it");
    assert!(
        engine.acts_as(&workspace, &scout).await,
        "a resumed agent's key takes its turn again"
    );
    let resumed = triggered::started_here(workspaces, &scout, &place("m-resumed"));
    assert!(
        work::takes_turn(workspaces, &workspace, &resumed)
            .await
            .is_ok(),
        "a resumed agent's trigger starts its turn"
    );

    // Retiring it takes the triggers it came with.
    ask(workspaces, &workspace, &owner, retire(&scout))
        .await
        .expect("the owner retires it");
    assert!(
        agents(workspaces, &workspace)
            .await
            .contains(&(scout.clone(), "Scout".to_owned(), false)),
        "the record says it does not work"
    );
    assert!(
        !saved_triggers(workspaces, &workspace)
            .await
            .iter()
            .any(|(id, _)| id == &shortcut),
        "retiring it takes its triggers with it"
    );
    server.shutdown().await;
}
