//! A bridged person's avatar, which only the room's host keeps: a member's
//! Chat asks the room for it over its live session, as it asks any member,
//! and the host answers in pieces addressed to that member alone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the relay and the member's session live until the test ends"
)]

use std::sync::Arc;

use neighbour_wire::NeighbourMsg;
use tilo_connector::BlobAccess as _;
use tilo_peer_state::peer_handle;
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::room_bots::installed::FsBlobs;
use tilo_relay::rooms::RoomSummary;
use tilo_relay::{Relay, RelayClient, RelayConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::TestDevice;

const CHAT: &str = "so.tilo.chat";

async fn device_endpoint(device: &TestDevice) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.sk.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// A room on `relay` that `member` entered as its owner's, and the member's
/// live session in it.
async fn member_in_a_new_room(
    relay: &Relay,
    member: &TestDevice,
) -> (RoomSummary, RelayCoeditSession) {
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Bridged").await.unwrap();
    rooms.enter(&room.room, &member.cert).await.unwrap();
    let client = RelayClient::connect_to_pinned_host(
        device_endpoint(member).await,
        relay.node_addr(),
        member.cert.clone(),
        member.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let session =
        RelayCoeditSession::join(client, room.workspace_id, member.device_pubkey()).unwrap();
    crate::common::wait_until_subscribed(relay, room.workspace_id, &member.device_pubkey()).await;
    (room, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_fetches_a_bridged_avatar_from_the_room_host() {
    let alice = crate::common::TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();

    // What a bridge fetched from the other service, kept as the host keeps
    // it: larger than one piece, so the member reassembles it.
    let blobs = Arc::new(FsBlobs::new(state.path().join("blobs"), None).unwrap());
    let avatar: Vec<u8> = (0..300_000_u32).map(|i| (i % 251) as u8).collect();
    let hash = blobs.store(&avatar).unwrap();
    relay
        .rooms()
        .expect("the host governs rooms")
        .serve_content(blobs);

    let (room, session) = member_in_a_new_room(&relay, &laptop).await;

    let requester = peer_handle(&laptop.device_pubkey());
    let want = |hash: &str| {
        postcard::to_stdvec(&NeighbourMsg::RecordContentWant {
            requester,
            app_id: CHAT.into(),
            cluster_id: room.room.clone(),
            hash: hash.into(),
        })
        .unwrap()
    };
    // Content the host does not keep goes unanswered; the avatar it keeps
    // comes back whole.
    session.publish_presence(want(&"0".repeat(64))).unwrap();
    session.publish_presence(want(&hash)).unwrap();

    let mut received = Vec::new();
    tilo_test_env::within("the avatar arrives", async {
        loop {
            let Inbound::RecordContent { from, frame } =
                session.recv().await.expect("the session stays open")
            else {
                continue;
            };
            assert_eq!(from, host.device_pubkey(), "the host answers");
            let NeighbourMsg::RecordContentPiece {
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
                (requester, CHAT, room.room.as_str(), hash.as_str()),
                "only the asked-for avatar is answered"
            );
            assert_eq!(offset, received.len() as u64, "pieces arrive in order");
            received.extend_from_slice(&bytes);
            if received.len() as u64 == total {
                break;
            }
        }
    })
    .await;
    assert_eq!(blake3::hash(&received).to_hex().as_str(), hash);
    assert_eq!(received, avatar);
}
