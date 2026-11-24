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
use tilo_network_actor::own_service::OwnService;

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
    own_service: Option<&OwnService>,
) -> Result<(), tilo_session_client::EnrollFailure> {
    let topic = request.workspace_id;
    identity.current_material(topic)?;
    let owner = engine.owner().clone();
    let client = tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint);
    let (bootstrap, at) = tokio::time::timeout(
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
    hold_on_service(own_service, &bootstrap, &at).await;
    Ok(())
}

/// Hold `workspace` on this device's own service too, as the member copy the session's own reads
/// and writes go through: the serving device admitted `bootstrap`, and this device's service acts
/// as this device, so the same membership holds the workspace's documents there. A service that is
/// not open, or that serves the workspace itself, keeps no copy; the workspace stays one this
/// device's engine holds alone, as before.
async fn hold_on_service(
    own_service: Option<&OwnService>,
    bootstrap: &tilo_peer_state::join::JoinBootstrap,
    hint: &tilo_peer_state::join::TransportHint,
) {
    let Some(service) = own_service.and_then(OwnService::get) else {
        tracing::warn!(
            "this device's workspace service is not open, so the workspace is not a member copy here"
        );
        return;
    };
    match tokio::time::timeout(
        SETTLE_WITHIN,
        service.workspaces().join_admitted(bootstrap, hint),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "the workspace was taken in here, but not on this device's service");
        }
        Err(_) => {
            tracing::warn!(
                "the workspace's member copy did not arrive on this device's service in time"
            );
        }
    }
}

/// Hold every workspace `engine` takes part in on this device's own service, `own_service`, as a
/// member copy synced with the device hosting it: those this device joined before its service held
/// the workspaces it joins, or while the service was not open, and those a copy of another device's
/// state brought. A copy the service kept from before is synced already as it opens.
pub(super) async fn hold_every_workspace(engine: &Engine, own_service: &OwnService) {
    let Some(service) = own_service.get() else {
        return;
    };
    let held = engine
        .workspaces()
        .into_iter()
        .filter_map(|workspace| Some((workspace, engine.member_of(workspace)?)));
    service.workspaces().hold_as_members(held).await;
}
