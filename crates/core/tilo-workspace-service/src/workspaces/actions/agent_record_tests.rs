#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test failing is its assertion failing"
)]

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, CatalogueQuery, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionTarget, WORKSPACE_AGENT_CREATE, WORKSPACE_AGENT_DELETE,
    WORKSPACE_AGENT_UPDATE, WORKSPACE_GOVERNANCE_PROVIDER, workspace_governance_operation,
};
use tilo_peer_state::join::Role;
#[cfg(feature = "agents")]
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::{OWNER, WorkspaceAgents};

use super::super::tests::{admit, settled};
use super::record_id;
#[cfg(feature = "agents")]
use crate::agents::agent_keys::AgentKeys;
#[cfg(feature = "agents")]
use crate::agents::triggered;
use crate::service::local_caller;
#[cfg(feature = "agents")]
use crate::work::{Ineligible, NotTaken};
use crate::workspaces::tests::{governor, test_host};
use crate::workspaces::{SetMemberRoleRequest, Workspaces};

/// Each agent the workspace's record names, by id: its name and whether it still works.
async fn agents(workspaces: &Workspaces, workspace: &str) -> Vec<(String, String, bool)> {
    workspaces
        .read_resource::<WorkspaceAgents>(workspace, (OWNER, &record_id()))
        .await
        .expect("the record reads")
        .map(|state| {
            state
                .agents
                .iter()
                .map(|agent| (agent.id.0.clone(), agent.name.clone(), agent.is_active()))
                .collect()
        })
        .unwrap_or_default()
}

/// `operation` of `workspace`'s agents with `arguments`, asked by the device `device` over this
/// device's dispatch, as every caller asks it: what it answered, or why not.
async fn ask(
    workspaces: &Workspaces,
    workspace: &str,
    device: &DeviceCert,
    (operation, arguments): (&str, Value),
) -> Result<Value, ActionCallError> {
    let Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, operation),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let caller = local_caller(device).in_workspace(workspace);
    let receipt = settled(workspaces.device_actions().invoke(&caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("an agent's change completes at once: {other:?}"),
    }
}

/// The agents' operations the catalogue offers `device` in `workspace`.
async fn offered(workspaces: &Workspaces, workspace: &str, device: &DeviceCert) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
        parent_resource: Some(tilo_actions::remote::workspace_governance_resource(
            workspace,
        )),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    let page = workspaces
        .catalogue(
            tilo_server_protocol::WorkspaceId::parse(workspace),
            &local_caller(device),
            Some(device),
            &query,
        )
        .await;
    let mut offered: Vec<String> = page
        .rows
        .into_iter()
        .filter_map(|row| row.action)
        .filter_map(|action| {
            workspace_governance_operation(&action.address).map(|(_, granted)| granted.to_owned())
        })
        .filter(|operation| super::is_agents_operation(operation))
        .collect();
    offered.sort();
    offered
}

/// The owner makes, renames and retires an agent through the actions the workspace publishes,
/// asked over this device's dispatch, and the record says each; a member whose grants only let
/// them read is refused every one of them, is offered none, and the record stays as it was.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one record told whole: each action refused, then taken, in order"
)]
async fn the_agents_record_changes_only_through_admitted_actions() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
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
                request_id: [31; 32],
                user_public_key: viewer.user_public_key,
                role: Role::Viewer,
            },
        )
        .await
        .unwrap();
    let create = |name: &str| (WORKSPACE_AGENT_CREATE, json!({ "name": name }));

    // The owner is offered all three; a viewer none of them.
    let mut all = vec![
        WORKSPACE_AGENT_CREATE.to_owned(),
        WORKSPACE_AGENT_DELETE.to_owned(),
        WORKSPACE_AGENT_UPDATE.to_owned(),
    ];
    all.sort();
    assert_eq!(offered(workspaces, &workspace, &owner).await, all);
    assert!(offered(workspaces, &workspace, &viewer).await.is_empty());

    // A viewer makes no agent, and no record is founded for them.
    let refused = ask(workspaces, &workspace, &viewer, create("Intruder")).await;
    assert!(
        matches!(refused, Err(ActionCallError::NotPermitted)),
        "a member without the grant is refused: {refused:?}"
    );
    assert!(agents(workspaces, &workspace).await.is_empty());

    // The owner makes one: the record is founded with it, under the id the record chose.
    let made = ask(workspaces, &workspace, &owner, create("Scout"))
        .await
        .expect("the owner makes an agent");
    let scout = made["agent"].as_str().expect("the agent's id").to_owned();
    assert_eq!(
        agents(workspaces, &workspace).await,
        vec![(scout.clone(), "Scout".to_owned(), true)]
    );
    // A second of the same name gets an id of its own, written on top of the first.
    let again = ask(workspaces, &workspace, &owner, create("Scout"))
        .await
        .expect("a second agent of the name");
    let again = again["agent"].as_str().expect("the agent's id").to_owned();
    assert_ne!(scout, again, "each agent keeps an id of its own");

    let rename = |name: &str| {
        (
            WORKSPACE_AGENT_UPDATE,
            json!({
                "action": WorkspaceAgents::RENAME_AGENT.id,
                "arguments": { "agent": scout, "name": name },
            }),
        )
    };
    let refused = ask(workspaces, &workspace, &viewer, rename("Mole")).await;
    assert!(
        matches!(refused, Err(ActionCallError::NotPermitted)),
        "{refused:?}"
    );
    ask(workspaces, &workspace, &owner, rename("Ranger"))
        .await
        .expect("the owner renames it");
    // Making or retiring is not a change of one.
    let sideways = ask(
        workspaces,
        &workspace,
        &owner,
        (
            WORKSPACE_AGENT_UPDATE,
            json!({
                "action": WorkspaceAgents::RETIRE_AGENT.id,
                "arguments": { "agent": scout },
            }),
        ),
    )
    .await;
    assert!(
        matches!(sideways, Err(ActionCallError::InvalidRequest { .. })),
        "{sideways:?}"
    );

    let retire = |agent: &str| (WORKSPACE_AGENT_DELETE, json!({ "agent": agent }));
    let refused = ask(workspaces, &workspace, &viewer, retire(&scout)).await;
    assert!(
        matches!(refused, Err(ActionCallError::NotPermitted)),
        "{refused:?}"
    );
    ask(workspaces, &workspace, &owner, retire(&scout))
        .await
        .expect("the owner retires it");
    let listed = agents(workspaces, &workspace).await;
    assert!(
        listed.contains(&(scout.clone(), "Ranger".to_owned(), false)),
        "renamed, then retired, and still read by its name: {listed:?}"
    );
    assert!(
        listed.contains(&(again, "Scout".to_owned(), true)),
        "{listed:?}"
    );

    // An agent the record does not name cannot be retired.
    let missing = ask(workspaces, &workspace, &owner, retire("nobody")).await;
    assert!(
        matches!(missing, Err(ActionCallError::Refused { .. })),
        "{missing:?}"
    );
    server.shutdown().await;
}

/// The triggers saved in `workspace` as this device's person, by their shortcut ids and the
/// names they go by.
#[cfg(feature = "agents")]
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
#[cfg(feature = "agents")]
struct Keyed;

#[cfg(feature = "agents")]
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
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one agent's life told whole: made, paused, resumed and retired"
)]
async fn an_agent_installs_with_its_triggers_pauses_resumes_and_leaves_with_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
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
    let shortcut = triggered::shortcut_id(&scout);
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
    let place = |item: &str| triggered::Where {
        app: "so.tilo.lang.chat".to_owned(),
        kind: triggered::MESSAGE.to_owned(),
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
            crate::work::takes_turn(workspaces, &workspace, &paused).await,
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
        crate::work::takes_turn(workspaces, &workspace, &resumed)
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

/// The steps the agents' actions declare are the record's own actions, of its kind in the
/// workspace's own app: making and retiring one, and every change but where an agent waits or
/// runs, which the device that runs it writes.
#[test]
fn the_agents_actions_declare_the_records_own_steps() {
    use tilo_actions::remote::{
        WORKSPACE_AGENT_CHANGES, WORKSPACE_AGENT_CREATE, WORKSPACE_AGENT_DELETE,
        WORKSPACE_AGENT_UPDATE, WORKSPACE_AGENTS_KIND, workspace_agent_effects,
    };
    use tilo_resource::Resource as _;
    assert_eq!(WORKSPACE_AGENTS_KIND, tilo_workspace_agent::KIND);
    let verbs: Vec<_> = WorkspaceAgents::declarations()
        .into_iter()
        .map(|declared| declared.id)
        .collect();
    let mut declared = Vec::new();
    for operation in [
        WORKSPACE_AGENT_CREATE,
        WORKSPACE_AGENT_UPDATE,
        WORKSPACE_AGENT_DELETE,
    ] {
        for effect in workspace_agent_effects(operation) {
            assert_eq!(
                (effect.app.as_str(), effect.kind.as_str()),
                (OWNER, WORKSPACE_AGENTS_KIND)
            );
            assert!(
                verbs.contains(&effect.verb.as_str()),
                "{operation}: {effect}"
            );
            declared.push(effect.verb);
        }
    }
    assert_eq!(declared.len(), WORKSPACE_AGENT_CHANGES.len() + 2);
    let undeclared: Vec<_> = verbs
        .iter()
        .filter(|verb| !declared.iter().any(|declared| declared == *verb))
        .collect();
    assert_eq!(
        undeclared,
        [
            &WorkspaceAgents::WAIT_TO_RUN_AGENT.id,
            &WorkspaceAgents::RUN_AGENT_ON.id
        ],
        "only where an agent waits or runs is left to the device that runs it"
    );
}
