//! Joining a voice channel is joining its call: nobody has to start one. A
//! guest the owner invited opens the channel's call, the owner opening it
//! again gets the same call, and a dozen devices take part in it, beyond
//! the eight a call once held.

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
use tilo_relay_protocol::voice::CallOperation;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_opens_a_voice_channels_call_and_twelve_devices_take_part() {
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
    let open = || CallOperation::Open {
        app_id: CHAT.into(),
        record_id: String::new(),
        channel_id: channel.clone(),
    };

    // Bob, a guest of Alice's room, joins its voice channel: the call
    // starts for him, though only the owner may end or moderate it.
    let bob = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bob_laptop = enrol(&bob, "bob-laptop", DeviceCapabilities::member());
    invite(&relay, &laptop.cert, &room.room, &bob, &bob_laptop).await;
    let bob_link = session(address.clone(), room.workspace_id, &bob_laptop).await;
    let opened = wait_for("the governor to learn the voice channel", || {
        let bob_link = &bob_link;
        let open = open();
        async move { operate(bob_link, room.workspace_id, 1, open).await.ok() }
    })
    .await;
    assert!(opened.active, "joining a voice channel starts its call");
    let refused = operate(
        &bob_link,
        room.workspace_id,
        2,
        CallOperation::End {
            call: opened.call,
            epoch: opened.epoch,
        },
    )
    .await;
    assert!(refused.is_err(), "a guest cannot end the call");

    // Alice opening the channel's call finds the same call, unchanged.
    let again = operate(&owner, room.workspace_id, 3, open()).await.unwrap();
    assert_eq!((again.call, again.epoch), (opened.call, opened.epoch));

    // Twelve devices in one call: no participant limit.
    let mut links = vec![owner, bob_link];
    for index in 0..10 {
        let device = enrol(
            &alice,
            &format!("alice-device-{index}"),
            DeviceCapabilities::primary(),
        );
        enter(&relay, &device.cert, &room.room).await;
        links.push(session(address.clone(), room.workspace_id, &device).await);
    }
    let mut joined = Vec::new();
    for link in &links {
        link.send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: opened.call,
            participation: None,
        })
        .unwrap();
        joined.push(native(link, granted).await.participation);
    }
    let full = native(&links[0], |envelope| {
        granted(envelope).filter(|grant| grant.participants.len() == 12)
    })
    .await;
    assert_eq!(
        full.subscriptions.len(),
        11,
        "everyone hears the eleven others"
    );

    // A device joining afresh replaces its own participation: still twelve.
    links[1]
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: opened.call,
            participation: None,
        })
        .unwrap();
    // Grants announcing the later joins are still queued on this link; the
    // answer to its fresh join names a new participation.
    let rejoined = native(&links[1], |envelope| {
        granted(envelope).filter(|grant| grant.participation != joined[1])
    })
    .await;
    assert_eq!(rejoined.participants.len(), 12);

    drop(web);
    relay.shutdown().await;
}
