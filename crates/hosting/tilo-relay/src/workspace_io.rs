//! Filesystem authority work is independent of state persistence. Admission is
//! bounded before spawning work; one authorization lane serializes each room's
//! reads and writes. No routing or registry guard crosses filesystem work.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{MutexGuard, OwnedSemaphorePermit, Semaphore, watch};

use crate::peer::WorkspaceRegistry;
use crate::store::{GovernedAccessRecord, StoreError, WorkspaceStore};
use crate::workspace::{Membership, SharedWorkspace, WorkspaceRuntime};

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

fn admit_work() -> Result<OwnedSemaphorePermit, StoreError> {
    authorization_slots()
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            if crate::persistence::correlated_diagnostics() {
                tracing::info!(
                    phase = "authorization_backpressure",
                    capacity = AUTHORIZATION_CAPACITY
                );
            }
            io_error("workspace authorization backpressure")
        })
}

#[derive(Clone, Copy)]
struct MembershipTrace {
    request_id: uuid::Uuid,
    workspace_id: u64,
}

fn membership_phase(trace: Option<MembershipTrace>, phase: &'static str) {
    if crate::persistence::diagnostics()
        && let Some(trace) = trace
    {
        tracing::info!(
            request_id = %trace.request_id,
            workspace_id = trace.workspace_id,
            phase,
            timestamp_ns = tilo_clock::unix_now_ns()
        );
    }
}

async fn authorization_io<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
    trace: Option<MembershipTrace>,
) -> Result<T, StoreError> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    membership_phase(trace, "delivery_membership_worker_wait");
    let permit = WORKERS
        .get_or_init(|| Arc::new(Semaphore::new(AUTHORIZATION_WORKERS)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| io_error(error.to_string()))?;
    membership_phase(trace, "delivery_membership_worker_admitted");
    // The permit stays in blocking work even when the waiting caller cancels.
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        membership_phase(trace, "delivery_membership_read_started");
        let result = operation();
        membership_phase(
            trace,
            if result.is_ok() {
                "delivery_membership_read_returned"
            } else {
                "delivery_membership_read_returned_error"
            },
        );
        result
    })
    .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            membership_phase(trace, "delivery_membership_worker_join_error");
            return Err(io_error(error.to_string()));
        }
    };
    membership_phase(trace, "delivery_membership_worker_joined");
    result
}

async fn authority_handles(
    workspace: &SharedWorkspace,
) -> (WorkspaceStore, Arc<tokio::sync::Mutex<()>>) {
    let runtime = workspace.lock().await;
    (runtime.store.clone(), runtime.authorization.clone())
}

/// Fresh external-file membership read. Callers must recheck certificate,
/// policy, room and exact sessions using the returned routing guard. The lane
/// is released while that guard remains held, so a newer update cannot be
/// overwritten by this read or slip between this read and the caller's check.
pub(crate) async fn fresh_membership(
    workspace: &SharedWorkspace,
) -> Result<MutexGuard<'_, WorkspaceRuntime>, StoreError> {
    fresh_membership_inner(workspace, None).await
}

pub(crate) async fn fresh_membership_for_delivery(
    workspace: &SharedWorkspace,
    workspace_id: u64,
    request_id: uuid::Uuid,
) -> Result<MutexGuard<'_, WorkspaceRuntime>, StoreError> {
    fresh_membership_inner(
        workspace,
        Some(MembershipTrace {
            request_id,
            workspace_id,
        }),
    )
    .await
}

async fn fresh_membership_inner(
    workspace: &SharedWorkspace,
    trace: Option<MembershipTrace>,
) -> Result<MutexGuard<'_, WorkspaceRuntime>, StoreError> {
    let _slot = admit_work()?;
    membership_phase(trace, "delivery_membership_handles_wait");
    let (store, lane) = authority_handles(workspace).await;
    membership_phase(trace, "delivery_membership_handles_acquired");
    membership_phase(trace, "delivery_membership_lane_wait");
    #[cfg(test)]
    let _lane = if trace.is_some() {
        use std::{future::Future as _, task::Poll};

        let root = store.root().to_path_buf();
        let mut waiting = Box::pin(lane.lock_owned());
        std::future::poll_fn(move |cx| match waiting.as_mut().poll(cx) {
            Poll::Ready(guard) => Poll::Ready(guard),
            Poll::Pending => {
                crate::store::test_barrier::signal(&root, "delivery-membership-lane-pending");
                Poll::Pending
            }
        })
        .await
    } else {
        lane.lock_owned().await
    };
    #[cfg(not(test))]
    let _lane = lane.lock_owned().await;
    membership_phase(trace, "delivery_membership_lane_acquired");
    let mut members = authorization_io(move || store.load_membership(), trace).await?;
    membership_phase(trace, "delivery_membership_runtime_wait");
    let mut runtime = workspace.lock().await;
    membership_phase(trace, "delivery_membership_runtime_acquired");
    if let Some(ceiling) = &runtime.membership_ceiling {
        members.retain(|device| ceiling.contains(device));
    }
    runtime.membership.set(members);
    Ok(runtime)
}

/// Persist an explicitly authorized admission before granting it. The detached,
/// bounded owner finishes a started update even if its caller disconnects.
pub(crate) async fn admit_member(
    workspace: &SharedWorkspace,
    device: [u8; 32],
    expected_revision: u64,
) -> Result<(), StoreError> {
    let slot = admit_work()?;
    let workspace = workspace.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let (store, lane) = authority_handles(&workspace).await;
        let _lane = lane.lock_owned().await;
        let reader = store.clone();
        let mut members = authorization_io(move || reader.load_membership(), None).await?;
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
        authorization_io(move || store.save_membership(&saved), None).await?;
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
pub(crate) async fn replace_membership(
    workspace: &SharedWorkspace,
    members: Vec<[u8; 32]>,
) -> Result<(), StoreError> {
    let slot = admit_work()?;
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
        let saved = members.clone();
        authorization_io(move || store.save_membership(&saved), None).await?;
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
/// durable projection. Removals narrow the live room before filesystem work;
/// additions appear only after the atomic record commits. A failed write
/// therefore leaves a fail-closed in-process ceiling until a later successful
/// replacement.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the runtime guard publishes the narrowed ceiling as one step and ends with the task"
)]
pub(crate) async fn replace_governed_access(
    workspace: &SharedWorkspace,
    access: GovernedAccessRecord,
) -> Result<(), StoreError> {
    let slot = admit_work()?;
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
            let requested_devices: std::collections::HashSet<_> =
                access.member_devices.iter().copied().collect();
            if let Some(ceiling) = &mut runtime.membership_ceiling {
                ceiling.retain(|device| requested_devices.contains(device));
            } else {
                runtime.membership_ceiling = Some(requested_devices);
            }
            let removed: Vec<[u8; 32]> = runtime
                .membership
                .devices
                .iter()
                .copied()
                .filter(|device| !access.member_devices.contains(device))
                .collect();
            runtime
                .membership
                .devices
                .retain(|device| access.member_devices.contains(device));
            runtime
                .governed_user_roots
                .retain(|root| access.member_user_roots.contains(root));
            runtime.governed_access = true;
            runtime.admit_authenticated = false;
            // Refuse while the authority lane is held so the writer can deliver
            // NotAuthorised before a waiting access audit closes the session.
            runtime.refuse_removed_live_members(&removed);
        }
        let saved = access.clone();
        authorization_io(move || store.save_governed_access(&saved), None).await?;
        let mut runtime = workspace.lock().await;
        runtime.membership.set(access.member_devices);
        runtime.governed_user_roots = access.member_user_roots.into_iter().collect();
        runtime.governed_access = true;
        runtime.admit_authenticated = false;
        runtime.membership_ceiling = None;
        Ok(())
    })
    .await
    .map_err(|error| io_error(error.to_string()))?
}

type OpenResult = Result<SharedWorkspace, String>;
type Opening = watch::Receiver<Option<OpenResult>>;
type ReservationKey = (usize, u64);
fn reservations() -> &'static Mutex<HashMap<ReservationKey, Opening>> {
    static RESERVATIONS: OnceLock<Mutex<HashMap<ReservationKey, Opening>>> = OnceLock::new();
    RESERVATIONS.get_or_init(Mutex::default)
}

async fn open_runtime(
    state_root: std::path::PathBuf,
    workspace_id: u64,
    pinned: bool,
    admit_authenticated: bool,
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
        let mut runtime =
            WorkspaceRuntime::open(&state_root, workspace_id, pinned, Membership::default())?;
        runtime.admit_authenticated = admit_authenticated;
        Ok::<_, StoreError>(Arc::new(tokio::sync::Mutex::new(runtime)))
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

/// One reserved initialization per registry/ID; reservations count against the
/// room cap. The owner retains the registry Arc (and therefore its identity)
/// until publication, even when every initial waiter disconnects.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the opened runtime is moved into the registry under its lock; the lint does not see the move"
)]
pub(crate) async fn get_or_initialize(
    workspaces: &WorkspaceRegistry,
    state_root: std::path::PathBuf,
    workspace_id: u64,
    pinned: bool,
    admit_authenticated: bool,
    capacity: usize,
) -> Result<SharedWorkspace, StoreError> {
    let registry_id = Arc::as_ptr(workspaces) as usize;
    let key = (registry_id, workspace_id);
    let mut result = {
        let map = workspaces.lock().await;
        if let Some(workspace) = map.get(&workspace_id) {
            return Ok(workspace.clone());
        }
        let mut pending = reservations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                let opened =
                    open_runtime(state_root, workspace_id, pinned, admit_authenticated).await;
                let mut map = registry.lock().await;
                let opened =
                    opened.map(|workspace| map.entry(workspace_id).or_insert(workspace).clone());
                sender.send_replace(Some(opened));
                reservations()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&key);
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

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod tests {
    use super::*;
    use crate::store::test_barrier;

    fn runtime(root: &std::path::Path) -> SharedWorkspace {
        Arc::new(tokio::sync::Mutex::new(
            WorkspaceRuntime::open(root, 7, false, Membership::default()).unwrap(),
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
        let path = tmp.path().join("workspaces/7/membership.json");
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
        let path = store.root().join("membership.json");
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

    #[tokio::test]
    async fn opening_reservation_counts_toward_limit_and_survives_waiter_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let root = tmp.path().to_path_buf();
        let (release, reached) = test_barrier::install(&root.join("workspaces/7"), "open");
        let first_registry = registry.clone();
        let first_root = root.clone();
        let first = tokio::spawn(async move {
            get_or_initialize(&first_registry, first_root, 7, false, false, 1).await
        });
        reached.await.unwrap();
        assert!(
            get_or_initialize(&registry, root.clone(), 8, false, false, 1)
                .await
                .is_err()
        );
        first.abort();
        drop(release);
        let workspace = get_or_initialize(&registry, root, 7, false, false, 1)
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&registry.lock().await[&7], &workspace));
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
            get_or_initialize(&first_registry, first_root, 7, false, false, 1).await
        });
        reached.await.unwrap();
        let mut second = Box::pin(get_or_initialize(
            &registry,
            root.clone(),
            7,
            false,
            false,
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
            get_or_initialize(&registry, root, 7, false, false, 1)
                .await
                .is_ok()
        );
    }
}
