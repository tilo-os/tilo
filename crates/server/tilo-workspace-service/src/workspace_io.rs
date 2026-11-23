//! Filesystem authority work is independent of state persistence. Each
//! workspace orders its own access-file work ([`AccessTurns`]): writes take
//! turns, and a read never waits for one. No routing or registry guard crosses
//! filesystem work, and no workspace's file work waits for another's.
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{MutexGuard, OwnedMutexGuard, Semaphore, watch};

use crate::membership_watch::DiskGeneration;
use crate::registry::WorkspaceRegistry;
use crate::workspace::{Membership, SharedWorkspace, WorkspaceRuntime};
use crate::workspace_access::AdmissionMode;
use crate::workspace_files::{FilesError, GovernedAccessRecord, WorkspaceFiles};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

const OPEN_WORKERS: usize = 4;

fn io_error(message: impl Into<String>) -> FilesError {
    FilesError::Io(std::io::Error::other(message.into()))
}

/// The order of one workspace's access-file work. Writes take turns. A read
/// that needs the file runs only while no write does; while one does, the read
/// answers from memory, where the write narrowed its removals before touching
/// the file. So an admission never waits for an access write's disk work, here
/// or in another workspace.
///
/// Governance's access is projected to the file behind its answers
/// ([`replace_governed_access`]): the latest projection waits in `projection`
/// and one writer at a time writes it, so the file follows governance in order
/// and only ever lags it by additions.
#[derive(Clone)]
pub(crate) struct AccessTurns {
    writes: Arc<tokio::sync::Mutex<()>>,
    file: Arc<tokio::sync::Mutex<()>>,
    /// Writes under way or waiting: reads answer from memory while any is.
    writing: Arc<AtomicUsize>,
    projection: Arc<Mutex<Projection>>,
    written: Arc<watch::Sender<Written>>,
}

impl Default for AccessTurns {
    fn default() -> Self {
        Self {
            writes: Arc::default(),
            file: Arc::default(),
            writing: Arc::default(),
            projection: Arc::default(),
            written: Arc::new(watch::channel(Written::default()).0),
        }
    }
}

/// Governance's access waiting for the file.
#[derive(Default)]
struct Projection {
    /// Projections queued so far, the waiting one's number.
    queued: u64,
    waiting: Option<GovernedAccessRecord>,
    /// Whether a writer is writing them.
    writer: bool,
}

/// What the file holds of governance's projections: every one up to
/// `through`, unless that write failed.
#[derive(Clone, Default)]
struct Written {
    through: u64,
    failed: Option<String>,
}

impl AccessTurns {
    /// Wait for the writes before this one, then for a read of the file in
    /// flight, and mark the file written. The returned file guard keeps reads
    /// off the file until the caller has narrowed what they answer from
    /// memory.
    async fn write(&self) -> (WriteTurn, OwnedMutexGuard<()>) {
        let writes = self.writes.clone().lock_owned().await;
        let file = self.file.clone().lock_owned().await;
        self.writing.fetch_add(1, Ordering::AcqRel);
        (
            WriteTurn {
                writing: self.writing.clone(),
                _writes: writes,
            },
            file,
        )
    }

    /// A read of the file's turn, or `None` while a write runs: the read
    /// answers from memory then.
    async fn read(&self) -> Option<OwnedMutexGuard<()>> {
        let file = self.file.clone().lock_owned().await;
        (!self.writing()).then_some(file)
    }

    /// Whether a write is under way or waiting, so the file may not hold what
    /// memory does.
    pub(crate) fn writing(&self) -> bool {
        self.writing.load(Ordering::Acquire) != 0
    }

    /// Wait until the writes started so far have ended.
    #[cfg(test)]
    pub(crate) async fn settled(&self) {
        let queued = self.projection.lock_recover().queued;
        let _ = self.projected(queued).await;
        drop(self.write().await);
    }

    /// Queue `access` for the file, under the runtime's lock that published
    /// it. Answers its number, and whether the caller starts the writer.
    fn queue(&self, access: GovernedAccessRecord) -> (u64, bool) {
        let mut projection = self.projection.lock_recover();
        projection.queued = projection.queued.saturating_add(1);
        projection.waiting = Some(access);
        let start = !projection.writer;
        if start {
            projection.writer = true;
            self.writing.fetch_add(1, Ordering::AcqRel);
        }
        (projection.queued, start)
    }

    /// Wait until the file holds projection `queued`.
    async fn projected(&self, queued: u64) -> Result<(), FilesError> {
        let mut written = self.written.subscribe();
        let written = written
            .wait_for(|written| written.through >= queued)
            .await
            .map_err(|error| io_error(error.to_string()))?;
        written
            .failed
            .clone()
            .map_or(Ok(()), |error| Err(io_error(error)))
    }
}

/// A write's turn: reads answer from memory until it drops.
struct WriteTurn {
    writing: Arc<AtomicUsize>,
    _writes: OwnedMutexGuard<()>,
}

impl Drop for WriteTurn {
    fn drop(&mut self) {
        self.writing.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Write governance's queued projections to `workspace`'s file, the latest
/// each time, until none waits.
async fn project(workspace: SharedWorkspace, store: WorkspaceFiles, turns: AccessTurns) {
    let (_turn, file) = turns.write().await;
    drop(file);
    loop {
        let (queued, access) = {
            let mut projection = turns.projection.lock_recover();
            let Some(access) = projection.waiting.take() else {
                projection.writer = false;
                turns.writing.fetch_sub(1, Ordering::AcqRel);
                return;
            };
            (projection.queued, access)
        };
        let file_store = store.clone();
        let waiting = crate::timing::start();
        let result = authorization_io(move || {
            crate::timing::event("workspace.settle.access_thread", waiting, 0);
            let saving = crate::timing::start();
            let (digest, written) = file_store.save_governed_access(&access)?;
            crate::timing::event("workspace.settle.access_write", saving, written);
            // Applied to the runtime already, its narrowing announced: the
            // watch need not announce the file again.
            crate::membership_watch::wrote_own(
                &file_store
                    .root()
                    .join(crate::workspace_files::GOVERNED_ACCESS_FILE),
                digest,
            );
            Ok(())
        })
        .await;
        if result.is_ok() {
            let mut runtime = workspace.lock().await;
            // The file holds what memory does: a later read of it can no
            // longer bring back a removed device, unless a newer projection
            // still waits.
            if turns.projection.lock_recover().queued == queued {
                runtime.membership_ceiling = None;
            }
        }
        if let Err(error) = &result {
            tracing::error!(%error, "a workspace's access was not written; its removals stay refused here");
        }
        turns.written.send_replace(Written {
            through: queued,
            failed: result.err().map(|error| error.to_string()),
        });
    }
}

async fn authorization_io<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, FilesError> + Send + 'static,
) -> Result<T, FilesError> {
    match tokio::task::spawn_blocking(operation).await {
        Ok(result) => result,
        Err(error) => Err(io_error(error.to_string())),
    }
}

async fn authority_handles(workspace: &SharedWorkspace) -> (WorkspaceFiles, AccessTurns) {
    let runtime = workspace.lock().await;
    (runtime.files.clone(), runtime.authorization.clone())
}

/// The handles a membership reload needs, attaching the workspace to the daemon's
/// membership watch when one now covers it (the runtime may have opened
/// before the watch started, or outlived a stopped one).
async fn reload_handles(
    workspace: &SharedWorkspace,
) -> (WorkspaceFiles, AccessTurns, Option<DiskGeneration>) {
    let mut runtime = workspace.lock().await;
    let generation = watched_generation(&mut runtime);
    (
        runtime.files.clone(),
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
        runtime.disk_generation = crate::membership_watch::attach(runtime.files.root());
    }
    runtime.disk_generation.clone()
}

/// Current membership: in memory, reloaded from the external file only after
/// the daemon's membership watch saw it change (or on every call while no live
/// watch covers the workspace). Callers must recheck certificate, policy, workspace and
/// exact sessions using the returned routing guard. A reload holds the file's
/// turn ([`AccessTurns`]) until it published what it read under that guard, so
/// no write's commit can be overwritten by this read or slip between this read
/// and the caller's check. In-process writers narrow the in-memory set under
/// the routing lock before any filesystem work and widen it only after the file
/// holds their change, so a read answers from memory while one writes: it never
/// waits for a write's disk work.
///
/// Only device membership is reloaded. `governed_user_roots` and
/// the governed admission mode change through `replace_governed_access` and the
/// runtime's open, as before the watch: an external edit of the governed file
/// narrows or widens the exact devices, not the handshake roots.
pub async fn fresh_membership(
    workspace: &SharedWorkspace,
) -> Result<MutexGuard<'_, WorkspaceRuntime>, FilesError> {
    {
        let locking = crate::timing::start();
        let runtime = workspace.lock().await;
        crate::timing::window("workspace.lock", locking);
        if runtime.membership_is_current() {
            // No disk IO, no worker, no turn.
            return Ok(runtime);
        }
    }
    let reloading = crate::timing::start();
    let reloaded = reload_membership(workspace).await;
    crate::timing::window("workspace.membership_reload", reloading);
    reloaded
}

async fn reload_membership(
    workspace: &SharedWorkspace,
) -> Result<MutexGuard<'_, WorkspaceRuntime>, FilesError> {
    let (store, turns, generation) = reload_handles(workspace).await;
    let Some(file) = turns.read().await else {
        // A write is under way: memory holds its removals and none of its
        // additions, and the next read after it loads the file again.
        return Ok(workspace.lock().await);
    };
    // Observed before the read: a change during it moves the generation on,
    // and the next call reads again.
    let observed = generation.as_ref().and_then(DiskGeneration::current);
    if observed.is_some() {
        let runtime = workspace.lock().await;
        if runtime.loaded_generation == observed {
            // Another reload finished while this one waited for the file.
            return Ok(runtime);
        }
    }
    // The file's turn goes with the read, so a cancelled caller cannot let a
    // write in beside it.
    let (members, _file) = authorization_io(move || Ok((store.load_membership()?, file))).await?;
    let mut runtime = workspace.lock().await;
    // A projection queued during the read is in memory and not in what it
    // read.
    if !turns.writing() {
        apply_loaded_membership(&mut runtime, members, observed);
    }
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
/// workspace's membership: the governance of a workspace this server governs records the device
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
) -> Result<(), FilesError> {
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

/// Persist an explicitly authorized admission before granting it, after the
/// workspace's writes before it. The detached owner finishes a started update
/// even if its caller disconnects. The decision it persists was taken from
/// memory; the file is read here only to keep what another process wrote.
pub(crate) async fn admit_member(
    workspace: &SharedWorkspace,
    device: [u8; 32],
    expected_revision: u64,
) -> Result<(), FilesError> {
    let workspace = workspace.clone();
    tokio::spawn(async move {
        let (store, turns) = authority_handles(&workspace).await;
        let (_turn, file) = turns.write().await;
        // It narrows nothing: reads answer from memory at once.
        drop(file);
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

/// Replace the workspace's devices after the writes before it. Removals deny
/// as soon as this operation has its turn; additions grant only after commit.
/// A failed write keeps removals denied in this process until a successful
/// explicit replacement, so a later read of the old file cannot resurrect them.
#[cfg(test)]
pub(crate) async fn replace_membership(
    workspace: &SharedWorkspace,
    members: Vec<[u8; 32]>,
) -> Result<(), FilesError> {
    let workspace = workspace.clone();
    tokio::spawn(async move {
        let (store, turns) = authority_handles(&workspace).await;
        let (_turn, file) = turns.write().await;
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
        drop(file);
        // Links of removed devices re-check now, before the write.
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
/// durable projection of governance, which has already committed them.
/// Removals narrow the live workspace at once and are answered only once the
/// file holds them too, so no reader of the file, in this process or another,
/// admits a removed device afterwards; a failed write leaves a fail-closed
/// in-process ceiling until a later projection is written. A replacement that
/// removes nothing is answered as soon as memory admits its additions, and
/// the file follows: a join never waits for an access write. Returns the
/// devices the replacement no longer admits.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the runtime guard publishes the narrowed ceiling and queues its projection as one step"
)]
pub(crate) async fn replace_governed_access(
    workspace: &SharedWorkspace,
    access: GovernedAccessRecord,
    revoked: Vec<[u8; 32]>,
) -> Result<Vec<[u8; 32]>, FilesError> {
    let workspace = workspace.clone();
    tokio::spawn(async move {
        // The governor's projection: from here on governance owns who joins.
        let access = GovernedAccessRecord {
            governed: true,
            ..access
        };
        let revoked: std::collections::HashSet<[u8; 32]> = revoked.into_iter().collect();
        let (removed, queued, turns) = {
            let mut runtime = workspace.lock().await;
            // What the workspace keeps as revoked keys now: refused with how to approve another,
            // wherever they ask, and no longer a device access admits.
            runtime.revoked_keys.extend(revoked.iter().copied());
            runtime
                .revoked_keys
                .retain(|device| !access.member_devices.contains(device));
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
            let requested_roots: std::collections::HashSet<_> =
                access.member_user_roots.iter().collect();
            let roots_removed = runtime
                .governed_user_roots
                .iter()
                .any(|root| !requested_roots.contains(root));
            let governed_before = runtime.admission == AdmissionMode::Governed;
            let narrows = !removed.is_empty() || roots_removed || !governed_before;
            runtime.admission = AdmissionMode::Governed;
            if narrows {
                runtime
                    .membership
                    .devices
                    .retain(|device| requested_devices.contains(device));
                runtime
                    .governed_user_roots
                    .retain(|root| requested_roots.contains(root));
                if let Some(ceiling) = &mut runtime.membership_ceiling {
                    ceiling.retain(|device| requested_devices.contains(device));
                } else {
                    runtime.membership_ceiling = Some(requested_devices);
                }
                // The links of the devices it no longer admits end on the
                // announced change ([`crate::authority_epoch`]).
            } else {
                // Only additions: admitted now, the file following.
                runtime.membership.set(access.member_devices.clone());
                runtime.governed_user_roots = access.member_user_roots.iter().copied().collect();
                if runtime.membership_ceiling.is_some() {
                    runtime.membership_ceiling = Some(requested_devices);
                }
            }
            let turns = runtime.authorization.clone();
            let (queued, start) = turns.queue(access.clone());
            if start {
                tokio::spawn(project(
                    workspace.clone(),
                    runtime.files.clone(),
                    turns.clone(),
                ));
            }
            (narrows.then_some(removed), queued, turns)
        };
        let Some(removed) = removed else {
            return Ok(Vec::new());
        };
        // Sessions of removed devices and roots re-check now, before the
        // write. A replacement that only admits narrows nobody's access: a
        // public workspace's every admission woke every session it held.
        crate::authority_epoch::bump();
        turns.projected(queued).await?;
        let mut runtime = workspace.lock().await;
        runtime.membership.set(access.member_devices);
        runtime.governed_user_roots = access.member_user_roots.into_iter().collect();
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
    workspace_id: WorkspaceId,
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
        // Every workspace this host opens, it holds: its own writable copy.
        let mut runtime = WorkspaceRuntime::open(&state_root, workspace_id, Membership::default())?;
        // A governed record on disk keeps the workspace governed.
        runtime.admission = runtime.admission.rebind(admission);
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
    admission: AdmissionMode,
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
                let opened = open_runtime(state_root, workspace_id, admission).await;
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
/// first. Membership comes from the same reload as every routing read, so a
/// pending or failed replacement's ceiling bounds it too; the enrolment is
/// the serialized async admission, never filesystem work under the routing
/// lock.
///
/// # Errors
///
/// The store's error when membership cannot be read or the enrolment cannot
/// be persisted; `Ok(Err(reason))` when the device is refused.
pub async fn admit_for(
    workspace: &SharedWorkspace,
    trust: &crate::workspace_access::ServerTrust<'_>,
    peer: &crate::auth::AuthenticatedPeer,
    operation: crate::workspace_access::WorkspaceOperation,
) -> Result<Result<(), crate::workspace_access::DenyReason>, FilesError> {
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
    use crate::workspace_files::test_barrier;

    fn runtime(root: &std::path::Path) -> SharedWorkspace {
        runtime_of(root, 7)
    }

    fn runtime_of(root: &std::path::Path, id: u128) -> SharedWorkspace {
        // A workspace whose membership is kept: its access record, the one
        // projection of who is a member.
        WorkspaceFiles::open(root, WorkspaceId::from_u128(id))
            .unwrap()
            .save_governed_access(&crate::workspace_files::GovernedAccessRecord::default())
            .unwrap();
        Arc::new(tokio::sync::Mutex::new(
            WorkspaceRuntime::open(root, WorkspaceId::from_u128(id), Membership::default())
                .unwrap(),
        ))
    }

    /// Wait until the writes `workspace` has started have ended.
    async fn writes_ended(workspace: &SharedWorkspace) {
        let turns = workspace.lock().await.authorization.clone();
        turns.settled().await;
    }

    #[tokio::test]
    async fn admission_never_waits_for_an_access_write() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        replace_membership(&workspace, vec![[1; 32], [2; 32]])
            .await
            .unwrap();
        // Eight workspaces write their access at once, each held at its disk
        // step; the first drops [1] and adds [3].
        let mut releases = Vec::new();
        let mut pending = Vec::new();
        for id in 7..15 {
            let writer = if id == 7 {
                workspace.clone()
            } else {
                runtime_of(tmp.path(), id)
            };
            let root = writer.lock().await.files.root().to_path_buf();
            let (release, reached) = test_barrier::install(&root, "access-write");
            let member_devices = if id == 7 {
                vec![[2; 32], [3; 32]]
            } else {
                vec![[1; 32]]
            };
            pending.push(tokio::spawn(async move {
                replace_governed_access(
                    &writer,
                    GovernedAccessRecord {
                        member_devices,
                        member_user_roots: Vec::new(),
                        governed: true,
                    },
                    Vec::new(),
                )
                .await
            }));
            tilo_test_env::within("an access write to reach its disk step", reached)
                .await
                .unwrap();
            releases.push(release);
        }
        let other = runtime_of(tmp.path(), 15);
        tilo_test_env::within("joins while every access write is held", async {
            // Its own workspace answers from memory: the device the write
            // removes is refused and the one it adds not yet admitted.
            assert_eq!(
                fresh_membership(&workspace)
                    .await
                    .unwrap()
                    .membership
                    .devices,
                vec![[2; 32]]
            );
            // Another workspace reads its file and enrols a device.
            let revision = fresh_membership(&other).await.unwrap().membership_revision;
            admit_member(&other, [4; 32], revision).await.unwrap();
            assert_eq!(
                fresh_membership(&other).await.unwrap().membership.devices,
                vec![[4; 32]]
            );
        })
        .await;
        drop(releases);
        for write in pending {
            write.await.unwrap().unwrap();
        }
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
        let (release, reached) = test_barrier::install(
            &tmp.path()
                .join(format!("workspaces/{}", WorkspaceId::from_u128(7))),
            "membership-write",
        );
        let worker = workspace.clone();
        let caller = tokio::spawn(async move { admit_member(&worker, [1; 32], 0).await });
        reached.await.unwrap();
        assert!(!workspace.lock().await.membership.contains(&[1; 32]));
        caller.abort();
        drop(release);
        // The detached write ends, and a fresh read then sees its commit.
        writes_ended(&workspace).await;
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
        let root = tmp
            .path()
            .join(format!("workspaces/{}", WorkspaceId::from_u128(7)));
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
            fresh_membership(&workspace)
                .await
                .unwrap()
                .membership
                .devices,
            vec![[2; 32]],
            "pending removals must deny; pending additions must not grant"
        );
        writer.abort();
        drop(release_write);
        writes_ended(&workspace).await;
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
            .join(format!("workspaces/{}", WorkspaceId::from_u128(7)))
            .join(crate::workspace_files::GOVERNED_ACCESS_FILE);
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
        let store = workspace.lock().await.files.clone();
        store.save_membership(&[[1; 32], [2; 32]]).unwrap();
        // This external update has not yet reached the cached routing metadata.
        assert!(workspace.lock().await.membership.devices.is_empty());
        let path = store
            .root()
            .join(crate::workspace_files::GOVERNED_ACCESS_FILE);
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
        tilo_test_env::within("the membership watch to vouch for the workspace", async {
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
        let store = workspace.lock().await.files.clone();
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
        .expect("an unchanged workspace must not wait for its membership file")
        .unwrap();
        assert_eq!(devices, vec![[1; 32]]);
        assert!(
            reached.try_recv().is_err(),
            "an unchanged workspace read its membership file"
        );
        watch.abort();
    }

    #[tokio::test]
    async fn external_membership_edit_is_observed_after_watch_event() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = runtime(tmp.path());
        let watch = crate::membership_watch::spawn_membership_watch(tmp.path());
        warm(&workspace).await;
        // another process may edit the file.
        let external = WorkspaceFiles::open(tmp.path(), WorkspaceId::from_u128(7)).unwrap();
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
        let store = workspace.lock().await.files.clone();
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
        tilo_test_env::within("the changed workspace to read its file", reached)
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
            .join(format!("workspaces/{}", WorkspaceId::from_u128(7)))
            .join(crate::workspace_files::GOVERNED_ACCESS_FILE);
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
        let agents = crate::registry::AgentRegistry::default();
        let trust = crate::workspace_access::ServerTrust {
            revocations: &revocations,
            policy: &policy,
            blocked: Box::leak(Box::default()),
            own_user: &owner,
            agents: &agents,
            environments: Box::leak(Box::default()),
        };
        let removed = crate::workspace_access::tests::unsigned_peer(owner, [1; 32]);
        {
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
        workspace.lock().await.admission = AdmissionMode::Explicit;
        let owner = [9; 32];
        // The handshake admits anyone: only the workspace may keep a stranger out.
        let revocations = std::sync::RwLock::new(tilo_user_identity::RevocationList::new());
        let policy = std::sync::RwLock::new(crate::policy::TrustPolicy::AllowAny);
        let agents = crate::registry::AgentRegistry::default();
        let trust = crate::workspace_access::ServerTrust {
            revocations: &revocations,
            policy: &policy,
            blocked: Box::leak(Box::default()),
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
    }

    #[tokio::test]
    async fn opening_reservation_counts_toward_limit_and_survives_waiter_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let root = tmp.path().to_path_buf();
        let (release, reached) = test_barrier::install(
            &root.join(format!("workspaces/{}", WorkspaceId::from_u128(7))),
            "open",
        );
        let first_registry = registry.clone();
        let first_root = root.clone();
        let first = tokio::spawn(async move {
            get_or_initialize(
                &first_registry,
                first_root,
                WorkspaceId::from_u128(7),
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
                WorkspaceId::from_u128(8),
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
            WorkspaceId::from_u128(7),
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
        std::fs::write(
            tmp.path()
                .join(format!("workspaces/{}", WorkspaceId::from_u128(7))),
            b"not a directory",
        )
        .unwrap();
        let registry = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let root = tmp.path().to_path_buf();
        let (release, reached) = test_barrier::install(
            &root.join(format!("workspaces/{}", WorkspaceId::from_u128(7))),
            "open",
        );
        let first_registry = registry.clone();
        let first_root = root.clone();
        let first = tokio::spawn(async move {
            get_or_initialize(
                &first_registry,
                first_root,
                WorkspaceId::from_u128(7),
                AdmissionMode::Explicit,
                1,
            )
            .await
        });
        reached.await.unwrap();
        let mut second = Box::pin(get_or_initialize(
            &registry,
            root.clone(),
            WorkspaceId::from_u128(7),
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
        std::fs::remove_file(
            tmp.path()
                .join(format!("workspaces/{}", WorkspaceId::from_u128(7))),
        )
        .unwrap();
        assert!(
            get_or_initialize(
                &registry,
                root,
                WorkspaceId::from_u128(7),
                AdmissionMode::Explicit,
                1
            )
            .await
            .is_ok()
        );
    }
}
