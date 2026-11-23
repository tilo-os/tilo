//! Certified admission for resource fixtures; no owner-only workspace entry.
use super::{Browser, Device, Hosted, RevocationList, ServerAddress, ready, start_peer};
use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_server::workspaces::{IssueInvitationRequest, Workspaces};
use tilo_session_client::{Cmd, Outbox};
use tilo_user_identity::DeviceCert;

pub(super) async fn open_admitted_peer(
    hosted: &Hosted,
    device: &Device,
    host: &Device,
    cluster_id: &str,
) -> Browser {
    open_admitted_at(&hosted.server, device, host, cluster_id).await
}

/// [`open_admitted_peer`] on whatever `server` serves the workspace: the page of `device`,
/// whose person was admitted by an invitation, joins as a member does, under `holder`, the
/// device that serves it.
pub(super) async fn open_admitted_at(
    server: &tilo_server::daemon::Server,
    device: &Device,
    holder: &Device,
    cluster_id: &str,
) -> Browser {
    let workspace = tilo_server_protocol::WorkspaceId::parse(cluster_id).unwrap();
    let peer = start_peer(
        ServerAddress::at(server.node_addr()),
        device.cert.clone(),
        holder.cert.clone(),
        device.key.clone(),
    );
    let events = peer.events();
    peer.send(Cmd::Join(workspace)).unwrap();
    ready(&[&peer]).await;
    Browser::joined((peer, events), device, holder, cluster_id)
}

pub(super) async fn admit_person(
    hosted: &Hosted,
    host: &Device,
    workspace: &str,
    device: &Device,
    role: Role,
) {
    admit_person_at(&hosted.server, host, workspace, device, role).await;
}

/// [`admit_person`] on whatever `server` serves the workspace, `holder` its device.
pub(super) async fn admit_person_at(
    server: &tilo_server::daemon::Server,
    holder: &Device,
    workspace: &str,
    device: &Device,
    role: Role,
) {
    admit_with(
        server.workspaces().unwrap(),
        &holder.cert,
        workspace,
        device,
        role,
        "Encrypted resource fixture",
    )
    .await;
}

/// Admit `device` to `workspace` as `role`, invited by the administrator `administrator` and
/// named `name`: the server issues an invitation and the device redeems it.
pub(super) async fn admit_with(
    workspaces: &Workspaces,
    administrator: &DeviceCert,
    workspace: &str,
    device: &Device,
    role: Role,
    name: &str,
) {
    let invitation = workspaces
        .issue(
            workspace,
            administrator,
            IssueInvitationRequest {
                request_id: device.cert.device_public_key,
                role,
                recipient_user_public_key: Some(device.cert.user_public_key),
                invitee_name: Some(name.to_owned()),
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    // As a device dials: relays on, since an invitation names what a stranger
    // can dial — the host's relay and its addresses — and an endpoint bound
    // with relays off redeems only an invitation naming a direct address of
    // the host's own network. Its relay is this machine's loopback one, so
    // nothing here reaches the person's relays out on the internet.
    let endpoint = test_server::endpoint(iroh::SecretKey::from_bytes(&device.key.to_bytes())).await;
    tilo_server::workspace_join::WorkspaceJoinClient::new(endpoint)
        .redeem_hint(
            verified.grant().transport.first().unwrap(),
            invitation.signed_invitation.clone(),
            device.cert.clone(),
            &device.key,
            device.cert.device_public_key,
        )
        .await
        .unwrap();
}
