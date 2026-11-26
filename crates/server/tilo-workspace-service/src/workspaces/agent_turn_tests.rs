//! Which device takes an agent's turn ([`takes_turn`] for [`Turn::Agent`]).
#![allow(
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps both devices to the end"
)]

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::super::Workspaces;
use super::super::fixtures::{TestHost, device_cert, test_host};
use super::super::work_tests::offer_compute;
use super::tests::{member_service, shared_with};
use crate::work::{AgentEngine, Ineligible, NotTaken, Turn, takes_turn};

const CHAT: &str = "so.tilo.chat";

/// An agent engine that holds an approved key of every agent, or of none.
struct Keys(bool);

impl AgentEngine for Keys {
    fn access<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a [u8; 32],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<crate::work::AgentAccess, Ineligible>>
                + Send
                + 'a,
        >,
    > {
        let access = if self.0 {
            crate::work::AgentAccess::Hosted {
                device: [0; 32],
                created_at: 0,
                scope: tilo_server_protocol::delegation::DelegateScope::default(),
            }
        } else {
            crate::work::AgentAccess::Device
        };
        Box::pin(std::future::ready(Ok(access)))
    }
}

/// The root of the person the agent acts for: the one the first device's fixture is
/// certified by.
fn persons_root() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

/// What somebody asked in Chat's `part` of the agent acting for `person`'s person, and the lease
/// it is claimed under.
fn turn_in(person: &DeviceCert, part: &str) -> (Turn, String) {
    let lease = format!("turn/scout/{part}");
    let turn = Turn::Agent {
        agent: "scout".to_owned(),
        signer: "scout".to_owned(),
        place: part.to_owned(),
        acts_for: person.user_public_key,
        app: CHAT.to_owned(),
        record: part.to_owned(),
    };
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
    desktop.workspaces().run_agents(Arc::new(Keys(false)));
    laptop.workspaces().run_agents(Arc::new(Keys(false)));
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

/// Both devices of the person the agent acts for read what started the turn and claim it. Once the claims meet, the
/// first keeps the turn and the other's renewal says it lost, so that device stops before it
/// writes; asked again, it says whose turn it is. A minute after the desktop last wrote, the
/// laptop takes the turn over.
#[tokio::test]
async fn two_devices_start_one_turn_and_exactly_one_answers_until_its_claim_goes_silent() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let laptop = device_cert(&persons_root(), "laptop", DeviceCapabilities::member());
    let (desktop, laptop, workspace) = two_devices((one.path(), two.path()), laptop, 41).await;
    let person = desktop.workspaces().serving_certificate().clone();
    let (turn, lease) = turn_in(&person, "messages-1");

    let first = takes_turn(desktop.workspaces(), &workspace, &turn)
        .await
        .expect("the desktop claims the turn");
    let second = takes_turn(laptop.workspaces(), &workspace, &turn).await;
    reads_claim(
        laptop.workspaces(),
        &workspace,
        &lease,
        person.device_public_key,
    )
    .await;
    let taken_by = NotTaken::TakenBy(person.device_public_key);
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

/// A device of another person runs no agent it holds no key of, whoever asked; one that holds
/// the agent's approved key runs it. A device the workspace keeps from what the turn is about
/// refuses for that reason and writes no claim.
#[tokio::test]
async fn only_a_device_that_holds_the_agents_key_or_belongs_to_the_person_it_acts_for_claims_what_it_may_open()
 {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let other = device_cert(
        &SigningKey::from_bytes(&[42; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let reader = other.1.user_public_key;
    let (desktop, laptop, workspace) = two_devices((one.path(), two.path()), other, 42).await;
    let person = desktop.workspaces().serving_certificate().clone();
    let (turn, _) = turn_in(&person, "messages-2");
    assert_eq!(
        takes_turn(laptop.workspaces(), &workspace, &turn)
            .await
            .err(),
        Some(NotTaken::Ineligible(Ineligible::CannotRunAgent)),
    );
    laptop.workspaces().run_agents(Arc::new(Keys(true)));
    takes_turn(laptop.workspaces(), &workspace, &turn)
        .await
        .expect("a device that holds the agent's approved key runs it");

    // Without the agent's key again, the device claims nothing while the grant reaches it.
    laptop.workspaces().run_agents(Arc::new(Keys(false)));
    let (unopened, lease) = turn_in(&person, "messages-3");
    desktop
        .workspaces()
        .put_workspace_grants(
            &workspace,
            &person,
            [42; 32],
            vec![Grant::new(
                "no-chat",
                Grantee::person(hex::encode(reader)),
                TreePath::workspace(&workspace).app(CHAT),
                KindSet::READ,
                Answer::Deny,
                Grantee::person(hex::encode(person.user_public_key)),
            )],
        )
        .await
        .unwrap();
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
    takes_turn(desktop.workspaces(), &workspace, &unopened)
        .await
        .expect("the person's device that opens Chat claims the turn");
}

/// A server of the person's whose entry offers no compute never runs their agent; once it offers
/// compute, it does.
#[tokio::test]
async fn a_server_that_offers_no_compute_never_claims() {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = device_cert(&persons_root(), "server", DeviceCapabilities::member());
    let server = test_host(state.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Asked").await.unwrap().workspace;
    workspaces.run_agents(Arc::new(Keys(false)));
    let (turn, lease) = turn_in(&cert, "messages-4");
    offer_compute(&server, &cert, false).await;
    assert_eq!(
        takes_turn(workspaces, &workspace, &turn).await.err(),
        Some(NotTaken::Ineligible(Ineligible::OffersNoCompute)),
    );
    let state = workspaces.state(&workspace).await.unwrap();
    assert!(
        state
            .workspace()
            .lease(&lease, tilo_clock::wall())
            .claims
            .is_empty()
    );
    offer_compute(&server, &cert, true).await;
    takes_turn(workspaces, &workspace, &turn)
        .await
        .expect("a server that offers compute claims it");
    server.shutdown().await;
}
