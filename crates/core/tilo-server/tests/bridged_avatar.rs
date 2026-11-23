//! A bridged person's avatar, which only the workspace's host keeps: a member's
//! Chat asks the workspace for it over its live session, as it asks any member,
//! and the host answers in pieces addressed to that member alone.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the server and the member's session live until the test ends"
)]

use neighbour_wire::NeighbourMsg;
use tilo_peer_state::peer_handle;
use tilo_server::coedit::{Inbound, ServerCoeditSession};
use tilo_server::workspaces::WorkspaceSummary;
use tilo_server::{Server, ServerClient, ServerConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, device_endpoint};

const CHAT: &str = "so.tilo.chat";

/// A workspace on `server` that `member` entered as its owner's, and the member's
/// live session in it.
async fn member_in_a_new_workspace(
    server: &Server,
    member: &TestDevice,
) -> (WorkspaceSummary, ServerCoeditSession) {
    let workspaces = server.workspaces().unwrap();
    let workspace = workspaces.create("Bridged").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &member.cert)
        .await
        .unwrap();
    let client = ServerClient::connect_to_pinned_host(
        device_endpoint(member).await,
        server.node_addr(),
        member.cert.clone(),
        member.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let session =
        ServerCoeditSession::join(client, workspace.workspace_id, member.device_pubkey()).unwrap();
    crate::common::wait_until_subscribed(server, workspace.workspace_id, &member.device_pubkey())
        .await;
    (workspace, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_fetches_a_bridged_avatar_from_the_workspace_host() {
    let alice = crate::common::TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = crate::common::start_server(
        ServerConfig::loopback(state.path().to_path_buf()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();

    let (workspace, session) = member_in_a_new_workspace(&server, &laptop).await;

    // What a bridge fetched from the other service, kept as the host keeps
    // it, in the workspace's content: larger than one piece, so the member
    // reassembles it.
    let avatar: Vec<u8> = (0..300_000_u32).map(|i| (i % 251) as u8).collect();
    let hash = server
        .content()
        .get(&workspace.workspace)
        .unwrap()
        .put(&avatar)
        .unwrap();

    let requester = peer_handle(&laptop.device_pubkey());
    let want = |hash: &str| {
        postcard::to_stdvec(&NeighbourMsg::DocumentContentWant {
            requester,
            app_id: CHAT.into(),
            cluster_id: workspace.workspace.clone(),
            hash: hash.into(),
            have: Vec::new(),
        })
        .unwrap()
    };
    // Content the host does not keep is answered with word that it keeps
    // none; the avatar it keeps comes back whole.
    session.publish_presence(want(&"0".repeat(64))).unwrap();
    session.publish_presence(want(&hash)).unwrap();

    let mut received = Vec::new();
    let mut lacking = Vec::new();
    tilo_test_env::within("the avatar arrives", async {
        loop {
            let Inbound::DocumentContent { from, frame } =
                session.recv().await.expect("the session stays open")
            else {
                continue;
            };
            assert_eq!(from, host.device_pubkey(), "the host answers");
            if let Ok(NeighbourMsg::DocumentContentLacks { hash, .. }) =
                postcard::from_bytes(&frame)
            {
                lacking.push(hash);
                if !received.is_empty() && received.len() == avatar.len() {
                    break;
                }
                continue;
            }
            let NeighbourMsg::DocumentContentPiece {
                requester: to,
                app_id,
                cluster_id,
                hash: piece_hash,
                total,
                offset,
                bytes,
                ..
            } = postcard::from_bytes(&frame).unwrap()
            else {
                panic!("a piece of content");
            };
            assert_eq!(
                (
                    to,
                    app_id.as_str(),
                    cluster_id.as_str(),
                    piece_hash.as_str()
                ),
                (requester, CHAT, workspace.workspace.as_str(), hash.as_str()),
                "only the asked-for avatar is answered"
            );
            assert_eq!(offset, received.len() as u64, "pieces arrive in order");
            received.extend_from_slice(&bytes);
            if received.len() as u64 == total && !lacking.is_empty() {
                break;
            }
        }
    })
    .await;
    assert_eq!(blake3::hash(&received).to_hex().as_str(), hash);
    assert_eq!(received, avatar);
    assert_eq!(
        lacking,
        ["0".repeat(64)],
        "what it does not keep, it says so"
    );
}
