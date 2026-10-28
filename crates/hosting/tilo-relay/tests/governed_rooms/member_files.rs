//! A visitor's file reaches the room's host whole, as large as the host
//! keeps: the pieces answering the host's own request count against its
//! attachment quotas, not the visitor's hourly content budget, which is far
//! smaller than the largest file the host keeps.
use super::*;
use std::sync::Arc;
use tilo_relay::room_bots::attachments::{AttachmentLimits, Referenced, RoomAttachments};
use tilo_relay::room_bots::installed::FsBlobs;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_visitors_file_as_large_as_the_host_keeps_reaches_it() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    // A public host: every visitor has the member budgets.
    let budgets = tilo_relay::member_limits::MemberLimitsConfig::default();
    let limits = AttachmentLimits::default();
    assert!(
        budgets.content_bytes_per_hour < limits.file_bytes,
        "the visitor's content budget alone would not carry the file"
    );
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            member_limits: Some(budgets),
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
    let room = create_and_enter(&relay, &host, &laptop, "Lounge").await;
    super::sealed_records::join(&relay, &laptop, &room.room, &bob, &phone, [5; 32])
        .await
        .unwrap();

    let rooms = relay.rooms().unwrap().clone();
    let blobs = Arc::new(FsBlobs::new(state.path().join("blobs"), None).unwrap());
    rooms.serve_content(blobs.clone());
    let keeper = RoomAttachments::new(
        rooms.clone(),
        room.room.clone(),
        host.device_pubkey(),
        blobs,
        state.path().join("attachments.json"),
        limits,
    );
    rooms.keep_content(keeper.clone());

    // Bob posted a file exactly as large as the host keeps; the host asks
    // for it, and his page answers as a browser page does.
    let file: Vec<u8> = (0..=u8::MAX)
        .cycle()
        .take(usize::try_from(limits.file_bytes).unwrap())
        .collect();
    let hash = blake3::hash(&file).to_hex().to_string();
    let bob_id = hex::encode(bob.user_pk);
    let asks = keeper.asks(
        &[Referenced {
            hash: hash.clone(),
            member: bob_id.clone(),
            size: Some(limits.file_bytes),
        }],
        0,
    );
    assert_eq!(asks.len(), 1, "the host asks for the file");
    let bob_session = session(&relay, &phone, room.workspace_id).await;
    crate::common::wait_until_subscribed(&relay, room.workspace_id, &phone.device_pubkey()).await;
    let pieces = tilo_browser_peer::content::answer(
        &phone.device_pubkey(),
        &host.device_pubkey(),
        &asks[0],
        |_, _, _| Some(file.clone()),
    );
    assert!(pieces.len() > 1, "the file travels in pieces");
    for piece in pieces {
        bob_session
            .publish_record_content(host.device_pubkey(), piece)
            .unwrap();
    }
    crate::common::wait_until("the host keeps Bob's file", || {
        keeper.kept_for(&hash).is_some()
    })
    .await;
    assert_eq!(keeper.kept_for(&hash), Some(bob_id));
    relay.shutdown().await;
}
