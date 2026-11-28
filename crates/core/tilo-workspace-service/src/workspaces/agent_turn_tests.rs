//! Which device takes an agent's turn ([`takes_turn`] for a [`Turn::Once`] acting as the agent).
#![allow(
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps both devices to the end"
)]

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, Grantee, TreePath};
use tilo_action_types::KindSet;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::super::Workspaces;
use super::super::fixtures::{TestHost, device_cert};
use super::super::fixtures::{member_service, persons_root, shared_with};
use crate::work::{Actor, Ineligible, NotTaken, Turn, takes_turn};
use crate::workspaces::fixtures::{ActsAs, given};

const CHAT: &str = "so.tilo.chat";

/// The agent's turn on what somebody wrote in Chat's `part`, and the lease it is claimed under.
fn turn_in(part: &str) -> (Turn, String) {
    let turn = Turn::once(
        "agents.agent:scout#answer",
        serde_json::json!({ "place": part }),
    )
    .acting_as(Actor::Agent("scout".to_owned()))
    .about(CHAT, part);
    let lease = turn.lease();
    assert!(
        lease.starts_with("once/agents.agent:scout#answer/"),
        "{lease}"
    );
    (turn, lease)
}

/// A desktop with a workspace, and `laptop` in it as a second device, both running an agent
/// engine that holds no agent's key.
async fn two_devices(
    states: (&std::path::Path, &std::path::Path),
    laptop: (SigningKey, DeviceCert),
    seed: u8,
) -> (TestHost, TestHost, String) {
    let (desktop, laptop, created, invitation) = shared_with(states.0, "Asked", seed, laptop).await;
    let laptop = member_service(states.1, laptop).await;
    laptop.workspaces().join_member(&invitation).await.unwrap();
    desktop.workspaces().run_agents(Arc::new(ActsAs::NoAgent));
    laptop.workspaces().run_agents(Arc::new(ActsAs::NoAgent));
    (desktop, laptop, created.workspace)
}

/// Wait until `workspaces` reads `device`'s claim on `lease`.
async fn reads_claim(workspaces: &Workspaces, workspace: &str, lease: &str, device: [u8; 32]) {
    let (workspaces, workspace, lease) =
        (workspaces.clone(), workspace.to_owned(), lease.to_owned());
    tilo_test_env::within("the claim reaches the other device", async move {
        loop {
            let state = workspaces.state(&workspace).await.unwrap();
            let lease = state.workspace().lease(&lease, tilo_clock::wall());
            if lease.claims.iter().any(|claim| claim.device == device) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// Both devices hold the agent's key, read what started the turn and claim it. Once the claims meet, the
/// first keeps the turn and the other's renewal says it lost, so that device stops before it
/// writes; asked again, it says whose turn it is. A minute after the desktop last wrote, the
/// laptop takes the turn over.
#[tokio::test]
async fn two_devices_start_one_turn_and_exactly_one_answers_until_its_claim_goes_silent() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let laptop = device_cert(&persons_root(), "laptop", DeviceCapabilities::member());
    let (desktop, laptop, workspace) = two_devices((one.path(), two.path()), laptop, 41).await;
    desktop
        .workspaces()
        .run_agents(Arc::new(ActsAs::EveryAgent));
    laptop.workspaces().run_agents(Arc::new(ActsAs::EveryAgent));
    let desktop_cert = desktop.workspaces().device_cert().clone();
    let (turn, lease) = turn_in("messages-1");

    let first = takes_turn(desktop.workspaces(), &workspace, &turn)
        .await
        .expect("the desktop claims the turn");
    let second = takes_turn(laptop.workspaces(), &workspace, &turn).await;
    reads_claim(
        laptop.workspaces(),
        &workspace,
        &lease,
        desktop_cert.device_public_key,
    )
    .await;
    let taken_by = NotTaken::TakenBy(desktop_cert.device_public_key);
    match second {
        Ok(lost) => assert!(!lost.renew().await, "the second claim loses once they meet"),
        Err(not_taken) => assert_eq!(not_taken, taken_by),
    }
    assert!(first.renew().await, "the first claim keeps the turn");
    let silent_since = tilo_clock::wall();
    assert_eq!(
        takes_turn(laptop.workspaces(), &workspace, &turn)
            .await
            .err(),
        Some(taken_by),
        "asked again, the laptop says whose turn it is"
    );

    // Both devices run on this test's one thread, so both read the clock a minute on.
    let wall = tilo_clock::testing::SharedWall::default();
    wall.follow();
    wall.pin(silent_since + Duration::from_nanos(tilo_peer_state::CLOCK_SKEW_NS + 1_000_000_000));
    let taken = takes_turn(laptop.workspaces(), &workspace, &turn).await;
    let renewed = match &taken {
        Ok(claim) => claim.renew().await,
        Err(_) => false,
    };
    wall.release();
    assert!(
        taken.is_ok(),
        "a silent claim is taken over: {:?}",
        taken.err()
    );
    assert!(renewed, "and the turn is the laptop's from then on");
}

/// A device runs no agent it cannot act as, whoever asked; one that holds the agent's approved
/// key runs it. A device the workspace keeps from what the turn is about refuses for that reason
/// and writes no claim.
#[tokio::test]
async fn only_a_device_that_acts_as_the_agent_claims_what_it_may_open() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let other = device_cert(
        &SigningKey::from_bytes(&[42; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let reader = other.1.user_public_key;
    let (desktop, laptop, workspace) = two_devices((one.path(), two.path()), other, 42).await;
    let desktop_cert = desktop.workspaces().device_cert().clone();
    let (turn, _) = turn_in("messages-2");
    assert_eq!(
        takes_turn(laptop.workspaces(), &workspace, &turn)
            .await
            .err(),
        Some(NotTaken::Ineligible(Ineligible::CannotActAs)),
    );
    laptop.workspaces().run_agents(Arc::new(ActsAs::EveryAgent));
    takes_turn(laptop.workspaces(), &workspace, &turn)
        .await
        .expect("a device that holds the agent's approved key runs it");

    // Without the agent's key again, the device claims nothing while the grant reaches it.
    laptop.workspaces().run_agents(Arc::new(ActsAs::NoAgent));
    let (unopened, lease) = turn_in("messages-3");
    given(
        desktop.workspaces(),
        &workspace,
        &desktop_cert,
        vec![Grant::new(
            "no-chat",
            Grantee::person(hex::encode(reader)),
            TreePath::workspace(&workspace).app(CHAT),
            KindSet::READ,
            Answer::Deny,
            Grantee::person(hex::encode(desktop_cert.user_public_key)),
        )],
    )
    .await;
    let (workspaces, place, that) = (
        laptop.workspaces().clone(),
        workspace.clone(),
        unopened.clone(),
    );
    tilo_test_env::within("the device reads that it may not open Chat", async move {
        while takes_turn(&workspaces, &place, &that).await.err()
            != Some(NotTaken::Ineligible(Ineligible::CannotOpen))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let state = laptop.workspaces().state(&workspace).await.unwrap();
    assert!(
        state
            .workspace()
            .lease(&lease, tilo_clock::wall())
            .claims
            .is_empty()
    );
    desktop
        .workspaces()
        .run_agents(Arc::new(ActsAs::EveryAgent));
    takes_turn(desktop.workspaces(), &workspace, &unopened)
        .await
        .expect("a device that acts as the agent and opens Chat claims the turn");
}
