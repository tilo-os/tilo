//! A workspace taken in: entered through its entry or redeemed from an invitation the shell made for
//! it, or entered as a further device of the person at the device hosting it. However this device
//! came to take part in a workspace another device serves, its own workspace service holds a member
//! copy of it, synced with that device: the copy every read and write of the workspace goes
//! through.

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

/// Come into the workspace the shell made for the join, through its entry or with an invitation: this
/// device becomes a member of the workspace, takes in what it held before it arrived, and takes part
/// in it from then on.
/// The workspace's state arrives as the host's answer: a person who joins late reads the whole
/// history, not only what is recorded from now on.
/// Renew this device's certificate before it is presented: a device that slept past the end of its
/// certificate joins with the renewal, reissued here or signed by the person's devices, and a join
/// that cannot renew says why rather than presenting what the host refuses.
async fn renew_before_joining(
    renewal: Option<std::sync::Arc<crate::renewal::FleetRenewal>>,
) -> Result<(), tilo_session_client::EnrollFailure> {
    let before =
        tokio::task::spawn_blocking(move || crate::renewal::before_presenting(renewal.as_deref()))
            .await;
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
    renew_before_joining(request.renewal.clone()).await?;
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
        tilo_workspace_directory::Way::Entry(entry) => {
            let hint = tokio::time::timeout(
                REDEEM_WITHIN,
                through_entry(
                    endpoint.clone(),
                    &identity,
                    &entry,
                    request.workspace_id,
                    &request.name,
                ),
            )
            .await
            .map_err(|_| "the workspace's host did not answer".to_owned())??;
            let client = tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint);
            let (bootstrap, at) = tokio::time::timeout(
                REDEEM_WITHIN,
                tilo_peer_state::owner::enroll::admit_as_member(
                    client,
                    identity,
                    owner.clone(),
                    request.workspace_id,
                    hint,
                    tilo_clock::wall(),
                ),
            )
            .await
            .map_err(|_| "the workspace's host did not answer".to_owned())??;
            tokio::time::timeout(
                SETTLE_WITHIN,
                take_part_in(&owner, request.workspace_id, link, network),
            )
            .await
            .map_err(|_| "the workspace was not taken in time".to_owned())??;
            hold_on_service(own_service, &bootstrap, &at).await;
            return Ok(());
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

/// How long the entry's host may take to answer the entry, beyond the bound it holds an entry to
/// itself: the connection takes some.
const ENTRY_ANSWER_SLACK: std::time::Duration = std::time::Duration::from_secs(15);

/// Enter `workspace` through `entry`, as this computer's person named `name`: the entry's host (at
/// the entry's own origin) says who it is, and this device's session with it asks to enter in its
/// first message, as every device enters a workspace through its entry
/// ([`tilo_server_protocol::entering`]). The entry decides there by the workspace's own rules, and the
/// workspace admits this device's own certificate. Answers the host's signed address for the
/// workspace, where this device then takes its state as a member's device does; the session itself
/// ends with the answer, since this device takes part over its own peer network.
async fn through_entry(
    endpoint: iroh::Endpoint,
    identity: &DeviceIdentity,
    entry: &url::Url,
    workspace: InviteWorkspaceId,
    name: &str,
) -> Result<tilo_peer_state::join::TransportHint, String> {
    use tilo_server_protocol::entering::{Entered, Entering, Through};
    let topic = workspace;
    let client =
        tilo_workspace_directory::client().map_err(|error| tilo_sheets::directory::said(&error))?;
    let origin = entry.origin().ascii_serialization();
    let host = tilo_server_client::https::Origin::new(&client, &origin);
    let info = tilo_server_client::https::server_info(&host)
        .await
        .map_err(|error| format!("the workspace's host could not be reached: {error}"))?;
    let address = info
        .address
        .ok_or("the workspace's host takes no device sessions")?;
    let (cert, _) = identity.current_material(topic)?;
    let mut request_id = [0; 32];
    getrandom::fill(&mut request_id).map_err(|error| error.to_string())?;
    let entering = Entering {
        workspace: workspace.to_string(),
        name: tilo_server_protocol::web::kept(
            name,
            tilo_server_protocol::web::MAX_DISPLAY_NAME_CHARS,
        ),
        request_id,
        through: Through::Entry,
    };
    let session = tilo_server_session::ServerClient::connect_with_signer(
        endpoint,
        address.endpoint_addr,
        Some(info.server_cert.user_public_key),
        Some(info.server_cert.device_public_key),
        tilo_server_session::ClientCredentials {
            cert,
            signer: identity.signer.as_ref(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: Some(tilo_server_session::Entry {
                entering,
                // This device's channels ask for the workspace's keys as a member, once it holds
                // the workspace.
                access: None,
                server_cert: info.server_cert,
            }),
        },
    )
    .await
    .map_err(|error| format!("the workspace's host could not be reached: {error}"))?;
    let answer = tokio::time::timeout(
        tilo_server_protocol::entering::ENTERING_DEADLINE.saturating_add(ENTRY_ANSWER_SLACK),
        session.recv(),
    )
    .await;
    let failure = session.handshake_failure();
    // The endpoint is this device's own, which its peer network goes on using: only the session's
    // connection is closed.
    session.disconnect().await;
    let entered = match answer {
        Ok(Some(tilo_server_protocol::transport::Envelope::Entered(entered))) => entered,
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
    let copies = service.workspaces();
    for workspace in engine.workspaces() {
        let Some(hint) = engine.member_of(workspace) else {
            continue;
        };
        if copies
            .is_member(&workspace.to_string())
            .await
            .unwrap_or(false)
        {
            continue;
        }
        match tokio::time::timeout(SETTLE_WITHIN, copies.hold_as_member(workspace, &hint)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, %workspace, "a workspace this device takes part in is not held on its service");
            }
            Err(_) => {
                tracing::warn!(%workspace, "a workspace's member copy did not arrive on this device's service in time");
            }
        }
    }
}
