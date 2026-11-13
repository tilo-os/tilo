//! Filesystem authority work is independent of state persistence. Admission is
//! bounded before spawning work; one authorization lane serializes each workspace's
//! reads and writes. No routing or registry guard crosses filesystem work.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{MutexGuard, OwnedSemaphorePermit, Semaphore, watch};

use crate::membership_watch::DiskGeneration;
use crate::peer::WorkspaceRegistry;
use crate::store::{GovernedAccessRecord, StoreError, WorkspaceStore};
use crate::workspace::{Membership, SharedWorkspace, WorkspaceRuntime};
use crate::workspace_access::AdmissionMode;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;

const AUTHORIZATION_CAPACITY: usize = 64;
const AUTHORIZATION_WORKERS: usize = 8;
const OPEN_WORKERS: usize = 4;

fn io_error(message: impl Into<String>) -> StoreError {
    StoreError::Io(std::io::Error::other(message.into()))
}

fn authorization_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(AUTHORIZATION_CAPACITY)))
}

/// How long authorization work waits for a slot before its caller fails.
/// Every guest who joins a public workspace changes its membership, so a burst
/// of arrivals sends every session's next check to disk at once; they queue
/// here rather than each ending its session.
const AUTHORIZATION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

async fn admit_work() -> Result<OwnedSemaphorePermit, StoreError> {
    let slots = authorization_slots().clone();
    if let Ok(permit) = slots.clone().try_acquire_owned() {
        return Ok(permit);
    }
    if crate::persistence::correlated_diagnostics() {
        tracing::info!(
            phase = "authorization_backpressure",
            capacity = AUTHORIZATION_CAPACITY
        );
    }
    tokio::time::timeout(AUTHORIZATION_WAIT, slots.acquire_owned())
        .await
        .map_err(|_| io_error("workspace authorization backpressure"))?
        .map_err(|error| io_error(error.to_string()))
}

async fn authorization_io<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = WORKERS
        .get_or_init(|| Arc::new(Semaphore::new(AUTHORIZATION_WORKERS)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| io_error(error.to_string()))?;
    // The permit stays in blocking work even when the waiting caller cancels.
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await;
    match result {
        Ok(result) => result,
        Err(error) => Err(io_error(error.to_string())),
    }
}

async fn authority_handles(
    workspace: &SharedWorkspace,
) -> (WorkspaceStore, Arc<tokio::sync::Mutex<()>>) {
    let runtime = workspace.lock().await;
    (runtime.store.clone(), runtime.authorization.clone())
}

/// The handles a membership reload needs, attaching the workspace to the daemon's
/// membership watch when one now covers it (the runtime may have opened
/// before the watch started, or outlived a stopped one).
async fn reload_handles(
    workspace: &SharedWorkspace,
) -> (
    WorkspaceStore,
    Arc<tokio::sync::Mutex<()>>,
    Option<DiskGeneration>,
) {
    let mut runtime = workspace.lock().await;
    let generation = watched_generation(&mut runtime);
    (
        runtime.store.clone(),
        runtime.authorization.clone(),
        generation,
    )
}

fn watched_generation(runtime: &mut WorkspaceRuntime) -> Option<DiskGeneration> {
    if !runtime
        .disk_generation
        .as_ref()
        .is_some_and(DiskGeneration::is_live)
    {
        runtime.disk_generation = crate::membership_watch::attach(runtime.store.root());
    }
    runtime.disk_generation.clone()
}

/// Current membership: in memory, reloaded from the external file only after
/// the daemon's membership watch saw it change (or on every call while no live
/// watch covers the workspace). Callers must recheck certificate, policy, workspace and
/// exact sessions using the returned routing guard. A reload holds the
/// authorization lane, and releases it while that guard remains held, so a
/// newer update cannot be overwritten by this read or slip between this read
/// and the caller's check. In-process writers narrow the in-memory set under
/// the routing lock before any filesystem work, so a read that needs no reload
/// already sees their removals.
///
/// Only device membership is reloaded. `governed_user_roots` and
/// the governed admission mode change through `replace_governed_access` and the
/// runtime's open, as before the watch: an external edit of the governed file
/// narrows or widens the exact devices, not the handshake roots.
pub async fn fresh_membership(
    workspace: &SharedWorkspace,
) -> Result<MutexGuard<'_, WorkspaceRuntime>, StoreError> {
    {
        let locking = crate::timing::start();
        let runtime = workspace.lock().await;
        crate::timing::window("room.lock", locking);
        if runtime.membership_is_current() {
            // No disk IO, no worker, no lane.
            return Ok(runtime);
        }
    }
    let reloading = crate::timing::start();
    let reloaded = reload_membership(workspace).await;
    crate::timing::window("room.membership_reload", reloading);
    reloaded
}

async fn reload_membership(
    workspace: &SharedWorkspace,
) -> Result<MutexGuard<'_, WorkspaceRuntime>, StoreError> {
    let _slot = admit_work().await?;
    let (store, lane, generation) = reload_handles(workspace).await;
    let _lane = lane.lock_owned().await;
    // Observed before the read: a change during it moves the generation on,
    // and the next call reads again.
    let observed = generation.as_ref().and_then(DiskGeneration::current);
    if observed.is_some() {
        let runtime = workspace.lock().await;
        if runtime.loaded_generation == observed {
            // Another reload finished while this one waited for the lane.
            return Ok(runtime);
        }
    }
    let members = authorization_io(move || store.load_membership()).await?;
    let mut runtime = workspace.lock().await;
    apply_loaded_membership(&mut runtime, members, observed);
    Ok(runtime)
}

/// Publish membership read from disk, bounded by any pending or failed
/// replacement's ceiling, as loaded at `observed`.
fn apply_loaded_membership(
    runtime: &mut WorkspaceRuntime,
    mut members: Vec<[u8; 32]>,
    observed: Option<u64>,
) {
    if let Some(ceiling) = &runtime.membership_ceiling {
        members.retain(|device| ceiling.contains(device));
    }
    runtime.membership.set(members);
    runtime.loaded_generation = observed;
}

/// Enrol `cert`'s device as a member of `workspace`, where [`crate::workspace_access::decide`]
/// answered [`crate::workspace_access::Decision::AdmitAndEnrol`], through whoever owns the
/// workspace's membership: the governance of a workspace this relay governs records the device
/// under its person, and its settle projects it here; any other workspace's membership file takes
/// it ([`admit_member`]). The caller decides again afterwards: a device governance did not take
/// is still no member.
///
/// # Errors
///
/// The store's error when the membership file cannot be read or written.
pub(crate) async fn enrol(
    workspace: &SharedWorkspace,
    cert: &tilo_user_identity::DeviceCert,
    expected_revision: u64,
) -> Result<(), StoreError> {
    let governed = {
        let runtime = workspace.lock().await;
        (runtime.admission == AdmissionMode::Governed).then(|| {
            runtime.workspace_authority.as_ref().and_then(|authority| {
                Some((authority.workspaces()?, authority.workspace().to_owned()))
            })
        })
    };
    match governed {
        None => admit_member(workspace, cert.device_public_key, expected_revision).await,
        Some(Some((workspaces, uuid))) => {
            workspaces.enrol_member_device(cert, &uuid).await;
            Ok(())
        }
        // Governed elsewhere: only that governance admits a device.
        Some(None) => Ok(()),
    }
}

/// Persist an explicitly authorized admission before granting it. The detached,
/// bounded owner finishes a started update even if its caller disconnects.
pub(crate) async fn admit_member(
    workspace: &SharedWorkspace,
    device: [u8; 32],
    expected_revision: u64,
) -> Result<(), StoreError> {
    let slot = admit_work().await?;
    let workspace = workspace.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let (store, lane) = authority_handles(&workspace).await;
        let _lane = lane.lock_owned().await;
        let reader = store.clone();
        let mut members = authorization_io(move || reader.load_membership()).await?;
        {
            let runtime = workspace.lock().await;
            if runtime.membership_revision != expected_revision {
                return Err(io_error("workspace admission authorization changed"));
            }
            if runtime
                .membership_ceiling
                .as_ref()
                .is_some_and(|ceiling| !ceiling.contains(&device))
            {
                return Err(io_error(
                    "workspace admission denied by membership replacement",
                ));
            }
            if let Some(ceiling) = &runtime.membership_ceiling {
                members.retain(|member| ceiling.contains(member));
            }
        }
        if !members.contains(&device) {
            members.push(device);
        }
        let saved = members.clone();
        authorization_io(move || store.save_membership(&saved)).await?;
        workspace.lock().await.membership.set(members);
        Ok(())
    })
    .await
    .map_err(|error| io_error(error.to_string()))?
}

/// Serialize admin replacement with fresh reads/admissions. Removals deny as
/// soon as this operation obtains the lane; additions grant only after commit.
/// A failed write keeps removals denied in this process until a successful
/// explicit replacement, so a later read of the old file cannot resurrect them.
#[cfg(test)]
pub(crate) async fn replace_membership(
    workspace: &SharedWorkspace,
    members: Vec<[u8; 32]>,
) -> Result<(), StoreError> {
    let slot = admit_work().await?;
    let workspace = workspace.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let (store, lane) = authority_handles(&workspace).await;
        let _lane = lane.lock_owned().await;
        {
            let mut runtime = workspace.lock().await;
            runtime.membership_revision = runtime
                .membership_revision
                .checked_add(1)
                .ok_or_else(|| io_error("workspace membership revision exhausted"))?;
            let requested: std::collections::HashSet<_> = members.iter().copied().collect();
            if let Some(ceiling) = &mut runtime.membership_ceiling {
                ceiling.retain(|device| requested.contains(device));
            } else {
                runtime.membership_ceiling = Some(requested);
            }
            runtime
                .membership
                .devices
                .retain(|device| members.contains(device));
        }
        // Sessions of removed devices re-check now, before the write.
        crate::authority_epoch::bump();
        let saved = members.clone();
        authorization_io(move || store.save_membership(&saved)).await?;
        let mut runtime = workspace.lock().await;
        runtime.membership.set(members);
        runtime.membership_ceiling = None;
        drop(runtime);
        Ok(())
    })
    .await
    .map_err(|error| io_error(error.to_string()))?
}

/// Replace the native governor's exact devices and handshake roots as one
/// durable projection. Removals narrow the live workspace before filesystem work;
/// additions appear only after the atomic record commits. A failed write
/// therefore leaves a fail-closed in-process ceiling until a later successful
/// replacement. Returns the devices the replacement no longer admits.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the runtime guard publishes the narrowed ceiling as one step and ends with the task"
)]
pub(crate) async fn replace_governed_access(
    workspace: &SharedWorkspace,
    access: GovernedAccessRecord,
) -> Result<Vec<[u8; 32]>, StoreError> {
    let slot = admit_work().await?;
    let workspace = workspace.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let (store, lane) = authority_handles(&workspace).await;
        let _lane = lane.lock_owned().await;
        // The governor's projection: from here on governance owns who joins.
        let access = GovernedAccessRecord {
            governed: true,
            ..access
        };
        let (narrowed, removed) = {
            let mut runtime = workspace.lock().await;
            runtime.membership_revision = runtime
                .membership_revision
                .checked_add(1)
                .ok_or_else(|| io_error("workspace membership revision exhausted"))?;
            let requested_devices: std::collections::HashSet<_> =
                access.member_devices.iter().copied().collect();
            let removed: Vec<[u8; 32]> = runtime
                .membership
                .devices
                .iter()
                .copied()
                .filter(|device| !requested_devices.contains(device))
                .collect();
            runtime
                .membership
                .devices
                .retain(|device| requested_devices.contains(device));
            if let Some(ceiling) = &mut runtime.membership_ceiling {
                ceiling.retain(|device| requested_devices.contains(device));
            } else {
                runtime.membership_ceiling = Some(requested_devices);
            }
            let requested_roots: std::collections::HashSet<_> =
                access.member_user_roots.iter().collect();
            let roots = runtime.governed_user_roots.len();
            runtime
                .governed_user_roots
                .retain(|root| requested_roots.contains(root));
            let roots_removed = runtime.governed_user_roots.len() != roots;
            let governed_before =
                std::mem::replace(&mut runtime.admission, AdmissionMode::Governed)
                    == AdmissionMode::Governed;
            // Refuse while the authority lane is held so the writer can deliver
            // NotAuthorised before a waiting access audit closes the session.
            runtime.refuse_removed_live_members(&removed);
            (
                !removed.is_empty() || roots_removed || !governed_before,
                removed,
            )
        };
        // Sessions of removed devices and roots re-check now, before the
        // write. A replacement that only admits narrows nobody's access: a
        // public workspace's every admission woke every session it held.
        if narrowed {
            crate::authority_epoch::bump();
        }
        let saved = access.clone();
        let writing = crate::timing::start();
        let written = authorization_io(move || {
            let (digest, written) = store.save_governed_access(&saved)?;
            // Applied to the runtime below, its narrowing announced
            // above: the watch need not announce the file again.
            crate::membership_watch::wrote_own(
                &store.root().join(crate::store::GOVERNED_ACCESS_FILE),
                digest,
            );
            Ok(written)
        })
        .await?;
        crate::timing::event("room.settle.access_write", writing, written);
        let mut runtime = workspace.lock().await;
        runtime.membership.set(access.member_devices);
        runtime.governed_user_roots = access.member_user_roots.into_iter().collect();
        runtime.admission = AdmissionMode::Governed;
        runtime.membership_ceiling = None;
        Ok(removed)
    })
    .await
    .map_err(|error| io_error(error.to_string()))?
}

type OpenResult = Result<SharedWorkspace, String>;
type Opening = watch::Receiver<Option<OpenResult>>;
type ReservationKey = (usize, WorkspaceId);
fn reservations() -> &'static Mutex<HashMap<ReservationKey, Opening>> {
    static RESERVATIONS: OnceLock<Mutex<HashMap<ReservationKey, Opening>>> = OnceLock::new();
    RESERVATIONS.get_or_init(Mutex::default)
}

async fn open_runtime(
    state_root: std::path::PathBuf,
    payloads: Arc<dyn crate::payloads::ShellPayloads>,
    workspace_id: WorkspaceId,
    pinned: bool,
    admission: AdmissionMode,
) -> OpenResult {
    static OPEN: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = OPEN
        .get_or_init(|| Arc::new(Semaphore::new(OPEN_WORKERS)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| error.to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut runtime = WorkspaceRuntime::open_with_payloads(
            &state_root,
            workspace_id,
            pinned,
            Membership::default(),
            payloads,
        )?;
        // A governed record on disk keeps the workspace governed.
        runtime.admission = runtime.admission.rebind(admission);
        Ok::<_, StoreError>(Arc::new(tokio::sync::Mutex::new(runtime)))
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
    let persistence = {
        let mut runtime = workspace.lock().await;
        runtime.evicted = true;
        runtime.persistence.take()
    };
    drop(workspace);
    if let Some(persistence) = persistence {
        persistence.close().await;
    }
}

/// One reserved initialization per registry/ID; reservations count against the
/// workspace cap. The owner retains the registry Arc (and therefore its identity)
/// until publication, even when every initial waiter disconnects.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the opened runtime is moved into the registry under its lock; the lint does not see the move"
)]
pub(crate) async fn get_or_initialize(
    workspaces: &WorkspaceRegistry,
    state_root: std::path::PathBuf,
    payloads: &Arc<dyn crate::payloads::ShellPayloads>,
    workspace_id: WorkspaceId,
    pinned: bool,
    admission: AdmissionMode,
    capacity: usize,
) -> Result<SharedWorkspace, StoreError> {
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
            let payloads = payloads.clone();
            tokio::spawn(async move {
                let opened =
                    open_runtime(state_root, payloads, workspace_id, pinned, admission).await;
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

/// Admit `peer` to `workspace` for `operation`, enrolling a same-owner
/// device first. Membership comes from the same reload as every routing
/// read, so a pending or failed replacement's ceiling bounds it too; the
/// enrolment is the serialized async admission, never filesystem work under
/// the routing lock.
///
/// # Errors
///
/// The store's error when membership cannot be read or the enrolment cannot
/// be persisted; `Ok(Err(reason))` when the device is refused.
pub async fn admit_for(
    workspace: &SharedWorkspace,
    trust: &crate::workspace_access::RelayTrust<'_>,
    peer: &crate::auth::AuthenticatedPeer,
    operation: crate::workspace_access::WorkspaceOperation,
) -> Result<Result<(), crate::workspace_access::DenyReason>, StoreError> {
    use crate::workspace_access::{Decision, DenyReason, decide};
    let (decision, revision) = {
        let runtime = fresh_membership(workspace).await?;
        (
            decide(&runtime, trust, peer, operation),
            runtime.membership_revision,
        )
    };
    match decision {
        Decision::Admit => Ok(Ok(())),
        Decision::Deny(reason) => Ok(Err(reason)),
        Decision::AdmitAndEnrol => {
            enrol(workspace, &peer.cert, revision).await?;
            let runtime = fresh_membership(workspace).await?;
            Ok(match decide(&runtime, trust, peer, operation) {
                Decision::Admit => Ok(()),
                Decision::AdmitAndEnrol => Err(DenyReason::NotAMember),
                Decision::Deny(reason) => Err(reason),
            })
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod tests {
    use super::*;
    use crate::store::test_barrier;

    fn runtime(root: &std::path::Path) -> SharedWorkspace {
        // A workspace whose membership is kept: its access record, the one
        // projection of who is a member.
        WorkspaceStore::open(root, WorkspaceId::from_u128(7), false)
            .unwrap()
            .save_governed_access(&crate::store::GovernedAccessRecord::default())
            .unwrap();
        Arc::new(tokio::sync::Mutex::new(
            WorkspaceRuntime::open(root, WorkspaceId::from_u128(7), false, Membership::default()).unwrap(),
        ))
    }

    #[tokio::test]
    async fn admission_authorized_before_replacement_cannot_resurrect_removed_member() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        let revision = workspace.lock().await.membership_revision;
        replace_membership(&workspace, vec![[1; 32]]).await.unwrap();
        assert!(
            admit_member(&workspace, [2; 32], revision).await.is_err(),
            "stale admission resurrected authority after replacement"
        );
        assert_eq!(
            fresh_membership(&workspace)
                .await
                .unwrap()
                .membership
                .devices,
            vec![[1; 32]]
        );
    }

    #[tokio::test]
    async fn admission_commit_survives_caller_cancellation_without_early_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        let (release, reached) =
            test_barrier::install(&tmp.path().join("workspaces/7"), "membership-write");
        let worker = workspace.clone();
        let caller = tokio::spawn(async move { admit_member(&worker, [1; 32], 0).await });
        reached.await.unwrap();
        assert!(!workspace.lock().await.membership.contains(&[1; 32]));
        caller.abort();
        drop(release);
        // Fresh read waits for the detached admitted write, then sees its commit.
        assert_eq!(
            fresh_membership(&workspace)
                .await
                .unwrap()
                .membership
                .devices,
            vec![[1; 32]]
        );
    }

    #[tokio::test]
    async fn stale_read_and_replacement_serialize_without_resurrecting_removals() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        replace_membership(&workspace, vec![[1; 32], [2; 32]])
            .await
            .unwrap();
        let root = tmp.path().join("workspaces/7");
        let (release_read, read_reached) = test_barrier::install(&root, "membership-read");
        let reader_workspace = workspace.clone();
        let reader = tokio::spawn(async move {
            fresh_membership(&reader_workspace)
                .await
                .unwrap()
                .membership
                .devices
                .clone()
        });
        read_reached.await.unwrap();
        let (release_write, write_reached) = test_barrier::install(&root, "membership-write");
        let writer_workspace = workspace.clone();
        let writer = tokio::spawn(async move {
            replace_membership(&writer_workspace, vec![[2; 32], [3; 32]]).await
        });
        drop(release_read);
        assert_eq!(reader.await.unwrap(), vec![[1; 32], [2; 32]]);
        write_reached.await.unwrap();
        assert_eq!(
            workspace.lock().await.membership.devices,
            vec![[2; 32]],
            "pending removals must deny; pending additions must not grant"
        );
        writer.abort();
        drop(release_write);
        assert_eq!(
            fresh_membership(&workspace)
                .await
                .unwrap()
                .membership
                .devices,
            vec![[2; 32], [3; 32]]
        );
    }

    #[tokio::test]
    async fn failed_replacement_keeps_removals_denied_and_additions_ungranted() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        replace_membership(&workspace, vec![[1; 32], [2; 32]])
            .await
            .unwrap();
        let path = tmp
            .path()
            .join("workspaces/7")
            .join(crate::store::GOVERNED_ACCESS_FILE);
        let saved = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            replace_membership(&workspace, vec![[2; 32], [3; 32]])
                .await
                .is_err()
        );
        assert_eq!(workspace.lock().await.membership.devices, vec![[2; 32]]);
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, saved).unwrap();
        assert_eq!(
            fresh_membership(&workspace)
                .await
                .unwrap()
                .membership
                .devices,
            vec![[2; 32]]
        );
        replace_membership(&workspace, vec![[1; 32], [3; 32]])
            .await
            .unwrap();
        assert_eq!(
            fresh_membership(&workspace)
                .await
                .unwrap()
                .membership
                .devices,
            vec![[1; 32], [3; 32]]
        );
    }

    #[tokio::test]
    async fn failed_replacement_cannot_restore_uncached_external_members() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        let store = workspace.lock().await.store.clone();
        store.save_membership(&[[1; 32], [2; 32]]).unwrap();
        // This external update has not yet reached the cached routing metadata.
        assert!(workspace.lock().await.membership.devices.is_empty());
        let path = store.root().join(crate::store::GOVERNED_ACCESS_FILE);
        let saved = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            replace_membership(&workspace, vec![[2; 32], [3; 32]])
                .await
                .is_err()
        );
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, saved).unwrap();
        assert_eq!(
            fresh_membership(&workspace)
                .await
                .unwrap()
                .membership
                .devices,
            vec![[2; 32]],
            "failed replacement must bound even previously uncached authority"
        );
    }

    /// Fresh reads until the started membership watch vouches for the workspace.
    async fn warm(workspace: &SharedWorkspace) {
        tilo_test_env::within("the membership watch to vouch for the room", async {
            while !fresh_membership(workspace)
                .await
                .unwrap()
                .membership_is_current()
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    async fn wait_for_members(workspace: &SharedWorkspace, expected: &[[u8; 32]]) {
        tilo_test_env::within("the external membership edit to be observed", async {
            while fresh_membership(workspace)
                .await
                .unwrap()
                .membership
                .devices
                != expected
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn fresh_membership_cache_hit_does_not_read_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        let store = workspace.lock().await.store.clone();
        store.save_membership(&[[1; 32]]).unwrap();
        let watch = crate::membership_watch::spawn_membership_watch(tmp.path());
        warm(&workspace).await;
        let (_release, mut reached) = test_barrier::install(store.root(), "membership-read");
        let devices = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            fresh_membership(&workspace)
                .await
                .map(|runtime| runtime.membership.devices.clone())
        })
        .await
        .expect("an unchanged room must not wait for its membership file")
        .unwrap();
        assert_eq!(devices, vec![[1; 32]]);
        assert!(
            reached.try_recv().is_err(),
            "an unchanged room read its membership file"
        );
        watch.abort();
    }

    #[tokio::test]
    async fn external_membership_edit_is_observed_after_watch_event() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        let watch = crate::membership_watch::spawn_membership_watch(tmp.path());
        warm(&workspace).await;
        // `tilo-relay-admin` edits the file from another process.
        let external = WorkspaceStore::open(tmp.path(), WorkspaceId::from_u128(7), false).unwrap();
        external.save_membership(&[[1; 32], [2; 32]]).unwrap();
        wait_for_members(&workspace, &[[1; 32], [2; 32]]).await;
        external.save_membership(&[[2; 32]]).unwrap();
        wait_for_members(&workspace, &[[2; 32]]).await;
        watch.abort();
    }

    #[tokio::test]
    async fn generation_changed_during_read_reloads_again() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        let store = workspace.lock().await.store.clone();
        store.save_membership(&[[1; 32]]).unwrap();
        let watch = crate::membership_watch::spawn_membership_watch(tmp.path());
        warm(&workspace).await;
        let generation = workspace.lock().await.disk_generation.clone().unwrap();
        let read = |workspace: &SharedWorkspace| {
            let workspace = workspace.clone();
            tokio::spawn(async move {
                fresh_membership(&workspace)
                    .await
                    .unwrap()
                    .membership
                    .devices
                    .clone()
            })
        };

        let (release, reached) = test_barrier::install(store.root(), "membership-read");
        generation.bump();
        let reader = read(&workspace);
        tilo_test_env::within("the changed room to read its file", reached)
            .await
            .unwrap();
        // Another change lands while that read is in flight.
        generation.bump();
        drop(release);
        assert_eq!(reader.await.unwrap(), vec![[1; 32]]);
        assert!(!workspace.lock().await.membership_is_current());

        let (release, reached) = test_barrier::install(store.root(), "membership-read");
        let reader = read(&workspace);
        tilo_test_env::within("the next authorization to read the file again", reached)
            .await
            .unwrap();
        drop(release);
        assert_eq!(reader.await.unwrap(), vec![[1; 32]]);
        assert!(workspace.lock().await.membership_is_current());
        watch.abort();
    }

    #[tokio::test]
    async fn compute_attach_respects_membership_ceiling() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        replace_membership(&workspace, vec![[1; 32], [2; 32]])
            .await
            .unwrap();
        let path = tmp
            .path()
            .join("workspaces/7")
            .join(crate::store::GOVERNED_ACCESS_FILE);
        let saved = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            replace_membership(&workspace, vec![[2; 32], [3; 32]])
                .await
                .is_err()
        );
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, saved).unwrap();
        // The old file still lists [1; 32]; the failed replacement removed it.
        let owner = [9; 32];
        let revocations = std::sync::RwLock::new(tilo_user_identity::RevocationList::new());
        let policy = std::sync::RwLock::new(crate::policy::TrustPolicy::AllowAny);
        let agents = crate::peer::AgentRegistry::default();
        let trust = crate::workspace_access::RelayTrust {
            revocations: &revocations,
            policy: &policy,
            own_user: &owner,
            agents: &agents,
            environments: Box::leak(Box::default()),
        };
        let removed = crate::workspace_access::tests::unsigned_peer(owner, [1; 32]);
        for mode in [AdmissionMode::Explicit, AdmissionMode::SameOwner] {
            // Not even a same-owner workspace enrols past the ceiling.
            workspace.lock().await.admission = mode;
            let admission = admit_for(
                &workspace,
                &trust,
                &removed,
                crate::workspace_access::WorkspaceOperation::Compute,
            )
            .await
            .unwrap();
            assert_eq!(
                admission,
                Err(crate::workspace_access::DenyReason::NotAMember),
                "a removed device attached as a compute peer"
            );
            assert_eq!(workspace.lock().await.membership.devices, vec![[2; 32]]);
        }
    }

    #[tokio::test]
    async fn compute_attach_does_not_enrol_cross_user_device() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        workspace.lock().await.admission = AdmissionMode::SameOwner;
        let owner = [9; 32];
        // The handshake admits anyone: only the workspace may keep a stranger out.
        let revocations = std::sync::RwLock::new(tilo_user_identity::RevocationList::new());
        let policy = std::sync::RwLock::new(crate::policy::TrustPolicy::AllowAny);
        let agents = crate::peer::AgentRegistry::default();
        let trust = crate::workspace_access::RelayTrust {
            revocations: &revocations,
            policy: &policy,
            own_user: &owner,
            agents: &agents,
            environments: Box::leak(Box::default()),
        };
        let stranger = crate::workspace_access::tests::unsigned_peer([8; 32], [1; 32]);
        assert_eq!(
            admit_for(
                &workspace,
                &trust,
                &stranger,
                crate::workspace_access::WorkspaceOperation::Compute
            )
            .await
            .unwrap(),
            Err(crate::workspace_access::DenyReason::NotAMember)
        );
        assert!(
            fresh_membership(&workspace)
                .await
                .unwrap()
                .membership
                .devices
                .is_empty(),
            "a cross-user device was enrolled through compute"
        );
        let own = crate::workspace_access::tests::unsigned_peer(owner, [2; 32]);
        assert_eq!(
            admit_for(
                &workspace,
                &trust,
                &own,
                crate::workspace_access::WorkspaceOperation::Compute
            )
            .await
            .unwrap(),
            Ok(())
        );
        assert_eq!(
            workspace.lock().await.store.load_membership().unwrap(),
            vec![[2; 32]],
            "the owner's device is enrolled durably"
        );
    }

    #[tokio::test]
    async fn opening_reservation_counts_toward_limit_and_survives_waiter_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let root = tmp.path().to_path_buf();
        let (release, reached) = test_barrier::install(&root.join("workspaces/7"), "open");
        let first_registry = registry.clone();
        let first_root = root.clone();
        let first = tokio::spawn(async move {
            get_or_initialize(
                &first_registry,
                first_root,
                &crate::payloads::neighbour(),
                WorkspaceId::from_u128(7),
                false,
                AdmissionMode::Explicit,
                1,
            )
            .await
        });
        reached.await.unwrap();
        assert!(
            get_or_initialize(
                &registry,
                root.clone(),
                &crate::payloads::neighbour(),
                WorkspaceId::from_u128(8),
                false,
                AdmissionMode::Explicit,
                1
            )
            .await
            .is_err()
        );
        first.abort();
        drop(release);
        let workspace = get_or_initialize(
            &registry,
            root,
            &crate::payloads::neighbour(),
            WorkspaceId::from_u128(7),
            false,
            AdmissionMode::Explicit,
            1,
        )
        .await
        .unwrap();
        assert!(Arc::ptr_eq(
            &registry.lock().await[&WorkspaceId::from_u128(7)],
            &workspace
        ));
        assert_eq!(registry.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn initialization_failure_is_shared_and_releases_reservation_for_retry() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("workspaces")).unwrap();
        std::fs::write(tmp.path().join("workspaces/7"), b"not a directory").unwrap();
        let registry = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let root = tmp.path().to_path_buf();
        let (release, reached) = test_barrier::install(&root.join("workspaces/7"), "open");
        let first_registry = registry.clone();
        let first_root = root.clone();
        let first = tokio::spawn(async move {
            get_or_initialize(
                &first_registry,
                first_root,
                &crate::payloads::neighbour(),
                WorkspaceId::from_u128(7),
                false,
                AdmissionMode::Explicit,
                1,
            )
            .await
        });
        reached.await.unwrap();
        let payloads = crate::payloads::neighbour();
        let mut second = Box::pin(get_or_initialize(
            &registry,
            root.clone(),
            &payloads,
            WorkspaceId::from_u128(7),
            false,
            AdmissionMode::Explicit,
            1,
        ));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                second.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        drop(release);
        let first_error = first.await.unwrap().err().unwrap().to_string();
        assert_eq!(second.await.err().unwrap().to_string(), first_error);
        assert!(registry.lock().await.is_empty());
        std::fs::remove_file(tmp.path().join("workspaces/7")).unwrap();
        assert!(
            get_or_initialize(
                &registry,
                root,
                &crate::payloads::neighbour(),
                WorkspaceId::from_u128(7),
                false,
                AdmissionMode::Explicit,
                1
            )
            .await
            .is_ok()
        );
    }
}
