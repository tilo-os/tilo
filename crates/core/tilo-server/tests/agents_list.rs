//! A workspace's agent list is the workspace's own record of its agents: a member
//! reads it from their device's copy of the workspace, and a second device of theirs reads the
//! same list from its own copy, taken over its link.
//!
//! A device that holds the workspace before an agent is named takes the list's change in over
//! its link; one that links afterwards reads the same list.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use tilo_peer_state::DelegatedAgent;
use tilo_server_protocol::delegation::{Delegate, DelegateScope, Delegation};
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Linked, TestUser, spawn_server};
use tilo_server::WorkspaceId;

const WS: WorkspaceId = WorkspaceId::from_u128(0x51);

/// The workspace's agent list, as `linked`'s device holds it.
fn agents_on(linked: &Linked) -> Vec<DelegatedAgent> {
    linked.read(|peer| {
        peer.governance()
            .delegated_agents(std::time::SystemTime::now())
            .unwrap_or_default()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_agent_list_reaches_their_second_device_over_its_link() {
    let ann = TestUser::fresh("Ann");
    let host = ann.enrol("home", DeviceCapabilities::member());
    let laptop = ann.enrol("laptop", DeviceCapabilities::primary());
    let phone = ann.enrol("phone", DeviceCapabilities::member());
    let server = Box::pin(spawn_server(&ann, host, &[WS], &[&laptop, &phone])).await;
    let workspaces = server
        .server
        .workspaces()
        .expect("the host holds workspaces");

    // The phone's link is up before any agent exists: the list it reads comes over the link,
    // never from what the host had when it dialled.
    let phone_link = Linked::open(&server.server, &phone, WS)
        .await
        .expect("the workspace admits Ann's phone");
    assert!(
        agents_on(&phone_link).is_empty(),
        "the workspace lists no agent yet"
    );

    // The host takes on Ann's agent for the workspace, as her device's agent does when it
    // connects.
    let agent = ann.enrol("scribe", DeviceCapabilities::nothing());
    let issued = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &ann.user_sk,
        agent.device_pubkey(),
        Delegate::Agent {
            agent_id: "scribe".into(),
        },
        "Scribe",
        [0x7c, 0x3a, 0xed],
        DelegateScope {
            workspaces: vec![WS],
            apps: Vec::new(),
        },
        issued,
        issued.checked_add(600_000_000_000).unwrap(),
    )
    .unwrap();
    server
        .server
        .register_agent((&agent.cert, &agent.sk), delegation)
        .await
        .expect("the host takes on the workspace's agent");

    // The phone's own copy takes the list's change in over its link.
    tilo_test_env::within("the agent list to reach the phone's copy", async {
        loop {
            if agents_on(&phone_link)
                .iter()
                .any(|listed| listed.device_public_key == agent.device_pubkey())
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;

    // A second device of Ann's reads the same list from its own copy of the workspace.
    let laptop_link = Linked::open(&server.server, &laptop, WS)
        .await
        .expect("the workspace admits Ann's laptop");
    let listed = agents_on(&laptop_link);
    assert_eq!(
        listed
            .iter()
            .map(|listed| listed.device_public_key)
            .collect::<Vec<_>>(),
        vec![agent.device_pubkey()],
        "the second device lists the workspace's agent"
    );
    assert_eq!(listed[0].agent_id, "scribe");
    assert_eq!(listed[0].label, "Scribe");
    assert_eq!(
        listed[0].user_public_key, ann.user_pk,
        "the list names the person it acts for"
    );
    assert!(listed[0].acting, "the delegation is current");

    // The host reads the same list on the workspace it serves.
    let on_host = workspaces.agents(&WS.to_string()).await.unwrap();
    assert_eq!(
        on_host
            .iter()
            .map(|listed| listed.device_public_key)
            .collect::<Vec<_>>(),
        listed
            .iter()
            .map(|listed| listed.device_public_key)
            .collect::<Vec<_>>(),
        "the host and its member's devices read one list"
    );

    // Both devices read the list of the person they belong to.
    assert_eq!(agents_on(&phone_link), listed);
}
