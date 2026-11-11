//! A workspace taken in: entered through its door or redeemed from an invitation the desk made for
//! it, or entered as a further device of the person at the device hosting it.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId as DeskWorkspaceId;
use tilo_network_actor::{HostIdentity, NetCmd, NetEvent, workspace_topic_id};

use crate::channels::ChannelLink;
use crate::engine::Engine;
use crate::sync::{Enrolling, Entering, REDEEM_WITHIN, SETTLE_WITHIN};

/// Come into the workspace the desk made a slot for, through its door or with an invitation: this
/// device becomes a member of the workspace, takes in what it held before it arrived, and takes part
/// in it from then on.
/// The workspace's state arrives as the host's answer: a person who joins late reads the whole
/// history, not only what is recorded from now on.
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
    let invitation = match request.way {
        tilo_workspace_directory::Way::Invitation(invitation) => invitation,
        tilo_workspace_directory::Way::Door(door) => {
            let hint = tokio::time::timeout(
                REDEEM_WITHIN,
                through_door(
                    endpoint.clone(),
                    &identity,
                    &door,
                    request.workspace_id,
                    &request.name,
                ),
            )
            .await
            .map_err(|_| "the workspace's host did not answer".to_owned())??;
            let client = tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint);
            tokio::time::timeout(
                REDEEM_WITHIN,
                tilo_peer_state::owner::enroll::admit_as_member(
                    client,
                    identity,
                    owner.clone(),
                    request.workspace_id,
                    topic,
                    hint,
                    std::time::SystemTime::now(),
                ),
            )
            .await
            .map_err(|_| "the workspace's host did not answer".to_owned())??;
            return tokio::time::timeout(
                SETTLE_WITHIN,
                take_part_in(&owner, request.workspace_id, link, plane, events),
            )
            .await
            .map_err(|_| "the workspace was not taken in time".to_owned())?;
        }
    };
    let client = tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint);
    let enrollment = tokio::time::timeout(
        REDEEM_WITHIN,
        tilo_peer_state::owner::enroll::prepare_and_complete(
            client,
            identity.clone(),
            topic,
            Some(request.slot),
            invitation,
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

/// How long the door's host may take to answer the entry, beyond the bound it holds an entry to
/// itself: the connection takes some.
const ENTRY_ANSWER_SLACK: std::time::Duration = std::time::Duration::from_secs(15);

/// Enter `workspace` through `door`, as this computer's person named `name`: the door's host (at
/// the door's own origin) says who it is, and this device's session with it asks to enter in its
/// first message, as every device enters a workspace through its door
/// ([`tilo_relay_protocol::entering`]). The door decides there by the workspace's own rules, and the
/// workspace admits this device's own certificate. Answers the host's signed address for the
/// workspace, where this device then takes its state as a member's device does; the session itself
/// ends with the answer, since this device takes part over its own plane.
async fn through_door(
    endpoint: iroh::Endpoint,
    identity: &HostIdentity,
    door: &url::Url,
    workspace: DeskWorkspaceId,
    name: &str,
) -> Result<tilo_peer_state::join::TransportHint, String> {
    use tilo_relay_protocol::entering::{Entered, Entering, Through};
    let topic = workspace_topic_id(workspace);
    let client = tilo_workspace_directory::client()?;
    let origin = door.origin().ascii_serialization();
    let host = tilo_relay_client::https::Origin::new(&client, &origin);
    let info = tilo_relay_client::https::host_info(&host)
        .await
        .map_err(|error| format!("the workspace's host could not be reached: {error}"))?;
    let address = info
        .address
        .ok_or("the workspace's host takes no device sessions")?;
    let (cert, _) = identity.current_material(topic)?;
    let key = identity.signing_key()?;
    let mut request_id = [0; 32];
    getrandom::fill(&mut request_id).map_err(|error| error.to_string())?;
    let entering = Entering {
        workspace: workspace.to_string(),
        name: tilo_relay_protocol::web::kept(
            name,
            tilo_relay_protocol::web::MAX_DISPLAY_NAME_CHARS,
        ),
        request_id,
        through: Through::Door,
    };
    let session = tilo_relay_session::RelayClient::connect_with_signer(
        endpoint,
        address.endpoint_addr,
        Some(info.host_cert.user_public_key),
        Some(info.host_cert.device_public_key),
        tilo_relay_session::ClientCredentials {
            cert,
            signer: &key,
            revocations: tilo_user_identity::RevocationList::new(),
            entering: Some(tilo_relay_session::Entry {
                entering,
                // This device's channels ask for the workspace's keys as a member, once it holds
                // the workspace.
                access: None,
                host_cert: info.host_cert,
            }),
        },
    )
    .await
    .map_err(|error| format!("the workspace's host could not be reached: {error}"))?;
    let answer = tokio::time::timeout(
        tilo_relay_protocol::entering::ENTERING_DEADLINE.saturating_add(ENTRY_ANSWER_SLACK),
        session.recv(),
    )
    .await;
    let failure = session.handshake_failure();
    // The endpoint is this device's own, which its plane goes on using: only the session's
    // connection is closed.
    session.disconnect().await;
    let entered = match answer {
        Ok(Some(tilo_relay_protocol::transport::Envelope::Entered(entered))) => entered,
        Ok(Some(_)) => {
            return Err("the workspace's host answered the entry with something else".into());
        }
        Ok(None) => {
            return Err(failure.map_or_else(
                || "the workspace's host ended the session without answering".to_owned(),
                |error| format!("the workspace's host did not prove itself: {error}"),
            ));
        }
        Err(_) => return Err("the workspace's host did not answer".into()),
    };
    match entered {
        Entered::Admitted {
            workspace: admitted,
            host: Some(hint),
            ..
        } if admitted == workspace.to_string() => postcard::from_bytes(&hint)
            .map_err(|error| format!("the workspace's host named no address of its own: {error}")),
        Entered::Admitted { host: None, .. } => {
            Err("the workspace's host named no address to take the workspace from".into())
        }
        Entered::Admitted { .. } => {
            Err("the workspace's host admitted this device elsewhere".into())
        }
        Entered::Refused(refusal) => Err(refusal.reason().to_owned()),
    }
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
