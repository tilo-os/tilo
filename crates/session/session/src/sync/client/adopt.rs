//! A workspace taken in: redeemed from an invitation the desk made for it, or entered as a further
//! device of the person at the device hosting it.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId as DeskWorkspaceId;
use tilo_network_actor::{HostIdentity, NetCmd, NetEvent, workspace_topic_id};

use super::{ChannelLink, Engine, Enrolling, Entering};
use crate::sync::{REDEEM_WITHIN, SETTLE_WITHIN};

/// Redeem an invitation into the workspace the desk made for its workspace: this device becomes a
/// member of the workspace, takes in what it held before it arrived, and takes part in it from then
/// on.
/// The workspace's state arrives as the host's answer to the enrollment: a person who joins late
/// reads the whole history, not only what is recorded from now on.
pub(super) async fn redeem(
    endpoint: iroh::Endpoint,
    identity: HostIdentity,
    engine: Arc<Engine>,
    plane: &tokio::sync::mpsc::Sender<NetCmd>,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
    request: Enrolling,
    link: Option<&ChannelLink>,
) -> Result<(), String> {
    let topic = workspace_topic_id(request.workspace_id);
    // Renewal precedes presentation: a device that slept past the end of its certificate joins with
    // the renewal, reissued here or signed by the person's devices, and a join that cannot renew
    // says why rather than presenting what the host refuses.
    let renewal = request.renewal.clone();
    let before =
        tokio::task::spawn_blocking(move || crate::renewal::before_presenting(renewal.as_deref()))
            .await;
    match before {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(error),
        Err(error) => {
            return Err(format!(
                "the renewal before this join did not finish: {error}"
            ));
        }
    }
    // The workspace is joined as this computer's person: the device's own certificate signs the
    // enrollment, so there is nothing to pick here.
    identity.current_material(topic)?;
    let owner = engine.owner().clone();
    let client = tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint);
    let enrollment = tokio::time::timeout(
        REDEEM_WITHIN,
        tilo_peer_state::owner::enroll::prepare_and_complete(
            client,
            identity.clone(),
            topic,
            Some(request.slot),
            request.invitation,
            owner.clone(),
        ),
    )
    .await
    .map_err(|_| "the workspace's host did not answer".to_owned())??;
    // What the host granted is taken in here, and the plane told to carry it: a person waiting to
    // be let into a workspace is told when it did not happen rather than left on a plane that
    // stopped taking work.
    tokio::time::timeout(SETTLE_WITHIN, async {
        owner.install(&enrollment, std::time::SystemTime::now())?;
        take_part_in(&owner, request.workspace_id, link, plane, events).await
    })
    .await
    .map_err(|_| "the workspace was not taken in time".to_owned())??;
    Ok(())
}

/// Take the workspace of `workspace_id` in as held here now: this device's channel service is told,
/// the workspace is subscribed to, and this device's native ingress connects to it.
pub(super) async fn take_part_in(
    owner: &tilo_peer_state::owner::NativeWorkspaceOwner,
    workspace_id: DeskWorkspaceId,
    link: Option<&ChannelLink>,
    plane: &tokio::sync::mpsc::Sender<NetCmd>,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
) -> Result<(), String> {
    // The workspace is held here now: this device takes part in it, so what its people say and
    // record reaches it as it happens, and the devices it admits are told to the plane.
    if let Some(link) = link {
        link.workspace_changed(workspace_id);
    }
    // The workspace's subscription comes before the session it carries: what the host says the
    // moment this device joins is the workspace as it stands, and a workspace nobody listens to yet
    // drops it.
    plane
        .send(NetCmd::JoinWorkspace {
            ws: workspace_topic_id(workspace_id),
            on_event: events.clone(),
        })
        .await
        .map_err(|_| "the collaboration plane is gone".to_owned())?;
    let connect = tilo_network_actor::native_ingress::Connect::new(owner.clone(), workspace_id)
        .map_err(|error| format!("the workspace was not taken in: {error}"))?;
    plane
        .send(NetCmd::NativeConnect(connect))
        .await
        .map_err(|_| "the collaboration plane is gone".to_owned())?;
    Ok(())
}

/// Take one of the person's workspaces in as their further device, at the device hosting it:
/// nothing is invited here — the workspace admits a device certified by a member's root as that
/// member's device — and what the host answers carries the workspace's governance, its roster and
/// the records it holds, so this device takes part in the workspace as it stands.
pub(super) async fn enter(
    endpoint: iroh::Endpoint,
    identity: HostIdentity,
    engine: Arc<Engine>,
    plane: &tokio::sync::mpsc::Sender<NetCmd>,
    events: &tokio::sync::mpsc::Sender<NetEvent>,
    request: Entering,
    link: Option<&ChannelLink>,
) -> Result<(), String> {
    let topic = workspace_topic_id(request.workspace_id);
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
            topic,
            request.host,
            std::time::SystemTime::now(),
        ),
    )
    .await
    .map_err(|_| "the workspace's host did not answer".to_owned())??;
    tokio::time::timeout(
        SETTLE_WITHIN,
        take_part_in(&owner, request.workspace_id, link, plane, events),
    )
    .await
    .map_err(|_| "the workspace was not taken in time".to_owned())?
}
