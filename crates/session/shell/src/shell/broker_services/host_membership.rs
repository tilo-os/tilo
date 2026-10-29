//! Membership changes in a room this device's own host governs: that host
//! commits them under the key this device shares with it, and this
//! device's replica takes what it wrote in process, so the answer carries
//! them.

/// Have this device's own host, which governs `room`, commit `command`, a
/// membership change of `caller`, this device
/// ([`super::super::network::ShellNetwork::own_host_governs`]), and answer
/// with this device's committed view once its replica carries the change.
/// `None` when the room is not governed by this device's own host: this
/// device's owner writes the change itself.
pub(super) async fn commit_at_own_host(
    network: &super::super::network::ShellNetwork,
    owner: &super::super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::super::state::RoomId,
    request: &tilo_owner_commit::OwnerCommitRequest,
    caller: &tilo_user_identity::DeviceCert,
    command: &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
) -> Option<Result<tilo_owner_commit::OwnerCommitReceipt, String>> {
    if !network.own_host_governs(room, caller) {
        return None;
    }
    let committed = async {
        let host = network
            .room_host()
            .ok_or("this device's host is not open yet")?;
        let (completion, added) = host
            .rooms()
            .commit_own_membership(&room.to_string(), caller, command.clone())
            .await
            .map_err(|error| error.to_string())?;
        drop(host);
        tracing::debug!(%room, generation = completion.generation, "this device's host committed its membership change");
        // What the host wrote is taken into this device's replica in
        // process, as its publication would bring it: in force here now.
        if !added.is_empty() {
            owner.receive(
                room,
                tilo_peer_state::TransportOrigin::RelayDelta(caller.device_public_key),
                neighbour_wire::NeighbourMsg::SyncResp {
                    changes: added,
                    room: room.to_string(),
                },
                std::time::SystemTime::now(),
            )?;
            network.schedule_native_owner_publication(room);
        }
        network.check_native_custody(owner, room)?;
        owner.with_current(room, |view| {
            Ok(tilo_owner_commit::OwnerCommitReceipt {
                binding: request.binding()?,
                generation: view.generation(),
                aggregate_hash: *blake3::hash(&view.peer().durable()?).as_bytes(),
            })
        })
    };
    Some(committed.await)
}
