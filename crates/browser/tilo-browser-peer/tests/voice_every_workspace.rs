//! Every workspace has voice. For each kind of workspace a person is in on
//! a host, two of its devices open a voice channel's call and are granted
//! the carrier, hearing each other:
//!
//! - a room the host governs (a room service's room, such as the Lounge);
//! - a room the host governs that a guest was invited into;
//! - a workspace of the host's owner, which the host only carries for them
//!   (it syncs across their devices through it, and no room governs it
//!   here);
//! - the person's own room, which a host carries for every person it
//!   trusts: a member of the host's Lounge has their own desktop there.
//!
//! A new kind of workspace belongs in [`KINDS`]; one without calls fails.

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

/// The kinds of workspace a person can be in on a host.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// A room the host governs, entered by its owner's devices.
    Governed,
    /// A room the host governs, which a guest was invited into.
    Invited,
    /// A workspace of the host's owner, which the host carries but no room
    /// governs here.
    Carried,
    /// The person's own room, carried for a person the host trusts because
    /// a room it governs let them in.
    Own,
}

const KINDS: [Kind; 4] = [Kind::Governed, Kind::Invited, Kind::Carried, Kind::Own];

/// Connects `device` to the host and asks to subscribe to `workspace_id`:
/// the host's answer, `Ready` or `NotAuthorised`.
async fn subscribe(
    address: iroh::EndpointAddr,
    workspace_id: u64,
    device: &Device,
) -> (RelayClient, Result<(), String>) {
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
    let answer = native(&client, |envelope| match envelope {
        Envelope::Ready { .. } => Some(Ok(())),
        Envelope::NotAuthorised { reason, .. } => Some(Err(reason)),
        _ => None,
    })
    .await;
    (client, answer)
}

/// `device`'s own session with `workspace_id`, subscribed.
async fn session(address: iroh::EndpointAddr, workspace_id: u64, device: &Device) -> RelayClient {
    let (client, answer) = subscribe(address, workspace_id, device).await;
    if let Err(reason) = answer {
        panic!("device not admitted: {reason}");
    }
    client
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_joined_in_every_kind_of_workspace() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let phone = enrol(&alice, "alice-phone", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let TestHost { relay, web } = start_room_host(&alice, &host, state.path(), false).await;
    let address = relay.node_addr();

    for (index, kind) in KINDS.into_iter().enumerate() {
        let request = u8::try_from(index).unwrap();
        // Two devices in the workspace, and the voice channel they open.
        let (workspace_id, first, second, channel) = match kind {
            Kind::Governed | Kind::Invited => {
                let room = relay
                    .rooms()
                    .unwrap()
                    .create(&format!("{kind:?}"))
                    .await
                    .unwrap();
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
                let other = if matches!(kind, Kind::Invited) {
                    let bob = SigningKey::generate(&mut UnwrapErr(SysRng));
                    let bob_laptop = enrol(&bob, "bob-laptop", DeviceCapabilities::member());
                    invite(&relay, &laptop.cert, &room.room, &bob, &bob_laptop).await;
                    session(address.clone(), room.workspace_id, &bob_laptop).await
                } else {
                    enter(&relay, &phone.cert, &room.room).await;
                    session(address.clone(), room.workspace_id, &phone).await
                };
                (room.workspace_id, owner, other, channel)
            }
            Kind::Carried => {
                // Her devices sync her workspace through her host: it takes
                // it on for them, and governs no room for it.
                let workspace_id = 0x0077_6e00_0000_0001;
                let first = session(address.clone(), workspace_id, &laptop).await;
                let second = session(address.clone(), workspace_id, &phone).await;
                (workspace_id, first, second, "voice".to_owned())
            }
            Kind::Own => {
                // Carol is a member of the host's Lounge, nothing more. Her
                // own room is carried for her laptop and phone alike, and
                // for no one else the host trusts.
                let lounge = relay.rooms().unwrap().create("Lounge").await.unwrap();
                enter(&relay, &laptop.cert, &lounge.room).await;
                let carol = SigningKey::generate(&mut UnwrapErr(SysRng));
                let carol_laptop = enrol(&carol, "carol-laptop", DeviceCapabilities::member());
                let carol_phone = enrol(&carol, "carol-phone", DeviceCapabilities::member());
                invite(&relay, &laptop.cert, &lounge.room, &carol, &carol_laptop).await;
                let dave = SigningKey::generate(&mut UnwrapErr(SysRng));
                let dave_laptop = enrol(&dave, "dave-laptop", DeviceCapabilities::member());
                invite(&relay, &laptop.cert, &lounge.room, &dave, &dave_laptop).await;
                let own = tilo_relay::own_room(&carol.verifying_key().to_bytes());
                let workspace_id = tilo_relay::room_workspace_id(&own).unwrap();
                let first = session(address.clone(), workspace_id, &carol_laptop).await;
                let second = session(address.clone(), workspace_id, &carol_phone).await;
                let (_dave, answer) = subscribe(address.clone(), workspace_id, &dave_laptop).await;
                assert!(
                    answer.is_err(),
                    "another person the host trusts is not admitted to carol's own room"
                );
                (workspace_id, first, second, "voice".to_owned())
            }
        };
        let open = || CallOperation::Open {
            app_id: CHAT.into(),
            record_id: String::new(),
            channel_id: channel.clone(),
        };
        let opened = wait_for("the host to open the voice channel's call", || {
            let second = &second;
            let open = open();
            async move { operate(second, workspace_id, request, open).await.ok() }
        })
        .await;
        assert!(
            opened.active,
            "{kind:?}: joining a voice channel opens its call"
        );
        let again = operate(&first, workspace_id, request, open()).await;
        assert_eq!(
            again.map(|state| (state.call, state.epoch)),
            Ok((opened.call, opened.epoch)),
            "{kind:?}: both devices open the same call"
        );
        for link in [&first, &second] {
            link.send(Envelope::VoiceJoin {
                workspace_id,
                call: opened.call,
                participation: None,
            })
            .unwrap();
        }
        let heard = native(&first, |envelope| match envelope {
            Envelope::VoiceRefused { reason, .. } => panic!("{kind:?}: join refused: {reason}"),
            envelope => granted(envelope).filter(|grant| grant.participants.len() == 2),
        })
        .await;
        assert!(
            heard.directions.publish && heard.directions.subscribe,
            "{kind:?}: a member speaks and hears"
        );
        assert_eq!(
            heard.subscriptions.len(),
            1,
            "{kind:?}: each device hears the other"
        );
        assert!(
            !heard.endpoint.is_empty(),
            "{kind:?}: the grant names the host's voice carrier"
        );
    }

    drop(web);
    relay.shutdown().await;
}
