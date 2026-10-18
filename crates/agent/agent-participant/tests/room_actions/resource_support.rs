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

/// The ids of the messages `page` holds in the parts of its chat's messages.
pub(super) fn message_ids(page: &super::Replica) -> Vec<String> {
    page.records_beside()
        .iter()
        .filter_map(|record| page.read_record(record, chat::messages::read_part))
        .flat_map(|part| {
            part.messages
                .ids()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Run `action` with `arguments` on the chain of the chat's messages, as
/// `page`'s person does from their window; the id of the message it posted.
pub(super) fn on_messages(
    page: &mut super::Replica,
    action: &str,
    arguments: &serde_json::Value,
) -> String {
    let before = message_ids(page);
    page.invoke(
        &super::ActionAddress::new(ui::record::chain_resource_id(&chat::parts::chain()), action),
        arguments.as_object().unwrap(),
        false,
    )
    .unwrap();
    message_ids(page)
        .into_iter()
        .find(|id| !before.contains(id))
        .expect("the post is in a part")
}

/// The resource id the chat's message `message` is published under, and
/// its kind, as `page` publishes it: an item of the part that keeps it.
pub(super) fn published_message(page: &super::Replica, message: &str) -> (String, String) {
    fn find<'a>(
        resources: &'a [ui::tilo_actions::Resource],
        id: &str,
    ) -> Option<&'a ui::tilo_actions::Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }
    let id = ui::tilo_resource::items::item_resource_id::<chat::parts::ChatPart>(
        "messages",
        &ui::tilo_resource::Id::from_raw(message),
    );
    let kind = find(&page.resources(), &id)
        .map(|resource| resource.kind.clone())
        .expect("the page publishes the message");
    (id, kind)
}

/// The route of `record`, one of the chat's message parts, in `room`.
pub(super) fn part_route(room: &str, record: &ui::record::RecordId) -> super::DocumentRoute {
    super::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: room.to_owned(),
        record: record.as_str().to_owned(),
    }
}
