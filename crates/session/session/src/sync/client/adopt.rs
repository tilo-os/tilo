//! A workspace taken in: entered through its entry or redeemed from an invitation the shell made for
//! it, or entered as a further device of the person at the device hosting it.

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
) -> Result<tilo_peer_state::join_transport::WorkspaceJoinClient, String> {
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
        return Ok(tilo_peer_state::join_transport::WorkspaceJoinClient::new(
            endpoint,
        ));
    }
    own_service
        .and_then(|service| service.join_client(own))
        .ok_or_else(|| "this device's workspace service is not open".to_owned())
}

/// Come into the workspace the shell made for the join, through its entry or with an invitation: this
/// device becomes a member of the workspace, takes in what it held before it arrived, and takes part
/// in it from then on.
/// The workspace's state arrives as the host's answer: a person who joins late reads the whole
/// history, not only what is recorded from now on.
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
    // Renewal precedes presentation: a device that slept past the end of its certificate joins with
    // the renewal, reissued here or signed by the person's devices, and a join that cannot renew
    // says why rather than presenting what the host refuses.
    let renewal = request.renewal.clone();
    let before =
        tokio::task::spawn_blocking(move || crate::renewal::before_presenting(renewal.as_deref()))
            .await;
    match before {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(error.into()),
        Err(error) => {
            return Err(format!("the renewal before this join did not finish: {error}").into());
        }
    }
    // The workspace is joined as this computer's person: the device's own certificate signs the
    // enrollment, so there is nothing to pick here.
    identity.current_material(topic)?;
    let owner = engine.owner().clone();
    let invitation = match request.way {
        tilo_workspace_directory::Way::Invitation(invitation) => invitation,
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
            tokio::time::timeout(
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
            return Ok(());
        }
    };
    let client = join_client(endpoint, own_service, &identity, &invitation)?;
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
    let info = tilo_server_client::https::host_info(&host)
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
        Some(info.host_cert.user_public_key),
        Some(info.host_cert.device_public_key),
        tilo_server_session::ClientCredentials {
            cert,
            signer: identity.signer.as_ref(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: Some(tilo_server_session::Entry {
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
