//! The person's agent door on their host, as their page reads and uses it: a
//! computer they logged in with `tilo login` that runs an agent is listed
//! with its open run; stopping the run ends that session and the agent can
//! come back; the root revoking the computer ends its session at once and
//! refuses it after. Nobody else stops or revokes it, and a tab posting as
//! a person is no agent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives until the test ends"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use std::time::Duration;

use tilo_actions::InvocationId;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::agent_door::{self, Door};
use tilo_actions::remote::{ActionHost as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus};
use tilo_browser_peer::PeerHandle;
use tilo_browser_peer::login::say_here;
use tilo_browser_peer::revoke_device;
use tilo_browser_peer::session_actions::SessionActions;
use tilo_user_identity::RevocationReason::Retired;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{door, enrol, enter, in_workspace, place, start_host, start_peer, wait_for};

fn now() -> u64 {
    tilo_clock::unix_now_ns()
}

fn caller() -> Caller {
    Caller::local_person("test")
}

/// Ask `door` for `action` with `argument`; whether the host took it.
fn ask(door: &SessionActions, action: &str, argument: InvokeArgument) -> (InvocationId, bool) {
    let invocation = InvocationId::new();
    let receipt = tokio::task::block_in_place(|| {
        door.invoke(
            &caller(),
            InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: agent_door::target(action),
                argument,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            },
        )
        .wait_timeout(Duration::from_secs(30))
    });
    let taken = matches!(
        receipt,
        Ok(Some(Ok(ref receipt))) if !matches!(receipt.status, ReceiptStatus::Rejected { .. })
    );
    (invocation, taken)
}

/// The door as the follow `read` reads it now.
fn door_now(door: &SessionActions, read: &InvocationId) -> Option<Door> {
    let state = tokio::task::block_in_place(|| door.observe(&caller(), read))?;
    match state.update {
        InvocationUpdate::Item { value, .. } => serde_json::from_value(value).ok(),
        _ => None,
    }
}

/// Wait until the door `read` follows reads as `wanted` says.
async fn door_until(
    what: &str,
    door: &SessionActions,
    read: &InvocationId,
    wanted: impl Fn(&Door) -> bool + Sync,
) -> Door {
    wait_for(what, || {
        let now = door_now(door, read).filter(|now| wanted(now));
        async move { now }
    })
    .await
}

fn stop(grant: &str) -> InvokeArgument {
    agent_door::argument(&agent_door::RunRef {
        grant: grant.to_owned(),
        run: "Claude Code".into(),
    })
}

async fn agent_says_here(agent: &PeerHandle) -> bool {
    say_here(agent, support::workspace(), "Claude Code", Vec::new(), true)
        .await
        .is_ok()
}

/// Someone else (`other`) reads nothing of the door and stops nothing of
/// the computer `device` the agent runs on; the agent itself keeps no door.
async fn nobody_else_reaches(
    other: &PeerHandle,
    agent: &PeerHandle,
    device: [u8; 32],
    host_key: [u8; 32],
) {
    let workspace = support::workspace();
    let grant = hex::encode(device);
    let others = SessionActions::new(other, workspace, host_key);
    assert!(!ask(&others, agent_door::DOOR, InvokeArgument::None).1);
    assert!(!ask(&others, agent_door::STOP, stop(&grant)).1);
    let own = SessionActions::new(agent, workspace, host_key);
    assert!(!ask(&own, agent_door::STOP, stop(&grant)).1);
    let still = say_here(agent, workspace, "Claude Code", Vec::new(), true).await;
    assert!(
        still.is_ok(),
        "the agent is still in: {still:?} {:?}",
        agent.status().failures
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_persons_door_lists_stops_and_revokes_their_agent_and_nobody_elses() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let (other_root, _) = UserIdentity::generate("Bob", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let stranger = enrol(&other_root, "browser", DeviceCapabilities::member());
    let state = support::scratch_dir();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let workspace = support::workspace();
    let mut peers = Vec::new();
    for device in [&browser, &computer] {
        place(&host.relay, &device.cert).await;
        let peer = start_peer(
            host.iroh(),
            device.cert.clone(),
            host_device.cert.clone(),
            device.key.clone(),
        );
        peer.join(workspace).unwrap();
        in_workspace(&peer, workspace).await;
        peers.push(peer);
    }
    // Someone else is a member of the workspace, let in by its owner.
    let person = other_root.verifying_key().to_bytes();
    let invited = door(&host, &root, &host_device, support::WORKSPACE, person).await;
    let (cert, key) = (&stranger.cert, &stranger.key);
    peers.push(enter(&host, invited, cert, key, &host_device.cert, workspace).await);
    let [page, agent, other] = <[PeerHandle; 3]>::try_from(peers).ok().unwrap();
    let host_key = host_device.cert.device_public_key;
    let grant = hex::encode(computer.cert.device_public_key);

    // The tab says who is here as a person; the computer as the agent it runs.
    say_here(&page, workspace, "Ada", Vec::new(), false)
        .await
        .unwrap();
    assert!(agent_says_here(&agent).await);
    let door = SessionActions::new(&page, workspace, host_key);
    let (read, taken) = ask(&door, agent_door::DOOR, InvokeArgument::None);
    assert!(taken, "the person reads their own door");
    let listed = door_until("the door to list the agent's run", &door, &read, |door| {
        door.run("Claude Code").is_some()
    })
    .await;
    assert_eq!(listed.grants.len(), 1, "the tab is no agent: {listed:?}");
    assert_eq!(listed.grants[0].id, grant);
    assert_eq!(listed.grants[0].runs.len(), 1);
    assert_eq!(listed.grants[0].reach, agent_door::Reach::Account);

    nobody_else_reaches(&other, &agent, computer.cert.device_public_key, host_key).await;
    assert!(door_now(&door, &read).unwrap().run("Claude Code").is_some());

    // Stopping the run ends that session; the computer comes back as the
    // same agent, a new run.
    assert!(ask(&door, agent_door::STOP, stop(&grant)).1);
    door_until("the stopped run to end", &door, &read, |door| {
        door.grants
            .iter()
            .any(|g| g.id == grant && g.runs.is_empty())
    })
    .await;
    in_workspace(&agent, workspace).await;
    wait_for("the agent to come back", || async {
        agent_says_here(&agent).await.then_some(())
    })
    .await;
    door_until("the agent's new run", &door, &read, |door| {
        door.run("Claude Code").is_some()
    })
    .await;

    // The root revokes the computer: its session ends at once, it leaves the
    // door, and the host refuses it from then on.
    let entry = revoke_device(&root, computer.cert.device_public_key, now(), Retired)
        .await
        .unwrap();
    page.revoke(entry).unwrap();
    door_until(
        "the revoked agent to leave the door",
        &door,
        &read,
        |door| door.grants.is_empty(),
    )
    .await;
    // Its session ended, and its next attempt to connect was turned away.
    wait_for("the host to refuse the revoked computer", || {
        let status = agent.status();
        async move { (!status.authenticated && status.attempt >= 1).then_some(()) }
    })
    .await;
    assert!(
        !agent_says_here(&agent).await,
        "a revoked computer is not let in"
    );
}
