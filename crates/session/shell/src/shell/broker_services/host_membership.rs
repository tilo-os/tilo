//! Letting an app author a room a host governs.
//!
//! Which apps author a room's records is the room's owner's to decide, not
//! a membership change: in a room a host governs, the host commits it, by
//! the room's own governance. This asks it and answers once the change is
//! in this device's admitted room, the same barrier the room's other
//! commits use.

/// How long an authorship the host committed may take to reach this
/// device's admitted room. With the host's own answer (at most
/// `relay_rooms::ADMIN_TIMEOUT`) it stays inside the 30 s a launch waits.
const HOST_AUTHORSHIP_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Have the host governing `room` let `app` author it, as `caller`, and
/// answer with this device's committed view once it carries the change.
/// `None` when no host governs the room: this device's own owner commits it.
pub(super) async fn grant_app_authorship_at_host(
    network: &super::super::network::ShellNetwork,
    owner: &super::super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::super::state::RoomId,
    request: &tilo_owner_commit::OwnerCommitRequest,
    caller: &tilo_user_identity::DeviceCert,
    app: &str,
) -> Option<Result<tilo_owner_commit::OwnerCommitReceipt, String>> {
    let generation = match network
        .grant_app_authorship_at_host(room, caller, app)
        .await?
    {
        Ok(generation) => generation,
        Err(error) => return Some(Err(error)),
    };
    tracing::info!(%room, app, generation, "the host let the app author the room");
    Some(wait_for_authorship(network, owner, room, request, caller, app).await)
}

/// Wait until the authorship a host committed is in this device's actual
/// admitted room, and answer with this device's committed view of it.
async fn wait_for_authorship(
    network: &super::super::network::ShellNetwork,
    owner: &super::super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::super::state::RoomId,
    request: &tilo_owner_commit::OwnerCommitRequest,
    caller: &tilo_user_identity::DeviceCert,
    app: &str,
) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
    tokio::time::timeout(HOST_AUTHORSHIP_ADMISSION_TIMEOUT, async {
        loop {
            network.check_native_custody(owner, room)?;
            let receipt = owner.with_current(room, |view| {
                let now = std::time::SystemTime::now();
                let member = view.peer().permissions(caller.device_public_key, now)?;
                if !member
                    .policy
                    .app_authors(app, &tilo_action_policy::Context::at_time(now))
                {
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
        tracing::warn!(%room, "the app's authorship did not reach this device in time");
        "the host let the app author the room but it did not reach this device in time"
    })?
}
