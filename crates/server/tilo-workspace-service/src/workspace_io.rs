//! Opening and evicting the runtime of each workspace this host serves, and
//! enrolling a member's further device. Who a workspace admits is its signed
//! member list, held in memory ([`WorkspaceRuntime::members`]); no workspace's
//! opening waits for another's, and no registry guard crosses filesystem work.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{Semaphore, watch};

use crate::registry::WorkspaceRegistry;
use crate::workspace::{SharedWorkspace, WorkspaceRuntime};
use crate::workspace_files::FilesError;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

const OPEN_WORKERS: usize = 4;

fn io_error(message: impl Into<String>) -> FilesError {
    FilesError::Io(std::io::Error::other(message.into()))
}

/// Enrol `cert`'s device as a member of `workspace`, where
/// [`crate::workspace_access::decide`] answered
/// [`crate::workspace_access::Decision::AdmitAndEnrol`]: the signed member list records the
/// device under its person, and its settle lists it here. The caller decides again afterwards: a
/// device the list did not take is still no member.
pub(crate) async fn enrol(workspace: &SharedWorkspace, cert: &tilo_user_identity::DeviceCert) {
    let authority = workspace.lock().await.workspace_authority.clone();
    if let Some(authority) = authority
        && let Some(workspaces) = authority.workspaces()
    {
        workspaces
            .enrol_member_device(cert, authority.workspace())
            .await;
    }
}

type OpenResult = Result<SharedWorkspace, String>;
type Opening = watch::Receiver<Option<OpenResult>>;
type ReservationKey = (usize, WorkspaceId);
fn reservations() -> &'static Mutex<HashMap<ReservationKey, Opening>> {
    static RESERVATIONS: OnceLock<Mutex<HashMap<ReservationKey, Opening>>> = OnceLock::new();
    RESERVATIONS.get_or_init(Mutex::default)
}

async fn open_runtime(state_root: std::path::PathBuf, workspace_id: WorkspaceId) -> OpenResult {
    static OPEN: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = OPEN
        .get_or_init(|| Arc::new(Semaphore::new(OPEN_WORKERS)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| error.to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // Every workspace this host opens, it holds: its own writable copy.
        let runtime = WorkspaceRuntime::open(&state_root, workspace_id)?;
        Ok::<_, FilesError>(Arc::new(tokio::sync::Mutex::new(runtime)))
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

/// Take workspace `workspace_id` out of `workspaces`: its files stay on disk,
/// whatever still holds its runtime writes them no more, and once this
/// returns its persistence owner has finished what was queued. Opening the
/// workspace again ([`get_or_initialize`]) starts from those files.
pub(crate) async fn evict(workspaces: &WorkspaceRegistry, workspace_id: WorkspaceId) {
    let Some(workspace) = workspaces.lock().await.remove(&workspace_id) else {
        return;
    };
    workspace.lock().await.evicted = true;
    drop(workspace);
}

/// One reserved initialization per registry/ID; reservations count against the
/// workspace cap. The owner retains the registry Arc (and therefore its identity)
/// until publication, even when every initial waiter disconnects.
pub(crate) async fn get_or_initialize(
    workspaces: &WorkspaceRegistry,
    state_root: std::path::PathBuf,
    workspace_id: WorkspaceId,
    capacity: usize,
) -> Result<SharedWorkspace, FilesError> {
    let registry_id = Arc::as_ptr(workspaces) as usize;
    let key = (registry_id, workspace_id);
    let mut result = {
        let map = workspaces.lock().await;
        if let Some(workspace) = map.get(&workspace_id) {
            return Ok(workspace.clone());
        }
        let mut pending = reservations().lock_recover();
        if let Some(opening) = pending.get(&key) {
            opening.clone()
        } else {
            let reserved = pending
                .keys()
                .filter(|(registry, _)| *registry == registry_id)
                .count();
            if map.len().saturating_add(reserved) >= capacity {
                return Err(io_error("home peer workspace limit reached"));
            }
            let (sender, receiver) = watch::channel(None);
            pending.insert(key, receiver.clone());
            drop(pending);
            drop(map);
            let registry = workspaces.clone();
            tokio::spawn(async move {
                let opened = open_runtime(state_root, workspace_id).await;
                let mut map = registry.lock().await;
                let opened =
                    opened.map(|workspace| map.entry(workspace_id).or_insert(workspace).clone());
                sender.send_replace(Some(opened));
                reservations().lock_recover().remove(&key);
            });
            receiver
        }
    };
    loop {
        let completed = result.borrow_and_update().clone();
        if let Some(result) = completed {
            return result.map_err(io_error);
        }
        result
            .changed()
            .await
            .map_err(|error| io_error(error.to_string()))?;
    }
}

/// Admit `peer` to `workspace` for `operation`, enrolling a member's device
/// first.
///
/// `Err(reason)` when the device is refused.
#[cfg(test)]
pub(crate) async fn admit_for(
    workspace: &SharedWorkspace,
    trust: &crate::workspace_access::ServerTrust<'_>,
    peer: &crate::auth::AuthenticatedPeer,
    operation: crate::workspace_access::WorkspaceOperation,
) -> Result<(), crate::workspace_access::DenyReason> {
    use crate::workspace_access::{Decision, DenyReason, decide};
    let decision = decide(&*workspace.lock().await, trust, peer, operation);
    match decision {
        Decision::Admit => Ok(()),
        Decision::Deny(reason) => Err(reason),
        Decision::AdmitAndEnrol => {
            enrol(workspace, &peer.cert).await;
            let decision = decide(&*workspace.lock().await, trust, peer, operation);
            match decision {
                Decision::Admit => Ok(()),
                Decision::AdmitAndEnrol => Err(DenyReason::NotAMember),
                Decision::Deny(reason) => Err(reason),
            }
        }
    }
}
