//! Native governance can add and remove independent people while the server is live.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]

use std::path::Path;
use std::time::Duration;

use tilo_peer_state::join::Role;
use tilo_server::workspaces::RemoveMemberRequest;
use tilo_server::{Envelope, Server, ServerClient, ServerConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, hold_workspace, invitation, redeem};
use tilo_server::WorkspaceId;

async fn connect(
    server: &Server,
    device: crate::common::TestDevice,
) -> Result<ServerClient, tilo_server::ClientError> {
    ServerClient::connect_to_pinned_host(
        client_endpoint().await,
        server.node_addr(),
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
}

/// How long a refused change is given to surface before its absence counts.
const QUIET: Duration = Duration::from_millis(400);

/// Whether `bytes` carries `marker`.
fn carries(bytes: &[u8], marker: &[u8]) -> bool {
    bytes.windows(marker.len()).any(|window| window == marker)
}

/// Nothing of the refused change reaches `client` within [`QUIET`]: the
/// workspace publishes the removal itself, and that is not the change.
async fn refused_change_is_unseen(client: &ServerClient, marker: &[u8]) -> bool {
    let deadline = tokio::time::Instant::now().checked_add(QUIET).unwrap();
    while let Ok(Some(envelope)) = tokio::time::timeout_at(deadline, client.recv()).await {
        if let Envelope::Delta { delta, .. } = &envelope {
            assert!(
                !carries(delta, marker),
                "the live member heard the refused change"
            );
        }
    }
    true
}

/// Waits until `client`'s session ends, whatever the server sends it first.
async fn closes(client: &ServerClient) {
    tilo_test_env::within("the session to close", async {
        while client.recv().await.is_some() {}
    })
    .await;
}

/// The server's first answer to `client` subscribing to `workspace`: its state
/// comes first on a workspace the server holds.
async fn subscribe(client: &ServerClient, workspace_id: WorkspaceId) -> Option<Envelope> {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("server answered subscription", async {
        loop {
            match client.recv().await {
                Some(answer @ (Envelope::Ready { .. } | Envelope::NotAuthorised { .. })) => {
                    break Some(answer);
                }
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await
}

fn retained_delta(body: &[u8]) -> Vec<u8> {
    tilo_server::coedit::CoeditPayload::RetainedDelta {
        channel: [9; 32],
        body: body.to_vec(),
    }
    .encode()
    .unwrap()
}

fn persisted(root: &Path, marker: &[u8]) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            persisted(&path, marker)
        } else {
            std::fs::read(&path)
                .is_ok_and(|bytes| bytes.windows(marker.len()).any(|window| window == marker))
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn governed_access_removes_a_live_person_without_widening_other_workspaces() {
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
    let alice_reconnect_cert = alice_a.cert.clone();
    let alice_reconnect_key = alice_a.sk.clone();
    let bob_key = bob_device.device_pubkey();
    let charlie_key = charlie_device.device_pubkey();
    let bob_restart_cert = bob_device.cert.clone();
    let bob_restart_key = bob_device.sk.clone();
    let state = tempfile::tempdir().unwrap();
    let server_cert = server_dev.cert.clone();
    let server_key = server_dev.sk.clone();
    let server = crate::common::start_server(
        ServerConfig::loopback(state.path().to_path_buf()),
        tilo_server::daemon::ServerIdentity {
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

    let alice_client = connect(&server, alice_a).await.unwrap();
    let bob_client = connect(&server, bob_device).await.unwrap();
    assert!(matches!(
        subscribe(&alice_client, shared).await,
        Some(Envelope::Ready { .. })
    ));
    assert!(matches!(
        subscribe(&bob_client, shared).await,
        Some(Envelope::Ready { .. })
    ));
    // Charlie's device may serve workspaces of his own; here it is one member
    // the workspace lists, so it takes part like any other.
    let charlie_client = connect(&server, charlie_device).await.unwrap();
    assert!(matches!(
        subscribe(&charlie_client, shared).await,
        Some(Envelope::Ready { .. })
    ));
    assert!(connect(&server, diana_device).await.is_err());

    // A device of a member's person joins as its person does: membership
    // follows the person, not one device key.
    let alice_new = connect(
        &server,
        alice.enrol("alice-b", DeviceCapabilities::primary()),
    )
    .await
    .unwrap();
    assert!(matches!(
        subscribe(&alice_new, shared).await,
        Some(Envelope::Ready { .. })
    ));
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

    // The already-connected writer gets an explicit authenticated refusal;
    // closure or lack of fanout alone is not sufficient revocation evidence.
    // The server sends it as it removes her, before an audit may close her
    // stream, so a write racing the removal can find the session already
    // ended (the client fails closed when the server closes); it never lands.
    let _ = alice_client.send(Envelope::Delta {
        workspace_id: shared,
        author_device_public_key: alice_a_key,
        authored_at_ns: 2,
        delta: retained_delta(b"removed-live-writer"),
        signature: [0; 64],
    });
    let told = tilo_test_env::within("a message on alice_client", async {
        loop {
            match alice_client.recv().await {
                Some(answer @ Envelope::NotAuthorised { workspace_id, .. })
                    if workspace_id == shared =>
                {
                    break Some(answer);
                }
                None => break None,
                Some(_) => {}
            }
        }
    })
    .await;
    assert!(
        matches!(
            &told,
            Some(Envelope::NotAuthorised { workspace_id, refusal })
                if *workspace_id == shared
                    && *refusal == tilo_server_protocol::refusal::Refusal::NotAMember(
                        "workspace membership changed".into()
                    )
        ),
        "{told:?}"
    );
    assert!(
        refused_change_is_unseen(&bob_client, b"removed-live-writer").await,
        "the live member hears nothing of the refused change"
    );
    assert!(
        !persisted(state.path(), b"removed-live-writer"),
        "explicitly refused retained delta must not reach durable state"
    );

    // Alice stays globally handshake-admitted through workspace 702, but the
    // periodic audit then closes her workspace-701 stream and that global
    // admission cannot reopen workspace 701.
    closes(&alice_client).await;
    assert!(!server.is_subscribed(shared, &alice_a_key).await);
    assert!(server.is_subscribed(shared, &bob_key).await);
    assert!(server.is_subscribed(shared, &charlie_key).await);

    let alice_reconnect = ServerClient::connect_to_pinned_host(
        client_endpoint().await,
        server.node_addr(),
        alice_reconnect_cert,
        alice_reconnect_key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    assert!(matches!(
        subscribe(&alice_reconnect, shared).await,
        Some(Envelope::NotAuthorised { .. })
    ));
    assert!(matches!(
        subscribe(&alice_reconnect, private).await,
        Some(Envelope::Ready { .. })
    ));
    alice_reconnect
        .send(Envelope::Delta {
            workspace_id: shared,
            author_device_public_key: alice_a_key,
            authored_at_ns: 2,
            delta: b"removed-writer".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    assert!(
        refused_change_is_unseen(&bob_client, b"removed-writer").await,
        "the live member hears nothing of the refused change"
    );
    closes(&alice_reconnect).await;

    // Another device of Alice's joins the workspace her person is still in,
    // and the one her person left no longer admits her.
    let alice_workspace_b = connect(
        &server,
        alice.enrol("alice-workspace-b", DeviceCapabilities::primary()),
    )
    .await
    .unwrap();
    assert!(matches!(
        subscribe(&alice_workspace_b, private).await,
        Some(Envelope::Ready { .. })
    ));
    assert!(matches!(
        subscribe(&alice_workspace_b, shared).await,
        Some(Envelope::NotAuthorised { .. })
    ));

    bob_client
        .send(Envelope::Delta {
            workspace_id: shared,
            author_device_public_key: bob_key,
            authored_at_ns: 1,
            delta: b"after-removal".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    closes(&alice_reconnect).await;

    drop(alice_client);
    drop(alice_workspace_b);
    drop(alice_reconnect);
    drop(bob_client);
    drop(charlie_client);
    server.shutdown().await;

    let restarted = crate::common::start_server(
        ServerConfig::loopback(state.path().to_path_buf()),
        tilo_server::daemon::ServerIdentity {
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
    let bob_after_restart = ServerClient::connect_to_pinned_host(
        client_endpoint().await,
        restarted.node_addr(),
        bob_restart_cert,
        bob_restart_key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    assert!(matches!(
        subscribe(&bob_after_restart, shared).await,
        Some(Envelope::Ready { .. })
    ));
    let alice_after_restart = connect(
        &restarted,
        alice.enrol("alice-after-restart", DeviceCapabilities::primary()),
    )
    .await
    .expect("workspace 702 keeps Alice's root in the handshake union");
    assert!(matches!(
        subscribe(&alice_after_restart, shared).await,
        Some(Envelope::NotAuthorised { .. })
    ));
    assert!(matches!(
        subscribe(&alice_after_restart, private).await,
        Some(Envelope::Ready { .. })
    ));
}
