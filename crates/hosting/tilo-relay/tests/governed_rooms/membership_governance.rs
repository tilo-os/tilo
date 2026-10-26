//! A room whose host stewards its channels offers the same membership
//! actions as one a device governs itself: adding and removing an agent and
//! giving and taking back a grant, asked as the room's own governance on the
//! room's governance resource, decided by the same room policy and committed
//! by the host. Every member's replica verifies the change.
use super::governance_actions::{ask, outcome, subscribed};
use super::*;
use tilo_action_policy::{ActionKind, Grant, GrantId, KindSet, Subject, TreePath};
use tilo_actions::remote::{ActionHostError, ROOM_GOVERNANCE_PROVIDER, RemoteActionMessage};

const AGENT: &str = "writer";

/// The room's governance action `action` with `arguments`, asked of `room`.
fn governance(room: &str, action: &str, arguments: serde_json::Value) -> RemoteActionMessage {
    super::governance_actions::invocation(
        ROOM_GOVERNANCE_PROVIDER,
        tilo_actions::remote::room_governance_resource(room),
        action,
        arguments,
    )
}

fn grants_agent(peer: &StatePeer, member: &TestDevice, agent: &str) -> bool {
    let now = SystemTime::now();
    peer.permissions(member.device_pubkey(), now)
        .is_ok_and(|member| {
            member
                .policy
                .grants
                .iter()
                .any(|grant| grant.subject == Subject::agent(agent))
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_room_adds_and_removes_an_agent_and_gives_and_takes_back_a_grant() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let host = alice.enrol("sealed-host", DeviceCapabilities::host());
    let laptop = alice.enrol("sealed-laptop", DeviceCapabilities::primary());
    let member = bob.enrol("sealed-member", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &host, &laptop, "Sealed membership").await;
    let workspace = room.workspace_id;
    topology_namespace::admit(&relay, &host, &member, &room.room).await;
    let owner = subscribed(&relay, &laptop, workspace).await;

    // The member's replica, as it sees the room the host stewards.
    let state_session = session(&relay, &member, workspace).await;
    let mut peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone(), laptop.cert.clone(), member.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_admission_v5().unwrap();

    // Adding an agent is the room's own grant action, so the room's person
    // may take it here: it commits at the host, reaches every member, and
    // the preset's grants are in the room.
    let added = outcome(
        ask(
            &owner,
            &host,
            workspace,
            governance(
                &room.room,
                "add-agent",
                serde_json::json!({ "agent": AGENT, "preset": "contributor" }),
            ),
        )
        .await,
    )
    .unwrap();
    assert!(added["generation"].as_u64().is_some(), "{added}");
    receive_until(
        "a member verifies the signed agent membership",
        &state_session,
        &mut peer,
        &mut Vec::new(),
        |peer, _| grants_agent(peer, &member, AGENT),
    )
    .await;

    // A grant the room's person gives travels the same way and the member
    // reads it too.
    let grant = Grant::new(
        "sealed-grant",
        Subject::agent(AGENT),
        TreePath::room(&room.room),
        KindSet::of(ActionKind::Read)
            .with(ActionKind::Undoable)
            .with(ActionKind::Outside),
        tilo_action_policy::Answer::Allow,
        Subject::person(hex::encode(alice.user_pk)),
    );
    let given = outcome(
        ask(
            &owner,
            &host,
            workspace,
            governance(&room.room, "grant", serde_json::json!({ "grant": grant })),
        )
        .await,
    )
    .unwrap();
    assert!(given["generation"].as_u64().is_some(), "{given}");
    receive_until(
        "a member verifies the signed grant",
        &state_session,
        &mut peer,
        &mut Vec::new(),
        |peer, _| {
            peer.permissions(member.device_pubkey(), SystemTime::now())
                .is_ok_and(|member| {
                    member
                        .policy
                        .grants
                        .iter()
                        .any(|held| held.id == GrantId::new("sealed-grant"))
                })
        },
    )
    .await;

    // Taking the grant back is the room's revoke action, and the member
    // stops holding it.
    let revoked = outcome(
        ask(
            &owner,
            &host,
            workspace,
            governance(
                &room.room,
                "revoke",
                serde_json::json!({ "grant": "sealed-grant" }),
            ),
        )
        .await,
    )
    .unwrap();
    assert!(revoked["generation"].as_u64().is_some(), "{revoked}");
    receive_until(
        "a member verifies the signed revocation",
        &state_session,
        &mut peer,
        &mut Vec::new(),
        |peer, _| {
            peer.permissions(member.device_pubkey(), SystemTime::now())
                .is_ok_and(|member| {
                    !member
                        .policy
                        .grants
                        .iter()
                        .any(|held| held.id == GrantId::new("sealed-grant"))
                })
        },
    )
    .await;

    // Removing the agent takes back what it held and reaches the member.
    let removed = outcome(
        ask(
            &owner,
            &host,
            workspace,
            governance(
                &room.room,
                "remove-agent",
                serde_json::json!({ "agent": AGENT }),
            ),
        )
        .await,
    )
    .unwrap();
    assert!(removed["generation"].as_u64().is_some(), "{removed}");
    receive_until(
        "a member verifies the agent's removal",
        &state_session,
        &mut peer,
        &mut Vec::new(),
        |peer, _| !grants_agent(peer, &member, AGENT),
    )
    .await;

    // A member the room grants nothing is not offered any of them.
    let editor = subscribed(&relay, &member, workspace).await;
    assert_eq!(
        outcome(
            ask(
                &editor,
                &host,
                workspace,
                governance(
                    &room.room,
                    "add-agent",
                    serde_json::json!({ "agent": AGENT, "preset": "observer" }),
                ),
            )
            .await,
        ),
        Err(ActionHostError::UnknownAction),
        "an editor by role alone adds no agent"
    );
    relay.shutdown().await;
}
