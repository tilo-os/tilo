#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test failing is its assertion failing"
)]

use serde_json::json;
use tilo_actions::remote::{
    ActionCallError, WORKSPACE_AGENT_CREATE, WORKSPACE_AGENT_DELETE, WORKSPACE_AGENT_UPDATE,
};
use tilo_peer_state::join::Role;
use tilo_workspace_agent::{OWNER, WorkspaceAgents};

use super::super::tests::admit;
use crate::workspaces::fixtures::{
    agents_listed as agents, ask_agents as ask, offered_agents as offered,
};
use crate::workspaces::tests::{hosting_device, test_host};

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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Studio").await.unwrap();
    let workspace = created.workspace.clone();
    let viewer = admit(workspaces, &workspace, &owner, 7).await;
    workspaces
        .change(
            &workspace,
            (&owner, [31; 32]),
            tilo_action_policy::Change::SetRole {
                person: hex::encode(viewer.user_public_key),
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

/// How long the roster and grants checks of a change of the workspace's agents may take.
const AGENTS_ADMIT_BUDGET: std::time::Duration = std::time::Duration::from_millis(20);

/// The roster and grants checks of a change of the workspace's agents read the workspace as it
/// stands, never waiting on its lock: while a writer holds it, with the owner's rename in flight
/// behind it, a viewer's rename is refused by the record's grants within [`AGENTS_ADMIT_BUDGET`],
/// and the owner's lands once the writer lets go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_change_is_admitted_while_a_writer_holds_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Studio").await.unwrap().workspace;
    let viewer = admit(workspaces, &workspace, &owner, 7).await;
    workspaces
        .change(
            &workspace,
            (&owner, [31; 32]),
            tilo_action_policy::Change::SetRole {
                person: hex::encode(viewer.user_public_key),
                role: Role::Viewer,
            },
        )
        .await
        .unwrap();
    let made = ask(
        workspaces,
        &workspace,
        &owner,
        (
            WORKSPACE_AGENT_CREATE,
            json!({ "name": "Scout", "mcp_client": "Claude Code" }),
        ),
    )
    .await
    .expect("the owner makes an agent");
    let scout = made["agent"].as_str().expect("the agent's id").to_owned();
    let rename = |name: &str| {
        (
            WORKSPACE_AGENT_UPDATE,
            json!({
                "action": WorkspaceAgents::RENAME_AGENT.id,
                "arguments": { "agent": scout, "name": name },
            }),
        )
    };

    // A writer holds the workspace, and the owner's rename waits behind it to write.
    let held = workspaces.workspace(&workspace).await.unwrap();
    let writer = held.lock().await;
    let in_flight = tokio::spawn({
        let (workspaces, workspace, owner) = (workspaces.clone(), workspace.clone(), owner.clone());
        let renamed = rename("Ranger");
        async move { ask(&workspaces, &workspace, &owner, renamed).await }
    });
    let mut took = Vec::new();
    for _ in 0..8 {
        let started = std::time::Instant::now();
        let refused = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            ask(workspaces, &workspace, &viewer, rename("Mole")),
        )
        .await
        .expect("the viewer's change is decided while the writer holds the workspace");
        took.push(started.elapsed());
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "{refused:?}"
        );
    }
    let fastest = took.iter().min().copied().unwrap();
    assert!(
        fastest <= AGENTS_ADMIT_BUDGET,
        "deciding a change takes {fastest:?} at its fastest, over {AGENTS_ADMIT_BUDGET:?}: {took:?}"
    );
    assert!(
        !in_flight.is_finished(),
        "the owner's write waits for the writer"
    );
    drop(writer);
    in_flight
        .await
        .unwrap()
        .expect("the owner's rename lands once the writer lets go");
    assert!(agents(workspaces, &workspace).await.contains(&(
        scout.clone(),
        "Ranger".to_owned(),
        true
    )));
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

/// An agent retired is refused by the followed roster at once, before its follower has read the
/// list again: what the app runtime's own thread admits and routes by never lets a removed agent
/// in for a turn. Until a write of the list begins, the roster names what it last read.
#[tokio::test]
async fn a_retired_agent_leaves_the_followed_members_before_it_is_read_again() {
    use tilo_broker_core::services::AgentRoster;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Studio").await.unwrap().workspace;
    let made = ask(
        workspaces,
        &workspace,
        &owner,
        (WORKSPACE_AGENT_CREATE, json!({ "name": "Scout" })),
    )
    .await
    .expect("the owner makes an agent");
    let scout = made["agent"].as_str().expect("the agent's id").to_owned();

    let followed = crate::workspaces::FollowedAgents::new(workspaces);
    let following = tokio::spawn(std::sync::Arc::clone(&followed).follow());
    let roster: &dyn AgentRoster = &*followed;
    let names = |roster: &dyn AgentRoster| -> Vec<String> {
        roster
            .listed(&workspace)
            .into_iter()
            .map(|agent| agent.id)
            .collect()
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while names(roster) != vec![scout.clone()] {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the roster reads the agent the list names: {:?}",
            names(roster)
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // The follower stops here, so nothing reads the list again after this.
    following.abort();
    let _ = following.await;
    assert_eq!(
        names(roster),
        vec![scout.clone()],
        "a list no write began on since it was read is answered as read"
    );

    ask(
        workspaces,
        &workspace,
        &owner,
        (WORKSPACE_AGENT_DELETE, json!({ "agent": scout })),
    )
    .await
    .expect("the owner retires it");
    assert!(
        names(roster).is_empty(),
        "the retired agent is refused before the list is read again: {:?}",
        names(roster)
    );
    server.shutdown().await;
}
