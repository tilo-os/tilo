//! Certified admission for resource fixtures; no owner-only workspace entry.
use super::{Browser, Device, Hosted, RevocationList, ServerAddress, ready, start_peer};
use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_server::workspaces::{IssueInvitationRequest, Workspaces};
use tilo_session_client::{Cmd, Outbox};
use tilo_user_identity::DeviceCert;

/// The folder a person's computer mounts, the device `cert` certifies: each change of its tree
/// names the person, as the computer's own mount does.
pub(super) fn computer_folder(cert: &DeviceCert) -> tilo_fs::Namespace {
    tilo_fs::Namespace::new(tilo_fs::namespace::MountAuthor::person(
        hex::encode(cert.device_public_key),
        ui::permissions::MemberId::person(hex::encode(cert.user_public_key)),
    ))
    .unwrap()
}

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

/// The ids of the messages `page` holds in the parts of its chat's messages.
pub(super) fn message_ids(page: &super::Replica) -> Vec<String> {
    page.resources_beside()
        .iter()
        .filter_map(|record| page.read_resource(record, chat::messages::read_part))
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
        &super::ActionAddress::new(
            ui::resource::chain_resource_id(&chat::parts::chain()),
            action,
        ),
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
        resources: &'a [ui::tilo_actions::ResourceSpec],
        id: &str,
    ) -> Option<&'a ui::tilo_actions::ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }
    let id = ui::tilo_resource::items::item_resource_id::<chat::parts::ChatMessages>(
        "messages",
        &ui::tilo_resource::Id::from_raw(message),
    );
    let kind = find(&page.resources(), &id)
        .map(|resource| resource.kind.clone())
        .expect("the page publishes the message");
    (id, kind)
}

/// The route of `record`, one of the chat's message parts, in `workspace`.
pub(super) fn part_route(
    workspace: &str,
    record: &ui::resource::DocumentId,
) -> super::DocumentRoute {
    super::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.to_owned(),
        document: record.as_str().to_owned(),
    }
}
