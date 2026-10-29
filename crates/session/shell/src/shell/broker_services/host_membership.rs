//! Membership changes in a room this device's own host governs: that host
//! commits them under the key this device shares with it, and the answer
//! waits until this device's replica carries them, the same barrier the
//! room's other commits use.

/// How long a change this device's host committed may take to reach this
/// device's admitted room, inside the 30 s a launch waits.
const HOST_COMMIT_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
        let (completion, heads) = host
            .rooms()
            .commit_own_membership(&room.to_string(), caller, command.clone())
            .await
            .map_err(|error| error.to_string())?;
        drop(host);
        tracing::debug!(%room, generation = completion.generation, "this device's host committed its membership change");
        network.schedule_native_owner_publication(room);
        wait_for_heads(network, owner, room, request, &heads).await
    };
    Some(committed.await)
}

/// Wait until this device's admitted room carries `heads`, what its own
/// host committed, and answer with this device's committed view of it.
async fn wait_for_heads(
    network: &super::super::network::ShellNetwork,
    owner: &super::super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::super::state::RoomId,
    request: &tilo_owner_commit::OwnerCommitRequest,
    heads: &[automerge::ChangeHash],
) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
    tokio::time::timeout(HOST_COMMIT_ADMISSION_TIMEOUT, async {
        loop {
            network.check_native_custody(owner, room)?;
            let receipt = owner.with_current(room, |view| {
                let carried = view.peer().admitted_room().is_some_and(|doc| {
                    heads
                        .iter()
                        .all(|head| automerge::ReadDoc::get_change_by_hash(doc, head).is_some())
                });
                if !carried {
                    return Ok(None);
                }
                Ok(Some(tilo_owner_commit::OwnerCommitReceipt {
                    binding: request.binding()?,
                    generation: view.generation(),
                    aggregate_hash: *blake3::hash(&view.peer().durable()?).as_bytes(),
                }))
            })?;
            if let Some(receipt) = receipt {
                return Ok(receipt);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| {
        tracing::warn!(%room, "a membership change this device's host committed did not reach this device in time");
        "this device's host committed the membership change but it did not reach this device in time"
    })?
}
