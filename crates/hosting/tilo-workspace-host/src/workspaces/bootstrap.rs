//! The membership answer a device gets when it joins a workspace.

use tilo_peer_state::StatePeer;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

/// The workspace-join contract's membership answer for `device`, a device
/// the workspace `peer` replicates just admitted to subscribe, postcard encoded:
/// what it builds its replica of the workspace from before the workspace's replay
/// ([`tilo_server_protocol::Envelope::WorkspaceBootstrap`]). A device of the
/// workspace's owner gets the owner's answer, any other member's device its
/// role's. The governance messages and
/// records stay out: the replay that follows carries them.
pub(crate) fn member_bootstrap(
    peer: &StatePeer,
    host: &DeviceCert,
    workspace_id: WorkspaceId,
    generation: u64,
    device: &DeviceCert,
) -> Option<(Vec<u8>, crate::peer::GovernanceCertificates)> {
    use tilo_peer_state::join::{JoinBootstrap, JoinTransportResponse, OwnerBootstrapV1};
    let now = tilo_clock::wall();
    let role = peer
        .role(&device.device_public_key, now)
        .unwrap_or(collab_types::Role::Viewer);
    // The replay after the bootstrap carries the workspace's governance, and
    // with it every member's certificate: the roster names only what the
    // device holds before that governance, or could not take from it.
    let certified_roster =
        peer.roster_beside_governance(&[device.device_public_key, host.device_public_key], now);
    let certified = crate::peer::GovernanceCertificates::new(&certified_roster, peer.revocations());
    let answer = match role {
        collab_types::Role::Owner => {
            JoinTransportResponse::OwnerMembershipComplete(OwnerBootstrapV1 {
                version: 1,
                device_cert: device.clone(),
                workspace_id,
                host: host.clone(),
                certified_roster,
                revocations: peer.revocations(),
                records: Vec::new(),
                governance_messages: Vec::new(),
                generation,
            })
        }
        role => JoinTransportResponse::MembershipComplete(JoinBootstrap {
            version: 1,
            invite_id: [0; 32],
            device_cert: device.clone(),
            role,
            workspace_id,
            host: host.clone(),
            certified_roster,
            revocations: peer.revocations(),
            records: Vec::new(),
            governance_messages: Vec::new(),
            generation,
        }),
    };
    Some((postcard::to_stdvec(&answer).ok()?, certified))
}
