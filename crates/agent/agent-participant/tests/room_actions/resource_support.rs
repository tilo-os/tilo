//! Certified admission for resource fixtures; no owner-only room entry.
use super::{BTreeMap, Browser, Device, Host, Hosted, RevocationList, ready, start_peer, support};
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_relay::rooms::IssueInvitationRequest;

pub(super) async fn open_admitted_peer(
    hosted: &Hosted,
    device: &Device,
    host: &Device,
    room: &str,
) -> Browser {
    let workspace = tilo_relay_protocol::room_workspace_id(room).unwrap();
    let peer = start_peer(
        Host::Https(support::https::DevHttps::new(
            hosted.web.addr,
            hosted.web.certificate_sha256.as_ref().unwrap(),
        )),
        device.cert.clone(),
        host.cert.clone(),
        device.key.clone(),
    );
    let events = peer.events();
    peer.join(workspace).unwrap();
    ready(&[&peer]).await;
    Browser {
        peer,
        events,
        host_key: host.cert.device_public_key,
        documents: BTreeMap::new(),
        answers: Vec::new(),
        correlation: 0,
        workspace,
        room: room.into(),
    }
}

pub(super) async fn admit_person(
    hosted: &Hosted,
    host: &Device,
    room: &str,
    device: &Device,
    role: MemberRole,
) {
    let invitation = hosted
        .relay
        .rooms()
        .unwrap()
        .issue(
            room,
            &host.cert,
            IssueInvitationRequest {
                request_id: device.cert.device_public_key,
                role,
                recipient_user_public_key: Some(device.cert.user_public_key),
                invitee_name: Some("Encrypted resource fixture".into()),
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
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.key.to_bytes()))
        .bind()
        .await
        .unwrap();
    tilo_relay::workspace_join::WorkspaceJoinClient::new(endpoint)
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
