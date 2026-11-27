//! Native governance can add and remove independent people while the server is live.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]

use std::time::Duration;

use tilo_peer_state::direct::Kept;
use tilo_peer_state::join::Role;
use tilo_server_protocol::config::ServerConfig;
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use tilo_workspace_service::workspaces::RemoveMemberRequest;

use crate::keep::common::{
    KEYS, Linked, RECORDS_APP as APP, TestUser, copy_says, hold_workspace, invitation, opened,
    redeem, write,
};
use tilo_server_protocol::WorkspaceId;

/// The record the devices write through their links.
const RECORD: &str = "note:1";

/// How long a refused change is given to surface before its absence counts.
const QUIET: Duration = Duration::from_millis(400);

/// `linked`'s copy of the record says `marker`.
fn carries(linked: &Linked, marker: &str) -> bool {
    linked.read(|peer| copy_says(peer, RECORD, marker))
}

/// Nothing of the refused change reaches `linked` within [`QUIET`]: the workspace publishes the
/// removal itself, and that is not the change.
async fn refused_change_is_unseen(linked: &Linked, marker: &str) -> bool {
    tokio::time::timeout(QUIET, async {
        loop {
            assert!(
                !carries(linked, marker),
                "the live member heard the refused change"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_err()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_list_access_removes_a_live_person_without_widening_other_workspaces() {
    let provider = TestUser::fresh("Provider");
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let charlie = TestUser::fresh("Charlie");
    let diana = TestUser::fresh("Diana");
    let server_dev = provider.enrol("server", DeviceCapabilities::member());
    let alice_a = alice.enrol("alice-a", DeviceCapabilities::primary());
    let bob_device = bob.enrol("bob", DeviceCapabilities::primary());
    let charlie_device = charlie.enrol("charlie-host", DeviceCapabilities::member());
    let diana_device = diana.enrol("diana", DeviceCapabilities::primary());
    let alice_a_key = alice_a.device_pubkey();
    let bob_key = bob_device.device_pubkey();
    let charlie_key = charlie_device.device_pubkey();
    let state = tempfile::tempdir().unwrap();
    let server_cert = server_dev.cert.clone();
    let server_key = server_dev.sk.clone();
    let server = crate::keep::common::start_server(
        ServerConfig::loopback(),
        state.path().to_path_buf(),
        tilo_runtime::device::keep::server::ServerIdentity {
            user_public_key: provider.user_pk,
            device_signing_key: server_key.clone(),
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();

    let workspaces = server.workspaces().unwrap();
    let (shared, private) = (WorkspaceId::from_u128(701), WorkspaceId::from_u128(702));
    for workspace in [shared, private] {
        hold_workspace(&server, workspace, &[]).await;
    }
    for (workspace, member, role) in [
        (shared, &alice_a, Role::Editor),
        (shared, &bob_device, Role::Editor),
        (shared, &charlie_device, Role::Editor),
        (private, &alice_a, Role::Editor),
    ] {
        let issued = invitation(
            workspaces,
            &server_dev,
            &workspace.to_string(),
            role,
            member,
        )
        .await;
        redeem(member, &issued).await.unwrap();
    }

    let alice_shared = Linked::open(&server, &alice_a, shared)
        .await
        .expect("the shared workspace admits Alice's device");
    let bob_shared = Linked::open(&server, &bob_device, shared)
        .await
        .expect("the shared workspace admits Bob's device");
    // Charlie's device may serve workspaces of his own; here it is one member
    // the workspace lists, so it takes part like any other.
    let charlie_shared = Linked::open(&server, &charlie_device, shared)
        .await
        .expect("the shared workspace admits Charlie's device");
    assert!(
        Linked::open(&server, &diana_device, shared).await.is_err(),
        "a person the workspace does not list links it"
    );

    // A device of a member's person joins as its person does: membership
    // follows the person, not one device key.
    let alice_new = Linked::open(
        &server,
        &alice.enrol("alice-b", DeviceCapabilities::primary()),
        shared,
    )
    .await
    .expect("another device of Alice's enters the workspace her person is in");
    drop(alice_new);

    workspaces
        .remove_member(
            &shared.to_string(),
            &server_dev.cert,
            RemoveMemberRequest {
                request_id: [0x71; 32],
                user_public_key: alice.user_pk,
            },
        )
        .await
        .unwrap();

    // The already-connected writer gets an explicit authenticated refusal, or finds its link
    // ended: closure or lack of fanout alone is not sufficient revocation evidence. It is sent
    // as she is removed, before an audit may close her link, so a write racing the removal can
    // find the link already ended; it never lands.
    let _ = write(&alice_shared, &alice_a, RECORD, "removed-live-writer");
    let mut answers = alice_shared.answers();
    let told = tokio::time::timeout(QUIET, async {
        loop {
            match answers.recv().await {
                // A kept write would be the refusal failing; a refused one is the evidence.
                Ok(answered) => {
                    if answered.kept == Kept::Durable {
                        return false;
                    }
                }
                // The link ended: the client fails closed on that.
                Err(_) => return true,
            }
        }
    })
    .await;
    assert!(
        !matches!(told, Ok(false)),
        "the removed writer's write was kept"
    );
    assert!(
        refused_change_is_unseen(&bob_shared, "removed-live-writer").await,
        "the live member hears nothing of the refused change"
    );
    assert!(
        !carries(&bob_shared, "removed-live-writer"),
        "explicitly refused write must not reach durable state"
    );

    // Alice stays globally handshake-admitted through workspace 702, but the
    // periodic audit then closes her workspace-701 link and that global
    // admission cannot reopen workspace 701.
    tilo_test_env::within("the removed member's link to end", async {
        while server.is_subscribed(shared, &alice_a_key).await {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(!server.is_subscribed(shared, &alice_a_key).await);
    assert!(server.is_subscribed(shared, &bob_key).await);
    assert!(server.is_subscribed(shared, &charlie_key).await);

    // The workspace her person still belongs to admits her device; the one her person left
    // admits no link at all, so nothing of hers is addressed to it.
    let alice_private = Linked::open(&server, &alice_a, private)
        .await
        .expect("the workspace her person is still in admits her device");
    assert!(
        Linked::open(&server, &alice_a, shared).await.is_err(),
        "the workspace her person left admits her device again"
    );
    drop(alice_private);

    // Another device of Alice's joins the workspace her person is still in,
    // and the one her person left no longer admits her.
    let alice_workspace_b = Linked::open(
        &server,
        &alice.enrol("alice-workspace-b", DeviceCapabilities::primary()),
        private,
    )
    .await
    .expect("the workspace her person is still in admits her new device");
    assert!(
        Linked::open(
            &server,
            &alice.enrol("alice-workspace-c", DeviceCapabilities::primary()),
            shared,
        )
        .await
        .is_err(),
        "the workspace her person left admits her new device"
    );

    // The host keeps Bob's write before it stops: the restart below hands it back from what the
    // host kept, not from Bob's own copy.
    let mut kept = bob_shared.answers();
    write(&bob_shared, &bob_device, RECORD, "after-removal").unwrap();
    crate::keep::common::receives(&bob_shared, |peer| {
        opened(peer, &KEYS, (APP, RECORD)).is_some()
    })
    .await;
    tilo_test_env::within("the host keeps Bob's write", async {
        while kept.recv().await.expect("Bob's link stays open").kept != Kept::Durable {}
    })
    .await;

    drop(alice_shared);
    drop(alice_workspace_b);
    drop(bob_shared);
    drop(charlie_shared);
    server.shutdown().await;

    let restarted = crate::keep::common::start_server(
        ServerConfig::loopback(),
        state.path().to_path_buf(),
        tilo_runtime::device::keep::server::ServerIdentity {
            user_public_key: provider.user_pk,
            device_signing_key: server_key,
            device_cert: server_cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    // A server holds no list of workspaces of its own: on start, each
    // workspace's governance restates who it admits.
    let bob_after_restart = Linked::open(&restarted, &bob_device, shared)
        .await
        .expect("the restarted host admits Bob to the shared workspace");
    crate::keep::common::receives(&bob_after_restart, |peer| {
        opened(peer, &KEYS, (APP, RECORD)).is_some()
    })
    .await;
    let alice_after_restart = Linked::open(
        &restarted,
        &alice.enrol("alice-after-restart", DeviceCapabilities::primary()),
        private,
    )
    .await
    .expect("workspace 702 keeps Alice's root in the handshake union");
    assert!(
        Linked::open(
            &restarted,
            &alice.enrol("alice-after-restart-b", DeviceCapabilities::primary()),
            shared,
        )
        .await
        .is_err(),
        "the restarted host admits a removed person to the shared workspace"
    );
    drop(alice_after_restart);
    drop(bob_after_restart);
    restarted.shutdown().await;
}
