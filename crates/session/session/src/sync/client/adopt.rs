//! A workspace taken in: redeemed from an invitation the shell made for it, or entered as a further
//! device of the person at the device hosting it. However this device came to take part in a
//! workspace another device serves, its own workspace service holds a member copy of it, synced
//! with that device: the copy every read and write of the workspace goes through.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId as InviteWorkspaceId;
use tilo_network_actor::{DeviceIdentity, NetCmd, NetEvent};

use crate::engine::Engine;
use crate::sync::{Enrolling, Entering, REDEEM_WITHIN, SETTLE_WITHIN};
use crate::workspace_keys::KeyLink;
use tilo_network_actor::own_service::OwnService;

/// Where `invitation` is redeemed: with this device's own workspace service in process when the
/// invitation names it (an endpoint cannot dial itself), over the network otherwise.
fn join_client(
    endpoint: iroh::Endpoint,
    own_service: Option<&OwnService>,
    identity: &DeviceIdentity,
    invitation: &str,
) -> Result<(tilo_peer_state::join_transport::WorkspaceJoinClient, bool), String> {
    let own = identity.device_cert.device_public_key;
    let hosted_here = tilo_peer_state::join::decode_and_verify_invitation_text(
        invitation,
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .is_ok_and(|verified| {
        verified
            .grant()
            .transport
            .iter()
            .any(|hint| tilo_peer_state::owner::session::hosted_by(hint, &own))
    });
    if !hosted_here {
        return Ok((
            tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint),
            false,
        ));
    }
    let client = own_service
        .and_then(|service| service.join_client(own))
        .ok_or_else(|| "this device's workspace service is not open".to_owned())?;
    Ok((client, true))
}

/// Come into the workspace the shell made for the join, with the invitation it holds: this device
/// becomes a member of the workspace, takes in what it held before it arrived, and takes part in it
/// from then on.
/// The workspace's state arrives as the host's answer: a person who joins late reads the whole
/// history, not only what is recorded from now on.
/// Renew this device's certificate before it is presented: a device that slept past the end of its
/// certificate joins with the renewal reissued here, and a join that cannot renew says why rather
/// than presenting what the host refuses.
async fn renew_before_joining() -> Result<(), tilo_session_client::EnrollFailure> {
    let before = tokio::task::spawn_blocking(crate::renewal::before_presenting).await;
    match before {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error.into()),
        Err(error) => Err(format!("the renewal before this join did not finish: {error}").into()),
    }
}

pub(super) async fn redeem(
    endpoint: iroh::Endpoint,
    identity: DeviceIdentity,
    engine: Arc<Engine>,
    network: Option<Carrier<'_>>,
    request: Enrolling,
    link: Option<&KeyLink>,
    own_service: Option<&OwnService>,
) -> Result<(), tilo_session_client::EnrollFailure> {
    let topic = request.workspace_id;
    renew_before_joining().await?;
    // The workspace is joined as this computer's person: the device's own certificate signs the
    // enrollment, so there is nothing to pick here.
    identity.current_material(topic)?;
    let owner = engine.owner().clone();
    let (client, hosted_here, invitation) = match request.way {
        tilo_workspace_directory::Way::Invitation(invitation) => {
            let (client, hosted_here) = join_client(endpoint, own_service, &identity, &invitation)?;
            (client, hosted_here, invitation)
        }
        tilo_workspace_directory::Way::Native(locator) => {
            use tilo_peer_state::join_transport::WorkspaceJoinClient;
            let hosted_here = endpoint.id().as_bytes() == &locator.service;
            let client = if hosted_here {
                own_service
                    .and_then(|service| service.join_client(identity.device_cert.device_public_key))
                    .ok_or("this device's workspace service is not open")?
            } else {
                WorkspaceJoinClient::new(endpoint)
            };
            let invitation = locator
                .to_link()
                .ok_or("the native invitation link is malformed")?;
            (client, hosted_here, invitation)
        }
    };
    let enrollment = tokio::time::timeout(
        REDEEM_WITHIN,
        tilo_peer_state::owner::enroll::prepare_and_complete(
            client,
            identity.clone(),
            topic,
            Some(request.workspace_id),
            invitation,
            owner.clone(),
        ),
    )
    .await
    .map_err(|_| "the workspace's host did not answer".to_owned())??;
    // What the host granted is taken in here, and the peer network told to carry it: a person waiting to
    // be let into a workspace is told when it did not happen rather than left on a peer network that
    // stopped taking work.
    tokio::time::timeout(SETTLE_WITHIN, async {
        owner
            .install(&enrollment, tilo_clock::wall())
            .map_err(tilo_session_client::EnrollFailure::from)?;
        take_part_in(&owner, request.workspace_id, link, network)
            .await
            .map_err(tilo_session_client::EnrollFailure::from)
    })
    .await
    .map_err(|_| "the workspace was not taken in time".to_owned())??;
    if !hosted_here {
        hold_on_service(
            own_service,
            enrollment.bootstrap(),
            enrollment.transport_hint(),
        )
        .await;
    }
    Ok(())
}

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
    tilo_workspace_service::own_device::hold_as_members(service.workspaces(), held).await;
}
