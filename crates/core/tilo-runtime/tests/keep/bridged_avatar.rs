//! A bridged person's avatar, which only the workspace's host keeps: a member's
//! Chat asks the workspace for it over its link, as it asks any member, and the
//! host answers in pieces addressed to that member alone.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the server and the member's link live until the test ends"
)]

use neighbour_wire::NeighbourMsg;
use tilo_peer_state::peer_handle;
use tilo_runtime::device::keep::server::Server;
use tilo_server_protocol::config::ServerConfig;
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use tilo_workspace_service::workspaces::WorkspaceSummary;

use crate::keep::common::{Linked, TestDevice, TestUser, start_server};

const CHAT: &str = "so.tilo.chat";

/// A workspace on `server` that `member` entered as its owner's, and the
/// member's link to it.
async fn member_in_a_new_workspace(
    server: &Server,
    member: &TestDevice,
) -> (WorkspaceSummary, Linked) {
    let workspaces = server.workspaces().unwrap();
    let workspace = workspaces.create("Bridged").await.unwrap();
    workspaces
        .enter(&workspace.workspace, (&member.cert, &member.sk))
        .await
        .unwrap();
    let linked = Linked::open(server, member, workspace.workspace_id)
        .await
        .expect("the workspace admits the member it entered");
    (workspace, linked)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_fetches_a_bridged_avatar_from_the_workspace_host() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = start_server(
        ServerConfig::loopback(state.path().to_path_buf()),
        tilo_runtime::device::keep::server::ServerIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();

    let (workspace, mut linked) = member_in_a_new_workspace(&server, &laptop).await;

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
    let want = |hash: &str| NeighbourMsg::DocumentContentWants {
        requester,
        cluster_id: workspace.workspace.clone(),
        wants: vec![neighbour_wire::content::Wanted {
            app_id: CHAT.into(),
            hash: hash.into(),
            have: Vec::new(),
        }],
    };
    // Content the host does not keep is answered with word that it keeps
    // none; the avatar it keeps comes back whole.
    linked.send(want(&"0".repeat(64))).unwrap();
    linked.send(want(&hash)).unwrap();

    let mut received = Vec::new();
    let mut lacking = Vec::new();
    tilo_test_env::within("the avatar arrives", async {
        loop {
            let heard = linked.next().await.expect("the link stays open");
            // The link first catches the member up on the workspace, each
            // change under the device that signed it, its own entry
            // included. Content answers come from the host alone.
            let answer = matches!(
                heard.message,
                NeighbourMsg::DocumentContentLacks { .. }
                    | NeighbourMsg::DocumentContentPiece { .. }
            );
            if answer {
                assert_eq!(heard.author, host.device_pubkey(), "the host answers");
            }
            match heard.message {
                NeighbourMsg::DocumentContentLacks { hash, .. } => {
                    lacking.push(hash);
                    if !received.is_empty() && received.len() == avatar.len() {
                        break;
                    }
                }
                NeighbourMsg::DocumentContentPieces {
                    requester: to,
                    cluster_id,
                    pieces,
                    ..
                } => {
                    for piece in pieces {
                        assert_eq!(
                            (
                                to,
                                piece.app_id.as_str(),
                                cluster_id.as_str(),
                                piece.hash.as_str()
                            ),
                            (requester, CHAT, workspace.workspace.as_str(), hash.as_str()),
                            "only the asked-for avatar is answered"
                        );
                        assert_eq!(
                            piece.offset,
                            received.len() as u64,
                            "pieces arrive in order"
                        );
                        assert_eq!(piece.total, avatar.len() as u64, "the whole avatar");
                        received.extend_from_slice(&piece.bytes);
                    }
                    if received.len() == avatar.len() && !lacking.is_empty() {
                        break;
                    }
                }
                // What the workspace sends its members besides content.
                _ => {}
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
