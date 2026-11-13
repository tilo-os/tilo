//! Every workspace has voice. For each kind of workspace a person is in on
//! a host, two of its devices open a voice channel's call and are granted
//! their parts in it, each seeing the other:
//!
//! - a workspace the host governs (one of a workspace service's, like the Lounge);
//! - a workspace the host governs that a guest was invited into;
//! - a workspace of the host's owner, which the host only carries for them
//!   (it syncs across their devices through it, and no workspace governs it
//!   here).
//!
//! A new kind of workspace belongs in [`KINDS`]; one without calls fails.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_relay::RelayClient;
use tilo_relay::coedit::RelayCoeditSession;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::CallOperation;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::call::{CHAT, create_voice_channel, endpoint, enter, granted, invite, native, operate};
use crate::support::{Device, TestHost, enrol, start_workspace_host, wait_for};
use collab_types::WorkspaceId;

/// The kinds of workspace a person can be in on a host.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// A workspace the host governs, entered by its owner's devices.
    Governed,
    /// A workspace the host governs, which a guest was invited into.
    Invited,
    /// A workspace of the host's owner, which the host carries but no workspace
    /// governs here.
    Carried,
}

const KINDS: [Kind; 3] = [Kind::Governed, Kind::Invited, Kind::Carried];

/// Connects `device` to the host and asks to subscribe to `workspace_id`:
/// the host's answer, `Ready` or `NotAuthorised`.
async fn subscribe(
    address: iroh::EndpointAddr,
    workspace_id: WorkspaceId,
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
async fn session(address: iroh::EndpointAddr, workspace_id: WorkspaceId, device: &Device) -> RelayClient {
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
    let TestHost { relay, web, .. } =
        start_workspace_host(&alice, &host, state.path(), false).await;
    let address = relay.node_addr();

    for (index, kind) in KINDS.into_iter().enumerate() {
        let request = u8::try_from(index).unwrap();
        // Two devices in the workspace, and the voice channel they open.
        let (workspace_id, first, second, channel) = match kind {
            Kind::Governed | Kind::Invited => {
                let workspace = relay
                    .workspaces()
                    .unwrap()
                    .create(&format!("{kind:?}"))
                    .await
                    .unwrap();
                enter(&relay, &laptop.cert, &workspace.workspace).await;
                let (owner, channel) = create_voice_channel(
                    address.clone(),
                    workspace.workspace_id,
                    &workspace.workspace,
                    &host.cert,
                    &laptop,
                )
                .await;
                let owner = owner.into_client();
                let other = if matches!(kind, Kind::Invited) {
                    let bob = SigningKey::generate(&mut UnwrapErr(SysRng));
                    let bob_laptop = enrol(&bob, "bob-laptop", DeviceCapabilities::member());
                    invite(
                        &relay,
                        &laptop.cert,
                        &workspace.workspace,
                        &bob,
                        &bob_laptop,
                    )
                    .await;
                    session(address.clone(), workspace.workspace_id, &bob_laptop).await
                } else {
                    enter(&relay, &phone.cert, &workspace.workspace).await;
                    session(address.clone(), workspace.workspace_id, &phone).await
                };
                (workspace.workspace_id, owner, other, channel)
            }
            Kind::Carried => {
                // Her devices sync her workspace through her host: it takes
                // it on for them, and governs no workspace for it.
                let workspace_id = 0x0077_6e00_0000_0001;
                let first = session(address.clone(), workspace_id, &laptop).await;
                let second = session(address.clone(), workspace_id, &phone).await;
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
            heard.participants.len(),
            2,
            "{kind:?}: each device sees the other in the call"
        );
    }

    drop(web);
    relay.shutdown().await;
}
