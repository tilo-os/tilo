//! The workspace-join exchange for the rooms this device governs, answered
//! on its iroh endpoint the way a relay answers for the rooms it governs
//! ([`tilo_relay_session::workspace_join::WorkspaceJoinProtocol`]). The
//! exchange itself is the native owner's
//! ([`tilo_joined_room::NativeWorkspaceOwner::serve_join`]); this only finds
//! the owner and keeps its disk writes off the network's threads.

use std::time::SystemTime;

use tilo_delivery_protocol::workspace_join::WorkspaceJoinService;
use tilo_peer_state::join::{JoinTransportRequest, JoinTransportResponse};

use crate::shell::state::WorkspaceTreeHandle;

/// What an answer carries when this device cannot answer yet.
const UNAVAILABLE: u16 = 503;

/// The rooms this device governs, as a workspace-join service: the tree
/// whose native owner holds them, and the device key that governs them.
#[derive(Clone)]
pub struct GoverningRooms {
    tree: WorkspaceTreeHandle,
    key: ed25519_dalek::SigningKey,
}

impl std::fmt::Debug for GoverningRooms {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GoverningRooms")
    }
}

impl GoverningRooms {
    #[must_use]
    pub fn new(tree: WorkspaceTreeHandle, key: ed25519_dalek::SigningKey) -> Self {
        Self { tree, key }
    }

    /// The rooms `tree` holds, answered for under the device key of
    /// `identity`; none without one, as a device without an identity
    /// governs nothing.
    pub fn of(
        tree: &WorkspaceTreeHandle,
        identity: Option<&super::actor::HostIdentity>,
    ) -> Option<Self> {
        identity.map(|identity| Self::new(tree.clone(), identity.device_signing_key.clone()))
    }

    fn answer(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        let unavailable = |message: String| JoinTransportResponse::Error {
            code: UNAVAILABLE,
            message,
        };
        let Some(owner) = crate::shell::native_workspace_owner::attached(&self.tree) else {
            return unavailable("this device's workspaces are still opening".into());
        };
        match tilo_joined_room::Nonces::fresh() {
            Ok(nonces) => owner.serve_join(
                remote_endpoint,
                request,
                &self.key,
                nonces,
                SystemTime::now(),
            ),
            Err(error) => unavailable(error),
        }
    }
}

impl WorkspaceJoinService for GoverningRooms {
    async fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        // An answer publishes the owner's record first: disk writes, which
        // must not hold a network thread.
        let rooms = self.clone();
        tokio::task::spawn_blocking(move || rooms.answer(remote_endpoint, request))
            .await
            .unwrap_or_else(|error| JoinTransportResponse::Error {
                code: UNAVAILABLE,
                message: error.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::GoverningRooms;
    use crate::network::actor::Actor;
    use crate::shell::state::WorkspaceTree;
    use ed25519_dalek::SigningKey;
    use std::sync::{Arc, Mutex};
    use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
    use tilo_peer_state::join::invitations::IssueInvitationRequest;
    use tilo_peer_state::join::{JoinTransportRequest, JoinTransportResponse, MemberRole};
    use tilo_relay_session::workspace_join::WorkspaceJoinClient;

    async fn local_endpoint(key: &SigningKey) -> iroh::Endpoint {
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    /// A device that governs a room answers the workspace-join exchange
    /// for it on its own endpoint, as a relay does for its rooms: a device
    /// holding the invitation reaches it there and hears what it grants.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_answers_the_join_exchange_for_a_room_it_governs() {
        let directory = tempfile::tempdir().unwrap();
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(directory.path().join("state.json"));
        let workspace = tree.desktop.active;
        let room = tree.room_of(workspace).unwrap();
        let tree = tree.shared();
        let owner = crate::shell::native_workspace_owner::open(tree.clone()).unwrap();
        let key = SigningKey::from_bytes(&[71; 32]);
        let device = tilo_joined_room::testing::certificate(
            &SigningKey::from_bytes(&[72; 32]),
            &key,
            "Desktop",
        );
        let seed = tilo_peer_state::workspace_seed(
            &room.to_string(),
            &device,
            std::slice::from_ref(&device),
            "Garden",
            1,
        )
        .unwrap();
        let now = std::time::SystemTime::now();
        owner
            .create_governed(room, workspace.raw(), &device, &key, seed, now)
            .unwrap();

        let endpoint = local_endpoint(&key).await;
        let port = endpoint.bound_sockets().first().unwrap().port();
        let addr = iroh::EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from(
            (std::net::Ipv4Addr::LOCALHOST, port),
        ));
        let workspace_id = tilo_peer_state::join::room_workspace_id(&room.to_string()).unwrap();
        let hint = IrohWorkspaceHostHintV1 {
            version: 1,
            mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
            room: room.to_string(),
            workspace_id,
            governing_authority: device.clone(),
            endpoint_addr: addr.clone(),
            host_user_public_key: device.user_public_key,
            host_device_certificate: device.clone(),
            packet_relay_url: "iroh:direct".into(),
        }
        .transport_hint(&key)
        .unwrap();
        let now_ns = tilo_joined_room::testing::ns(now).unwrap();
        let issued = owner
            .issue_invitation(
                room,
                IssueInvitationRequest {
                    request_id: [1; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: None,
                    invitee_name: None,
                    expires_at_ns: now_ns + 60_000_000_000,
                    inviter_name: None,
                },
                hint,
                &key,
                tilo_joined_room::Nonces::fresh().unwrap(),
                now,
            )
            .unwrap();
        let (_commands, actor) = Actor::spawn_governing(
            endpoint,
            iroh::address_lookup::memory::MemoryLookup::new(),
            None,
            Arc::new(Mutex::new(Vec::new())),
            crate::network::Reachability::default(),
            Some(GoverningRooms::new(tree, key)),
        );

        let client = local_endpoint(&SigningKey::from_bytes(&[73; 32])).await;
        let response = WorkspaceJoinClient::new(client.clone())
            .request(
                addr,
                JoinTransportRequest::Inspect {
                    signed_invitation: issued.signed_invitation,
                },
            )
            .await
            .unwrap();
        let JoinTransportResponse::Inspection(inspection) = response else {
            panic!("the governing device answers the invitation: {response:?}");
        };
        assert_eq!(inspection.invite_id, issued.invite_id);
        assert_eq!(inspection.grant.room, room.to_string());
        assert_eq!(inspection.grant.role, MemberRole::Editor);
        actor.abort();
        client.close().await;
    }
}
