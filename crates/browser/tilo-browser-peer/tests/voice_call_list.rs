//! A voice channel shows who is in its call the moment that changes: a
//! device that asked the room for its calls is told the list again when
//! someone joins or leaves, without asking again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/call.rs"]
mod call;
mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_relay::RelayClient;
use tilo_relay::coedit::RelayCoeditSession;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, CallSummary};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use call::{CHAT, create_voice_channel, endpoint, enter, granted, invite, native, operate};
use support::{Device, TestHost, enrol, start_room_host, wait_for};

/// `device`'s own session with the room, subscribed.
async fn session(address: iroh::EndpointAddr, workspace_id: u64, device: &Device) -> RelayClient {
    let client = RelayCoeditSession::join(
        RelayClient::connect_to_pinned_host(
            endpoint(&device.key).await,
            address,
            device.cert.clone(),
            device.key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        workspace_id,
        device.cert.device_public_key,
    )
    .unwrap()
    .into_client();
    native(&client, |envelope| match envelope {
        Envelope::Ready { .. } => Some(()),
        Envelope::NotAuthorised { reason, .. } => panic!("device not admitted: {reason}"),
        _ => None,
    })
    .await;
    client
}

/// The next call list `client` is told.
fn listed(envelope: Envelope) -> Option<Vec<CallSummary>> {
    match envelope {
        Envelope::VoiceCallList { result, .. } => Some(result.expect("the room lists its calls")),
        _ => None,
    }
}

/// How many are in the call on `channel` in `list`.
fn in_call(list: &[CallSummary], channel: &str) -> usize {
    list.iter()
        .filter(|call| call.channel_id == channel && call.state.active)
        .map(|call| call.participants.len())
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whoever_watches_a_rooms_calls_is_told_each_join_and_leave_unasked() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let TestHost { relay, web } = start_room_host(&alice, &host, state.path(), false).await;
    let address = relay.node_addr();
    let room = relay.rooms().unwrap().create("Lounge").await.unwrap();
    enter(&relay, &laptop.cert, &room.room).await;
    let (owner, channel) = create_voice_channel(
        address.clone(),
        room.workspace_id,
        &room.room,
        &host.cert,
        &laptop,
    )
    .await;
    let owner = owner.into_client();

    // Bob looks at the room's voice channels once.
    let bob = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bob_laptop = enrol(&bob, "bob-laptop", DeviceCapabilities::member());
    invite(&relay, &laptop.cert, &room.room, &bob, &bob_laptop).await;
    let bob_link = session(address.clone(), room.workspace_id, &bob_laptop).await;
    bob_link
        .send(Envelope::VoiceCalls {
            workspace_id: room.workspace_id,
        })
        .unwrap();
    let first = native(&bob_link, listed).await;
    assert_eq!(in_call(&first, &channel), 0, "nobody is in the call yet");

    // Alice joins the voice channel: Bob is told, without asking again.
    let opened = wait_for("the governor to learn the voice channel", || {
        let owner = &owner;
        let open = CallOperation::Open {
            app_id: CHAT.into(),
            record_id: String::new(),
            channel_id: channel.clone(),
        };
        async move { operate(owner, room.workspace_id, 1, open).await.ok() }
    })
    .await;
    owner
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: opened.call,
            participation: None,
        })
        .unwrap();
    let grant = native(&owner, granted).await;
    let joined = native(&bob_link, |envelope| {
        listed(envelope).filter(|list| in_call(list, &channel) == 1)
    })
    .await;
    let alice_in = joined
        .iter()
        .flat_map(|call| &call.participants)
        .next()
        .expect("Alice is listed");
    assert_eq!(alice_in.user_public_key, laptop.cert.user_public_key);

    // Alice leaves: Bob is told the call is empty.
    owner
        .send(Envelope::VoiceLeave {
            workspace_id: room.workspace_id,
            call: opened.call,
            participation: grant.participation,
        })
        .unwrap();
    native(&bob_link, |envelope| {
        listed(envelope).filter(|list| in_call(list, &channel) == 0)
    })
    .await;

    drop(web);
    relay.shutdown().await;
}
