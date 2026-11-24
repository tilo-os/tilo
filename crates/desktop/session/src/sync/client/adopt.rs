//! A workspace taken in: entered as a further device of the person at the device hosting it. Its
//! own workspace service holds a member copy of it, synced with that device: the copy every read
//! and write of the workspace goes through. An invitation is redeemed by the device's daemon
//! (`account/workspaces#join`), never here.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId as InviteWorkspaceId;
use tilo_network_actor::{DeviceIdentity, NetCmd, NetEvent};

use crate::engine::Engine;
use crate::sync::{Entering, REDEEM_WITHIN, SETTLE_WITHIN};
use crate::workspace_keys::KeyLink;

/// The peer network a workspace taken in is carried on: its actor, and where what it hears of the
/// workspace goes.
#[derive(Clone, Copy)]
pub(super) struct Carrier<'a> {
    pub(super) network: &'a tokio::sync::mpsc::Sender<NetCmd>,
    pub(super) events: &'a tokio::sync::mpsc::Sender<NetEvent>,
}

/// Take the workspace of `workspace_id` in as held here now: this device's channel service is told,
/// and, on a peer network, the workspace is subscribed to and this device's native ingress connects
/// to it. Without one (its endpoint never bound, or the session takes part in none) the workspace is
/// held here all the same, and connected to once a session's network comes up and joins it.
pub(super) async fn take_part_in(
    owner: &tilo_peer_state::owner::NativeWorkspaceOwner,
    workspace_id: InviteWorkspaceId,
    link: Option<&KeyLink>,
    network: Option<Carrier<'_>>,
) -> Result<(), String> {
    // The workspace is held here now: this device takes part in it, so what its people say and
    // record reaches it as it happens, and the devices it admits are told to the peer network.
    if let Some(link) = link {
        link.workspace_changed(workspace_id);
    }
    let Some(Carrier { network, events }) = network else {
        return Ok(());
    };
    // The workspace's subscription comes before the session it carries: what the host says the
    // moment this device joins is the workspace as it stands, and a workspace nobody listens to yet
    // drops it.
    network
        .send(NetCmd::JoinWorkspace {
            ws: workspace_id,
            on_event: events.clone(),
        })
        .await
        .map_err(|_| "the peer network is gone".to_owned())?;
    let connect = tilo_network_actor::native_ingress::Connect::new(owner.clone(), workspace_id)
        .map_err(|error| format!("the workspace was not taken in: {error}"))?;
    network
        .send(NetCmd::NativeConnect(connect))
        .await
        .map_err(|_| "the peer network is gone".to_owned())?;
    Ok(())
}

/// Take one of the person's workspaces in as their further device, at the device hosting it:
/// nothing is invited here — the workspace admits a device certified by a member's root as that
/// member's device — and what the host answers carries the workspace's governance, its roster and
/// the records it holds, so this device takes part in the workspace as it stands.
pub(super) async fn enter(
    endpoint: iroh::Endpoint,
    identity: DeviceIdentity,
    engine: Arc<Engine>,
    network: &tokio::sync::mpsc::Sender<NetCmd>,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
    request: Entering,
    link: Option<&KeyLink>,
) -> Result<(), tilo_session_client::EnrollFailure> {
    let topic = request.workspace_id;
    identity.current_material(topic)?;
    let owner = engine.owner().clone();
    let client = tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint);
    tokio::time::timeout(
        REDEEM_WITHIN,
        tilo_peer_state::owner::enroll::admit_as_member(
            client,
            identity,
            owner.clone(),
            request.workspace_id,
            request.host,
            tilo_clock::wall(),
        ),
    )
    .await
    .map_err(|_| "the workspace's host did not answer".to_owned())??;
    tokio::time::timeout(
        SETTLE_WITHIN,
        take_part_in(
            &owner,
            request.workspace_id,
            link,
            Some(Carrier { network, events }),
        ),
    )
    .await
    .map_err(|_| "the workspace was not taken in time".to_owned())??;
    Ok(())
}
