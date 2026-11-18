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
use tilo_peer_state::CopyRole;
use tilo_server::coedit::ServerCoeditSession;
use tilo_server::{Server, ServerClient};
use tilo_server_protocol::Envelope;
use tilo_server_protocol::voice::CallOperation;
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
) -> (ServerClient, Result<(), String>) {
    let client = ServerCoeditSession::join(
        ServerClient::connect_to_pinned_host(
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
        Envelope::NotAuthorised { refusal, .. } => Some(Err(refusal.to_string())),
        _ => None,
    })
    .await;
    (client, answer)
}

/// `device`'s own session with `workspace_id`, subscribed.
async fn session(
    address: iroh::EndpointAddr,
    workspace_id: WorkspaceId,
    device: &Device,
) -> ServerClient {
    let (client, answer) = subscribe(address, workspace_id, device).await;
    if let Err(reason) = answer {
        panic!("device not admitted: {reason}");
    }
    client
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_joined_in_every_kind_of_workspace() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let home = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let phone = enrol(&alice, "alice-phone", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let TestHost { server, web, .. } =
        start_workspace_host(&alice, &home, state.path(), false).await;
    let address = server.node_addr();
    // The servers that make the workspaces this test's server only carries,
    // with the state they keep them in.
    let mut governors: Vec<(tempfile::TempDir, Server)> = Vec::new();

    for (index, kind) in KINDS.into_iter().enumerate() {
        let request = u8::try_from(index).unwrap();
        // Two devices in the workspace, and the voice channel they open.
        let (workspace_id, first, second, channel) = match kind {
            Kind::Governed | Kind::Invited => {
                let workspace = server
                    .workspaces()
                    .unwrap()
                    .create(&format!("{kind:?}"))
                    .await
                    .unwrap();
                enter(&server, &laptop.cert, &workspace.workspace).await;
                let (owner, channel) = create_voice_channel(
                    address.clone(),
                    workspace.workspace_id,
                    &workspace.workspace,
                    &home.cert,
                    &laptop,
                )
                .await;
                let owner = owner.into_client();
                let other = if matches!(kind, Kind::Invited) {
                    let bob = SigningKey::generate(&mut UnwrapErr(SysRng));
                    let bob_laptop = enrol(&bob, "bob-laptop", DeviceCapabilities::member());
                    invite(
                        &server,
                        &laptop.cert,
                        &workspace.workspace,
                        &bob,
                        &bob_laptop,
                    )
                    .await;
                    session(address.clone(), workspace.workspace_id, &bob_laptop).await
                } else {
                    enter(&server, &phone.cert, &workspace.workspace).await;
                    session(address.clone(), workspace.workspace_id, &phone).await
                };
                (workspace.workspace_id, owner, other, channel)
            }
            Kind::Carried => {
                // Her workspace is another server's of hers: that server
                // hands its copy to the one her devices reach, which keeps it
                // for them and governs no workspace of its own for.
                let own_state = tempfile::tempdir().unwrap();
                let office = enrol(&alice, "alice-office", DeviceCapabilities::primary());
                let own_server = start_workspace_host(&alice, &office, own_state.path(), false)
                    .await
                    .server;
                let workspaces = own_server.workspaces().unwrap();
                let workspace = workspaces.create("Carried").await.unwrap();
                enter(&own_server, &laptop.cert, &workspace.workspace).await;
                enter(&own_server, &phone.cert, &workspace.workspace).await;
                workspaces
                    .attach_server(
                        &workspace.workspace,
                        home.cert.device_public_key,
                        address.clone(),
                        CopyRole::KeyHolder,
                    )
                    .await
                    .unwrap();
                let first = session(address.clone(), workspace.workspace_id, &laptop).await;
                let second = session(address.clone(), workspace.workspace_id, &phone).await;
                governors.push((own_state, own_server));
                (workspace.workspace_id, first, second, "voice".to_owned())
            }
        };
        let open = || CallOperation::Open {
            app_id: CHAT.into(),
            document_id: String::new(),
            channel_id: channel.clone(),
        };
        let opened = wait_for(
            &format!("{kind:?}: the server to open the voice channel's call"),
            || {
                let second = &second;
                let open = open();
                async move { operate(second, workspace_id, request, open).await.ok() }
            },
        )
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
            envelope => granted(envelope).filter(|grant| grant.members.len() == 2),
        })
        .await;
        assert!(
            heard.directions.publish && heard.directions.subscribe,
            "{kind:?}: a member speaks and hears"
        );
        assert_eq!(
            heard.members.len(),
            2,
            "{kind:?}: each device sees the other in the call"
        );
    }

    drop(web);
    for (_, governor) in governors {
        governor.shutdown().await;
    }
    server.shutdown().await;
}
