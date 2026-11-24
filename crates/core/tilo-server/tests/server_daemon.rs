//! A host relays a member's write to the workspace's other members, hands a device that links
//! later what it missed, and never sends a device its own write back.
//!
//! Spins up:
//! - one host bound to workspace 42, membership = {A, B}
//! - two member devices A + B, each with a link to the workspace
//!
//! A publishes a record change. B's copy takes the same change in, and A's link hands it
//! nothing back.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use automerge::{ROOT, transaction::Transactable as _};
use tilo_server::WorkspaceId;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{KEYS, Linked, TestUser, opened, spawn_server, wait_until_subscribed};

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(42);

/// The app and record the members write through their links.
const APP: &str = "test.app";
const RECORD: &str = "note:1";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn relay_daemon_bootstraps_remembers_and_fans_out() {
    let user = TestUser::fresh("Alice");
    let server_dev = user.enrol("home-server", DeviceCapabilities::member());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("new-phone", DeviceCapabilities::member());

    let server = Box::pin(spawn_server(&user, server_dev, &[WORKSPACE], &[&a, &b])).await;
    let workspaces = server
        .server
        .workspaces()
        .expect("the server holds workspaces");

    // What B held of the workspace before A wrote anything: the host hands a
    // device that links later everything it lacks from here.
    let seed = workspaces
        .state(&WORKSPACE.to_string())
        .await
        .expect("the host holds the workspace");

    // Phase `first_time_home_device_bootstraps_from_retained_checkpoint`.
    let mut a_link = Linked::open(&server.server, &a, WORKSPACE)
        .await
        .expect("the workspace admits A's device");
    wait_until_subscribed(&server.server, WORKSPACE, &a.device_pubkey()).await;
    a_link
        .write(&a.sk, (APP, RECORD), |document| {
            document.put(ROOT, "body", "baseline-before-phone").unwrap();
        })
        .expect("A writes into the workspace");

    // B links from what it held before: the change it missed reaches it over
    // the link, not from the seed.
    let b_link = Linked::open_seeded(&server.server, &b, WORKSPACE, seed)
        .await
        .expect("the workspace admits B's device");
    crate::common::receives(&b_link, |peer| {
        opened(peer, &KEYS, (APP, RECORD)).is_some()
    })
    .await;

    // Phase `home_host_remembers_authenticated_devices_for_offline_delivery`.
    // Both devices are on the workspace's signed member list, the one record
    // that admits them.
    let members = crate::common::member_devices(&server.server, WORKSPACE).await;
    assert!(
        members.contains(&a.device_pubkey()) && members.contains(&b.device_pubkey()),
        "both devices persist as members"
    );

    // Phase `two_peers_fan_out_through_relay`: a write A makes now reaches
    // B's live link.
    a_link
        .write(&a.sk, (APP, RECORD), |live| {
            live.put(ROOT, "body", "the-crdt-delta").unwrap();
        })
        .expect("A writes again");

    // A does NOT receive its own write back (author-exclusion).
    let echoed = tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            match a_link.next().await {
                Some(heard) if heard.author == a.device_pubkey() => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await;
    assert!(
        !matches!(echoed, Ok(true)),
        "A should not receive its own write"
    );
    assert!(
        server
            .server
            .is_subscribed(WORKSPACE, &a.device_pubkey())
            .await
            && server
                .server
                .is_subscribed(WORKSPACE, &b.device_pubkey())
                .await,
        "at least both members' links are up"
    );
}
