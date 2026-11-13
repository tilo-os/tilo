//! Bounded descriptor-backed mutation jobs. Only the host authority owns the
//! joinable worker; queued executions retain scope and durable reports.
use crate::native::{NativeIdentity, io_error, lock};
use crate::recovery::{
    Fingerprint, OperationId, OperationOwner, RecoveryId, RecoveryKind, Step, ensure_recovery,
};
use crate::{DirectoryGrant, EntryName, FsError, FsRight, FsSession, GrantedEntry};
use cap_std::fs::{Dir, File, MetadataExt};
use std::{
    collections::VecDeque,
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

const JOB_QUEUE_CAPACITY: usize = 32;
const MAX_ITEMS: usize = 256;
const COPY_CHUNK: usize = 1024 * 1024;
const MAX_DEPTH: usize = 128;
const MAX_TREE_ENTRIES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictPolicy {
    Fail,
    KeepBoth,
}
pub enum FsOperation {
    CreateDirectory {
        parent: DirectoryGrant,
        name: EntryName,
        conflict: ConflictPolicy,
    },
    CreateFile {
        parent: DirectoryGrant,
        name: EntryName,
        bytes: Vec<u8>,
        conflict: ConflictPolicy,
    },
    Rename {
        entry: GrantedEntry,
        name: EntryName,
    },
}
pub use tilo_fs_client::directory_types::ItemState;
#[derive(Debug)]
pub struct ItemOutcome {
    pub item: usize,
    pub state: ItemState,
    pub destination: Option<GrantedEntry>,
    pub error: Option<FsError>,
    pub recoveries: Vec<RecoveryId>,
}
#[derive(Debug)]
pub enum FsUpdate {
    Progress {
        sequence: u64,
        item: usize,
        phase: &'static str,
        bytes: u64,
    },
    Item(ItemOutcome),
    Finished {
        completed: usize,
        failed: usize,
        cancelled: usize,
    },
}
struct Results {
    queue: VecDeque<FsUpdate>,
    sequence: u64,
    last_progress: Option<(usize, &'static str, u64)>,
    error: bool,
    publication_error: Option<tilo_files::native::atomic::PublicationError>,
}
struct Control {
    cancel: AtomicBool,
    results: Mutex<Results>,
}
pub struct FsJob {
    id: OperationId,
    _owner: Arc<OperationOwner>,
    control: Arc<Control>,
}
impl FsJob {
    pub fn operation_id(&self) -> OperationId {
        self.id.clone()
    }
    /// Drain this job's bounded updates. Durable host reports survive consumption.
    /// # Errors
    /// After queued updates drain, returns the first owned `Publication` error
    /// once, then sticky `StoreUnavailable` on later polls. The error may outlive
    /// this job, but carries no writer lock: keep the `OperationOwner` alive before
    /// manually retrying its commit. A retry does not resume or repair bookkeeping.
    pub fn poll(&mut self) -> Result<Vec<FsUpdate>, FsError> {
        let mut results = lock(&self.control.results)?;
        if results.error && results.queue.is_empty() {
            return Err(results
                .publication_error
                .take()
                .map_or(FsError::StoreUnavailable, FsError::Publication));
        }
        Ok(results.queue.drain(..).collect())
    }
    pub fn request_cancel(&self) {
        self.control.cancel.store(true, Ordering::Release);
    }
}
struct QueueState {
    jobs: VecDeque<Work>,
    reserved: usize,
    stop: bool,
}
struct QueueShared {
    state: Mutex<QueueState>,
    wake: Condvar,
}
pub(super) struct WorkerQueue {
    shared: Arc<QueueShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl WorkerQueue {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(QueueShared {
                state: Mutex::new(QueueState {
                    jobs: VecDeque::new(),
                    reserved: 0,
                    stop: false,
                }),
                wake: Condvar::new(),
            }),
            worker: Mutex::new(None),
        })
    }
    fn reserve(self: &Arc<Self>) -> Result<QueueReservation, FsError> {
        let mut worker = lock(&self.worker)?;
        if worker.is_none() {
            let shared = Arc::clone(&self.shared);
            *worker = Some(
                std::thread::Builder::new()
                    .name("files-mutations".into())
                    .spawn(move || worker_loop(&shared))
                    .map_err(io_error)?,
            );
        }
        drop(worker);
        let mut state = lock(&self.shared.state)?;
        if state.stop || state.jobs.len().saturating_add(state.reserved) >= JOB_QUEUE_CAPACITY {
            return Err(FsError::Busy);
        }
        state.reserved = state.reserved.saturating_add(1);
        drop(state);
        Ok(QueueReservation {
            queue: Arc::clone(self),
            pending: true,
        })
    }
}
impl Drop for WorkerQueue {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stop = true;
            self.shared.wake.notify_all();
        }
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}
struct QueueReservation {
    queue: Arc<WorkerQueue>,
    pending: bool,
}
impl QueueReservation {
    fn submit(mut self, work: Work) -> Result<(), FsError> {
        let mut state = lock(&self.queue.shared.state)?;
        state.reserved = state.reserved.saturating_sub(1);
        state.jobs.push_back(work);
        drop(state);
        self.pending = false;
        self.queue.shared.wake.notify_one();
        Ok(())
    }
}
impl Drop for QueueReservation {
    fn drop(&mut self) {
        if self.pending
            && let Ok(mut state) = self.queue.shared.state.lock()
        {
            state.reserved = state.reserved.saturating_sub(1);
        }
    }
}
fn worker_loop(shared: &QueueShared) {
    loop {
        let work = {
            let Ok(mut state) = shared.state.lock() else {
                return;
            };
            while state.jobs.is_empty() && !state.stop {
                let Ok(next) = shared.wake.wait(state) else {
                    return;
                };
                state = next;
            }
            if let Some(work) = state.jobs.pop_front() {
                work
            } else {
                return;
            }
        };
        execute(work);
    }
}
struct ActiveSession(FsSession);
impl Drop for ActiveSession {
    fn drop(&mut self) {
        self.0.release_mutation();
    }
}
struct Work {
    session: ActiveSession,
    owner: Arc<OperationOwner>,
    id: OperationId,
    control: Arc<Control>,
    request: Request,
    #[cfg(test)]
    hooks: tests::Hooks,
}
enum Request {
    Operation(FsOperation),
    Restore {
        recovery: RecoveryId,
        step: Box<Step>,
        parent: DirectoryGrant,
        name: EntryName,
    },
    RestoreCaptured {
        _claim: Arc<crate::recovery::CaptureClaim>,
        recovery: RecoveryId,
        step: Box<Step>,
        parent: DirectoryGrant,
        name: EntryName,
    },
    Acknowledge {
        recovery: RecoveryId,
        note: String,
    },
}
impl FsSession {
    pub fn start(&self, operation: FsOperation) -> Result<FsJob, FsError> {
        let owner = self.operation_store()?;
        admit(self, owner, Request::Operation(operation), 1)
    }
}
fn admit(
    session: &FsSession,
    owner: Arc<OperationOwner>,
    request: Request,
    count: usize,
) -> Result<FsJob, FsError> {
    let (reservation, work, job) = prepare_admission(session, owner, request, count)?;
    reservation.submit(work)?;
    Ok(job)
}
fn prepare_admission(
    session: &FsSession,
    owner: Arc<OperationOwner>,
    request: Request,
    count: usize,
) -> Result<(QueueReservation, Work, FsJob), FsError> {
    let reservation = session.operation_queue()?.reserve()?;
    session.claim_mutation()?;
    let active = ActiveSession(session.clone());
    // One active mutation serializes registration. Reserve capacity before a
    // genuine create can publish a sibling whose custody cannot be retained.
    let id = owner.admit(session.mount_id(), count)?;
    let control = Arc::new(Control {
        cancel: AtomicBool::new(false),
        results: Mutex::new(Results {
            queue: VecDeque::with_capacity(MAX_ITEMS * 4 + 1),
            sequence: 0,
            last_progress: None,
            error: false,
            publication_error: None,
        }),
    });
    #[cfg(test)]
    let hooks = lock(&owner.hooks)?.clone();
    let job = FsJob {
        id: id.clone(),
        _owner: Arc::clone(&owner),
        control: Arc::clone(&control),
    };
    let work = Work {
        session: active,
        owner,
        id,
        control,
        request,
        #[cfg(test)]
        hooks,
    };
    Ok((reservation, work, job))
}
pub(super) fn start_restore(
    session: &FsSession,
    recovery: RecoveryId,
    step: Step,
    parent: DirectoryGrant,
    name: EntryName,
) -> Result<FsJob, FsError> {
    admit(
        session,
        session.operation_store()?,
        Request::Restore {
            recovery,
            step: Box::new(step),
            parent,
            name,
        },
        1,
    )
}
pub(super) fn start_captured_restore(
    session: &FsSession,
    recovery: RecoveryId,
    step: Step,
    parent: DirectoryGrant,
    name: EntryName,
    claim: Arc<crate::recovery::CaptureClaim>,
) -> Result<FsJob, FsError> {
    admit(
        session,
        session.operation_store()?,
        Request::RestoreCaptured {
            _claim: claim,
            recovery,
            step: Box::new(step),
            parent,
            name,
        },
        1,
    )
}
pub(super) fn start_acknowledgment(
    session: &FsSession,
    recovery: RecoveryId,
    note: String,
) -> Result<FsJob, FsError> {
    admit(
        session,
        session.operation_store()?,
        Request::Acknowledge { recovery, note },
        1,
    )
}
struct Context<'a> {
    work: &'a Work,
    item: usize,
    bytes: u64,
    staged_identity: Option<NativeIdentity>,
    stage_created: bool,
    stage_published: bool,
}
impl Context<'_> {
    fn session(&self) -> &FsSession {
        &self.work.session.0
    }
    fn cancelled(&self) -> Result<(), FsError> {
        if self.work.control.cancel.load(Ordering::Acquire) {
            Err(FsError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn effect<T>(
        &self,
        source: Option<(&GrantedEntry, FsRight)>,
        destination: Option<&DirectoryGrant>,
        action: impl FnOnce() -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        self.cancelled()?;
        let entries: Vec<_> = source.into_iter().collect();
        self.session().operation_effect(&entries, destination, || {
            self.cancelled()?;
            action()
        })
    }
    fn checkpoint(&self, step: &mut Step, phase: &str) -> Result<(), FsError> {
        let mut next = step.clone();
        next.phase = phase.into();
        self.work.owner.checkpoint(&self.work.id, &next)?;
        *step = next;
        Ok(())
    }
    fn progress(&self, phase: &'static str) -> Result<(), FsError> {
        let mut results = lock(&self.work.control.results)?;
        let progress = (self.item, phase, self.bytes);
        if results.last_progress == Some(progress) {
            return Ok(());
        }
        results.last_progress = Some(progress);
        results.sequence = results
            .sequence
            .checked_add(1)
            .ok_or(FsError::LimitExceeded)?;
        let sequence = results.sequence;
        results.queue.retain(|update|!matches!(update,FsUpdate::Progress{item,phase:old,..} if *item==self.item&&*old==phase));
        results.queue.push_back(FsUpdate::Progress {
            sequence,
            item: self.item,
            phase,
            bytes: self.bytes,
        });
        drop(results);
        Ok(())
    }
    #[cfg(test)]
    fn barrier(&self, boundary: &str) {
        if boundary == "RemoveRestoreBoundary"
            && let Some(barrier) = &self.work.hooks.restore_barrier
        {
            barrier.hold();
        }
        if let Some((named, barrier)) = &self.work.hooks.barrier
            && *named == boundary
        {
            barrier.hold();
        }
    }
    fn failure(&self, error: FsError) -> ItemOutcome {
        let cancelled = matches!(error, FsError::Cancelled | FsError::Closed);
        ItemOutcome {
            item: self.item,
            state: if cancelled {
                ItemState::Cancelled
            } else {
                ItemState::Failed
            },
            destination: None,
            error: Some(error),
            recoveries: vec![],
        }
    }
    fn completed(&self, destination: Option<GrantedEntry>) -> ItemOutcome {
        ItemOutcome {
            item: self.item,
            state: ItemState::Completed,
            destination,
            error: None,
            recoveries: vec![],
        }
    }
    fn recovery(
        &self,
        mut step: Step,
        kind: RecoveryKind,
        error: FsError,
        destination: Option<GrantedEntry>,
    ) -> ItemOutcome {
        let recovery = ensure_recovery(&mut step, kind);
        if let Some(record) = step.recovery.as_mut() {
            record.acknowledged = false;
        }
        let saved = self.work.owner.checkpoint(&self.work.id, &step);
        // The first publication failure owns its phase/commit obligation. A
        // subsequent poisoned-store save cannot replace it with a generic code.
        let error = match (error, saved) {
            (original @ FsError::Publication(_), _) | (original, Ok(())) => original,
            (_, Err(followup)) => followup,
        };
        ItemOutcome {
            item: self.item,
            state: match kind {
                RecoveryKind::KnownStaged
                | RecoveryKind::NeedsRevalidation
                | RecoveryKind::QuarantinedNative => ItemState::RecoveryRequired,
                RecoveryKind::UnknownNativeCommit => ItemState::OutcomeUnknown,
                RecoveryKind::PublishedSourceRetained => ItemState::CopiedSourceRetained,
            },
            destination,
            error: Some(error),
            recoveries: vec![recovery],
        }
    }
}
fn execute(work: Work) {
    let mut completed = 0_usize;
    let mut failed = 0_usize;
    let mut cancelled = 0_usize;
    {
        let mut ctx = Context {
            work: &work,
            item: 0,
            bytes: 0,
            staged_identity: None,
            stage_created: false,
            stage_published: false,
        };
        let mut outcome = match ctx.cancelled() {
            Err(e) => ctx.failure(e),
            Ok(()) => execute_request(&mut ctx),
        };
        match outcome.state {
            ItemState::Completed => completed = completed.saturating_add(1),
            ItemState::Cancelled => cancelled = cancelled.saturating_add(1),
            _ => failed = failed.saturating_add(1),
        }
        if let Err(error) = work.owner.item(&work.id, &outcome) {
            if let Ok(mut results) = work.control.results.lock() {
                results.error = true;
                let first = match outcome.error.take() {
                    Some(original @ FsError::Publication(_)) => original,
                    _ => error,
                };
                if let FsError::Publication(publication) = first {
                    results.publication_error.get_or_insert(publication);
                }
            }
            return;
        }
        if let Ok(mut results) = work.control.results.lock() {
            results.queue.push_back(FsUpdate::Item(outcome));
        } else {
            return;
        }
    }
    if let Err(error) = work.owner.finish(&work.id) {
        if let Ok(mut results) = work.control.results.lock() {
            results.error = true;
            if let FsError::Publication(publication) = error {
                results.publication_error.get_or_insert(publication);
            }
        }
        return;
    }
    let control = Arc::clone(&work.control);
    drop(work);
    if let Ok(mut results) = control.results.lock() {
        results.queue.push_back(FsUpdate::Finished {
            completed,
            failed,
            cancelled,
        });
    }
}
fn execute_request(ctx: &mut Context<'_>) -> ItemOutcome {
    let work = ctx.work;
    match &work.request {
        Request::Operation(operation) => perform(ctx, operation),
        Request::Restore {
            recovery,
            step,
            parent,
            name,
        } => restore(ctx, recovery, step, parent, name),
        Request::RestoreCaptured {
            _claim: _,
            recovery,
            step,
            parent,
            name,
        } => restore_namespace(ctx, recovery, step, parent, name),
        Request::Acknowledge { recovery, note } => match ctx.effect(None, None, || {
            ctx.work
                .owner
                .acknowledge_recovery(recovery, Some(note.clone()))
        }) {
            Ok(()) => ctx.completed(None),
            Err(error) => ctx.failure(error),
        },
    }
}
fn perform(ctx: &mut Context<'_>, operation: &FsOperation) -> ItemOutcome {
    let result = match operation {
        FsOperation::CreateDirectory {
            parent,
            name,
            conflict,
        } => copy_publish(
            ctx,
            PublishRequest {
                into: parent,
                name,
                policy: *conflict,
                bytes: None,
            },
        ),
        FsOperation::CreateFile {
            parent,
            name,
            bytes,
            conflict,
        } => copy_publish(
            ctx,
            PublishRequest {
                into: parent,
                name,
                policy: *conflict,
                bytes: Some(bytes),
            },
        ),
        FsOperation::Rename { entry, name } => rename_entry(ctx, entry, name),
    };
    match result {
        Ok(outcome) => outcome,
        Err(error) => ctx.failure(error),
    }
}

fn check_named(parent: &Dir, name: &EntryName, identity: NativeIdentity) -> Result<(), FsError> {
    if NativeIdentity::of(
        &parent
            .symlink_metadata(name.as_os_str())
            .map_err(io_error)?,
    ) != identity
    {
        return Err(FsError::StaleEntry);
    }
    Ok(())
}
fn sync_dir(dir: &Dir) -> Result<(), FsError> {
    rustix::fs::fsync(dir).map_err(|e| io_error(e.into()))
}
fn no_replace(
    from: &Dir,
    name: &EntryName,
    into: &Dir,
    target: &EntryName,
) -> Result<(), rustix::io::Errno> {
    rustix::fs::renameat_with(
        from,
        name.as_os_str(),
        into,
        target.as_os_str(),
        rustix::fs::RenameFlags::NOREPLACE,
    )
}
fn candidate(name: &EntryName, attempt: usize) -> Result<EntryName, FsError> {
    if attempt == 0 {
        return Ok(name.clone());
    }
    let bytes = name.as_os_str().as_bytes();
    let split = bytes
        .iter()
        .rposition(|b| *b == b'.')
        .filter(|i| *i > 0)
        .unwrap_or(bytes.len());
    let mut result = bytes.get(..split).ok_or(FsError::InvalidName)?.to_vec();
    result.extend_from_slice(format!(" {}", attempt.saturating_add(1)).as_bytes());
    result.extend_from_slice(bytes.get(split..).ok_or(FsError::InvalidName)?);
    EntryName::new(OsString::from_vec(result)).map_err(FsError::from)
}
fn attempts(ctx: &Context<'_>, policy: ConflictPolicy) -> usize {
    let limit = if policy == ConflictPolicy::Fail {
        1
    } else {
        9999
    };
    #[cfg(test)]
    let limit = ctx.work.hooks.attempts.unwrap_or(limit);
    let _ = ctx;
    limit
}
fn rename_entry(
    ctx: &Context<'_>,
    entry: &GrantedEntry,
    name: &EntryName,
) -> Result<ItemOutcome, FsError> {
    let source = ctx.session().mutation_source(entry, FsRight::Rename)?;
    let parent = ctx.session().mutation_parent(entry)?;
    let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
    step.source = Some(source.locator.clone());
    let mut target = source.locator.clone();
    target.name = name.as_os_str().as_bytes().to_vec();
    step.destination = Some(target);
    ctx.checkpoint(&mut step, "PublishStarted")?;
    let mut published = false;
    let result = ctx.effect(Some((entry, FsRight::Rename)), None, || {
        check_named(&source.parent, &source.name, source.object.identity())?;
        no_replace(&source.parent, &source.name, &source.parent, name)
            .map_err(|e| io_error(e.into()))?;
        published = true;
        sync_dir(&source.parent)
    });
    let destination = if published {
        ctx.session().rename_result(&parent, name).ok()
    } else {
        None
    };
    if let Err(error) = result {
        if published {
            return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, destination));
        }
        return Err(error);
    }
    if let Err(e) = ctx.checkpoint(&mut step, "Published") {
        return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, e, destination));
    }
    ctx.progress("publish")?;
    Ok(ctx.completed(destination))
}
/// One item to publish as `name` in `into`: a copy (or, `moving`, a move) of `source`, or
/// without one a new file holding `bytes`, or a new directory when there are none.
#[derive(Clone, Copy)]
struct PublishRequest<'a> {
    into: &'a DirectoryGrant,
    name: &'a EntryName,
    policy: ConflictPolicy,
    bytes: Option<&'a Vec<u8>>,
}
fn copy_publish(
    ctx: &mut Context<'_>,
    request: PublishRequest<'_>,
) -> Result<ItemOutcome, FsError> {
    let PublishRequest { into, name, .. } = request;
    let destination = ctx.session().mutation_directory(into)?;
    let stage_name =
        EntryName::new(format!(".tilo-stage-{}-{}", ctx.work.id.native(), ctx.item).into())?;
    let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
    step.stage = Some(ctx.session().mutation_locator(into, &stage_name, None)?);
    step.destination = Some(ctx.session().mutation_locator(into, name, None)?);
    ctx.checkpoint(&mut step, "Admitted")?;
    let result = copy_publish_staged(ctx, request, &destination, &stage_name, &mut step);
    match result {
        Err(error) if ctx.stage_created && !ctx.stage_published => {
            if let Some(stage) = step.stage.as_mut() {
                stage.identity = ctx.staged_identity;
            }
            match cleanup_owned(ctx, &destination, &stage_name, ctx.staged_identity) {
                Ok(()) => {
                    let mut discarded = step.clone();
                    discarded.stage = None;
                    discarded.fingerprint = None;
                    discarded.recovery = None;
                    if let Err(error) = ctx.checkpoint(&mut discarded, "StageDiscarded") {
                        return Ok(ctx.recovery(
                            step,
                            RecoveryKind::NeedsRevalidation,
                            error,
                            None,
                        ));
                    }
                    Err(error)
                }
                Err(cleanup_error) => {
                    let kind = if step.fingerprint.is_some()
                        && ctx.staged_identity.is_some_and(|identity| {
                            check_named(&destination, &stage_name, identity).is_ok()
                        }) {
                        RecoveryKind::KnownStaged
                    } else {
                        RecoveryKind::NeedsRevalidation
                    };
                    Ok(ctx.recovery(step, kind, cleanup_error, None))
                }
            }
        }
        Err(error) if ctx.stage_published => {
            let destination = step
                .destination
                .as_ref()
                .and_then(|target| EntryName::new(OsString::from_vec(target.name.clone())).ok())
                .and_then(|name| ctx.session().mutation_result(into, &name).ok());
            Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, destination))
        }
        other => other,
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "one owner encloses every post-creation exit while the inner scope tracks publication/removal"
)]
fn copy_publish_staged(
    ctx: &mut Context<'_>,
    request: PublishRequest<'_>,
    destination: &Dir,
    stage_name: &EntryName,
    step: &mut Step,
) -> Result<ItemOutcome, FsError> {
    let PublishRequest {
        into,
        name,
        policy,
        bytes,
    } = request;
    let auth = None;
    let copied = if let Some(bytes) = bytes {
        create_bytes(ctx, auth, into, destination, stage_name, bytes)
    } else {
        (|| {
            ctx.effect(auth, Some(into), || {
                rustix::fs::mkdirat(
                    destination,
                    stage_name.as_os_str(),
                    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
                )
                .map_err(|e| io_error(e.into()))
            })?;
            ctx.stage_created = true;
            let stage = ctx.effect(auth, Some(into), || {
                Ok(Dir::from_std_file(
                    open_object(destination, stage_name, true)?.into_std(),
                ))
            })?;
            ctx.staged_identity =
                Some(NativeIdentity::of(&stage.dir_metadata().map_err(io_error)?));
            ctx.effect(auth, Some(into), || {
                sync_dir(&stage)?;
                sync_dir(destination)
            })
        })()
    };
    copied?;
    check_named(
        destination,
        stage_name,
        ctx.staged_identity.ok_or(FsError::StaleEntry)?,
    )?;
    let staged_fingerprint = fingerprint(ctx, auth, into, destination, stage_name)?;
    if Some(staged_fingerprint.identity) != ctx.staged_identity {
        return Err(FsError::StaleEntry);
    }
    step.stage
        .as_mut()
        .ok_or(FsError::StoreUnavailable)?
        .identity = Some(staged_fingerprint.identity);
    step.fingerprint = Some(staged_fingerprint);
    ensure_recovery(step, RecoveryKind::KnownStaged);
    if let Err(error) = ctx.checkpoint(step, "Staged") {
        return Ok(ctx.recovery(step.clone(), RecoveryKind::KnownStaged, error, None));
    }
    #[cfg(test)]
    if ctx.work.hooks.stop_after_stage {
        return Ok(ctx.recovery(
            step.clone(),
            RecoveryKind::KnownStaged,
            FsError::StoreUnavailable,
            None,
        ));
    }
    for attempt in 0..attempts(ctx, policy) {
        let target = candidate(name, attempt)?;
        step.destination = Some(ctx.session().mutation_locator(
            into,
            &target,
            step.stage.as_ref().and_then(|s| s.identity),
        )?);
        if let Err(error) = ctx.checkpoint(step, "PublishStarted") {
            return Ok(ctx.recovery(step.clone(), RecoveryKind::KnownStaged, error, None));
        }
        let mut published = false;
        let result = ctx.effect(auth, Some(into), || {
            check_named(
                destination,
                stage_name,
                step.fingerprint
                    .as_ref()
                    .ok_or(FsError::StoreUnavailable)?
                    .identity,
            )?;
            no_replace(destination, stage_name, destination, &target)
                .map_err(|e| io_error(e.into()))?;
            published = true;
            sync_dir(destination)
        });
        ctx.stage_published = published;
        if published && result.is_err() {
            let error = result.err().ok_or(FsError::StoreUnavailable)?;
            let grant = ctx.session().mutation_result(into, &target).ok();
            return Ok(ctx.recovery(
                step.clone(),
                RecoveryKind::UnknownNativeCommit,
                error,
                grant,
            ));
        }
        if matches!(result, Err(FsError::Conflict)) {
            continue;
        }
        if let Err(error) = result {
            return Ok(ctx.recovery(step.clone(), RecoveryKind::KnownStaged, error, None));
        }
        let grant = ctx.session().mutation_result(into, &target).ok();
        if let Some(recovery) = step.recovery.as_mut() {
            recovery.kind = RecoveryKind::PublishedSourceRetained;
            recovery.acknowledged = true;
        }
        if let Err(error) = ctx.checkpoint(step, "Published") {
            return Ok(ctx.recovery(
                step.clone(),
                RecoveryKind::PublishedSourceRetained,
                error,
                grant,
            ));
        }
        ctx.progress("publish")?;
        return Ok(ctx.completed(grant));
    }
    Err(FsError::Conflict)
}

fn open_object(parent: &Dir, name: &EntryName, directory: bool) -> Result<File, FsError> {
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC
        | if directory {
            rustix::fs::OFlags::DIRECTORY
        } else {
            rustix::fs::OFlags::empty()
        };
    let fd = rustix::fs::openat(parent, name.as_os_str(), flags, rustix::fs::Mode::empty())
        .map_err(|e| io_error(e.into()))?;
    Ok(File::from_std(std::fs::File::from(fd)))
}
fn create_file(parent: &Dir, name: &EntryName) -> Result<File, FsError> {
    let fd = rustix::fs::openat(
        parent,
        name.as_os_str(),
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .map_err(|e| io_error(e.into()))?;
    Ok(File::from_std(std::fs::File::from(fd)))
}
fn write_chunk(file: &File, bytes: &[u8], offset: u64) -> Result<(), FsError> {
    let mut written = 0_usize;
    while written < bytes.len() {
        let n = rustix::io::pwrite(
            file,
            bytes.get(written..).ok_or(FsError::LimitExceeded)?,
            offset
                .checked_add(written as u64)
                .ok_or(FsError::LimitExceeded)?,
        )
        .map_err(|e| io_error(e.into()))?;
        if n == 0 {
            return Err(FsError::Io(std::io::Error::from(
                std::io::ErrorKind::WriteZero,
            )));
        }
        written = written.checked_add(n).ok_or(FsError::LimitExceeded)?;
    }
    Ok(())
}
fn create_bytes(
    ctx: &mut Context<'_>,
    source: Option<(&GrantedEntry, FsRight)>,
    into: &DirectoryGrant,
    parent: &Dir,
    name: &EntryName,
    bytes: &[u8],
) -> Result<(), FsError> {
    let file = ctx.effect(source, Some(into), || create_file(parent, name))?;
    ctx.stage_created = true;
    ctx.staged_identity = Some(NativeIdentity::of(&file.metadata().map_err(io_error)?));
    for (index, chunk) in bytes.chunks(COPY_CHUNK).enumerate() {
        ctx.effect(source, Some(into), || {
            write_chunk(
                &file,
                chunk,
                index
                    .checked_mul(COPY_CHUNK)
                    .ok_or(FsError::LimitExceeded)? as u64,
            )
        })?;
        ctx.bytes = ctx
            .bytes
            .checked_add(chunk.len() as u64)
            .ok_or(FsError::LimitExceeded)?;
        ctx.progress("copy")?;
    }
    ctx.effect(source, Some(into), || {
        file.sync_all().map_err(io_error)?;
        sync_dir(parent)
    })
}
fn remove_object(parent: &Dir, name: &EntryName) -> Result<(), FsError> {
    let metadata = parent
        .symlink_metadata(name.as_os_str())
        .map_err(io_error)?;
    if metadata.is_dir() {
        parent.remove_dir_all(name.as_os_str()).map_err(io_error)
    } else {
        parent.remove_file(name.as_os_str()).map_err(io_error)
    }
}

fn cleanup_owned(
    ctx: &Context<'_>,
    parent: &Dir,
    name: &EntryName,
    identity: Option<NativeIdentity>,
) -> Result<(), FsError> {
    check_named(parent, name, identity.ok_or(FsError::StaleEntry)?)?;
    remove_stage_object(ctx, parent, name)?;
    #[cfg(test)]
    if ctx.work.hooks.cleanup_sync_error {
        return Err(FsError::Io(rustix::io::Errno::IO.into()));
    }
    sync_dir(parent)
}

fn remove_stage_object(ctx: &Context<'_>, parent: &Dir, name: &EntryName) -> Result<(), FsError> {
    #[cfg(test)]
    if ctx.work.hooks.cleanup_error {
        return Err(FsError::Io(rustix::io::Errno::IO.into()));
    }
    let _ = ctx;
    remove_object(parent, name)
}
fn fingerprint(
    ctx: &Context<'_>,
    source: Option<(&GrantedEntry, FsRight)>,
    into: &DirectoryGrant,
    parent: &Dir,
    name: &EntryName,
) -> Result<Fingerprint, FsError> {
    #[cfg(test)]
    if name.as_os_str().as_bytes().starts_with(b".tilo-stage-") {
        ctx.barrier("stage-fingerprint");
        if ctx.work.hooks.stage_fingerprint_error {
            return Err(FsError::Io(rustix::io::Errno::IO.into()));
        }
    }
    let mut walk = TreeFingerprint {
        ctx,
        source,
        into,
        hash: blake3::Hasher::new(),
        length: 0,
        count: 0,
    };
    let walked = walk.object(parent, name, 0)?;
    let identity = NativeIdentity::of(
        &parent
            .symlink_metadata(name.as_os_str())
            .map_err(io_error)?,
    );
    if identity != walked {
        return Err(FsError::StaleEntry);
    }
    Ok(Fingerprint {
        identity,
        length: walk.length,
        digest: *walk.hash.finalize().as_bytes(),
    })
}
/// One bounded fingerprint walk over an object tree, every effect authorised as `source`
/// within `into`: the whole-tree `hash`, the content `length` it covers, and the entries
/// visited so far.
struct TreeFingerprint<'c, 'a> {
    ctx: &'c Context<'a>,
    source: Option<(&'c GrantedEntry, FsRight)>,
    into: &'c DirectoryGrant,
    hash: blake3::Hasher,
    length: u64,
    count: usize,
}
impl TreeFingerprint<'_, '_> {
    /// Hash the object `name` and everything under it; returns its identity.
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded traversal frame hashes each file, directory or link"
    )]
    fn object(
        &mut self,
        parent: &Dir,
        name: &EntryName,
        depth: usize,
    ) -> Result<NativeIdentity, FsError> {
        if depth > MAX_DEPTH || self.count == MAX_TREE_ENTRIES {
            return Err(FsError::LimitExceeded);
        }
        self.count = self.count.checked_add(1).ok_or(FsError::LimitExceeded)?;
        let metadata = self.ctx.effect(self.source, Some(self.into), || {
            parent.symlink_metadata(name.as_os_str()).map_err(io_error)
        })?;
        if metadata.is_symlink() {
            let target = self.ctx.effect(self.source, Some(self.into), || {
                parent
                    .read_link_contents(name.as_os_str())
                    .map_err(io_error)
            })?;
            self.hash.update(b"link");
            self.hash
                .update(&(target.as_os_str().as_bytes().len() as u64).to_le_bytes());
            self.hash.update(target.as_os_str().as_bytes());
            self.length = self
                .length
                .checked_add(target.as_os_str().as_bytes().len() as u64)
                .ok_or(FsError::LimitExceeded)?;
        } else if metadata.is_file() {
            self.hash.update(b"file");
            self.hash.update(&metadata.len().to_le_bytes());
            let file = self.ctx.effect(self.source, Some(self.into), || {
                let file = open_object(parent, name, false)?;
                if NativeIdentity::of(&file.metadata().map_err(io_error)?)
                    != NativeIdentity::of(&metadata)
                {
                    return Err(FsError::StaleEntry);
                }
                Ok(file)
            })?;
            let mut buffer = vec![0; COPY_CHUNK];
            let mut offset = 0;
            while offset < metadata.len() {
                let len =
                    usize::try_from(metadata.len().saturating_sub(offset).min(COPY_CHUNK as u64))
                        .map_err(|_| FsError::LimitExceeded)?;
                let n = self.ctx.effect(self.source, Some(self.into), || {
                    rustix::io::pread(
                        &file,
                        buffer.get_mut(..len).ok_or(FsError::LimitExceeded)?,
                        offset,
                    )
                    .map_err(|e| io_error(e.into()))
                })?;
                if n == 0 {
                    return Err(FsError::StaleEntry);
                }
                self.hash
                    .update(buffer.get(..n).ok_or(FsError::LimitExceeded)?);
                offset = offset.checked_add(n as u64).ok_or(FsError::LimitExceeded)?;
                self.length = self
                    .length
                    .checked_add(n as u64)
                    .ok_or(FsError::LimitExceeded)?;
            }
            self.ctx.effect(self.source, Some(self.into), || {
                let observed = file.metadata().map_err(io_error)?;
                if observed.len() != metadata.len()
                    || observed.mtime() != metadata.mtime()
                    || observed.mtime_nsec() != metadata.mtime_nsec()
                {
                    return Err(FsError::StaleEntry);
                }
                Ok(())
            })?;
        } else if metadata.is_dir() {
            self.hash.update(b"dir");
            let dir = self.ctx.effect(self.source, Some(self.into), || {
                let dir = Dir::from_std_file(open_object(parent, name, true)?.into_std());
                if NativeIdentity::of(&dir.dir_metadata().map_err(io_error)?)
                    != NativeIdentity::of(&metadata)
                {
                    return Err(FsError::StaleEntry);
                }
                Ok(dir)
            })?;
            let mut entries = self.ctx.effect(self.source, Some(self.into), || {
                dir.entries().map_err(io_error)
            })?;
            let mut names = Vec::new();
            loop {
                let next = self.ctx.effect(self.source, Some(self.into), || {
                    entries.next().transpose().map_err(io_error)
                })?;
                let Some(next) = next else { break };
                if names.len() >= MAX_TREE_ENTRIES.saturating_sub(self.count) {
                    return Err(FsError::LimitExceeded);
                }
                names.push(next.file_name());
            }
            names.sort();
            for native in names {
                self.hash
                    .update(&(native.as_bytes().len() as u64).to_le_bytes());
                self.hash.update(native.as_bytes());
                let child = EntryName::new(native)?;
                self.object(&dir, &child, depth.saturating_add(1))?;
            }
            self.hash.update(b"end");
        } else {
            return Err(FsError::Unsupported("special staged object"));
        }
        self.ctx.effect(self.source, Some(self.into), || {
            check_named(parent, name, NativeIdentity::of(&metadata))
        })?;
        Ok(NativeIdentity::of(&metadata))
    }
}
fn restore_namespace(
    ctx: &Context<'_>,
    recovery: &RecoveryId,
    saved: &Step,
    parent: &DirectoryGrant,
    name: &EntryName,
) -> ItemOutcome {
    let result = (|| {
        let locator = saved.stage.as_ref().ok_or(FsError::StaleEntry)?;
        let custody = ctx.session().recovery_custody(parent, locator)?;
        let stage_name = EntryName::new(OsString::from_vec(locator.name.clone()))?;
        let observed = custody
            .directory
            .symlink_metadata(stage_name.as_os_str())
            .map_err(io_error)?;
        let identity = NativeIdentity::of(&observed);
        if locator
            .identity
            .is_some_and(|expected| expected != identity)
        {
            return Err(FsError::StaleEntry);
        }
        let destination = ctx.session().mutation_directory(parent)?;
        let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
        step.stage = Some(locator.clone());
        step.destination = Some(
            ctx.session()
                .mutation_locator(parent, name, Some(identity))?,
        );
        ctx.checkpoint(&mut step, "CapturedRestoreStarted")?;
        let mut published = false;
        ctx.cancelled()?;
        let result = ctx.session().captured_restore_effect(&custody, parent, || {
            ctx.cancelled()?;
            check_named(&custody.directory, &stage_name, identity)?;
            no_replace(&custody.directory, &stage_name, &destination, name)
                .map_err(|error| io_error(error.into()))?;
            published = true;
            sync_dir(&custody.directory)?;
            sync_dir(&destination)
        });
        let grant = published
            .then(|| ctx.session().mutation_result(parent, name).ok())
            .flatten();
        if let Err(error) = result {
            return if published {
                Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, grant))
            } else {
                Err(error)
            };
        }
        if let Err(error) = ctx.checkpoint(&mut step, "CapturedRestored") {
            return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, grant));
        }
        if let Err(error) = ctx.work.owner.acknowledge_recovery(recovery, None) {
            return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, grant));
        }
        Ok(ctx.completed(grant))
    })();
    match result {
        Ok(outcome) => outcome,
        Err(error) => ctx.failure(error),
    }
}
fn restore(
    ctx: &Context<'_>,
    recovery: &RecoveryId,
    saved: &Step,
    parent: &DirectoryGrant,
    name: &EntryName,
) -> ItemOutcome {
    let result = (|| {
        let locator = saved.stage.as_ref().ok_or(FsError::StaleEntry)?;
        let (read_grant, source, stage_name) = ctx.session().recovery_source(locator)?;
        let expected = saved.fingerprint.as_ref().ok_or(FsError::StaleEntry)?;
        let observed = self::fingerprint(
            ctx,
            Some((&read_grant, FsRight::Read)),
            parent,
            &source,
            &stage_name,
        )?;
        if observed.identity != expected.identity
            || observed.length != expected.length
            || observed.digest != expected.digest
        {
            return Err(FsError::StaleEntry);
        }
        let destination = ctx.session().mutation_directory(parent)?;
        let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
        step.stage = Some(locator.clone());
        step.source = Some(locator.clone());
        step.fingerprint = Some(expected.clone());
        step.destination = Some(ctx.session().mutation_locator(
            parent,
            name,
            Some(expected.identity),
        )?);
        ctx.checkpoint(&mut step, "Staged")?;
        ctx.checkpoint(&mut step, "PublishStarted")?;
        let mut published = false;
        let result = ctx.effect(Some((&read_grant, FsRight::Read)), Some(parent), || {
            check_named(&source, &stage_name, expected.identity)?;
            no_replace(&source, &stage_name, &destination, name).map_err(|e| io_error(e.into()))?;
            published = true;
            sync_dir(&source)?;
            sync_dir(&destination)
        });
        let grant = if published {
            ctx.session().mutation_result(parent, name).ok()
        } else {
            None
        };
        if let Err(error) = result {
            if published {
                return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, grant));
            }
            return Err(error);
        }
        if let Err(error) = ctx.checkpoint(&mut step, "Published") {
            return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, grant));
        }
        if let Err(error) = ctx.work.owner.acknowledge_recovery(recovery, None) {
            return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, grant));
        }
        ctx.progress("publish")?;
        Ok(ctx.completed(grant))
    })();
    match result {
        Ok(outcome) => outcome,
        Err(e) => ctx.failure(e),
    }
}

#[cfg(test)]
#[allow(
    clippy::too_many_lines,
    reason = "bounded temporary fixtures and explicit lifecycle assertions"
)]
pub(super) mod tests {
    use super::*;
    use crate::recovery::{OperationOwner, OperationState, RecoveryKind, RecoveryResolution};
    use crate::{FsOwner, FsRight, NativeGrantAuthority};
    use std::{
        fs,
        sync::{Arc, Condvar, Mutex},
        time::Instant,
    };
    use tempfile::TempDir;
    #[cfg(target_os = "linux")]
    fn assert_publication(error: &FsError, expected: tilo_files::native::atomic::PublicationState) {
        let source = std::error::Error::source(error).expect("owned publication source");
        let publication = source
            .downcast_ref::<tilo_files::native::atomic::PublicationError>()
            .expect("typed publication error");
        assert_eq!(publication.state(), expected);
    }
    #[test]
    fn receipt_publication_initial_admission_phases() {
        use tilo_files::native::atomic::{
            PublicationState as State,
            test_support::{Checkpoint, with_failure},
        };
        for (fault, state) in [
            (Checkpoint::BeforeDataFlush, State::NotPublished),
            (Checkpoint::PublishSubmitted, State::PublicationUnknown),
            (Checkpoint::AfterPublish, State::PublicationUnknown),
            (Checkpoint::BeforeCommit, State::PublishedDurabilityUnknown),
        ] {
            let f = Fixture::new();
            fs::write(f.source("a"), b"source").unwrap();
            let failed = with_failure(fault, || f.session.start(f.copy("a")));
            let error = failed
                .err()
                .expect("receipt publication must fail before job admission");
            assert_publication(&error, state);
            assert_eq!(f.store.submissions.load(Ordering::SeqCst), 0);
            assert!(!f.dest("a").exists());
            if state == State::NotPublished {
                let mut job = f.session.start(f.copy("a")).unwrap();
                drain(&mut job);
            } else {
                assert!(matches!(
                    f.session.start(f.copy("a")),
                    Err(FsError::StoreUnavailable)
                ));
            }
        }
    }
    #[test]
    fn receipt_publication_worker_preserves_first_failure_and_drains_updates() {
        use tilo_files::native::atomic::{PublicationState as State, test_support::Checkpoint};
        for phase in ["Staged", "Published", "Item", "Finished"] {
            for (fault, expected) in [
                (Checkpoint::BeforeDataFlush, State::NotPublished),
                (Checkpoint::PublishSubmitted, State::PublicationUnknown),
                (Checkpoint::AfterPublish, State::PublicationUnknown),
                (Checkpoint::BeforeCommit, State::PublishedDurabilityUnknown),
            ] {
                let f = Fixture::new();
                fs::write(f.source("a"), b"source").unwrap();
                f.hooks(Hooks {
                    publication_failure: Some((phase, fault)),
                    ..Hooks::default()
                });
                let mut job = f.session.start(f.copy("a")).unwrap();
                let id = job.operation_id();
                let deadline = tilo_test_env::hang_deadline();
                let mut updates = vec![];
                let error = loop {
                    match job.poll() {
                        Ok(batch) => updates.extend(batch),
                        Err(error) => break error,
                    }
                    assert!(
                        Instant::now() < deadline,
                        "terminal receipt failure was not surfaced"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                };
                assert_publication(&error, expected);
                assert!(matches!(job.poll(), Err(FsError::StoreUnavailable)));
                assert!(
                    !updates
                        .iter()
                        .any(|u| matches!(u, FsUpdate::Finished { .. }))
                );
                assert!(!f.store.checkpoints(&id).unwrap().iter().any(|p| p == phase));
                assert!(matches!(
                    f.store.admit(uuid::Uuid::nil(), 1),
                    Err(FsError::StoreUnavailable)
                ));
                assert_eq!(fs::read(f.source("a")).unwrap(), b"source");
                if phase != "Staged" {
                    assert_eq!(fs::read(f.dest("a")).unwrap(), b"source");
                }
                if phase == "Finished" {
                    assert!(updates.iter().any(|u| matches!(u, FsUpdate::Item(_))));
                }
                if phase == "Staged" {
                    assert_eq!(fs::read(f.stage()).unwrap(), b"source");
                }
                drop(job);
                // The original error can outlive the job. A successful manual
                // commit while retaining the owner cannot repair its bookkeeping.
                if fault == Checkpoint::BeforeCommit {
                    let FsError::Publication(publication) = error else {
                        panic!("publication lost")
                    };
                    publication.retry_commit().unwrap();
                    assert!(matches!(
                        f.store.admit(uuid::Uuid::nil(), 1),
                        Err(FsError::StoreUnavailable)
                    ));
                } else {
                    drop(error);
                }
                let f = f.reopen();
                if phase == "Staged" {
                    assert_eq!(fs::read(f.stage()).unwrap(), b"source");
                    assert_eq!(
                        f.store.report(&id).unwrap().state,
                        OperationState::RecoveryRequired
                    );
                } else {
                    assert_eq!(fs::read(f.dest("a")).unwrap(), b"source");
                    if phase == "Published" {
                        // A create has no source to retain: an interrupted
                        // publication's outcome is unknown until acknowledged.
                        assert_eq!(
                            f.store.report(&id).unwrap().state,
                            OperationState::OutcomeUnknown
                        );
                        assert_eq!(
                            f.store.recoveries().unwrap()[0].kind,
                            RecoveryKind::UnknownNativeCommit
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn receipt_publication_restore_preserves_committed_user_bytes() {
        let f = Fixture::new();
        fs::write(f.source("a"), b"restore bytes").unwrap();
        f.hooks(Hooks {
            stop_after_stage: true,
            ..Hooks::default()
        });
        f.run(f.copy("a"));
        let recovery = f.store.recoveries().unwrap().remove(0);
        let stage = f.stage();
        f.hooks(Hooks {
            publication_failure: Some((
                "Published",
                tilo_files::native::atomic::test_support::Checkpoint::BeforeDataFlush,
            )),
            ..Hooks::default()
        });
        let mut job = f
            .store
            .resolve(
                &recovery.id,
                &f.session,
                RecoveryResolution::RestoreKnown {
                    parent: f.dst.clone(),
                    name: name("restored"),
                },
            )
            .unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let error = loop {
            if let Err(error) = job.poll() {
                break error;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_publication(
            &error,
            tilo_files::native::atomic::PublicationState::NotPublished,
        );
        assert_eq!(fs::read(f.dest("restored")).unwrap(), b"restore bytes");
        assert!(!stage.exists());
        assert!(
            !f.store
                .recoveries()
                .unwrap()
                .iter()
                .find(|r| r.id == recovery.id)
                .unwrap()
                .acknowledged
        );
        assert!(matches!(job.poll(), Err(FsError::StoreUnavailable)));
    }
    #[derive(Clone, Default)]
    #[allow(
        clippy::struct_excessive_bools,
        reason = "independent cfg(test) syscall failure seams"
    )]
    pub(crate) struct Hooks {
        pub fail_checkpoint: Option<&'static str>,
        pub publication_failure: Option<(
            &'static str,
            tilo_files::native::atomic::test_support::Checkpoint,
        )>,
        pub stop_after_stage: bool,
        pub stage_fingerprint_error: bool,
        pub cleanup_error: bool,
        pub cleanup_sync_error: bool,
        pub barrier: Option<(&'static str, Arc<Barrier>)>,
        pub restore_barrier: Option<Arc<Barrier>>,
        pub attempts: Option<usize>,
        pub report_limit: Option<usize>,
        pub metadata_limit: Option<usize>,
    }
    #[derive(Default)]
    pub(crate) struct Barrier {
        state: Mutex<(bool, bool)>,
        wake: Condvar,
    }
    impl Barrier {
        pub(super) fn hold(&self) {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.wake.notify_all();
            while !state.1 {
                state = self.wake.wait(state).unwrap();
            }
            drop(state);
        }
        fn entered(&self) {
            let deadline = tilo_test_env::hang_deadline();
            let mut s = self.state.lock().unwrap();
            while !s.0 {
                let left = deadline
                    .checked_duration_since(Instant::now())
                    .expect("barrier entry deadline");
                s = self.wake.wait_timeout(s, left).unwrap().0;
            }
            drop(s);
        }
        fn release(&self) {
            self.state.lock().unwrap().1 = true;
            self.wake.notify_all();
        }
    }
    struct Release(Arc<Barrier>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    fn name(s: &str) -> EntryName {
        EntryName::new(s.into()).unwrap()
    }
    fn owner() -> FsOwner {
        FsOwner {
            workspace: "test-room".into(),
            app_id: "so.tilo.file-explorer".into(),
        }
    }
    fn rights() -> Vec<FsRight> {
        vec![
            FsRight::Enumerate,
            FsRight::Read,
            FsRight::Write,
            FsRight::Create,
            FsRight::Rename,
            FsRight::Replace,
            FsRight::Purge,
        ]
    }
    pub(crate) struct Fixture {
        pub(crate) authority: NativeGrantAuthority,
        pub(crate) session: FsSession,
        pub(crate) src: DirectoryGrant,
        pub(crate) dst: DirectoryGrant,
        pub(crate) store: Arc<OperationOwner>,
        pub(crate) disk: TempDir,
        pub(crate) meta: TempDir,
    }
    impl Fixture {
        pub(crate) fn new() -> Self {
            Self::with_rights(rights())
        }
        fn with_rights(allowed: Vec<FsRight>) -> Self {
            let disk = tempfile::tempdir().unwrap();
            let meta = tempfile::Builder::new()
                .permissions(
                    <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
                )
                .tempdir()
                .unwrap();
            Self::with_storage(allowed, disk, meta)
        }
        fn with_storage(allowed: Vec<FsRight>, disk: TempDir, meta: TempDir) -> Self {
            fs::create_dir(disk.path().join("src")).unwrap();
            fs::create_dir(disk.path().join("dst")).unwrap();
            let authority = NativeGrantAuthority::new();
            let src = authority
                .admit_directory(
                    fs::File::open(disk.path().join("src")).unwrap(),
                    owner(),
                    allowed.clone(),
                )
                .unwrap();
            let dst = authority
                .admit_directory(
                    fs::File::open(disk.path().join("dst")).unwrap(),
                    owner(),
                    allowed,
                )
                .unwrap();
            let session = authority
                .bind(
                    owner(),
                    uuid::Uuid::from_u128(0xa11),
                    &[src.clone(), dst.clone()],
                    &[],
                )
                .unwrap();
            let store =
                OperationOwner::open(fs::File::open(meta.path()).unwrap(), owner()).unwrap();
            authority.attach_operations(store.clone()).unwrap();
            Self {
                authority,
                session,
                src,
                dst,
                store,
                disk,
                meta,
            }
        }
        fn source(&self, n: &str) -> std::path::PathBuf {
            self.disk.path().join("src").join(n)
        }
        fn dest(&self, n: &str) -> std::path::PathBuf {
            self.disk.path().join("dst").join(n)
        }
        fn hooks(&self, h: Hooks) {
            *self.store.hooks.lock().unwrap() = h;
        }
        /// Create `n` in the destination with the bytes of the source's `n`.
        fn copy(&self, n: &str) -> FsOperation {
            FsOperation::CreateFile {
                parent: self.dst.clone(),
                name: name(n),
                bytes: fs::read(self.source(n)).unwrap(),
                conflict: ConflictPolicy::Fail,
            }
        }
        fn run(&self, operation: FsOperation) -> (crate::recovery::OperationId, Vec<FsUpdate>) {
            let mut job = self.session.start(operation).unwrap();
            let id = job.operation_id();
            let u = drain(&mut job);
            (id, u)
        }
        fn reopen(self) -> Self {
            let Self {
                authority,
                session,
                src,
                dst,
                store,
                disk,
                meta,
            } = self;
            drop(authority);
            drop(session);
            drop(src);
            drop(dst);
            drop(store);
            let authority = NativeGrantAuthority::new();
            let src = authority
                .admit_directory(
                    fs::File::open(disk.path().join("src")).unwrap(),
                    owner(),
                    rights(),
                )
                .unwrap();
            let dst = authority
                .admit_directory(
                    fs::File::open(disk.path().join("dst")).unwrap(),
                    owner(),
                    rights(),
                )
                .unwrap();
            let session = authority
                .bind(
                    owner(),
                    uuid::Uuid::from_u128(0xb22),
                    &[src.clone(), dst.clone()],
                    &[],
                )
                .unwrap();
            let store =
                OperationOwner::open(fs::File::open(meta.path()).unwrap(), owner()).unwrap();
            authority.attach_operations(store.clone()).unwrap();
            Self {
                authority,
                session,
                src,
                dst,
                store,
                disk,
                meta,
            }
        }
        fn stage(&self) -> std::path::PathBuf {
            fs::read_dir(self.dest(""))
                .unwrap()
                .map(|r| r.unwrap().path())
                .find(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".tilo-stage-")
                })
                .unwrap()
        }
        fn receipt_bytes(&self) -> Vec<Vec<u8>> {
            let mut names: Vec<_> = fs::read_dir(self.meta.path())
                .unwrap()
                .map(|r| r.unwrap().path())
                .filter(|p| p.extension().is_some_and(|x| x == "receipt"))
                .collect();
            names.sort();
            names.into_iter().map(|p| fs::read(p).unwrap()).collect()
        }
    }
    fn drain(job: &mut FsJob) -> Vec<FsUpdate> {
        let deadline = tilo_test_env::hang_deadline();
        let mut updates = vec![];
        loop {
            updates.extend(job.poll().unwrap());
            if matches!(updates.last(), Some(FsUpdate::Finished { .. })) {
                return updates;
            }
            assert!(
                Instant::now() < deadline,
                "terminal update deadline; release guard runs before fixture worker join"
            );
            std::thread::yield_now();
        }
    }
    fn item(u: &[FsUpdate]) -> &ItemOutcome {
        u.iter()
            .find_map(|u| {
                if let FsUpdate::Item(i) = u {
                    Some(i)
                } else {
                    None
                }
            })
            .unwrap()
    }
    fn terminal(f: &Fixture, id: &crate::recovery::OperationId) {
        let deadline = tilo_test_env::hang_deadline();
        while f.store.report(id).unwrap().state == OperationState::Running {
            assert!(
                Instant::now() < deadline,
                "terminal report deadline; roots retained for worker join"
            );
            std::thread::yield_now();
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn recovery_state_is_durable_before_and_after_every_effect() {
        // phase: recovery_storage_failure_prevents_first_effect
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"original").unwrap();
            f.hooks(Hooks {
                fail_checkpoint: Some("Admitted"),
                ..Hooks::default()
            });
            assert!(matches!(
                f.session.start(f.copy("a")),
                Err(FsError::StoreUnavailable)
            ));
            assert_eq!(fs::read(f.source("a")).unwrap(), b"original");
            assert_eq!(fs::read_dir(f.dest("")).unwrap().count(), 0);
            assert!(f.receipt_bytes().is_empty());
        }
        // phase: restart_known_stage_requires_new_grant
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"recover me").unwrap();
            f.hooks(Hooks {
                stop_after_stage: true,
                ..Hooks::default()
            });
            let (id, u) = f.run(f.copy("a"));
            assert_eq!(item(&u).state, ItemState::RecoveryRequired);
            let old = f.dst.clone();
            let stage = f.stage();
            assert_eq!(fs::read(&stage).unwrap(), b"recover me");
            let f = f.reopen();
            assert_eq!(
                f.store.report(&id).unwrap().state,
                OperationState::RecoveryRequired
            );
            let r = f.store.recoveries().unwrap().remove(0);
            assert_eq!(r.kind, RecoveryKind::KnownStaged);
            assert!(
                f.store
                    .resolve(
                        &r.id,
                        &f.session,
                        RecoveryResolution::RestoreKnown {
                            parent: old,
                            name: name("restored")
                        }
                    )
                    .is_err()
            );
            fs::write(f.dest("restored"), b"collision").unwrap();
            let mut job = f
                .store
                .resolve(
                    &r.id,
                    &f.session,
                    RecoveryResolution::RestoreKnown {
                        parent: f.dst.clone(),
                        name: name("restored"),
                    },
                )
                .unwrap();
            assert_eq!(item(&drain(&mut job)).state, ItemState::Failed);
            assert_eq!(fs::read(f.dest("restored")).unwrap(), b"collision");
            assert_eq!(fs::read(&stage).unwrap(), b"recover me");
            drop(job);
            fs::remove_file(f.dest("restored")).unwrap();
            let mut job = f
                .store
                .resolve(
                    &r.id,
                    &f.session,
                    RecoveryResolution::RestoreKnown {
                        parent: f.dst.clone(),
                        name: name("restored"),
                    },
                )
                .unwrap();
            assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
            assert_eq!(fs::read(f.dest("restored")).unwrap(), b"recover me");
            assert!(!stage.exists());
            assert!(
                f.store
                    .recoveries()
                    .unwrap()
                    .iter()
                    .find(|i| i.id == r.id)
                    .unwrap()
                    .acknowledged
            );
        }
        // phase: recovery_quota_never_deletes_staged_user_bytes
        {
            use std::os::unix::fs::MetadataExt;
            let f = Fixture::new();
            let bytes = vec![82; 1024 * 1024 + 23];
            fs::write(f.source("a"), &bytes).unwrap();
            f.hooks(Hooks {
                stop_after_stage: true,
                report_limit: Some(1),
                ..Hooks::default()
            });
            f.run(f.copy("a"));
            let stage = f.stage();
            let inode = fs::metadata(&stage).unwrap().ino();
            let receipts = f.receipt_bytes();
            assert!(matches!(
                f.session.start(f.copy("a")),
                Err(FsError::Busy | FsError::LimitExceeded)
            ));
            assert_eq!(fs::read(&stage).unwrap(), bytes);
            assert_eq!(fs::metadata(stage).unwrap().ino(), inode);
            assert_eq!(f.receipt_bytes(), receipts);
            assert_eq!(f.store.recoveries().unwrap().len(), 1);
        }
        // phase: failed_publish_checkpoint_keeps_durable_stage_recovery
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"recoverable").unwrap();
            f.hooks(Hooks {
                fail_checkpoint: Some("PublishStarted"),
                ..Hooks::default()
            });
            let (id, u) = f.run(f.copy("a"));
            assert_eq!(item(&u).state, ItemState::RecoveryRequired);
            assert!(!f.dest("a").exists());
            assert_eq!(fs::read(f.stage()).unwrap(), b"recoverable");
            let recoveries = f.store.recoveries().unwrap();
            assert_eq!(recoveries.len(), 1);
            assert_eq!(recoveries[0].kind, RecoveryKind::KnownStaged);
            assert_eq!(item(&u).recoveries, [recoveries[0].id.clone()]);
            let f = f.reopen();
            assert_eq!(
                f.store.report(&id).unwrap().state,
                OperationState::RecoveryRequired
            );
            assert_eq!(f.store.recoveries().unwrap().len(), 1);
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn cancellation_revocation_and_teardown_keep_committed_work() {
        // phase: worker_teardown_joins_after_last_owner
        {
            let f = Fixture::new();
            fs::write(f.source("a"), vec![17; 2 * 1024 * 1024]).unwrap();
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                barrier: Some(("stage-fingerprint", barrier.clone())),
                ..Hooks::default()
            });
            let job = f.session.start(f.copy("a")).unwrap();
            let id = job.operation_id();
            let release = Release(barrier.clone());
            barrier.entered();
            f.session.close();
            drop(job);
            drop(release);
            terminal(&f, &id);
            let store = f.store.clone();
            drop(f.authority);
            assert_ne!(store.report(&id).unwrap().state, OperationState::Running);
        }
    }
    #[test]
    fn queue_item_and_candidate_limits_precede_effects() {
        // phase: worker_queue_and_item_limits_precede_effects
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"queued").unwrap();
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                barrier: Some(("stage-fingerprint", barrier.clone())),
                ..Hooks::default()
            });
            let job = f.session.start(f.copy("a")).unwrap();
            let release = Release(barrier.clone());
            barrier.entered();
            assert!(matches!(f.session.start(f.copy("a")), Err(FsError::Busy)));
            let mut jobs = vec![];
            for i in 0..32 {
                let session = f
                    .authority
                    .bind(
                        owner(),
                        uuid::Uuid::from_u128(100 + i),
                        &[f.src.clone(), f.dst.clone()],
                        &[],
                    )
                    .unwrap();
                jobs.push(
                    session
                        .start(FsOperation::CreateDirectory {
                            parent: f.dst.clone(),
                            name: name(&format!("queued-{i}")),
                            conflict: ConflictPolicy::Fail,
                        })
                        .unwrap(),
                );
            }
            let session = f
                .authority
                .bind(
                    owner(),
                    uuid::Uuid::from_u128(999),
                    std::slice::from_ref(&f.dst),
                    &[],
                )
                .unwrap();
            let before = f.receipt_bytes();
            assert!(matches!(
                session.start(FsOperation::CreateDirectory {
                    parent: f.dst.clone(),
                    name: name("overflow"),
                    conflict: ConflictPolicy::Fail
                }),
                Err(FsError::Busy)
            ));
            assert_eq!(f.receipt_bytes(), before);
            assert!(!f.dest("overflow").exists());
            job.request_cancel();
            for j in &jobs {
                j.request_cancel();
            }
            drop(release);
            drop(job);
            for mut j in jobs {
                drain(&mut j);
            }
        }
        // phase: keep_both_exhaustion_preserves_every_candidate
        {
            let f = Fixture::new();
            fs::write(f.dest("a"), b"one").unwrap();
            fs::write(f.dest("a 2"), b"two").unwrap();
            f.hooks(Hooks {
                attempts: Some(2),
                ..Hooks::default()
            });
            let (_, u) = f.run(FsOperation::CreateFile {
                parent: f.dst.clone(),
                name: name("a"),
                bytes: b"new".to_vec(),
                conflict: ConflictPolicy::KeepBoth,
            });
            assert!(matches!(item(&u).error, Some(FsError::Conflict)));
            assert_eq!(fs::read(f.dest("a")).unwrap(), b"one");
            assert_eq!(fs::read(f.dest("a 2")).unwrap(), b"two");
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn stages_are_published_or_cleaned_only_when_owned_and_accounted() {
        // phase: replaced_stage_is_never_published_or_cleaned_as_owned
        {
            let f = Fixture::new();
            let bytes = vec![37; 2 * 1024 * 1024];
            fs::write(f.source("a"), &bytes).unwrap();
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                barrier: Some(("stage-fingerprint", barrier.clone())),
                ..Hooks::default()
            });
            let mut job = f.session.start(f.copy("a")).unwrap();
            let release = Release(barrier.clone());
            barrier.entered();
            let stage = f.stage();
            fs::rename(&stage, f.dest("held-provider-bytes")).unwrap();
            fs::write(&stage, b"unrelated replacement").unwrap();
            drop(release);
            let u = drain(&mut job);
            assert_ne!(item(&u).state, ItemState::Completed);
            assert!(!f.dest("a").exists());
            assert_eq!(fs::read(&stage).unwrap(), b"unrelated replacement");
            assert_eq!(fs::read(f.source("a")).unwrap(), bytes);
        }
        // phase: fix1_cleanup_sync_failure_never_claims_known_stage
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"source bytes").unwrap();
            fs::write(f.dest("a"), b"collision bytes").unwrap();
            f.hooks(Hooks {
                cleanup_sync_error: true,
                ..Hooks::default()
            });
            let (id, updates) = f.run(f.copy("a"));
            assert_eq!(item(&updates).state, ItemState::RecoveryRequired);
            let recovery = f.store.recoveries().unwrap().remove(0);
            assert_eq!(recovery.kind, RecoveryKind::NeedsRevalidation);
            assert!(!recovery.acknowledged);
            assert_eq!(fs::read_dir(f.dest("")).unwrap().count(), 1);
            assert!(f.store.acknowledge_report(&id).is_err());
            let f = f.reopen();
            assert_eq!(
                f.store.recoveries().unwrap()[0].kind,
                RecoveryKind::UnknownNativeCommit
            );
            assert!(f.store.acknowledge_report(&id).is_err());
            assert_eq!(fs::read(f.dest("a")).unwrap(), b"collision bytes");
            assert_eq!(fs::read(f.source("a")).unwrap(), b"source bytes");
        }
        // phase: fix1_conflict_cleanup_failure_preserves_restorable_stage
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"staged bytes").unwrap();
            fs::write(f.dest("a"), b"existing bytes").unwrap();
            f.hooks(Hooks {
                cleanup_error: true,
                ..Hooks::default()
            });
            let (id, updates) = f.run(f.copy("a"));
            assert_eq!(item(&updates).state, ItemState::RecoveryRequired);
            assert_eq!(fs::read(f.stage()).unwrap(), b"staged bytes");
            assert!(f.store.acknowledge_report(&id).is_err());
            let f = f.reopen();
            let recovery = f.store.recoveries().unwrap().remove(0);
            assert_eq!(recovery.kind, RecoveryKind::KnownStaged);
            let mut job = f
                .store
                .resolve(
                    &recovery.id,
                    &f.session,
                    RecoveryResolution::RestoreKnown {
                        parent: f.dst.clone(),
                        name: name("restored"),
                    },
                )
                .unwrap();
            assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
            assert_eq!(fs::read(f.dest("restored")).unwrap(), b"staged bytes");
            assert_eq!(fs::read(f.dest("a")).unwrap(), b"existing bytes");
        }
        // phase: fix1_stage_fingerprint_and_cleanup_errors_keep_unresolved_receipt
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"unverified bytes").unwrap();
            f.hooks(Hooks {
                stage_fingerprint_error: true,
                cleanup_error: true,
                ..Hooks::default()
            });
            let (id, updates) = f.run(f.copy("a"));
            assert_eq!(item(&updates).state, ItemState::RecoveryRequired);
            assert!(!item(&updates).recoveries.is_empty());
            assert_eq!(fs::read(f.stage()).unwrap(), b"unverified bytes");
            assert!(f.store.acknowledge_report(&id).is_err());
            let f = f.reopen();
            assert_eq!(
                f.store.recoveries().unwrap()[0].kind,
                RecoveryKind::NeedsRevalidation
            );
            assert!(!f.store.recoveries().unwrap()[0].acknowledged);
            assert!(f.store.acknowledge_report(&id).is_err());
            assert_eq!(fs::read(f.stage()).unwrap(), b"unverified bytes");
        }
        // phase: fix1_cancel_during_stage_fingerprint_accounts_for_stage
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"copied bytes").unwrap();
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                barrier: Some(("stage-fingerprint", barrier.clone())),
                ..Hooks::default()
            });
            let mut job = f.session.start(f.copy("a")).unwrap();
            let id = job.operation_id();
            let release = Release(barrier.clone());
            barrier.entered();
            assert_eq!(fs::read(f.stage()).unwrap(), b"copied bytes");
            job.request_cancel();
            drop(release);
            let updates = drain(&mut job);
            assert_eq!(item(&updates).state, ItemState::Cancelled);
            assert_eq!(fs::read_dir(f.dest("")).unwrap().count(), 0);
            assert!(
                f.store
                    .checkpoints(&id)
                    .unwrap()
                    .iter()
                    .any(|p| p == "StageDiscarded")
            );
            drop(job);
            let f = f.reopen();
            assert!(f.store.recoveries().unwrap().is_empty());
            f.store.acknowledge_report(&id).unwrap();
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn restore_binds_fresh_separate_grants_and_reports_its_commit() {
        // phase: restore_rejects_unbound_changed_and_revoked_stages
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"expected").unwrap();
            f.hooks(Hooks {
                stop_after_stage: true,
                ..Hooks::default()
            });
            f.run(f.copy("a"));
            let f = f.reopen();
            let recovery = f.store.recoveries().unwrap().remove(0);
            let unbound = f
                .authority
                .bind(
                    owner(),
                    uuid::Uuid::from_u128(0xc33),
                    std::slice::from_ref(&f.src),
                    &[],
                )
                .unwrap();
            assert!(matches!(
                f.store.resolve(
                    &recovery.id,
                    &unbound,
                    RecoveryResolution::RestoreKnown {
                        parent: f.src.clone(),
                        name: name("restore")
                    }
                ),
                Err(FsError::Denied)
            ));
            let stage = f.stage();
            fs::write(&stage, b"modified").unwrap();
            let mut job = f
                .store
                .resolve(
                    &recovery.id,
                    &f.session,
                    RecoveryResolution::RestoreKnown {
                        parent: f.dst.clone(),
                        name: name("restore"),
                    },
                )
                .unwrap();
            assert!(matches!(
                item(&drain(&mut job)).error,
                Some(FsError::StaleEntry)
            ));
            drop(job);
            assert!(!f.dest("restore").exists());
            assert_eq!(fs::read(&stage).unwrap(), b"modified");
            f.authority.revoke_directory(&f.dst).unwrap();
            assert!(
                f.store
                    .resolve(
                        &recovery.id,
                        &f.session,
                        RecoveryResolution::RestoreKnown {
                            parent: f.dst.clone(),
                            name: name("restore")
                        }
                    )
                    .is_err()
            );
            assert!(!f.store.recoveries().unwrap()[0].acknowledged);
        }
        // phase: restore_checkpoint_failure_reports_committed_destination
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"restore bytes").unwrap();
            f.hooks(Hooks {
                stop_after_stage: true,
                ..Hooks::default()
            });
            f.run(f.copy("a"));
            let recovery = f.store.recoveries().unwrap().remove(0);
            f.hooks(Hooks {
                fail_checkpoint: Some("Published"),
                ..Hooks::default()
            });
            let mut job = f
                .store
                .resolve(
                    &recovery.id,
                    &f.session,
                    RecoveryResolution::RestoreKnown {
                        parent: f.dst.clone(),
                        name: name("restored"),
                    },
                )
                .unwrap();
            let u = drain(&mut job);
            assert_eq!(item(&u).state, ItemState::OutcomeUnknown);
            assert!(item(&u).destination.is_some());
            assert!(!item(&u).recoveries.is_empty());
            assert_eq!(fs::read(f.dest("restored")).unwrap(), b"restore bytes");
            assert!(
                !f.store
                    .recoveries()
                    .unwrap()
                    .iter()
                    .find(|r| r.id == recovery.id)
                    .unwrap()
                    .acknowledged
            );
        }
        // phase: restore_uses_separate_read_and_create_grants
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"separate rights").unwrap();
            f.hooks(Hooks {
                stop_after_stage: true,
                ..Hooks::default()
            });
            f.run(f.copy("a"));
            f.hooks(Hooks::default());
            let create = f
                .authority
                .admit_directory(
                    fs::File::open(f.dest("")).unwrap(),
                    owner(),
                    vec![FsRight::Create],
                )
                .unwrap();
            let read = f
                .authority
                .admit_directory(
                    fs::File::open(f.dest("")).unwrap(),
                    owner(),
                    vec![FsRight::Read],
                )
                .unwrap();
            let session = f
                .authority
                .bind(
                    owner(),
                    uuid::Uuid::from_u128(0xb22),
                    &[create.clone(), read],
                    &[],
                )
                .unwrap();
            let id = f.store.recoveries().unwrap()[0].id.clone();
            let mut job = f
                .store
                .resolve(
                    &id,
                    &session,
                    RecoveryResolution::RestoreKnown {
                        parent: create,
                        name: name("restored"),
                    },
                )
                .unwrap();
            assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
            assert_eq!(fs::read(f.dest("restored")).unwrap(), b"separate rights");
        }
    }
    #[test]
    fn metadata_only_reopen_requires_fresh_readable_reconciliation() {
        let f = Fixture::new();
        fs::write(f.source("a"), b"pending bytes").unwrap();
        f.hooks(Hooks {
            stop_after_stage: true,
            fail_checkpoint: Some("Item"),
            ..Hooks::default()
        });
        let job = f.session.start(f.copy("a")).unwrap();
        let id = job.operation_id();
        drop(job);
        let Fixture {
            authority,
            session,
            src,
            dst,
            store,
            disk,
            meta,
        } = f;
        drop(authority);
        drop(session);
        drop(src);
        drop(dst);
        drop(store);
        let store = OperationOwner::open(fs::File::open(meta.path()).unwrap(), owner()).unwrap();
        assert_eq!(
            store.recoveries().unwrap()[0].kind,
            RecoveryKind::NeedsRevalidation
        );
        assert!(store.acknowledge_report(&id).is_err());
        let authority = NativeGrantAuthority::new();
        let src = authority
            .admit_directory(
                fs::File::open(disk.path().join("src")).unwrap(),
                owner(),
                rights(),
            )
            .unwrap();
        authority.attach_operations(store.clone()).unwrap();
        assert_eq!(
            store.recoveries().unwrap()[0].kind,
            RecoveryKind::NeedsRevalidation
        );
        let dst = authority
            .admit_directory(
                fs::File::open(disk.path().join("dst")).unwrap(),
                owner(),
                rights(),
            )
            .unwrap();
        authority.attach_operations(store.clone()).unwrap();
        assert_eq!(
            store.recoveries().unwrap()[0].kind,
            RecoveryKind::KnownStaged
        );
        let restricted = authority
            .bind(
                owner(),
                uuid::Uuid::from_u128(0xb22),
                std::slice::from_ref(&src),
                &[],
            )
            .unwrap();
        let recovery = store.recoveries().unwrap()[0].id.clone();
        assert!(matches!(
            store.resolve(
                &recovery,
                &restricted,
                RecoveryResolution::RestoreKnown {
                    parent: src.clone(),
                    name: name("restore")
                }
            ),
            Err(FsError::Denied)
        ));
        let session = authority
            .bind(
                owner(),
                uuid::Uuid::from_u128(0xc33),
                &[src, dst.clone()],
                &[],
            )
            .unwrap();
        let mut job = store
            .resolve(
                &recovery,
                &session,
                RecoveryResolution::RestoreKnown {
                    parent: dst,
                    name: name("restore"),
                },
            )
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        assert_eq!(
            fs::read(disk.path().join("dst/restore")).unwrap(),
            b"pending bytes"
        );
        drop(job);
        drop(restricted);
        drop(session);
        drop(authority);
        drop(store);
        drop(disk);
        drop(meta);
    }
    #[test]
    fn fix1_an_interrupted_published_create_requires_revalidation() {
        let f = Fixture::new();
        f.hooks(Hooks {
            fail_checkpoint: Some("Item"),
            ..Hooks::default()
        });
        let operation = FsOperation::CreateFile {
            parent: f.dst.clone(),
            name: name("a"),
            bytes: b"published bytes".to_vec(),
            conflict: ConflictPolicy::Fail,
        };
        let job = f.session.start(operation).unwrap();
        let id = job.operation_id();
        drop(job);
        let Fixture {
            authority,
            session,
            src,
            dst,
            store,
            disk,
            meta,
        } = f;
        drop(authority);
        drop(session);
        drop(src);
        drop(dst);
        drop(store);
        let store = OperationOwner::open(fs::File::open(meta.path()).unwrap(), owner()).unwrap();
        let recovery = store.recoveries().unwrap().remove(0);
        assert_eq!(recovery.kind, RecoveryKind::NeedsRevalidation);
        assert!(!recovery.acknowledged);
        assert!(store.acknowledge_report(&id).is_err());
        assert_eq!(
            fs::read(disk.path().join("dst/a")).unwrap(),
            b"published bytes"
        );
        let authority = NativeGrantAuthority::new();
        let src = authority
            .admit_directory(
                fs::File::open(disk.path().join("src")).unwrap(),
                owner(),
                rights(),
            )
            .unwrap();
        let dst = authority
            .admit_directory(
                fs::File::open(disk.path().join("dst")).unwrap(),
                owner(),
                rights(),
            )
            .unwrap();
        authority.attach_operations(store.clone()).unwrap();
        let observed = store.recoveries().unwrap().remove(0);
        assert!(!observed.acknowledged);
        assert_eq!(observed.kind, RecoveryKind::UnknownNativeCommit);
        let session = authority
            .bind(
                owner(),
                uuid::Uuid::from_u128(0xb22),
                &[src.clone(), dst.clone()],
                &[],
            )
            .unwrap();
        let mut job = store
            .resolve(
                &observed.id,
                &session,
                RecoveryResolution::AcknowledgeUnknown {
                    note: "observed published create; accepting interrupted outcome".into(),
                },
            )
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        drop(job);
        drop(session);
        store.acknowledge_report(&id).unwrap();
        assert_eq!(
            fs::read(disk.path().join("dst/a")).unwrap(),
            b"published bytes"
        );
        drop(authority);
        drop(src);
        drop(dst);
        drop(store);
        drop(disk);
        drop(meta);
    }
    #[test]
    fn queued_directory_effect_refuses_closed_session_or_revoked_parent() {
        for revoke in [false, true] {
            let f = Fixture::new();
            fs::write(f.source("blocking"), b"held chunk").unwrap();
            let barrier = Arc::new(Barrier::default());
            let release = Release(Arc::clone(&barrier));
            f.hooks(Hooks {
                barrier: Some(("stage-fingerprint", Arc::clone(&barrier))),
                ..Hooks::default()
            });
            let mut blocking = f.session.start(f.copy("blocking")).unwrap();
            barrier.entered();
            let path = f.disk.path().join("queued-root");
            fs::create_dir(&path).unwrap();
            let root = f
                .authority
                .admit_directory(fs::File::open(&path).unwrap(), owner(), rights())
                .unwrap();
            let session = f
                .authority
                .bind(
                    owner(),
                    uuid::Uuid::from_u128(0xc105),
                    std::slice::from_ref(&root),
                    &[],
                )
                .unwrap();
            let mut job = session
                .start(FsOperation::CreateDirectory {
                    parent: root.clone(),
                    name: name("must-not-exist"),
                    conflict: ConflictPolicy::Fail,
                })
                .unwrap();
            let id = job.operation_id();
            if revoke {
                f.authority.revoke_directory(&root).unwrap();
            } else {
                session.close();
            }
            drop(release);
            let blocking_updates = drain(&mut blocking);
            assert_eq!(item(&blocking_updates).state, ItemState::Completed);
            let updates = drain(&mut job);
            let expected = if revoke {
                ItemState::Failed
            } else {
                ItemState::Cancelled
            };
            assert_eq!(item(&updates).state, expected);
            assert_eq!(
                matches!(&item(&updates).error, Some(FsError::Revoked)),
                revoke
            );
            assert_ne!(
                matches!(&item(&updates).error, Some(FsError::Closed)),
                revoke
            );
            assert!(!path.join("must-not-exist").exists());
            let report = f.store.report(&id).unwrap();
            assert_eq!(report.items.first().unwrap().state, expected);
            assert_eq!(fs::read(f.dest("blocking")).unwrap(), b"held chunk");
        }
    }
}
