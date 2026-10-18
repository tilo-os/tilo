//! Bounded descriptor-backed mutation jobs. Only the host authority owns the
//! joinable worker; queued executions retain scope and durable reports.
use crate::native::{MutationSource, NativeIdentity, NativeObject, io_error, lock};
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

#[path = "operations/replacement.rs"]
mod replacement;
use replacement::replace_entry;
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
    /// Atomic same-source replacing rename; captures the displaced inode without
    /// reading bytes. The host must enroll its protected capture in history.
    Replace {
        entry: GrantedEntry,
        into: DirectoryGrant,
        name: EntryName,
        victim: GrantedEntry,
    },
    Copy {
        entries: Vec<GrantedEntry>,
        into: DirectoryGrant,
        conflict: ConflictPolicy,
    },
    Move {
        entries: Vec<GrantedEntry>,
        into: DirectoryGrant,
        conflict: ConflictPolicy,
    },
    Trash {
        entries: Vec<GrantedEntry>,
    },
    /// Native host removal under explicit Purge authority and protected capture.
    Remove {
        entry: GrantedEntry,
        directory: bool,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ItemState {
    Completed,
    Failed,
    Cancelled,
    CopiedSourceRetained,
    RecoveryRequired,
    OutcomeUnknown,
}
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
    created_temporary: Option<Arc<NativeCreatedTemporary>>,
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
mod created_temporary;
pub use created_temporary::NativeCreatedTemporary;

impl FsJob {
    /// Obtain only the opaque custody minted by this exact durable `CreateFile`
    /// job. A polled receipt, UUID, name or caller label cannot construct it.
    /// # Errors
    /// Refuses unfinished/failed/noncreation jobs and expired native authority.
    pub fn created_temporary(&self) -> Result<Arc<NativeCreatedTemporary>, FsError> {
        let value = lock(&self.control.results)?
            .created_temporary
            .clone()
            .ok_or(FsError::Denied)?;
        value.check()?;
        Ok(value)
    }
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
/// Genuine admitted replacement with no native effects submitted yet. This
/// opaque host object supplies the actual operation ID for signed prepare intent.
pub struct PreparedNativeReplacement {
    reservation: Option<QueueReservation>,
    work: Option<Work>,
    job: Option<FsJob>,
    source_owner: [u8; 32],
    source: [u64; 2],
    expected_victim: [u64; 2],
}
impl PreparedNativeReplacement {
    /// Maximum descriptive metadata observation; these bytes grant no authority.
    pub const DESCRIPTION_LIMIT: usize = 2 * 1024 * 1024;
    /// Identifier of this live admitted replacement.
    /// # Errors
    /// Returns `FsError::Closed` when the reservation has been consumed.
    pub fn operation_id(&self) -> Result<&OperationId, FsError> {
        self.job.as_ref().map(|job| &job.id).ok_or(FsError::Closed)
    }
    /// Descriptive purpose of this same live held native reservation; it is
    /// already part of the signed intent bytes, not an execution authority.
    /// # Errors
    /// Refuses consumed reservations or stale native intent observations.
    pub fn purpose(&self) -> Result<NativeReplacementPurpose, FsError> {
        self.intent_digest()?;
        Ok(self
            .work
            .as_ref()
            .ok_or(FsError::Closed)?
            .replacement_purpose)
    }
    pub fn source_owner(&self) -> [u8; 32] {
        self.source_owner
    }
    pub fn source_identity(&self) -> [u64; 2] {
        self.source
    }
    pub fn expected_victim_identity(&self) -> [u64; 2] {
        self.expected_victim
    }
    /// Observe a domain-separated digest of this actual admitted namespace intent.
    /// Hardlink aliases are distinguished by held parent identities and raw loci.
    /// The observation itself grants no execution or canonical history authority.
    /// # Errors
    /// Refuses closed reservations, changed source scope or inconsistent intent.
    pub fn intent_digest(&self) -> Result<[u8; 32], FsError> {
        let work = self.work.as_ref().ok_or(FsError::Closed)?;
        let expected = work.prepared_intent.as_ref().ok_or(FsError::Denied)?;
        let actual = replacement_intent(work, expected.owner)?;
        if actual != *expected {
            return Err(FsError::StaleEntry);
        }
        Ok(expected.digest)
    }
    /// Exact source-minted canonical metadata bytes hashed by `intent_digest`.
    /// This description cannot construct Work, grants or history authority.
    /// # Errors
    /// Refuses closed/stale reservations and oversized metadata descriptions.
    pub fn reservation_description(&self) -> Result<Vec<u8>, FsError> {
        self.intent_digest()?;
        let intent = self
            .work
            .as_ref()
            .and_then(|work| work.prepared_intent.as_ref())
            .ok_or(FsError::Closed)?;
        if intent.description.len() > Self::DESCRIPTION_LIMIT {
            return Err(FsError::LimitExceeded);
        }
        let description = intent.description.as_ref().clone();
        self.intent_digest()?;
        Ok(description)
    }
    /// Observe this actual admitted future protected capture plan. This is not
    /// evidence of an existing capture, `RecoveryId`, or immutable byte archive.
    /// # Errors
    /// Refuses closed reservations, changed intent or non-prepared journal state.
    pub fn archive_plan_digest(&self) -> Result<[u8; 32], FsError> {
        let intent = self.intent_digest()?;
        let work = self.work.as_ref().ok_or(FsError::Closed)?;
        let step = work.owner.step(&work.id, 0)?;
        if step.phase != "Admitted"
            || work.owner.report(&work.id)?.state != crate::recovery::OperationState::Running
        {
            return Err(FsError::Closed);
        }
        let stage = replacement::stage_component(&work.id, 0)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo-native-protected-replacement-capture-plan-v1\0");
        hash.update(work.id.native().as_bytes());
        hash.update(&intent);
        hash.update(&(stage.as_os_str().as_bytes().len() as u64).to_le_bytes());
        hash.update(stage.as_os_str().as_bytes());
        self.intent_digest()?;
        Ok(*hash.finalize().as_bytes())
    }
    /// Verify and durably retain the enrolled daemon's exact preparation before
    /// submitting this actual source-held reservation. A receipt cannot create
    /// a reservation or grant Replace rights.
    /// # Errors
    /// Refuses stale scope, mismatched intent/plan, invalid signer or journal loss.
    pub fn start_with_history(
        mut self,
        prepared: &crate::external_history::PreparedReceipt,
    ) -> Result<FsJob, FsError> {
        let intent = self.intent_digest()?;
        let archive = self.archive_plan_digest()?;
        let work = self.work.as_ref().ok_or(FsError::Closed)?;
        let verifier = work.session.0.retained_history_verifier()?;
        prepared.verify(&verifier).map_err(|_| FsError::Denied)?;
        if prepared.operation_id != work.id.native().to_string()
            || prepared.source_owner != self.source_owner
            || prepared.intent_digest != intent
            || prepared.archive_binding_digest != archive
        {
            return Err(FsError::Denied);
        }
        let Request::Operation(FsOperation::Replace {
            entry,
            into,
            victim,
            ..
        }) = &work.request
        else {
            return Err(FsError::Denied);
        };
        work.session.0.with_retained_replacement_scope(
            entry,
            victim,
            into,
            self.source_owner,
            || work.owner.bind_history_prepare(&work.id, prepared),
        )?;
        self.intent_digest()?;
        self.work
            .as_mut()
            .ok_or(FsError::Closed)?
            .verified_history_prepare = Some(Arc::new(prepared.clone()));
        self.start()
    }
    /// Submit only after the enrolled canonical host has durably signed prepare.
    /// Native effects still revalidate actual grants/cancellation at their boundary.
    /// # Errors
    /// Refuses closed queue or original source scope.
    pub fn start(mut self) -> Result<FsJob, FsError> {
        self.intent_digest()?;
        let work = self.work.as_ref().ok_or(FsError::Closed)?;
        if work.session.0.has_selected_replace_authority()?
            && work.verified_history_prepare.is_none()
        {
            return Err(FsError::Denied);
        }
        self.work
            .as_ref()
            .ok_or(FsError::Closed)?
            .session
            .0
            .retained_source_owner_key()?;
        let work = self.work.take().ok_or(FsError::Closed)?;
        self.reservation
            .take()
            .ok_or(FsError::Closed)?
            .submit(work)?;
        self.job.take().ok_or(FsError::Closed)
    }
}
type MetadataFacts = (u64, [i64; 2], [i64; 2], u64);
fn metadata_facts(metadata: &cap_std::fs::Metadata) -> MetadataFacts {
    (
        metadata.len(),
        [metadata.mtime(), metadata.mtime_nsec()],
        [metadata.ctime(), metadata.ctime_nsec()],
        metadata.nlink(),
    )
}
#[derive(Clone, Eq, PartialEq)]
struct ReplacementIntent {
    owner: [u8; 32],
    digest: [u8; 32],
    permissions: (u32, u32, u32),
    attributes: Arc<crate::native::xattrs::NativeAttributes>,
    description: Arc<Vec<u8>>,
    facts: (MetadataFacts, MetadataFacts),
}
fn replacement_intent(work: &Work, source_owner: [u8; 32]) -> Result<ReplacementIntent, FsError> {
    let session = &work.session.0;
    let owner = session.retained_source_owner_key()?;
    if owner != source_owner {
        return Err(FsError::Denied);
    }
    let Request::Operation(FsOperation::Replace {
        entry,
        into,
        name,
        victim,
    }) = &work.request
    else {
        return Err(FsError::Denied);
    };
    let source = session.mutation_source(entry, FsRight::Rename)?;
    let old = session.mutation_source(victim, FsRight::Replace)?;
    let parent = session.mutation_directory(into)?;
    let parent_identity = NativeIdentity::of(&parent.dir_metadata().map_err(io_error)?);
    let old_parent = NativeIdentity::of(&old.parent.dir_metadata().map_err(io_error)?);
    let source_parent = NativeIdentity::of(&source.parent.dir_metadata().map_err(io_error)?);
    let source_id = source.object.identity();
    let victim_id = old.object.identity();
    if parent_identity != old_parent || &old.name != name {
        return Err(FsError::Denied);
    }
    let source_metadata = source
        .parent
        .symlink_metadata(source.name.as_os_str())
        .map_err(io_error)?;
    let victim_metadata = old
        .parent
        .symlink_metadata(old.name.as_os_str())
        .map_err(io_error)?;
    if source_metadata.nlink() != 1 {
        return Err(FsError::Unsupported(
            "replacement temporary must have one link",
        ));
    }
    if NativeIdentity::of(&source_metadata) != source_id
        || NativeIdentity::of(&victim_metadata) != victim_id
    {
        return Err(FsError::StaleEntry);
    }
    let metadata = (
        (
            source_metadata.uid(),
            source_metadata.gid(),
            source_metadata.mode() & 0o7777,
        ),
        (
            victim_metadata.uid(),
            victim_metadata.gid(),
            victim_metadata.mode() & 0o7777,
        ),
    );
    let facts = (
        metadata_facts(&source_metadata),
        metadata_facts(&victim_metadata),
    );
    let attributes = session.replacement_attributes(victim)?;
    let destination = session.mutation_locator(into, name, Some(source_id))?;
    let custody = session.replacement_directory(entry)?;
    custody.check()?;
    // Postcard's typed tuple encoding includes raw component lengths and
    // component-vector boundaries. None of these bytes construct a grant.
    let bytes = postcard::to_stdvec(&(
        *work.id.native().as_bytes(),
        owner,
        source_parent,
        parent_identity,
        custody.identity,
        metadata,
        facts,
        &attributes,
        &source.locator,
        &old.locator,
        &destination,
        work.replacement_purpose,
    ))
    .map_err(|_| FsError::StoreUnavailable)?;
    if bytes.len() > PreparedNativeReplacement::DESCRIPTION_LIMIT {
        return Err(FsError::LimitExceeded);
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-native-retained-replacement-intent-v2\0");
    hash.update(&bytes);
    session.retained_source_owner_key().and_then(|current| {
        if current == owner {
            Ok(ReplacementIntent {
                owner,
                digest: *hash.finalize().as_bytes(),
                permissions: metadata.1,
                attributes: Arc::new(attributes),
                description: Arc::new(bytes),
                facts,
            })
        } else {
            Err(FsError::Denied)
        }
    })
}
impl Drop for PreparedNativeReplacement {
    fn drop(&mut self) {
        if let Some(work) = &self.work {
            let outcome = ItemOutcome {
                item: 0,
                state: ItemState::Cancelled,
                destination: None,
                error: Some(FsError::Cancelled),
                recoveries: Vec::new(),
            };
            if work.owner.item(&work.id, &outcome).is_ok() {
                let _ = work.owner.finish(&work.id);
            }
        }
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
pub use crate::external_history::NativeReplacementPurpose;
struct Work {
    session: ActiveSession,
    owner: Arc<OperationOwner>,
    id: OperationId,
    control: Arc<Control>,
    request: Request,
    replacement_purpose: NativeReplacementPurpose,
    prepared_intent: Option<ReplacementIntent>,
    verified_history_prepare: Option<Arc<crate::external_history::PreparedReceipt>>,
    #[cfg(test)]
    hooks: tests::Hooks,
}
enum Request {
    RetireCreatedTemporary(Arc<NativeCreatedTemporary>),
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
impl FsOperation {
    fn count(&self) -> usize {
        match self {
            Self::Copy { entries, .. } | Self::Move { entries, .. } | Self::Trash { entries } => {
                entries.len()
            }
            _ => 1,
        }
    }
}
impl FsSession {
    /// Reserve an actual replacing job before any namespace effect, for the
    /// enrolled daemon's canonical signed external-effect prepare transaction.
    /// # Errors
    /// Refuses absent authentic source owner, stale grants or durable admission.
    pub fn prepare_replace(
        &self,
        entry: GrantedEntry,
        into: DirectoryGrant,
        name: EntryName,
        victim: GrantedEntry,
    ) -> Result<PreparedNativeReplacement, FsError> {
        self.prepare_replace_for(
            entry,
            into,
            name,
            victim,
            NativeReplacementPurpose::ReplacementRename,
        )
    }
    /// Only the authentic selected scope and its own completed creator cap can
    /// mint the document-preserving save purpose. No caller boolean upgrades a
    /// generic Replace operation into a Save transaction.
    pub(crate) fn prepare_atomic_save(
        &self,
        temporary: &NativeCreatedTemporary,
        into: DirectoryGrant,
        name: EntryName,
        victim: GrantedEntry,
    ) -> Result<PreparedNativeReplacement, FsError> {
        if !self.has_selected_replace_authority()? {
            return Err(FsError::Denied);
        }
        let entry = temporary.selected_entry(self)?;
        self.prepare_replace_for(
            entry,
            into,
            name,
            victim,
            NativeReplacementPurpose::AtomicSave,
        )
    }
    fn prepare_replace_for(
        &self,
        entry: GrantedEntry,
        into: DirectoryGrant,
        name: EntryName,
        victim: GrantedEntry,
        purpose: NativeReplacementPurpose,
    ) -> Result<PreparedNativeReplacement, FsError> {
        let source_owner = self.retained_source_owner_key()?;
        let source = self.entry_identity(&entry)?;
        let expected_victim = self.entry_identity(&victim)?;
        self.validate_selected_temporary(&entry)?;
        self.validate_selected_victim(&victim)?;
        self.mutation_source(&entry, FsRight::Rename)?;
        self.mutation_source(&entry, FsRight::Write)?;
        self.mutation_source(&victim, FsRight::Replace)?;
        self.mutation_directory(&into)?;
        let owner = self.operation_store()?;
        let request = Request::Operation(FsOperation::Replace {
            entry,
            into,
            name,
            victim,
        });
        let (reservation, mut work, job) = prepare_admission(self, owner, request, 1)?;
        work.replacement_purpose = purpose;
        let mut prepared = PreparedNativeReplacement {
            reservation: Some(reservation),
            work: Some(work),
            job: Some(job),
            source_owner,
            source,
            expected_victim,
        };
        let intent =
            replacement_intent(prepared.work.as_ref().ok_or(FsError::Closed)?, source_owner)?;
        prepared
            .work
            .as_mut()
            .ok_or(FsError::Closed)?
            .prepared_intent = Some(intent);
        Ok(prepared)
    }
    /// Admit a bounded mutation job only after a durable scoped receipt exists.
    /// # Errors
    /// Returns `StoreUnavailable`, `Busy`, `LimitExceeded`, or live scope errors
    /// before admission. Individual source failures are durable item outcomes.
    pub fn start(&self, operation: FsOperation) -> Result<FsJob, FsError> {
        if matches!(operation, FsOperation::Replace { .. })
            && self.has_selected_replace_authority()?
        {
            return Err(FsError::Denied);
        }
        let count = operation.count();
        if count > MAX_ITEMS || count == 0 {
            return Err(FsError::LimitExceeded);
        }
        let owner = self.operation_store()?;
        if matches!(operation, FsOperation::Trash { .. }) {
            #[cfg(not(test))]
            return Err(FsError::Unsupported("native trash adapter not installed"));
            #[cfg(test)]
            if !lock(&owner.hooks)?.unknown_trash {
                return Err(FsError::Unsupported("native trash adapter not installed"));
            }
        }
        admit(self, owner, Request::Operation(operation), count)
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
    if matches!(&request, Request::Operation(FsOperation::CreateFile { .. })) {
        session.check_created_temporary_capacity()?;
    }
    let id = owner.admit(session.mount_id(), count)?;
    let control = Arc::new(Control {
        cancel: AtomicBool::new(false),
        results: Mutex::new(Results {
            queue: VecDeque::with_capacity(MAX_ITEMS * 4 + 1),
            sequence: 0,
            last_progress: None,
            error: false,
            publication_error: None,
            created_temporary: None,
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
        replacement_purpose: NativeReplacementPurpose::ReplacementRename,
        prepared_intent: None,
        verified_history_prepare: None,
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
        // One replaceable update per item/phase, preserving phase ordering and
        // reserving bounded terminal slots for every admitted item plus Finished.
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
    let count = match &work.request {
        Request::Operation(op) => op.count(),
        _ => 1,
    };
    let mut created_temporary = None;
    let mut completed = 0_usize;
    let mut failed = 0_usize;
    let mut cancelled = 0_usize;
    for index in 0..count {
        let mut ctx = Context {
            work: &work,
            item: index,
            bytes: 0,
            staged_identity: None,
            stage_created: false,
            stage_published: false,
        };
        #[cfg(test)]
        if index == 1 {
            ctx.barrier("before-item-two");
        }
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
        if outcome.state == ItemState::Completed
            && let Request::Operation(FsOperation::CreateFile { parent, .. }) = &work.request
            && let Some(entry) = &outcome.destination
        {
            created_temporary = NativeCreatedTemporary::mint(&work, parent, entry)
                .ok()
                .map(Arc::new);
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
    // Keep mutation admission until the actual completed create owns its
    // reserved registry slot. Releasing earlier lets a second create pass the
    // pre-effect capacity check while this completed cap is still unregistered.
    #[cfg(test)]
    if created_temporary.is_some()
        && let Some(("CreatedTemporaryBeforeRegistrationBoundary", barrier)) = &work.hooks.barrier
    {
        barrier.hold();
    }
    if let Some(temporary) = &created_temporary
        && let Err(error) = temporary.register()
    {
        if let Ok(mut results) = work.control.results.lock() {
            results.created_temporary = created_temporary;
            results.error = true;
            if let FsError::Publication(publication) = error {
                results.publication_error.get_or_insert(publication);
            }
        }
        return;
    }
    // Release admission before Finished can be consumed and the next UI action starts.
    let control = Arc::clone(&work.control);
    drop(work);
    if let Ok(mut results) = control.results.lock() {
        results.created_temporary = created_temporary;
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
        Request::RetireCreatedTemporary(temporary) => created_temporary::retire(ctx, temporary),
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
    let selected = match operation {
        FsOperation::Copy { entries, .. }
        | FsOperation::Move { entries, .. }
        | FsOperation::Trash { entries } => entries.get(ctx.item),
        _ => None,
    };
    let result = match operation {
        FsOperation::CreateDirectory {
            parent,
            name,
            conflict,
        } => copy_publish(
            ctx,
            PublishRequest {
                source: None,
                into: parent,
                name,
                policy: *conflict,
                bytes: None,
                moving: false,
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
                source: None,
                into: parent,
                name,
                policy: *conflict,
                bytes: Some(bytes),
                moving: false,
            },
        ),
        FsOperation::Copy {
            entries: _,
            into,
            conflict,
        } => match selected.ok_or(FsError::LimitExceeded).and_then(|entry| {
            ctx.session()
                .mutation_source(entry, FsRight::Copy)
                .map(|source| (entry, source))
        }) {
            Ok((entry, source)) => {
                let name = source.name.clone();
                copy_publish(
                    ctx,
                    PublishRequest {
                        source: Some((entry, source)),
                        into,
                        name: &name,
                        policy: *conflict,
                        bytes: None,
                        moving: false,
                    },
                )
            }
            Err(e) => Err(e),
        },
        FsOperation::Move {
            entries: _,
            into,
            conflict,
        } => selected
            .ok_or(FsError::LimitExceeded)
            .and_then(|entry| move_entry(ctx, entry, into, *conflict)),
        FsOperation::Rename { entry, name } => rename_entry(ctx, entry, name),
        FsOperation::Replace {
            entry,
            into,
            name,
            victim,
        } => replace_entry(ctx, entry, into, name, victim),
        FsOperation::Remove { entry, directory } => remove_entry(ctx, entry, *directory),
        FsOperation::Trash { .. } => selected
            .ok_or(FsError::LimitExceeded)
            .and_then(|entry| trash(ctx, entry)),
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
    EntryName::new(OsString::from_vec(result))
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
fn reject_ancestry(source: &MutationSource, destination: &Dir) -> Result<(), FsError> {
    if !matches!(source.object, NativeObject::Directory { .. }) {
        return Ok(());
    }
    let target = source.object.identity();
    let mut current = destination.try_clone().map_err(io_error)?;
    for _ in 0..1024 {
        let here = NativeIdentity::of(&current.dir_metadata().map_err(io_error)?);
        if here == target {
            return Err(FsError::Denied);
        }
        let parent = rustix::fs::openat(
            &current,
            "..",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| io_error(e.into()))?;
        let parent = Dir::from_std_file(std::fs::File::from(parent));
        if NativeIdentity::of(&parent.dir_metadata().map_err(io_error)?) == here {
            return Ok(());
        }
        current = parent;
    }
    Err(FsError::LimitExceeded)
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
struct CaptureRestoreFailure {
    error: FsError,
    published: bool,
}
fn restore_captured(
    ctx: &Context<'_>,
    source: &MutationSource,
    custody: &crate::native::removal::RemovalCustody,
    stage: &EntryName,
) -> Result<(), CaptureRestoreFailure> {
    let mut published = false;
    ctx.session()
        .captured_effect(custody, || {
            // The guard is inaccessible to external original-UID processes. Restore
            // never overwrites a name that appeared after capture.
            no_replace(&custody.directory, stage, &source.parent, &source.name)
                .map_err(|e| io_error(e.into()))?;
            published = true;
            sync_dir(&source.parent)?;
            sync_dir(&custody.directory)
        })
        .map_err(|error| CaptureRestoreFailure { error, published })
}
fn restore_refused_capture(
    ctx: &Context<'_>,
    source: &MutationSource,
    custody: &crate::native::removal::RemovalCustody,
    stage: &EntryName,
    mut step: Step,
    refusal: FsError,
) -> Result<ItemOutcome, FsError> {
    if let Err(failure) = restore_captured(ctx, source, custody, stage) {
        let kind = if failure.published {
            RecoveryKind::UnknownNativeCommit
        } else {
            RecoveryKind::QuarantinedNative
        };
        return Ok(ctx.recovery(step, kind, failure.error, None));
    }
    if let Err(error) = ctx.checkpoint(&mut step, "RemoveRestored") {
        return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, None));
    }
    Err(refusal)
}
fn remove_entry(
    ctx: &Context<'_>,
    entry: &GrantedEntry,
    directory: bool,
) -> Result<ItemOutcome, FsError> {
    let source = ctx.session().mutation_source(entry, FsRight::Purge)?;
    let custody = ctx.session().removal_directory(entry)?;
    let stage = EntryName::new(OsString::from(format!(
        "removed-{}-{}",
        ctx.work.id.native(),
        ctx.item
    )))?;
    let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
    step.source = Some(source.locator.clone());
    step.stage = Some(crate::native::NativeLocator {
        root: custody.identity,
        parents: Vec::new(),
        name: stage.as_os_str().as_bytes().to_vec(),
        identity: None,
    });
    ctx.checkpoint(&mut step, "RemoveCaptureStarted")?;
    if let Some(outcome) = capture_removal(ctx, entry, &source, &custody, &stage, &step)? {
        return Ok(outcome);
    }
    commit_captured_removal(ctx, &source, &custody, &stage, directory, step)
}
fn capture_removal(
    ctx: &Context<'_>,
    entry: &GrantedEntry,
    source: &MutationSource,
    custody: &crate::native::removal::RemovalCustody,
    stage: &EntryName,
    step: &Step,
) -> Result<Option<ItemOutcome>, FsError> {
    let mut captured = false;
    let capture = ctx.effect(Some((entry, FsRight::Purge)), None, || {
        custody.check()?;
        check_named(&source.parent, &source.name, source.object.identity())?;
        #[cfg(test)]
        ctx.barrier("RemoveCaptureBoundary");
        no_replace(&source.parent, &source.name, &custody.directory, stage)
            .map_err(|e| io_error(e.into()))?;
        captured = true;
        sync_dir(&source.parent)?;
        sync_dir(&custody.directory)
    });
    if let Err(error) = capture {
        return if captured {
            Ok(Some(ctx.recovery(
                step.clone(),
                RecoveryKind::QuarantinedNative,
                error,
                None,
            )))
        } else {
            Err(error)
        };
    }
    Ok(None)
}
fn commit_captured_removal(
    ctx: &Context<'_>,
    source: &MutationSource,
    custody: &crate::native::removal::RemovalCustody,
    stage: &EntryName,
    directory: bool,
    mut step: Step,
) -> Result<ItemOutcome, FsError> {
    let observed = match custody.directory.symlink_metadata(stage.as_os_str()) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Ok(ctx.recovery(step, RecoveryKind::QuarantinedNative, io_error(error), None));
        }
    };
    let actual = NativeIdentity::of(&observed);
    step.stage
        .as_mut()
        .ok_or(FsError::StoreUnavailable)?
        .identity = Some(actual);
    if let Err(error) = ctx.checkpoint(&mut step, "RemoveCaptured") {
        return Ok(ctx.recovery(step, RecoveryKind::QuarantinedNative, error, None));
    }
    let refusal = if actual != source.object.identity() {
        Some(FsError::StaleEntry)
    } else if observed.is_dir() != directory {
        Some(FsError::Io(std::io::Error::from_raw_os_error(
            if directory {
                libc::ENOTDIR
            } else {
                libc::EISDIR
            },
        )))
    } else {
        None
    };
    if let Some(error) = refusal {
        #[cfg(test)]
        ctx.barrier("RemoveRestoreBoundary");
        return restore_refused_capture(ctx, source, custody, stage, step, error);
    }
    if let Err(error) = ctx.checkpoint(&mut step, "RemoveStarted") {
        return Ok(ctx.recovery(step, RecoveryKind::QuarantinedNative, error, None));
    }
    let mut removed = false;
    let result = ctx.session().captured_effect(custody, || {
        ctx.cancelled()?;
        check_named(&custody.directory, stage, actual)?;
        let flags = if directory {
            rustix::fs::AtFlags::REMOVEDIR
        } else {
            rustix::fs::AtFlags::empty()
        };
        // Only this held entry in the root-protected namespace is destructive.
        // No check-then-unlink against the externally writable Home name exists.
        rustix::fs::unlinkat(&*custody.directory, stage.as_os_str(), flags)
            .map_err(|e| io_error(e.into()))?;
        removed = true;
        sync_dir(&custody.directory)
    });
    if let Err(error) = result {
        if removed {
            return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, None));
        }
        return restore_refused_capture(ctx, source, custody, stage, step, error);
    }
    if let Err(error) = ctx.checkpoint(&mut step, "Removed") {
        return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, None));
    }
    if let Err(error) = ctx.progress("remove") {
        return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, None));
    }
    Ok(ctx.completed(None))
}
fn move_entry(
    ctx: &mut Context<'_>,
    entry: &GrantedEntry,
    into: &DirectoryGrant,
    policy: ConflictPolicy,
) -> Result<ItemOutcome, FsError> {
    let source = ctx.session().mutation_source(entry, FsRight::Move)?;
    let destination = ctx.session().mutation_directory(into)?;
    if NativeIdentity::of(&source.parent.dir_metadata().map_err(io_error)?)
        == NativeIdentity::of(&destination.dir_metadata().map_err(io_error)?)
    {
        return Err(FsError::Conflict);
    }
    ctx.effect(Some((entry, FsRight::Move)), Some(into), || {
        reject_ancestry(&source, &destination)
    })?;
    let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
    step.source = Some(source.locator.clone());
    for attempt in 0..attempts(ctx, policy) {
        let target = candidate(&source.name, attempt)?;
        step.destination = Some(ctx.session().mutation_locator(
            into,
            &target,
            Some(source.object.identity()),
        )?);
        ctx.checkpoint(&mut step, "PublishStarted")?;
        let mut published = false;
        let result = ctx.effect(Some((entry, FsRight::Move)), Some(into), || {
            check_named(&source.parent, &source.name, source.object.identity())?;
            #[cfg(test)]
            if let Some(error) = ctx.work.hooks.rename_error {
                return Err(FsError::Io(error.into()));
            }
            no_replace(&source.parent, &source.name, &destination, &target)
                .map_err(|e| FsError::Io(e.into()))?;
            published = true;
            sync_dir(&source.parent)?;
            sync_dir(&destination)
        });
        if published && result.is_err() {
            let error = result.err().ok_or(FsError::StoreUnavailable)?;
            let grant = ctx.session().mutation_result(into, &target).ok();
            return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, grant));
        }
        match result {
            Ok(()) => {
                let grant = ctx.session().mutation_result(into, &target).ok();
                if let Err(e) = ctx.checkpoint(&mut step, "Published") {
                    return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, e, grant));
                }
                ctx.progress("publish")?;
                return Ok(ctx.completed(grant));
            }
            Err(FsError::Io(ref e))
                if e.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) =>
            {
                let name = source.name.clone();
                return copy_publish(
                    ctx,
                    PublishRequest {
                        source: Some((entry, source)),
                        into,
                        name: &name,
                        policy,
                        bytes: None,
                        moving: true,
                    },
                );
            }
            Err(FsError::Io(ref e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(FsError::Conflict)
}
/// One item to publish as `name` in `into`: a copy (or, `moving`, a move) of `source`, or
/// without one a new file holding `bytes`, or a new directory when there are none.
struct PublishRequest<'a> {
    source: Option<(&'a GrantedEntry, MutationSource)>,
    into: &'a DirectoryGrant,
    name: &'a EntryName,
    policy: ConflictPolicy,
    bytes: Option<&'a Vec<u8>>,
    moving: bool,
}
fn copy_publish(
    ctx: &mut Context<'_>,
    request: PublishRequest<'_>,
) -> Result<ItemOutcome, FsError> {
    let PublishRequest {
        into, name, moving, ..
    } = request;
    let destination = ctx.session().mutation_directory(into)?;
    let right = if moving { FsRight::Move } else { FsRight::Copy };
    let auth = request.source.as_ref().map(|(entry, _)| (*entry, right));
    if let Some((_, source)) = &request.source {
        ctx.effect(auth, Some(into), || reject_ancestry(source, &destination))?;
    }
    let stage_name =
        EntryName::new(format!(".tilo-stage-{}-{}", ctx.work.id.native(), ctx.item).into())?;
    let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
    step.source = request.source.as_ref().map(|(_, s)| s.locator.clone());
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
        source,
        into,
        name,
        policy,
        bytes,
        moving,
    } = request;
    let right = if moving { FsRight::Move } else { FsRight::Copy };
    let auth = source.as_ref().map(|(entry, _)| (*entry, right));
    let copied = if let Some((_, source)) = &source {
        TreeCopy {
            ctx: &mut *ctx,
            source: auth,
            into,
            count: 0,
        }
        .object(&source.parent, &source.name, destination, stage_name, 0)
    } else if let Some(bytes) = bytes {
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
    if source.is_some() {
        ctx.progress("copy")?;
    }
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
            if let Some((_, source)) = &source {
                check_named(&source.parent, &source.name, source.object.identity())?;
            }
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
            recovery.acknowledged = !moving;
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
        if moving {
            let (entry, source) = source.as_ref().ok_or(FsError::StoreUnavailable)?;
            if let Err(error) = ctx.cancelled() {
                return Ok(ctx.recovery(
                    step.clone(),
                    RecoveryKind::PublishedSourceRetained,
                    error,
                    grant,
                ));
            }
            if let Err(error) = ctx.checkpoint(step, "SourceRemovalStarted") {
                return Ok(ctx.recovery(
                    step.clone(),
                    RecoveryKind::PublishedSourceRetained,
                    error,
                    grant,
                ));
            }
            #[cfg(test)]
            ctx.barrier("before-source-removal");
            let removed = (|| {
                let (observed, verified) = fingerprint_with_tree(
                    ctx,
                    Some((entry, FsRight::Move)),
                    into,
                    &source.parent,
                    &source.name,
                )?;
                let expected = step.fingerprint.as_ref().ok_or(FsError::StoreUnavailable)?;
                if observed.length != expected.length || observed.digest != expected.digest {
                    return Err(FsError::StaleEntry);
                }
                #[cfg(test)]
                ctx.barrier("after-source-fingerprint");
                #[cfg(test)]
                if ctx.work.hooks.remove_error {
                    return Err(FsError::Io(rustix::io::Errno::IO.into()));
                }
                let mut count = 0;
                remove_source_object(
                    ctx,
                    entry,
                    into,
                    &source.parent,
                    &source.name,
                    &verified,
                    0,
                    &mut count,
                )?;
                // The original name is gone; syncing its retained parent does not
                // acquire or modify another user object.
                sync_dir(&source.parent)
            })();
            if let Err(error) = removed {
                return Ok(ctx.recovery(
                    step.clone(),
                    RecoveryKind::PublishedSourceRetained,
                    error,
                    grant,
                ));
            }
            ctx.progress("remove-source")?;
            if let Some(recovery) = step.recovery.as_mut() {
                recovery.acknowledged = true;
            }
            if let Err(error) = ctx.checkpoint(step, "SourceRemovalCompleted") {
                return Ok(ctx.recovery(
                    step.clone(),
                    RecoveryKind::PublishedSourceRetained,
                    error,
                    grant,
                ));
            }
        }
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
/// One bounded recursive copy of an object tree, every effect authorised as `source` within
/// `into`; `count` is the entries copied so far.
struct TreeCopy<'c, 'a> {
    ctx: &'c mut Context<'a>,
    source: Option<(&'c GrantedEntry, FsRight)>,
    into: &'c DirectoryGrant,
    count: usize,
}
impl TreeCopy<'_, '_> {
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded recursion frame owns each file, directory or link copy"
    )]
    fn object(
        &mut self,
        from: &Dir,
        name: &EntryName,
        to: &Dir,
        target: &EntryName,
        depth: usize,
    ) -> Result<(), FsError> {
        if depth > MAX_DEPTH || self.count == MAX_TREE_ENTRIES {
            return Err(FsError::LimitExceeded);
        }
        self.count = self.count.checked_add(1).ok_or(FsError::LimitExceeded)?;
        let metadata = self.ctx.effect(self.source, Some(self.into), || {
            from.symlink_metadata(name.as_os_str()).map_err(io_error)
        })?;
        let identity = NativeIdentity::of(&metadata);
        if metadata.is_symlink() {
            self.ctx.effect(self.source, Some(self.into), || {
                let contents = from
                    .read_link_contents(name.as_os_str())
                    .map_err(io_error)?;
                check_named(from, name, identity)?;
                rustix::fs::symlinkat(contents.as_os_str(), to, target.as_os_str())
                    .map_err(|e| io_error(e.into()))?;
                Ok(())
            })?;
            if depth == 0 {
                self.ctx.stage_created = true;
                self.ctx.staged_identity = Some(NativeIdentity::of(
                    &to.symlink_metadata(target.as_os_str()).map_err(io_error)?,
                ));
            }
            self.ctx
                .effect(self.source, Some(self.into), || sync_dir(to))?;
            self.ctx.progress("copy")?;
            return Ok(());
        }
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(FsError::Unsupported("special file copy"));
        }
        let input = self.ctx.effect(self.source, Some(self.into), || {
            let input = open_object(from, name, metadata.is_dir())?;
            if NativeIdentity::of(&input.metadata().map_err(io_error)?) != identity {
                return Err(FsError::StaleEntry);
            }
            Ok(input)
        })?;
        if metadata.is_dir() {
            let input = Dir::from_std_file(input.into_std());
            self.ctx.effect(self.source, Some(self.into), || {
                rustix::fs::mkdirat(
                    to,
                    target.as_os_str(),
                    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
                )
                .map_err(|e| io_error(e.into()))
            })?;
            if depth == 0 {
                self.ctx.stage_created = true;
            }
            let output = self.ctx.effect(self.source, Some(self.into), || {
                Ok(Dir::from_std_file(
                    open_object(to, target, true)?.into_std(),
                ))
            })?;
            if depth == 0 {
                self.ctx.staged_identity = Some(NativeIdentity::of(
                    &output.dir_metadata().map_err(io_error)?,
                ));
            }
            let mut entries = self.ctx.effect(self.source, Some(self.into), || {
                input.entries().map_err(io_error)
            })?;
            loop {
                let next = self.ctx.effect(self.source, Some(self.into), || {
                    entries.next().transpose().map_err(io_error)
                })?;
                let Some(next) = next else { break };
                let child = EntryName::new(next.file_name())?;
                self.object(&input, &child, &output, &child, depth.saturating_add(1))?;
            }
            self.ctx.effect(self.source, Some(self.into), || {
                check_named(from, name, identity)?;
                sync_dir(&output)?;
                sync_dir(to)
            })?;
        } else {
            let output = self
                .ctx
                .effect(self.source, Some(self.into), || create_file(to, target))?;
            if depth == 0 {
                self.ctx.stage_created = true;
                self.ctx.staged_identity =
                    Some(NativeIdentity::of(&output.metadata().map_err(io_error)?));
            }
            let mut buffer = vec![0; COPY_CHUNK];
            let mut offset = 0;
            loop {
                #[cfg(test)]
                if offset > 0 && self.ctx.work.hooks.copy_eio {
                    return Err(FsError::Io(rustix::io::Errno::IO.into()));
                }
                let read = self.ctx.effect(self.source, Some(self.into), || {
                    check_named(from, name, identity)?;
                    rustix::io::pread(&input, &mut buffer, offset).map_err(|e| io_error(e.into()))
                })?;
                if read == 0 {
                    break;
                }
                self.ctx.effect(self.source, Some(self.into), || {
                    write_chunk(
                        &output,
                        buffer.get(..read).ok_or(FsError::LimitExceeded)?,
                        offset,
                    )
                })?;
                offset = offset
                    .checked_add(read as u64)
                    .ok_or(FsError::LimitExceeded)?;
                self.ctx.bytes = self
                    .ctx
                    .bytes
                    .checked_add(read as u64)
                    .ok_or(FsError::LimitExceeded)?;
                self.ctx.progress("copy")?;
                #[cfg(test)]
                if offset == COPY_CHUNK as u64 || offset == metadata.len() {
                    self.ctx.barrier("after-chunk");
                }
            }
            self.ctx.effect(self.source, Some(self.into), || {
                output.sync_all().map_err(io_error)?;
                sync_dir(to)
            })?;
        }
        Ok(())
    }
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
struct VerifiedRemoval {
    identity: NativeIdentity,
    length: u64,
    modified: (i64, i64),
    content: Option<Fingerprint>,
    children: Vec<(EntryName, Self)>,
}
fn remove_source_object(
    ctx: &Context<'_>,
    entry: &GrantedEntry,
    into: &DirectoryGrant,
    parent: &Dir,
    name: &EntryName,
    expected: &VerifiedRemoval,
    depth: usize,
    count: &mut usize,
) -> Result<(), FsError> {
    if depth > MAX_DEPTH || *count == MAX_TREE_ENTRIES {
        return Err(FsError::LimitExceeded);
    }
    *count = count.checked_add(1).ok_or(FsError::LimitExceeded)?;
    let auth = Some((entry, FsRight::Move));
    let metadata = ctx.effect(auth, Some(into), || {
        check_named(parent, name, expected.identity)?;
        parent.symlink_metadata(name.as_os_str()).map_err(io_error)
    })?;
    if metadata.is_dir() {
        if expected.content.is_some() {
            return Err(FsError::StaleEntry);
        }
        let dir = ctx.effect(auth, Some(into), || {
            let dir = Dir::from_std_file(open_object(parent, name, true)?.into_std());
            if NativeIdentity::of(&dir.dir_metadata().map_err(io_error)?) != expected.identity {
                return Err(FsError::StaleEntry);
            }
            Ok(dir)
        })?;
        // This enumeration checks membership only; it never adds removal authority.
        let mut entries = ctx.effect(auth, Some(into), || dir.entries().map_err(io_error))?;
        let mut names = Vec::new();
        loop {
            let next = ctx.effect(auth, Some(into), || {
                entries.next().transpose().map_err(io_error)
            })?;
            let Some(next) = next else { break };
            if names.len() >= expected.children.len() {
                return Err(FsError::StaleEntry);
            }
            names.push(next.file_name());
        }
        names.sort();
        if !names
            .iter()
            .map(OsString::as_os_str)
            .eq(expected.children.iter().map(|(name, _)| name.as_os_str()))
        {
            return Err(FsError::StaleEntry);
        }
        for (child, verified) in &expected.children {
            remove_source_object(
                ctx,
                entry,
                into,
                &dir,
                child,
                verified,
                depth.saturating_add(1),
                count,
            )?;
        }
    } else {
        let expected_content = expected.content.as_ref().ok_or(FsError::StaleEntry)?;
        let current = fingerprint(ctx, auth, into, parent, name)?;
        if current.identity != expected_content.identity
            || current.length != expected_content.length
            || current.digest != expected_content.digest
        {
            return Err(FsError::StaleEntry);
        }
    }
    ctx.effect(auth, Some(into), || {
        check_named(parent, name, expected.identity)?;
        if !metadata.is_dir() {
            let current = parent
                .symlink_metadata(name.as_os_str())
                .map_err(io_error)?;
            if current.len() != expected.length
                || (current.mtime(), current.mtime_nsec()) != expected.modified
            {
                return Err(FsError::StaleEntry);
            }
        }
        // A child added after membership verification keeps this directory nonempty.
        rustix::fs::unlinkat(
            parent,
            name.as_os_str(),
            if metadata.is_dir() {
                rustix::fs::AtFlags::REMOVEDIR
            } else {
                rustix::fs::AtFlags::empty()
            },
        )
        .map_err(|e| io_error(e.into()))?;
        sync_dir(parent)
    })
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
    fingerprint_with_tree(ctx, source, into, parent, name).map(|(fingerprint, _)| fingerprint)
}
fn fingerprint_with_tree(
    ctx: &Context<'_>,
    source: Option<(&GrantedEntry, FsRight)>,
    into: &DirectoryGrant,
    parent: &Dir,
    name: &EntryName,
) -> Result<(Fingerprint, VerifiedRemoval), FsError> {
    let mut walk = TreeFingerprint {
        ctx,
        source,
        into,
        hash: blake3::Hasher::new(),
        length: 0,
        count: 0,
    };
    let verified = walk.object(parent, name, 0)?;
    let identity = NativeIdentity::of(
        &parent
            .symlink_metadata(name.as_os_str())
            .map_err(io_error)?,
    );
    if identity != verified.identity {
        return Err(FsError::StaleEntry);
    }
    Ok((
        Fingerprint {
            identity,
            length: walk.length,
            digest: *walk.hash.finalize().as_bytes(),
        },
        verified,
    ))
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
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded traversal produces both the content fingerprint and its exact removal set"
    )]
    fn object(
        &mut self,
        parent: &Dir,
        name: &EntryName,
        depth: usize,
    ) -> Result<VerifiedRemoval, FsError> {
        if depth > MAX_DEPTH || self.count == MAX_TREE_ENTRIES {
            return Err(FsError::LimitExceeded);
        }
        self.count = self.count.checked_add(1).ok_or(FsError::LimitExceeded)?;
        let metadata = self.ctx.effect(self.source, Some(self.into), || {
            parent.symlink_metadata(name.as_os_str()).map_err(io_error)
        })?;
        let mut content = blake3::Hasher::new();
        let initial_length = self.length;
        let mut children = Vec::new();
        if metadata.is_symlink() {
            let target = self.ctx.effect(self.source, Some(self.into), || {
                parent
                    .read_link_contents(name.as_os_str())
                    .map_err(io_error)
            })?;
            self.hash.update(b"link");
            content.update(b"link");
            self.hash
                .update(&(target.as_os_str().as_bytes().len() as u64).to_le_bytes());
            content.update(&(target.as_os_str().as_bytes().len() as u64).to_le_bytes());
            self.hash.update(target.as_os_str().as_bytes());
            content.update(target.as_os_str().as_bytes());
            self.length = self
                .length
                .checked_add(target.as_os_str().as_bytes().len() as u64)
                .ok_or(FsError::LimitExceeded)?;
        } else if metadata.is_file() {
            self.hash.update(b"file");
            content.update(b"file");
            self.hash.update(&metadata.len().to_le_bytes());
            content.update(&metadata.len().to_le_bytes());
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
                content.update(buffer.get(..n).ok_or(FsError::LimitExceeded)?);
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
                let verified = self.object(&dir, &child, depth.saturating_add(1))?;
                children.push((child, verified));
            }
            self.hash.update(b"end");
        } else {
            return Err(FsError::Unsupported("special staged object"));
        }
        self.ctx.effect(self.source, Some(self.into), || {
            check_named(parent, name, NativeIdentity::of(&metadata))
        })?;
        Ok(VerifiedRemoval {
            identity: NativeIdentity::of(&metadata),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            content: if metadata.is_dir() {
                None
            } else {
                Some(Fingerprint {
                    identity: NativeIdentity::of(&metadata),
                    length: self
                        .length
                        .checked_sub(initial_length)
                        .ok_or(FsError::LimitExceeded)?,
                    digest: *content.finalize().as_bytes(),
                })
            },
            children,
        })
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
fn trash(ctx: &Context<'_>, entry: &GrantedEntry) -> Result<ItemOutcome, FsError> {
    let source = ctx.session().mutation_source(entry, FsRight::Trash)?;
    let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
    step.source = Some(source.locator);
    ctx.checkpoint(&mut step, "NativeTrashSubmitted")?;
    #[cfg(test)]
    {
        ctx.effect(Some((entry, FsRight::Trash)), None, || {
            ctx.work.owner.submissions.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
        Ok(ctx.recovery(
            step,
            RecoveryKind::UnknownNativeCommit,
            FsError::StoreUnavailable,
            None,
        ))
    }
    #[cfg(not(test))]
    {
        Err(FsError::Unsupported("native trash adapter not installed"))
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
    use std::os::unix::fs::DirBuilderExt;
    use std::{
        fs,
        sync::{Arc, Condvar, Mutex},
        time::Instant,
    };
    use tempfile::TempDir;

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
                        assert_eq!(
                            f.store.report(&id).unwrap().state,
                            OperationState::RecoveryRequired
                        );
                        assert_eq!(
                            f.store.recoveries().unwrap()[0].kind,
                            RecoveryKind::PublishedSourceRetained
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn receipt_publication_later_failure_drains_already_queued_success_first() {
        let f = Fixture::new();
        fs::write(f.source("a"), b"first").unwrap();
        fs::write(f.source("b"), b"second").unwrap();
        let barrier = Arc::new(Barrier::default());
        let _release = Release(Arc::clone(&barrier));
        f.hooks(Hooks {
            barrier: Some(("before-item-two", Arc::clone(&barrier))),
            ..Hooks::default()
        });
        let mut job = f
            .session
            .start(FsOperation::Copy {
                entries: vec![f.entry("a"), f.entry("b")],
                into: f.dst.clone(),
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        barrier.entered();
        f.store.hooks.lock().unwrap().publication_failure = Some((
            "Item",
            tilo_files::native::atomic::test_support::Checkpoint::BeforeCommit,
        ));
        barrier.release();
        let deadline = tilo_test_env::hang_deadline();
        // Wait for the terminal error without consuming the queued first item.
        while !job.control.results.lock().unwrap().error {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let updates = job.poll().unwrap();
        assert!(updates.iter().any(|u| matches!(u, FsUpdate::Item(item) if item.item == 0 && item.state == ItemState::Completed)));
        assert!(
            !updates
                .iter()
                .any(|u| matches!(u, FsUpdate::Finished { .. }))
        );
        assert_publication(
            &job.poll().unwrap_err(),
            tilo_files::native::atomic::PublicationState::PublishedDurabilityUnknown,
        );
        assert!(matches!(job.poll(), Err(FsError::StoreUnavailable)));
        assert_eq!(fs::read(f.dest("a")).unwrap(), b"first");
        assert_eq!(fs::read(f.dest("b")).unwrap(), b"second");
    }

    #[test]
    fn receipt_publication_native_move_effect_is_not_undone_by_unpublished_receipt() {
        let f = Fixture::new();
        fs::write(f.source("a"), b"moved").unwrap();
        f.hooks(Hooks {
            publication_failure: Some((
                "Published",
                tilo_files::native::atomic::test_support::Checkpoint::BeforeDataFlush,
            )),
            ..Hooks::default()
        });
        let mut job = f.session.start(f.moving("a")).unwrap();
        let id = job.operation_id();
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
        assert!(!f.source("a").exists());
        assert_eq!(fs::read(f.dest("a")).unwrap(), b"moved");
        drop(job);
        drop(error);
        let f = f.reopen();
        assert_eq!(
            f.store.recoveries().unwrap()[0].kind,
            RecoveryKind::UnknownNativeCommit
        );
        assert_eq!(
            f.store.report(&id).unwrap().items[0].state,
            ItemState::OutcomeUnknown
        );
        assert_eq!(fs::read(f.dest("a")).unwrap(), b"moved");
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

    #[test]
    fn receipt_publication_recovery_save_surfaces_publication_after_native_failure() {
        let f = Fixture::new();
        fs::write(f.source("a"), b"retained source").unwrap();
        let barrier = Arc::new(Barrier::default());
        let _release = Release(Arc::clone(&barrier));
        f.hooks(Hooks {
            rename_error: Some(rustix::io::Errno::XDEV),
            remove_error: true,
            barrier: Some(("before-source-removal", Arc::clone(&barrier))),
            ..Hooks::default()
        });
        let mut job = f.session.start(f.moving("a")).unwrap();
        barrier.entered();
        f.store.hooks.lock().unwrap().publication_failure = Some((
            "SourceRemovalStarted",
            tilo_files::native::atomic::test_support::Checkpoint::BeforeCommit,
        ));
        barrier.release();
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
            tilo_files::native::atomic::PublicationState::PublishedDurabilityUnknown,
        );
        assert_eq!(fs::read(f.source("a")).unwrap(), b"retained source");
        assert_eq!(fs::read(f.dest("a")).unwrap(), b"retained source");
        assert!(matches!(job.poll(), Err(FsError::StoreUnavailable)));
    }

    #[derive(Clone, Default)]
    #[allow(
        clippy::struct_excessive_bools,
        reason = "independent cfg(test) syscall failure seams"
    )]
    pub(crate) struct Hooks {
        pub rename_error: Option<rustix::io::Errno>,
        pub fail_checkpoint: Option<&'static str>,
        pub publication_failure: Option<(
            &'static str,
            tilo_files::native::atomic::test_support::Checkpoint,
        )>,
        pub stop_after_stage: bool,
        pub copy_eio: bool,
        pub remove_error: bool,
        pub stage_fingerprint_error: bool,
        pub cleanup_error: bool,
        pub cleanup_sync_error: bool,
        pub unknown_trash: bool,
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
    #[test]
    fn remove_nonrecursive_native_effects_and_commit_recovery() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        f.bind_removal();
        fs::write(f.source("victim"), b"held bytes").unwrap();
        let held = fs::File::open(f.source("victim")).unwrap();
        let (id, updates) = f.run(FsOperation::Remove {
            entry: f.entry("victim"),
            directory: false,
        });
        assert_eq!(item(&updates).state, ItemState::Completed);
        assert!(item(&updates).destination.is_none());
        assert!(!f.source("victim").exists());
        assert_eq!(held.metadata().unwrap().nlink(), 0);
        assert_eq!(
            f.store.report(&id).unwrap().state,
            crate::recovery::OperationState::Finished
        );

        fs::create_dir(f.source("empty")).unwrap();
        let (_, updates) = f.run(FsOperation::Remove {
            entry: f.entry("empty"),
            directory: true,
        });
        assert_eq!(item(&updates).state, ItemState::Completed);
        assert!(!f.source("empty").exists());

        fs::create_dir(f.source("nonempty")).unwrap();
        fs::write(f.source("nonempty/child"), b"preserved").unwrap();
        let (_, updates) = f.run(FsOperation::Remove {
            entry: f.entry("nonempty"),
            directory: true,
        });
        assert_eq!(item(&updates).state, ItemState::Failed);
        assert!(
            matches!(item(&updates).error.as_ref(), Some(FsError::Io(e)) if e.raw_os_error() == Some(libc::ENOTEMPTY))
        );
        assert_eq!(fs::read(f.source("nonempty/child")).unwrap(), b"preserved");

        fs::write(f.dest("outside"), b"outside unchanged").unwrap();
        std::os::unix::fs::symlink("../dst/outside", f.source("link")).unwrap();
        let (_, updates) = f.run(FsOperation::Remove {
            entry: f.entry("link"),
            directory: false,
        });
        assert_eq!(item(&updates).state, ItemState::Completed);
        assert!(
            matches!(fs::symlink_metadata(f.source("link")), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        );
        assert_eq!(fs::read(f.dest("outside")).unwrap(), b"outside unchanged");

        fs::write(f.source("durable"), b"original").unwrap();
        f.hooks(Hooks {
            fail_checkpoint: Some("Removed"),
            ..Hooks::default()
        });
        let (_, updates) = f.run(FsOperation::Remove {
            entry: f.entry("durable"),
            directory: false,
        });
        assert_eq!(item(&updates).state, ItemState::OutcomeUnknown);
        assert!(!f.source("durable").exists());
        assert_eq!(
            f.store.recoveries().unwrap()[0].kind,
            RecoveryKind::UnknownNativeCommit
        );
    }

    #[test]
    fn remove_requires_purge_and_current_named_identity() {
        let f = Fixture::new();
        f.bind_removal();
        fs::write(f.source("victim"), b"original").unwrap();
        let entry = f.entry("victim");
        fs::rename(f.source("victim"), f.source("held")).unwrap();
        fs::write(f.source("victim"), b"replacement").unwrap();
        let (_, updates) = f.run(FsOperation::Remove {
            entry,
            directory: false,
        });
        assert_eq!(item(&updates).state, ItemState::Failed);
        assert!(matches!(item(&updates).error, Some(FsError::StaleEntry)));
        assert_eq!(fs::read(f.source("victim")).unwrap(), b"replacement");
        assert_eq!(fs::read(f.source("held")).unwrap(), b"original");

        let root = f
            .authority
            .admit_directory(
                fs::File::open(f.source("")).unwrap(),
                owner(),
                vec![FsRight::Enumerate, FsRight::Read],
            )
            .unwrap();
        let session = f
            .authority
            .bind(
                owner(),
                uuid::Uuid::new_v4(),
                std::slice::from_ref(&root),
                &[],
            )
            .unwrap();
        let entry = session.lookup(&root, &name("victim")).unwrap();
        let mut job = session
            .start(FsOperation::Remove {
                entry,
                directory: false,
            })
            .unwrap();
        let updates = drain(&mut job);
        assert_eq!(item(&updates).state, ItemState::Failed);
        assert!(matches!(item(&updates).error, Some(FsError::Denied)));
        assert_eq!(fs::read(f.source("victim")).unwrap(), b"replacement");
    }

    #[test]
    fn remove_boundary_swap_restores_replacement_without_deleting_either_inode() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        f.bind_removal();
        fs::write(f.source("victim"), b"expected original").unwrap();
        let entry = f.entry("victim");
        let original = fs::metadata(f.source("victim")).unwrap().ino();
        let boundary = Arc::new(Barrier::default());
        f.hooks(Hooks {
            barrier: Some(("RemoveCaptureBoundary", boundary.clone())),
            ..Hooks::default()
        });
        let _release = Release(boundary.clone());
        let mut job = f
            .session
            .start(FsOperation::Remove {
                entry,
                directory: false,
            })
            .unwrap();
        boundary.entered();
        // Real namespace edits bypass the session table mutex, exactly as an
        // original-OS process does after the final expected-inode check.
        fs::rename(f.source("victim"), f.source("held-original")).unwrap();
        fs::write(f.source("victim"), b"external replacement").unwrap();
        let replacement = fs::metadata(f.source("victim")).unwrap().ino();
        boundary.release();
        let updates = drain(&mut job);
        assert_eq!(item(&updates).state, ItemState::Failed);
        assert!(matches!(item(&updates).error, Some(FsError::StaleEntry)));
        assert_eq!(fs::metadata(f.source("victim")).unwrap().ino(), replacement);
        assert_eq!(
            fs::metadata(f.source("held-original")).unwrap().ino(),
            original
        );
        assert_eq!(
            fs::read(f.source("victim")).unwrap(),
            b"external replacement"
        );
        assert_eq!(
            fs::read(f.source("held-original")).unwrap(),
            b"expected original"
        );
    }
    #[test]
    fn remove_boundary_swap_restore_collision_retains_exact_captured_inode() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        f.bind_removal();
        fs::write(f.source("victim"), b"expected original").unwrap();
        let entry = f.entry("victim");
        let capture = Arc::new(Barrier::default());
        let restore = Arc::new(Barrier::default());
        f.hooks(Hooks {
            barrier: Some(("RemoveCaptureBoundary", capture.clone())),
            restore_barrier: Some(restore.clone()),
            ..Hooks::default()
        });
        let _capture_release = Release(capture.clone());
        let _restore_release = Release(restore.clone());
        let mut job = f
            .session
            .start(FsOperation::Remove {
                entry,
                directory: false,
            })
            .unwrap();
        capture.entered();
        fs::rename(f.source("victim"), f.source("held-original")).unwrap();
        fs::write(f.source("victim"), b"captured replacement").unwrap();
        let captured_inode = fs::metadata(f.source("victim")).unwrap().ino();
        capture.release();
        restore.entered();
        fs::write(f.source("victim"), b"later arrival").unwrap();
        restore.release();
        let updates = drain(&mut job);
        assert_eq!(item(&updates).state, ItemState::RecoveryRequired);
        let recovery = f.store.recoveries().unwrap().remove(0);
        assert_eq!(recovery.kind, RecoveryKind::QuarantinedNative);
        assert_eq!(fs::read(f.source("victim")).unwrap(), b"later arrival");
        assert_eq!(
            fs::read(f.source("held-original")).unwrap(),
            b"expected original"
        );
        f.hooks(Hooks::default());
        let mut job = f
            .store
            .resolve(
                &recovery.id,
                &f.session,
                RecoveryResolution::RestoreCaptured {
                    parent: f.src.clone(),
                    name: name("returned-replacement"),
                },
            )
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        assert_eq!(
            fs::metadata(f.source("returned-replacement"))
                .unwrap()
                .ino(),
            captured_inode
        );
        assert_eq!(
            fs::read(f.source("returned-replacement")).unwrap(),
            b"captured replacement"
        );
        assert_eq!(fs::read(f.source("victim")).unwrap(), b"later arrival");
        assert!(f.store.recoveries().unwrap()[0].acknowledged);
    }
    #[test]
    fn captured_restore_does_not_read_unreadable_bytes_or_replace_destination() {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let f = Fixture::new();
        f.bind_removal();
        fs::write(f.source("victim"), b"original unreadable").unwrap();
        let mut original = fs::File::open(f.source("victim")).unwrap();
        fs::set_permissions(f.source("victim"), fs::Permissions::from_mode(0o000)).unwrap();
        let entry = f.entry("victim");
        f.hooks(Hooks {
            fail_checkpoint: Some("RemoveCaptured"),
            ..Hooks::default()
        });
        let (_, updates) = f.run(FsOperation::Remove {
            entry,
            directory: false,
        });
        assert_eq!(item(&updates).state, ItemState::RecoveryRequired);
        let recovery = f.store.recoveries().unwrap().remove(0);
        assert_eq!(recovery.kind, RecoveryKind::QuarantinedNative);
        f.hooks(Hooks::default());
        fs::write(f.source("victim"), b"new arrival").unwrap();
        let mut job = f
            .store
            .resolve(
                &recovery.id,
                &f.session,
                RecoveryResolution::RestoreCaptured {
                    parent: f.src.clone(),
                    name: name("victim"),
                },
            )
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Failed);
        assert_eq!(fs::read(f.source("victim")).unwrap(), b"new arrival");
        assert!(!f.store.recoveries().unwrap()[0].acknowledged);
        fs::remove_file(f.source("victim")).unwrap();
        let mut job = f
            .store
            .resolve(
                &recovery.id,
                &f.session,
                RecoveryResolution::RestoreCaptured {
                    parent: f.src.clone(),
                    name: name("victim"),
                },
            )
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        assert_eq!(
            fs::metadata(f.source("victim"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0
        );
        assert!(f.store.recoveries().unwrap()[0].acknowledged);
        // The verifier retains its own read descriptor before chmod; the job
        // never receives it and cannot reopen the mode-000 restored leaf.
        assert_eq!(
            fs::metadata(f.source("victim")).unwrap().ino(),
            original.metadata().unwrap().ino()
        );
        let mut bytes = Vec::new();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original unreadable");
    }

    #[test]
    fn retained_directory_recovery_is_scoped_and_revocable_and_remove_is_refused() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        f.bind_removal();
        fs::write(f.source("victim"), b"same original bytes").unwrap();
        let original = fs::metadata(f.source("victim")).unwrap();
        let directory =
            crate::directory::DirectoryCapability::new(f.session.clone(), f.src.clone()).unwrap();
        assert!(matches!(
            directory.remove(&name("victim"), [original.dev(), original.ino()], false),
            Err(FsError::Unsupported(_))
        ));
        assert_eq!(
            fs::read(f.source("victim")).unwrap(),
            b"same original bytes"
        );
        f.hooks(Hooks {
            fail_checkpoint: Some("RemoveCaptured"),
            ..Hooks::default()
        });
        let (operation, updates) = f.run(FsOperation::Remove {
            entry: f.entry("victim"),
            directory: false,
        });
        assert_eq!(item(&updates).state, ItemState::RecoveryRequired);
        f.hooks(Hooks::default());
        let page = directory.recoveries(&[]).unwrap();
        assert!(page.done);
        assert!(page.next.is_empty());
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].operation, *operation.as_bytes());
        assert_eq!(page.entries[0].original_name, name("victim"));
        assert!(directory.recoveries(&[0; 23]).is_err());
        let other =
            crate::directory::DirectoryCapability::new(f.session.clone(), f.dst.clone()).unwrap();
        assert!(other.recoveries(&[]).is_err());
        assert!(
            other
                .restore_captured(page.entries[0].recovery, name("stolen"))
                .is_err()
        );
        let mut restored = directory
            .restore_captured(page.entries[0].recovery, name("restored"))
            .unwrap();
        assert_eq!(item(&drain(&mut restored)).state, ItemState::Completed);
        assert_eq!(
            fs::metadata(f.source("restored")).unwrap().ino(),
            original.ino()
        );
        assert_eq!(
            fs::read(f.source("restored")).unwrap(),
            b"same original bytes"
        );
        assert!(directory.recoveries(&[]).unwrap().entries.is_empty());
        f.authority.revoke_directory(&f.src).unwrap();
        assert!(matches!(directory.recoveries(&[]), Err(FsError::Revoked)));
        assert!(
            directory
                .restore_captured(page.entries[0].recovery, name("again"))
                .is_err()
        );
    }

    #[test]
    fn replacing_save_captures_exact_victim_and_pin_excludes_public_restore() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old bytes").unwrap();
        fs::write(f.source("temporary"), b"new bytes").unwrap();
        let old = fs::metadata(f.source("document")).unwrap().ino();
        let new = fs::metadata(f.source("temporary")).unwrap().ino();
        let (id, updates) = f.run(FsOperation::Replace {
            entry: f.entry("temporary"),
            into: f.src.clone(),
            name: name("document"),
            victim: f.entry("document"),
        });
        assert_eq!(item(&updates).state, ItemState::Completed);
        assert_eq!(fs::metadata(f.source("document")).unwrap().ino(), new);
        assert_eq!(fs::read(f.source("document")).unwrap(), b"new bytes");
        assert!(!f.source("temporary").exists());
        let pin = f.store.pin_replacement(&f.session, &id, &f.src).unwrap();
        assert_eq!(pin.operation_id(), &id);
        assert_eq!(pin.source_owner(), [37; 32]);
        assert_eq!(pin.source_identity()[1], new);
        assert_eq!(pin.destination_identity()[1], new);
        assert_eq!(pin.victim_identity()[1], old);
        let recovery = pin.capture_id();
        let directory =
            crate::directory::DirectoryCapability::new(f.session.clone(), f.src.clone()).unwrap();
        assert!(matches!(
            directory.restore_captured(*recovery.as_bytes(), name("old-document")),
            Err(FsError::Busy)
        ));
        drop(pin);
        let mut restored = directory
            .restore_captured(*recovery.as_bytes(), name("old-document"))
            .unwrap();
        assert_eq!(item(&drain(&mut restored)).state, ItemState::Completed);
        assert_eq!(fs::metadata(f.source("old-document")).unwrap().ino(), old);
        assert_eq!(fs::read(f.source("old-document")).unwrap(), b"old bytes");
        assert_eq!(fs::read(f.source("document")).unwrap(), b"new bytes");
        assert!(f.store.pin_replacement(&f.session, &id, &f.src).is_err());
    }
    #[test]
    fn replacing_save_final_boundary_swap_preserves_actual_unexpected_victim() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"expected old").unwrap();
        fs::write(f.source("temporary"), b"saved bytes").unwrap();
        let barrier = Arc::new(Barrier::default());
        let _release = Release(barrier.clone());
        f.hooks(Hooks {
            barrier: Some(("ReplaceExchangeBoundary", barrier.clone())),
            ..Hooks::default()
        });
        let mut job = f
            .session
            .start(FsOperation::Replace {
                entry: f.entry("temporary"),
                into: f.src.clone(),
                name: name("document"),
                victim: f.entry("document"),
            })
            .unwrap();
        let id = job.operation_id();
        barrier.entered();
        // Destination remains present while temp is staged and immediately before
        // the single atomic exchange. An external namespace writer can still race.
        assert_eq!(fs::read(f.source("document")).unwrap(), b"expected old");
        fs::rename(f.source("document"), f.source("held-old")).unwrap();
        fs::write(f.source("document"), b"unexpected victim").unwrap();
        let unexpected = fs::metadata(f.source("document")).unwrap().ino();
        barrier.release();
        assert_eq!(item(&drain(&mut job)).state, ItemState::RecoveryRequired);
        assert!(f.store.pin_replacement(&f.session, &id, &f.src).is_err());
        assert_eq!(fs::read(f.source("document")).unwrap(), b"saved bytes");
        assert_eq!(fs::read(f.source("held-old")).unwrap(), b"expected old");
        f.hooks(Hooks::default());
        let recovery = f.store.recoveries().unwrap().remove(0);
        let directory =
            crate::directory::DirectoryCapability::new(f.session.clone(), f.src.clone()).unwrap();
        let mut restored = directory
            .restore_captured(*recovery.id.as_bytes(), name("unexpected-returned"))
            .unwrap();
        assert_eq!(item(&drain(&mut restored)).state, ItemState::Completed);
        assert_eq!(
            fs::metadata(f.source("unexpected-returned")).unwrap().ino(),
            unexpected
        );
        assert_eq!(
            fs::read(f.source("unexpected-returned")).unwrap(),
            b"unexpected victim"
        );
        assert_eq!(fs::read(f.source("document")).unwrap(), b"saved bytes");
    }
    #[test]
    fn replacing_save_uncertain_exchange_cannot_mint_history_success() {
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        f.hooks(Hooks {
            fail_checkpoint: Some("ReplaceVictimCaptured"),
            ..Hooks::default()
        });
        let (id, updates) = f.run(FsOperation::Replace {
            entry: f.entry("temporary"),
            into: f.src.clone(),
            name: name("document"),
            victim: f.entry("document"),
        });
        assert_eq!(item(&updates).state, ItemState::OutcomeUnknown);
        assert!(f.store.pin_replacement(&f.session, &id, &f.src).is_err());
        assert_eq!(fs::read(f.source("document")).unwrap(), b"new");
        assert_eq!(
            f.store.recoveries().unwrap()[0].kind,
            RecoveryKind::UnknownNativeCommit
        );
    }

    fn create_cleanup_fixture() -> Fixture {
        let f = Fixture::with_rights(
            rights()
                .into_iter()
                .filter(|right| !matches!(right, FsRight::Purge | FsRight::Replace))
                .collect(),
        );
        f.bind_removal();
        f
    }
    fn create_actual_temporary(f: &Fixture) -> Arc<NativeCreatedTemporary> {
        let mut job = f
            .session
            .start(FsOperation::CreateFile {
                parent: f.src.clone(),
                name: name("temporary"),
                bytes: b"own save bytes".to_vec(),
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        job.created_temporary().unwrap()
    }
    #[test]
    fn actual_created_temporary_permissions_leave_victim_and_replacement_untouched() {
        use std::os::unix::fs::MetadataExt;
        let fixture = create_cleanup_fixture();
        fs::write(fixture.source("document"), b"user document").unwrap();
        let victim = fs::metadata(fixture.source("document")).unwrap();
        let temporary = create_actual_temporary(&fixture);
        temporary.set_permissions(0o640).unwrap();
        assert_eq!(
            fs::metadata(fixture.source("temporary")).unwrap().mode() & 0o7777,
            0o640
        );
        assert_eq!(
            fs::metadata(fixture.source("document")).unwrap().mode(),
            victim.mode()
        );
        assert_eq!(
            fs::read(fixture.source("document")).unwrap(),
            b"user document"
        );
        assert!(temporary.set_permissions(0o10000).is_err());
        fs::rename(fixture.source("temporary"), fixture.source("held-created")).unwrap();
        fs::write(fixture.source("temporary"), b"external replacement").unwrap();
        let replacement = fs::metadata(fixture.source("temporary")).unwrap();
        assert!(temporary.set_permissions(0o600).is_err());
        assert_eq!(
            fs::metadata(fixture.source("temporary")).unwrap().mode(),
            replacement.mode()
        );
        assert_eq!(
            fs::read(fixture.source("temporary")).unwrap(),
            b"external replacement"
        );
        assert_eq!(
            fs::metadata(fixture.source("held-created")).unwrap().mode() & 0o7777,
            0o640
        );
    }
    #[test]
    fn reserved_exchange_preserves_concurrent_inode_edits_as_recovery() {
        for changed in ["temporary", "document"] {
            let fixture = Fixture::new();
            fixture.bind_removal();
            fixture.bind_source_key();
            fs::write(fixture.source("temporary"), b"prepared new").unwrap();
            fs::write(fixture.source("document"), b"prepared old").unwrap();
            let mut original_writer = fs::OpenOptions::new()
                .write(true)
                .open(fixture.source(changed))
                .unwrap();
            let barrier = Arc::new(Barrier::default());
            let _release = Release(barrier.clone());
            fixture.hooks(Hooks {
                barrier: Some(("ReplaceExchangeBoundary", barrier.clone())),
                ..Hooks::default()
            });
            let prepared = fixture
                .session
                .prepare_replace(
                    fixture.entry("temporary"),
                    fixture.src.clone(),
                    name("document"),
                    fixture.entry("document"),
                )
                .unwrap();
            let mut job = prepared.start().unwrap();
            barrier.entered();
            original_writer.set_len(0).unwrap();
            std::io::Write::write_all(&mut original_writer, b"concurrent original-host edit")
                .unwrap();
            original_writer.sync_all().unwrap();
            barrier.release();
            assert_eq!(item(&drain(&mut job)).state, ItemState::OutcomeUnknown);
            assert!(
                fixture
                    .store
                    .pin_replacement(&fixture.session, &job.operation_id(), &fixture.src)
                    .is_err()
            );
            let record = fixture
                .store
                .recoveries()
                .unwrap()
                .into_iter()
                .find(|record| record.operation == job.operation_id())
                .unwrap();
            assert_eq!(record.kind, RecoveryKind::UnknownNativeCommit);
            let (_, step) = fixture.store.find_recovery(&record.id).unwrap();
            let capture = fixture
                .disk
                .path()
                .join("root-capture-guard/entries")
                .join(OsString::from_vec(step.stage.unwrap().name));
            if changed == "document" {
                assert_eq!(fs::read(capture).unwrap(), b"concurrent original-host edit");
                assert_eq!(
                    fs::read(fixture.source("document")).unwrap(),
                    b"prepared new"
                );
            } else {
                assert_eq!(fs::read(capture).unwrap(), b"prepared old");
                assert_eq!(
                    fs::read(fixture.source("document")).unwrap(),
                    b"concurrent original-host edit"
                );
            }
        }
    }
    #[test]
    fn completed_create_holds_admission_until_custody_registration() {
        let fixture = create_cleanup_fixture();
        let barrier = Arc::new(Barrier::default());
        let _release = Release(barrier.clone());
        fixture.hooks(Hooks {
            barrier: Some((
                "CreatedTemporaryBeforeRegistrationBoundary",
                barrier.clone(),
            )),
            ..Hooks::default()
        });
        let mut first = fixture
            .session
            .start(FsOperation::CreateFile {
                parent: fixture.src.clone(),
                name: name("first"),
                bytes: b"first own temporary".to_vec(),
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        barrier.entered();
        assert!(matches!(
            fixture.session.start(FsOperation::CreateFile {
                parent: fixture.src.clone(),
                name: name("second"),
                bytes: b"second own temporary".to_vec(),
                conflict: ConflictPolicy::Fail,
            }),
            Err(FsError::Busy)
        ));
        assert!(!fixture.source("second").exists());
        barrier.release();
        assert_eq!(item(&drain(&mut first)).state, ItemState::Completed);
        first.created_temporary().unwrap().check().unwrap();
        fixture.hooks(Hooks::default());
        let mut second = fixture
            .session
            .start(FsOperation::CreateFile {
                parent: fixture.src.clone(),
                name: name("second"),
                bytes: b"second own temporary".to_vec(),
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        assert_eq!(item(&drain(&mut second)).state, ItemState::Completed);
        second.created_temporary().unwrap().check().unwrap();
    }
    #[test]
    fn genuine_created_temporary_retires_recoverably_without_purge_or_replace() {
        let f = create_cleanup_fixture();
        fs::write(f.source("document"), b"user document").unwrap();
        let temporary = create_actual_temporary(&f);
        temporary.check().unwrap();
        let mut job = temporary.retire().unwrap();
        let updates = drain(&mut job);
        assert_eq!(item(&updates).state, ItemState::Completed);
        assert!(!f.source("temporary").exists());
        assert_eq!(fs::read(f.source("document")).unwrap(), b"user document");
        temporary.check_scope().unwrap();
        assert!(temporary.retire().is_err());
        let recovery = f
            .store
            .recoveries()
            .unwrap()
            .into_iter()
            .find(|r| r.operation == job.operation_id())
            .unwrap();
        let mut restore = f
            .store
            .resolve(
                &recovery.id,
                &f.session,
                RecoveryResolution::RestoreCaptured {
                    parent: f.src.clone(),
                    name: name("restored temporary"),
                },
            )
            .unwrap();
        assert_eq!(item(&drain(&mut restore)).state, ItemState::Completed);
        assert_eq!(
            fs::read(f.source("restored temporary")).unwrap(),
            b"own save bytes"
        );
    }
    #[test]
    fn created_temporary_does_not_accept_existing_entry_or_other_job_receipt() {
        let f = create_cleanup_fixture();
        fs::write(f.source("document"), b"existing user document").unwrap();
        let mut job = f
            .session
            .start(FsOperation::Rename {
                entry: f.entry("document"),
                name: name("renamed"),
            })
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        assert!(job.created_temporary().is_err());
        assert_eq!(
            fs::read(f.source("renamed")).unwrap(),
            b"existing user document"
        );
    }
    #[test]
    fn created_temporary_revoke_refuses_retirement_without_namespace_effect() {
        let f = create_cleanup_fixture();
        let temporary = create_actual_temporary(&f);
        f.authority.revoke_directory(&f.src).unwrap();
        assert!(temporary.retire().is_err());
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"own save bytes");
    }
    #[test]
    fn created_temporary_capture_swap_keeps_wrong_replacement_recoverable() {
        use std::os::unix::fs::MetadataExt;
        let f = create_cleanup_fixture();
        let temporary = create_actual_temporary(&f);
        fs::write(f.source("replacement"), b"unrelated original-user bytes").unwrap();
        let replacement_identity = fs::metadata(f.source("replacement")).unwrap().ino();
        let barrier = Arc::new(Barrier::default());
        let _release = Release(barrier.clone());
        f.hooks(Hooks {
            barrier: Some(("CreatedTemporaryCaptureBoundary", barrier.clone())),
            ..Hooks::default()
        });
        let mut job = temporary.retire().unwrap();
        barrier.entered();
        fs::rename(f.source("temporary"), f.source("held temporary")).unwrap();
        fs::rename(f.source("replacement"), f.source("temporary")).unwrap();
        barrier.release();
        assert_eq!(item(&drain(&mut job)).state, ItemState::RecoveryRequired);
        assert_eq!(
            fs::read(f.source("held temporary")).unwrap(),
            b"own save bytes"
        );
        let recovery = f
            .store
            .recoveries()
            .unwrap()
            .into_iter()
            .find(|r| r.operation == job.operation_id())
            .unwrap();
        let mut restore = f
            .store
            .resolve(
                &recovery.id,
                &f.session,
                RecoveryResolution::RestoreCaptured {
                    parent: f.src.clone(),
                    name: name("temporary"),
                },
            )
            .unwrap();
        assert_eq!(item(&drain(&mut restore)).state, ItemState::Completed);
        assert_eq!(
            fs::metadata(f.source("temporary")).unwrap().ino(),
            replacement_identity
        );
        assert_eq!(
            fs::read(f.source("temporary")).unwrap(),
            b"unrelated original-user bytes"
        );
    }

    #[test]
    fn actual_pending_refuses_claimed_prepare_without_enrolled_source_verifier() {
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"original bytes").unwrap();
        fs::write(f.source("temporary"), b"pending bytes").unwrap();
        let pending = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        let id = pending.operation_id().unwrap().clone();
        let receipt = crate::external_history::PreparedReceipt {
            schema: 1,
            operation_id: id.native().to_string(),
            source_owner: pending.source_owner(),
            intent_digest: pending.intent_digest().unwrap(),
            personal_room: uuid::Uuid::new_v4().to_string(),
            person: [7; 32],
            device: [8; 32],
            transaction_digest: [9; 32],
            archive_binding_digest: pending.archive_plan_digest().unwrap(),
            signature: vec![0; 64],
        };
        assert!(matches!(
            pending.start_with_history(&receipt),
            Err(FsError::Denied)
        ));
        assert_eq!(fs::read(f.source("document")).unwrap(), b"original bytes");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"pending bytes");
        assert!(f.store.recoveries().unwrap().is_empty());
        assert!(f.store.history_prepare(&id).is_err());
        assert_eq!(f.store.submissions.load(Ordering::Acquire), 0);
    }

    #[test]
    fn prepared_replace_has_no_effect_before_actual_signed_prepare_submission() {
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        let id = prepared.operation_id().unwrap().clone();
        assert_eq!(prepared.source_owner(), [37; 32]);
        let intent = prepared.intent_digest().unwrap();
        assert_ne!(intent, [0; 32]);
        assert_eq!(prepared.intent_digest().unwrap(), intent);
        let description = prepared.reservation_description().unwrap();
        assert!(description.len() <= PreparedNativeReplacement::DESCRIPTION_LIMIT);
        let mut digest = blake3::Hasher::new();
        digest.update(b"tilo-native-retained-replacement-intent-v2\0");
        digest.update(&description);
        assert_eq!(*digest.finalize().as_bytes(), intent);
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        assert_eq!(f.store.report(&id).unwrap().state, OperationState::Running);
        let mut job = prepared.start().unwrap();
        assert_eq!(job.operation_id(), id);
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        assert_eq!(fs::read(f.source("document")).unwrap(), b"new");
    }

    #[test]
    fn replacing_save_preserves_victim_owner_and_mode_on_published_temporary() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::set_permissions(f.source("document"), fs::Permissions::from_mode(0o640)).unwrap();
        let old = fs::metadata(f.source("document")).unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        fs::set_permissions(f.source("temporary"), fs::Permissions::from_mode(0o600)).unwrap();
        let (id, updates) = f.run(FsOperation::Replace {
            entry: f.entry("temporary"),
            into: f.src.clone(),
            name: name("document"),
            victim: f.entry("document"),
        });
        assert_eq!(item(&updates).state, ItemState::Completed);
        let new = fs::metadata(f.source("document")).unwrap();
        assert_ne!(new.ino(), old.ino());
        assert_eq!(
            (new.uid(), new.gid(), new.mode() & 0o7777),
            (old.uid(), old.gid(), old.mode() & 0o7777)
        );
        assert_eq!(fs::read(f.source("document")).unwrap(), b"new");
        let pin = f.store.pin_replacement(&f.session, &id, &f.src).unwrap();
        assert_eq!(pin.victim_identity()[1], old.ino());
    }

    #[test]
    fn replacing_save_without_temporary_write_refuses_before_namespace_effects() {
        let f = Fixture::with_rights(
            rights()
                .into_iter()
                .filter(|r| *r != FsRight::Write)
                .collect(),
        );
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        assert!(
            f.session
                .prepare_replace(
                    f.entry("temporary"),
                    f.src.clone(),
                    name("document"),
                    f.entry("document")
                )
                .is_err()
        );
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        assert!(
            fs::read_dir(f.disk.path().join("root-capture-guard/entries"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[cfg(target_os = "linux")]
    fn put_attribute(path: &std::path::Path, name: &[u8], value: &[u8]) {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = std::ffi::CString::new(name).unwrap();
        // SAFETY: owned disposable fixture paths/names and bounded values.
        assert_eq!(
            unsafe {
                libc::setxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            },
            0,
            "actual fixture xattr operation required: {}",
            std::io::Error::last_os_error()
        );
    }
    #[cfg(target_os = "linux")]
    fn assert_path_only_victim(session: &FsSession, victim: &GrantedEntry) {
        let source = session.mutation_source(victim, FsRight::Replace).unwrap();
        let NativeObject::File { descriptor, .. } = source.object else {
            panic!("actual regular victim descriptor required");
        };
        assert!(
            rustix::fs::fcntl_getfl(&*descriptor)
                .unwrap()
                .contains(rustix::fs::OFlags::PATH),
            "victim lookup must exercise the production O_PATH fallback"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn replacing_save_path_only_write_only_victim_refuses_unreadable_user_attributes() {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        put_attribute(&f.source("document"), b"user.original", b"preserved data");
        put_attribute(&f.source("temporary"), b"user.temporary", b"temporary data");
        // This verifier-only descriptor is never given to the session or job.
        let mut original = fs::File::open(f.source("document")).unwrap();
        fs::set_permissions(f.source("document"), fs::Permissions::from_mode(0o200)).unwrap();
        let before = fs::metadata(f.source("document")).unwrap();
        let temporary = fs::metadata(f.source("temporary")).unwrap();
        let victim = f.entry("document");
        assert_path_only_victim(&f.session, &victim);
        assert!(matches!(
            f.session.prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                victim,
            ),
            Err(FsError::Denied)
        ));
        for (path, expected) in [("document", before), ("temporary", temporary)] {
            let actual = fs::metadata(f.source(path)).unwrap();
            assert_eq!(
                (
                    actual.dev(),
                    actual.ino(),
                    actual.uid(),
                    actual.gid(),
                    actual.mode()
                ),
                (
                    expected.dev(),
                    expected.ino(),
                    expected.uid(),
                    expected.gid(),
                    expected.mode()
                )
            );
        }
        let mut bytes = Vec::new();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        assert!(f.store.recoveries().unwrap().is_empty());
        assert_eq!(f.store.submissions.load(Ordering::Acquire), 0);
        assert!(
            fs::read_dir(f.disk.path().join("root-capture-guard/entries"))
                .unwrap()
                .next()
                .is_none()
        );
        // DAC also checks fgetxattr on a pre-chmod fd. Only after the refusal
        // and unchanged-mode assertions may the verifier restore read access.
        original
            .set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        for (file, attribute, expected) in [
            (&original, c"user.original", b"preserved data".as_slice()),
            (
                &fs::File::open(f.source("temporary")).unwrap(),
                c"user.temporary",
                b"temporary data".as_slice(),
            ),
        ] {
            let mut value = [0_u8; 64];
            // SAFETY: verifier-owned fd, NUL-terminated name and live bounded buffer.
            let length = unsafe {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    attribute.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            };
            assert_eq!(length, libc::ssize_t::try_from(expected.len()).unwrap());
            assert_eq!(&value[..expected.len()], expected);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn replacing_save_preserves_posix_acl_and_user_attributes_and_removes_temporary_extras() {
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        let mut acl = 2_u32.to_le_bytes().to_vec();
        for (tag, permissions, id) in [
            (1_u16, 6_u16, u32::MAX),
            (2, 6, 65534),
            (4, 0, u32::MAX),
            (16, 6, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&permissions.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        put_attribute(&f.source("document"), b"system.posix_acl_access", &acl);
        put_attribute(&f.source("document"), b"user.original", b"preserved data");
        put_attribute(&f.source("temporary"), b"user.temporary", b"discard this");
        let expected = f
            .session
            .replacement_attributes(&f.entry("document"))
            .unwrap();
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        let original_plan = prepared.archive_plan_digest().unwrap();
        put_attribute(
            &f.source("document"),
            b"user.original",
            b"changed before capture",
        );
        assert!(matches!(
            prepared.archive_plan_digest(),
            Err(FsError::StaleEntry)
        ));
        // A changed original attribute invalidates the actual reserved plan
        // before either named entry is moved or its bytes are touched.
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        put_attribute(&f.source("document"), b"user.original", b"preserved data");
        drop(prepared);
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        // The fresh actual journal reservation is a separate capture plan,
        // even after restoring the original ACL and user attribute value.
        assert_ne!(original_plan, prepared.archive_plan_digest().unwrap());
        let mut job = prepared.start().unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        let actual = f
            .session
            .replacement_attributes(&f.entry("document"))
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(fs::read(f.source("document")).unwrap(), b"new");
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn replacing_save_actual_original_uid_preserves_group_acl_and_attributes() {
        original_uid_replacement_case(
            "operations::tests::replacing_save_actual_original_uid_preserves_group_acl_and_attributes",
            false,
        );
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn replacing_save_actual_original_uid_refuses_nonowned_group_writable_victim() {
        original_uid_replacement_case(
            "operations::tests::replacing_save_actual_original_uid_refuses_nonowned_group_writable_victim",
            true,
        );
    }
    #[cfg(target_os = "linux")]
    fn original_uid_replacement_case(test: &str, nonowned: bool) {
        const ORIGINAL_UID: u32 = 60001;
        const ORIGINAL_GID: u32 = 60002;
        const SHARED_GID: u32 = 60003;
        use std::io::Write;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        assert_eq!(
            rustix::process::geteuid().as_raw(),
            0,
            "root fixture host required"
        );
        let marker = "TILO_NATIVE_REPLACEMENT_PERMISSION_CASE";
        if std::env::var(marker).as_deref() != Ok(test) {
            let outer = tempfile::tempdir().unwrap();
            // Hold the exact running native test ELF. current_exe's printable
            // path can be a deleted memfd in the established native provider.
            let executable = fs::File::open("/proc/self/exe").unwrap();
            let child =
                std::process::Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()))
                    .args(["--exact", test, "--nocapture", "--test-threads=1"])
                    .env(marker, test)
                    .env("TILO_NATIVE_REPLACEMENT_PERMISSION_ROOT", outer.path())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
            let mut child = OwnedPermissionChild {
                child,
                retired: false,
            };
            let mut stdout = child.child.stdout.take().unwrap();
            let mut stderr = child.child.stderr.take().unwrap();
            for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
                // SAFETY: actual owned child pipes, no descriptor mutation outside
                // this isolated fixture. Nonblocking reads keep the wait bounded.
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                assert!(flags >= 0);
                assert_eq!(
                    unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
                    0
                );
            }
            let mut out = Vec::new();
            let mut err = Vec::new();
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(30))
                .expect("test deadline fits the monotonic clock");
            let status = loop {
                drain_permission_output(&mut stdout, &mut out);
                drain_permission_output(&mut stderr, &mut err);
                if let Some(status) = child.child.try_wait().unwrap() {
                    child.retired = true;
                    drain_permission_output(&mut stdout, &mut out);
                    drain_permission_output(&mut stderr, &mut err);
                    break status;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "owned native original-UID test must finish"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            assert!(
                status.success(),
                "actual original-UID subprocess failed: {}\n{}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&err)
            );
            return;
        }
        // Credential changes are confined to this freshly exec'd, single-test
        // process. No fork inherits locks and no other test changes identity.
        let outer = std::path::PathBuf::from(
            std::env::var_os("TILO_NATIVE_REPLACEMENT_PERMISSION_ROOT").unwrap(),
        );
        let f = Fixture::with_storage(
            rights()
                .into_iter()
                .filter(|r| *r != FsRight::Purge)
                .collect(),
            tempfile::tempdir_in(&outer).unwrap(),
            tempfile::Builder::new()
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(&outer)
                .unwrap(),
        );
        fs::write(f.source("document"), b"original bytes").unwrap();
        fs::write(f.source("temporary"), b"saved bytes").unwrap();
        let old_inode = fs::metadata(f.source("document")).unwrap().ino();
        put_attribute(
            &f.source("document"),
            b"user.original",
            b"original attribute",
        );
        let mut acl = 2_u32.to_le_bytes().to_vec();
        for (tag, permissions, id) in [
            (1_u16, 6_u16, u32::MAX),
            (2, 4, 60004),
            (4, 6, u32::MAX),
            (16, 6, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&permissions.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        put_attribute(&f.source("document"), b"system.posix_acl_access", &acl);
        put_attribute(&f.source("temporary"), b"user.temporary", b"temporary only");
        chown_fixture_tree(f.disk.path(), ORIGINAL_UID, ORIGINAL_GID);
        chown_fixture_tree(f.meta.path(), ORIGINAL_UID, ORIGINAL_GID);
        chown_fixture_tree(
            &f.source("document"),
            if nonowned { 0 } else { ORIGINAL_UID },
            SHARED_GID,
        );
        // The host owns the inaccessible parent; the original user owns only the
        // held custody leaf. It cannot reach custody by its public pathname.
        // The guard's namespace parent is root-owned and not writable by the
        // original user, just as on the protected original source volume.
        let guard = outer.join("root-capture-guard");
        fs::DirBuilder::new().mode(0o700).create(&guard).unwrap();
        let leaf = guard.join("entries");
        fs::DirBuilder::new().mode(0o700).create(&leaf).unwrap();
        chown_fixture_tree(&leaf, ORIGINAL_UID, ORIGINAL_GID);
        let custody = fs::File::open(&leaf).unwrap();
        let guard_parent = fs::metadata(&outer).unwrap();
        assert_eq!(guard_parent.uid(), 0);
        assert_eq!(guard_parent.mode() & 0o022, 0);
        let control = fs::metadata(f.meta.path()).unwrap();
        assert_eq!(control.uid(), ORIGINAL_UID);
        assert_eq!(control.mode() & 0o777, 0o700);
        let held_custody = custody.metadata().unwrap();
        assert_eq!(held_custody.uid(), ORIGINAL_UID);
        assert_eq!(held_custody.mode() & 0o777, 0o700);
        // Root-created sealed derivative uses the production admission method.
        let raw = unsafe {
            libc::memfd_create(
                c"actual-original-uid-owner".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        assert!(raw >= 0);
        let mut source_owner = unsafe { fs::File::from_raw_fd(raw) };
        source_owner.write_all(&[37; 32]).unwrap();
        assert_eq!(
            unsafe {
                libc::fcntl(
                    source_owner.as_raw_fd(),
                    libc::F_ADD_SEALS,
                    libc::F_SEAL_WRITE
                        | libc::F_SEAL_GROW
                        | libc::F_SEAL_SHRINK
                        | libc::F_SEAL_SEAL,
                )
            },
            0
        );
        // Only disposable fixture ancestors are traversable after the drop.
        fs::set_permissions(&outer, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(unsafe { libc::setgroups(1, [SHARED_GID].as_ptr()) }, 0);
        assert_eq!(unsafe { libc::setgid(ORIGINAL_GID) }, 0);
        assert_eq!(unsafe { libc::setuid(ORIGINAL_UID) }, 0);
        assert_eq!(rustix::process::getuid().as_raw(), ORIGINAL_UID);
        assert_eq!(rustix::process::geteuid().as_raw(), ORIGINAL_UID);
        assert_eq!(rustix::process::getgid().as_raw(), ORIGINAL_GID);
        let mut groups = [0_u32; 1];
        assert_eq!(unsafe { libc::getgroups(1, groups.as_mut_ptr()) }, 1);
        assert_eq!(groups, [SHARED_GID]);
        f.session.bind_removal_directory(&f.src, custody).unwrap();
        f.session
            .bind_retained_source_owner(&f.src, source_owner)
            .unwrap();
        // The root-owned victim is readable/writable only through the actual
        // supplementary group. This assertion precedes any replacement syscall.
        assert_eq!(fs::read(f.source("document")).unwrap(), b"original bytes");
        let expected = f
            .session
            .replacement_attributes(&f.entry("document"))
            .unwrap();
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        assert!(prepared.archive_plan_digest().is_ok());
        let mut job = prepared.start().unwrap();
        let updates = drain(&mut job);
        if nonowned {
            assert_eq!(item(&updates).state, ItemState::RecoveryRequired);
            let metadata = fs::metadata(f.source("document")).unwrap();
            assert_eq!(metadata.ino(), old_inode);
            assert_eq!(metadata.uid(), 0);
            assert_eq!(metadata.gid(), SHARED_GID);
            assert_eq!(fs::read(f.source("document")).unwrap(), b"original bytes");
            assert_eq!(
                f.session
                    .replacement_attributes(&f.entry("document"))
                    .unwrap(),
                expected
            );
            let directory =
                crate::directory::DirectoryCapability::new(f.session.clone(), f.src.clone())
                    .unwrap();
            let recovery = directory.recoveries(&[]).unwrap();
            assert_eq!(recovery.entries.len(), 1);
            let mut restore = directory
                .restore_captured(recovery.entries[0].recovery, name("returned-temporary"))
                .unwrap();
            assert_eq!(item(&drain(&mut restore)).state, ItemState::Completed);
            assert_eq!(
                fs::read(f.source("returned-temporary")).unwrap(),
                b"saved bytes"
            );
        } else {
            assert_eq!(item(&updates).state, ItemState::Completed);
            let metadata = fs::metadata(f.source("document")).unwrap();
            assert_eq!(metadata.uid(), ORIGINAL_UID);
            assert_eq!(metadata.gid(), SHARED_GID);
            assert_eq!(fs::read(f.source("document")).unwrap(), b"saved bytes");
            assert_eq!(
                f.session
                    .replacement_attributes(&f.entry("document"))
                    .unwrap(),
                expected
            );
            assert!(!f.source("temporary").exists());
        }
    }
    #[cfg(target_os = "linux")]
    struct OwnedPermissionChild {
        child: std::process::Child,
        retired: bool,
    }
    #[cfg(target_os = "linux")]
    impl Drop for OwnedPermissionChild {
        fn drop(&mut self) {
            // Failed kill/wait never permits deletion of the child's fixture.
            // Keep ownership and retry off the UI until actual observed reap;
            // the enclosing native-provider watchdog owns the total failure bound.
            let mut reported = false;
            while !self.retired {
                let killed = self.child.kill();
                match self.child.wait() {
                    Ok(_) => self.retired = true,
                    Err(error) => {
                        if !reported {
                            eprintln!(
                                "owned native child not reaped; retaining fixture: kill={killed:?}, wait={error}"
                            );
                            reported = true;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    fn drain_permission_output(reader: &mut impl std::io::Read, output: &mut Vec<u8>) {
        let mut bytes = [0_u8; 4096];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    assert!(
                        output.len().saturating_add(count) <= 65_536,
                        "owned native test output exceeded bounded diagnostics"
                    );
                    output.extend_from_slice(&bytes[..count]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => break,
                Err(error) => panic!("actual owned child output failed: {error}"),
            }
        }
    }
    #[cfg(target_os = "linux")]
    fn chown_fixture_tree(path: &std::path::Path, uid: u32, gid: u32) {
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: root fixture construction, owned disposable paths only.
        assert_eq!(
            unsafe { libc::chown(name.as_ptr(), uid, gid) },
            0,
            "chown {path:?} to {uid}:{gid}: {}; credentials: {}; uid map: {}; gid map: {}",
            std::io::Error::last_os_error(),
            fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .filter(|line| {
                    ["Uid:", "Gid:", "Groups:", "Cap", "NoNewPrivs:", "Seccomp:"]
                        .iter()
                        .any(|prefix| line.starts_with(prefix))
                })
                .collect::<Vec<_>>()
                .join("; "),
            fs::read_to_string("/proc/self/uid_map").unwrap(),
            fs::read_to_string("/proc/self/gid_map").unwrap(),
        );
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                chown_fixture_tree(&entry.unwrap().path(), uid, gid);
            }
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn replacing_save_attribute_change_at_pre_capture_boundary_keeps_both_entries() {
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        put_attribute(&f.source("document"), b"user.original", b"first");
        let barrier = Arc::new(Barrier::default());
        let _release = Release(barrier.clone());
        f.hooks(Hooks {
            barrier: Some(("ReplaceMetadataObservationBoundary", barrier.clone())),
            ..Hooks::default()
        });
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        let mut job = prepared.start().unwrap();
        barrier.entered();
        put_attribute(&f.source("document"), b"user.original", b"external change");
        barrier.release();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Failed);
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        assert!(
            fs::read_dir(f.disk.path().join("root-capture-guard/entries"))
                .unwrap()
                .next()
                .is_none()
        );
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn prepared_replacement_refuses_changed_attributes_without_namespace_effects() {
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        put_attribute(&f.source("document"), b"user.original", b"first");
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        assert!(prepared.archive_plan_digest().is_ok());
        put_attribute(&f.source("document"), b"user.original", b"changed");
        assert!(prepared.intent_digest().is_err());
        assert!(prepared.archive_plan_digest().is_err());
        assert!(prepared.reservation_description().is_err());
        assert!(prepared.start().is_err());
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        assert!(
            fs::read_dir(f.disk.path().join("root-capture-guard/entries"))
                .unwrap()
                .next()
                .is_none()
        );
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn prepared_replacement_refuses_changed_lengths_without_namespace_effects() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        let old_identity = fs::metadata(f.source("document")).unwrap().ino();
        let temporary_identity = fs::metadata(f.source("temporary")).unwrap().ino();
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        assert!(prepared.reservation_description().is_ok());
        // An external original-user writer changes the held victim's real length.
        fs::write(f.source("document"), b"external original-user update").unwrap();
        assert!(prepared.reservation_description().is_err());
        assert!(prepared.intent_digest().is_err());
        assert!(prepared.archive_plan_digest().is_err());
        assert!(prepared.start().is_err());
        assert_eq!(
            fs::metadata(f.source("document")).unwrap().ino(),
            old_identity
        );
        assert_eq!(
            fs::metadata(f.source("temporary")).unwrap().ino(),
            temporary_identity
        );
        assert_eq!(
            fs::read(f.source("document")).unwrap(),
            b"external original-user update"
        );
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        assert!(
            fs::read_dir(f.disk.path().join("root-capture-guard/entries"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn prepared_replacement_intent_refuses_revoked_actual_scope_without_effects() {
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        assert!(prepared.intent_digest().is_ok());
        f.authority.revoke_directory(&f.src).unwrap();
        assert!(prepared.intent_digest().is_err());
        assert!(prepared.start().is_err());
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
    }

    #[test]
    fn replacing_save_preserves_unreadable_victim_without_read_grant() {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let f = Fixture::with_rights(
            rights()
                .into_iter()
                .filter(|right| *right != FsRight::Read)
                .collect(),
        );
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"unreadable old").unwrap();
        let mut original = fs::File::open(f.source("document")).unwrap();
        fs::set_permissions(f.source("document"), fs::Permissions::from_mode(0o0)).unwrap();
        let old = fs::metadata(f.source("document")).unwrap().ino();
        fs::write(f.source("temporary"), b"new").unwrap();
        let mut saved = fs::File::open(f.source("temporary")).unwrap();
        let victim = f.entry("document");
        #[cfg(target_os = "linux")]
        assert_path_only_victim(&f.session, &victim);
        let (id, updates) = f.run(FsOperation::Replace {
            entry: f.entry("temporary"),
            into: f.src.clone(),
            name: name("document"),
            victim,
        });
        assert_eq!(item(&updates).state, ItemState::Completed);
        let pin = f.store.pin_replacement(&f.session, &id, &f.src).unwrap();
        assert_eq!(pin.victim_identity()[1], old);
        let recovery = pin.capture_id();
        drop(pin);
        let directory =
            crate::directory::DirectoryCapability::new(f.session.clone(), f.src.clone()).unwrap();
        let mut restore = directory
            .restore_captured(*recovery.as_bytes(), name("old-restored"))
            .unwrap();
        assert_eq!(item(&drain(&mut restore)).state, ItemState::Completed);
        let metadata = fs::metadata(f.source("old-restored")).unwrap();
        assert_eq!(metadata.ino(), old);
        assert_eq!(metadata.permissions().mode() & 0o777, 0);
        let published = fs::metadata(f.source("document")).unwrap();
        assert_eq!(published.ino(), saved.metadata().unwrap().ino());
        assert_eq!(published.permissions().mode() & 0o777, 0);
        // Verifier-only read descriptors predate chmod and are never admitted
        // to the session, whose namespace-only job has no Read grant.
        let mut bytes = Vec::new();
        saved.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"new");
        assert_eq!(metadata.ino(), original.metadata().unwrap().ino());
        bytes.clear();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"unreadable old");
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
            FsRight::Copy,
            FsRight::Move,
            FsRight::Trash,
        ]
    }
    #[test]
    fn replace_permission_preserves_history_without_admitting_raw_remove() {
        let f = Fixture::with_rights(
            rights()
                .into_iter()
                .filter(|r| *r != FsRight::Purge)
                .collect(),
        );
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        let (_, refused) = f.run(FsOperation::Remove {
            entry: f.entry("document"),
            directory: false,
        });
        assert_eq!(item(&refused).state, ItemState::Failed);
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        let prepared = f
            .session
            .prepare_replace(
                f.entry("temporary"),
                f.src.clone(),
                name("document"),
                f.entry("document"),
            )
            .unwrap();
        let mut job = prepared.start().unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        assert_eq!(fs::read(f.source("document")).unwrap(), b"new");
        assert_eq!(f.store.recoveries().unwrap().len(), 1);
    }
    #[test]
    fn purge_permission_does_not_admit_history_preserving_replace() {
        let f = Fixture::with_rights(
            rights()
                .into_iter()
                .filter(|r| *r != FsRight::Replace)
                .collect(),
        );
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        assert!(
            f.session
                .prepare_replace(
                    f.entry("temporary"),
                    f.src.clone(),
                    name("document"),
                    f.entry("document")
                )
                .is_err()
        );
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        assert!(
            fs::read_dir(f.disk.path().join("root-capture-guard/entries"))
                .unwrap()
                .next()
                .is_none()
        );
    }
    #[test]
    fn replacing_save_refuses_aliased_temporary_before_any_effect() {
        let f = Fixture::new();
        f.bind_removal();
        f.bind_source_key();
        fs::write(f.source("document"), b"old").unwrap();
        fs::write(f.source("temporary"), b"new").unwrap();
        fs::hard_link(f.source("temporary"), f.source("alias")).unwrap();
        assert!(
            f.session
                .prepare_replace(
                    f.entry("temporary"),
                    f.src.clone(),
                    name("document"),
                    f.entry("document")
                )
                .is_err()
        );
        assert_eq!(fs::read(f.source("document")).unwrap(), b"old");
        assert_eq!(fs::read(f.source("temporary")).unwrap(), b"new");
        assert_eq!(fs::read(f.source("alias")).unwrap(), b"new");
        assert!(
            fs::read_dir(f.disk.path().join("root-capture-guard/entries"))
                .unwrap()
                .next()
                .is_none()
        );
    }
    /// Real completed native creation for connected custody-only RPC tests.
    /// It grants neither Replace nor Purge and constructs no daemon proof.
    pub(crate) struct ActualCreatedTemporaryFixture {
        fixture: Fixture,
        pub session: FsSession,
        pub parent: DirectoryGrant,
        pub job: Option<FsJob>,
    }
    impl ActualCreatedTemporaryFixture {
        pub(crate) fn revoke(&self) -> Result<(), FsError> {
            self.fixture.authority.revoke_directory(&self.fixture.src)
        }
        pub(crate) fn assert_original_entries(&self) {
            assert_eq!(
                fs::read(self.fixture.source("temporary")).unwrap(),
                b"own save bytes"
            );
            assert_eq!(
                fs::read(self.fixture.source("document")).unwrap(),
                b"untouched document"
            );
        }
        pub(crate) fn assert_retired_recoverably(&self) {
            assert!(!self.fixture.source("temporary").exists());
            assert_eq!(
                fs::read(self.fixture.source("document")).unwrap(),
                b"untouched document"
            );
            assert!(
                self.fixture
                    .store
                    .recoveries()
                    .unwrap()
                    .iter()
                    .any(|r| r.kind == RecoveryKind::QuarantinedNative)
            );
            let files: Vec<_> =
                fs::read_dir(self.fixture.disk.path().join("root-capture-guard/entries"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
            assert_eq!(files.len(), 1);
            assert_eq!(fs::read(&files[0]).unwrap(), b"own save bytes");
        }
    }
    pub(crate) fn completed_created_temporary_for_transport() -> ActualCreatedTemporaryFixture {
        let f = create_cleanup_fixture();
        fs::write(f.source("document"), b"untouched document").unwrap();
        let mut job = f
            .session
            .start(FsOperation::CreateFile {
                parent: f.src.clone(),
                name: name("temporary"),
                bytes: b"own save bytes".to_vec(),
                conflict: ConflictPolicy::Fail,
            })
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        job.created_temporary().unwrap();
        ActualCreatedTemporaryFixture {
            session: f.session.clone(),
            parent: f.src.clone(),
            job: Some(job),
            fixture: f,
        }
    }

    /// Genuine metadata/read source custody only. No Edit, Replace, Purge,
    /// daemon enrollment or installed source proof is synthesized by this test.
    pub(crate) struct ActualHistoryReadFixture {
        fixture: Fixture,
        pub session: FsSession,
        pub parent: DirectoryGrant,
        pub name: EntryName,
    }
    impl ActualHistoryReadFixture {
        pub(crate) fn revoke(&self) -> Result<(), FsError> {
            self.fixture.authority.revoke_directory(&self.fixture.src)
        }
        pub(crate) fn link_name(&self) -> EntryName {
            let alias = name("alias");
            std::os::unix::fs::symlink(
                self.name.as_os_str(),
                self.fixture.source("nested").join(alias.as_os_str()),
            )
            .unwrap();
            alias
        }
        pub(crate) fn substitute_leaf(&self) {
            let parent = self.fixture.source("nested");
            fs::rename(parent.join(self.name.as_os_str()), parent.join("held-old")).unwrap();
            fs::write(parent.join(self.name.as_os_str()), b"substituted").unwrap();
        }
        pub(crate) fn move_parent(&self) {
            fs::rename(self.fixture.source("nested"), self.fixture.source("moved")).unwrap();
            fs::create_dir(self.fixture.source("nested")).unwrap();
            fs::write(
                self.fixture.source("nested").join(self.name.as_os_str()),
                b"new-parent",
            )
            .unwrap();
        }
    }
    pub(crate) fn history_read_for_transport(with_read: bool) -> ActualHistoryReadFixture {
        let allowed = if with_read {
            vec![FsRight::Enumerate, FsRight::Read]
        } else {
            vec![FsRight::Enumerate]
        };
        let fixture = Fixture::with_rights(allowed);
        fixture.bind_removal();
        fixture.bind_source_key();
        fs::create_dir(fixture.source("nested")).unwrap();
        let name = EntryName::new(OsString::from_vec(b"document-\xff".to_vec())).unwrap();
        fs::write(
            fixture.source("nested").join(name.as_os_str()),
            b"actual readable bytes",
        )
        .unwrap();
        let GrantedEntry::Directory(parent) = fixture
            .session
            .lookup(&fixture.src, &name_for_history_parent())
            .unwrap()
        else {
            panic!("actual nested directory required")
        };
        ActualHistoryReadFixture {
            session: fixture.session.clone(),
            parent,
            name,
            fixture,
        }
    }
    fn name_for_history_parent() -> EntryName {
        name("nested")
    }

    /// Genuine zero-effect native reservation for connected negative RPC tests.
    /// This fixture does not construct a daemon verifier or a positive receipt.
    pub(crate) struct ActualPendingFixture {
        fixture: Fixture,
        pub session: FsSession,
        pub parent: DirectoryGrant,
        pub prepared: Option<PreparedNativeReplacement>,
        pub temporary_identity: [u64; 2],
        pub victim_identity: [u64; 2],
    }
    impl ActualPendingFixture {
        pub(crate) fn revoke(&self) -> Result<(), FsError> {
            self.fixture.authority.revoke_directory(&self.fixture.src)
        }
        pub(crate) fn assert_original_entries(&self) {
            use std::os::unix::fs::MetadataExt;
            for (name, identity, bytes) in [
                (
                    "temporary",
                    self.temporary_identity,
                    b"transport new".as_slice(),
                ),
                (
                    "document",
                    self.victim_identity,
                    b"transport old".as_slice(),
                ),
            ] {
                let metadata = fs::metadata(self.fixture.source(name)).unwrap();
                assert_eq!([metadata.dev(), metadata.ino()], identity);
                assert_eq!(fs::read(self.fixture.source(name)).unwrap(), bytes);
            }
            assert!(
                fs::read_dir(self.fixture.disk.path().join("root-capture-guard/entries"))
                    .unwrap()
                    .next()
                    .is_none()
            );
        }
    }
    #[test]
    fn genuine_creator_without_selected_authority_cannot_mint_atomic_save() {
        fn journal(
            root: &std::path::Path,
        ) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
            fn walk(
                root: &std::path::Path,
                directory: &std::path::Path,
                out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
            ) {
                for entry in fs::read_dir(directory).unwrap() {
                    let entry = entry.unwrap();
                    let kind = entry.file_type().unwrap();
                    assert!(
                        !kind.is_symlink(),
                        "owned journal fixture never follows links"
                    );
                    if kind.is_dir() {
                        walk(root, &entry.path(), out);
                    } else {
                        assert!(kind.is_file());
                        out.insert(
                            entry.path().strip_prefix(root).unwrap().to_path_buf(),
                            fs::read(entry.path()).unwrap(),
                        );
                    }
                }
            }
            let mut out = std::collections::BTreeMap::new();
            walk(root, root, &mut out);
            out
        }
        let fixture = completed_created_temporary_for_transport();
        let cap = fixture.job.as_ref().unwrap().created_temporary().unwrap();
        let victim = fixture
            .session
            .lookup(&fixture.parent, &name("document"))
            .unwrap();
        // The real CreateFile setup durably retained its published source receipt.
        // Denial must leave that journal and every original inode unchanged.
        let recoveries = || {
            fixture
                .fixture
                .store
                .recoveries()
                .unwrap()
                .into_iter()
                .map(|r| (r.id, r.operation, r.kind, r.original_name, r.acknowledged))
                .collect::<Vec<_>>()
        };
        let originals = || {
            use std::os::unix::fs::MetadataExt;
            ["temporary", "document"].map(|name| {
                let path = fixture.fixture.source(name);
                let m = fs::symlink_metadata(&path).unwrap();
                (
                    [m.dev(), m.ino(), m.len(), m.nlink()],
                    [m.uid(), m.gid(), m.mode()],
                    [m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()],
                    fs::read(path).unwrap(),
                )
            })
        };
        let before_recoveries = recoveries();
        assert_eq!(
            before_recoveries.len(),
            1,
            "actual completed creator has its durable staging receipt"
        );
        assert_eq!(
            before_recoveries[0].2,
            RecoveryKind::PublishedSourceRetained
        );
        assert_eq!(
            before_recoveries[0].1,
            fixture.job.as_ref().unwrap().operation_id()
        );
        let before_journal = journal(fixture.fixture.meta.path());
        assert!(!before_journal.is_empty());
        let before_originals = originals();
        assert!(
            fs::read_dir(
                fixture
                    .fixture
                    .disk
                    .path()
                    .join("root-capture-guard/entries")
            )
            .unwrap()
            .next()
            .is_none()
        );
        assert!(matches!(
            fixture.session.prepare_atomic_save(
                &cap,
                fixture.parent.clone(),
                name("document"),
                victim,
            ),
            Err(FsError::Denied)
        ));
        cap.check().unwrap();
        fixture.assert_original_entries();
        assert_eq!(
            recoveries(),
            before_recoveries,
            "denied AtomicSave added or changed a durable recovery"
        );
        assert_eq!(
            journal(fixture.fixture.meta.path()),
            before_journal,
            "denied AtomicSave changed durable journal bytes"
        );
        assert_eq!(
            originals(),
            before_originals,
            "denied AtomicSave changed source inode, attributes, or bytes"
        );
        assert!(
            fs::read_dir(
                fixture
                    .fixture
                    .disk
                    .path()
                    .join("root-capture-guard/entries")
            )
            .unwrap()
            .next()
            .is_none(),
            "denied AtomicSave captured an original inode"
        );
    }
    #[test]
    fn actual_pending_description_decodes_real_source_facts_without_effects() {
        use crate::external_history::NativeReservationDescription;
        use std::os::unix::fs::MetadataExt;
        let fixture = pending_replacement_for_transport();
        let pending = fixture.prepared.as_ref().unwrap();
        let decoded = NativeReservationDescription::decode(
            &pending.reservation_description().unwrap(),
            *pending.operation_id().unwrap().as_bytes(),
            pending.source_owner(),
            pending.intent_digest().unwrap(),
        )
        .unwrap();
        assert_eq!(
            decoded.purpose(),
            NativeReplacementPurpose::ReplacementRename
        );
        for (name, locator, facts, permissions) in [
            (
                "temporary",
                decoded.source(),
                decoded.source_facts(),
                decoded.source_permissions(),
            ),
            (
                "document",
                decoded.victim(),
                decoded.victim_facts(),
                decoded.victim_permissions(),
            ),
        ] {
            let actual = fs::metadata(fixture.fixture.source(name)).unwrap();
            assert_eq!(locator.name(), name.as_bytes());
            assert_eq!(locator.identity(), Some([actual.dev(), actual.ino()]));
            assert_eq!(facts.byte_len, actual.len());
            assert_eq!(facts.mtime, [actual.mtime(), actual.mtime_nsec()]);
            assert_eq!(facts.ctime, [actual.ctime(), actual.ctime_nsec()]);
            assert_eq!(facts.nlink, actual.nlink());
            assert_eq!(
                permissions,
                (actual.uid(), actual.gid(), actual.mode() & 0o7777)
            );
        }
        assert_eq!(decoded.destination().name(), b"document");
        assert_eq!(
            decoded.destination().identity(),
            decoded.source().identity()
        );
        fixture.assert_original_entries();
        fixture.revoke().unwrap();
        assert!(pending.reservation_description().is_err());
        fixture.assert_original_entries();
    }
    pub(crate) fn pending_replacement_for_transport() -> ActualPendingFixture {
        let fixture = Fixture::with_rights(
            rights()
                .into_iter()
                .filter(|r| *r != FsRight::Purge)
                .collect(),
        );
        fixture.bind_removal();
        fixture.bind_source_key();
        fs::write(fixture.source("document"), b"transport old").unwrap();
        fs::write(fixture.source("temporary"), b"transport new").unwrap();
        let prepared = fixture
            .session
            .prepare_replace(
                fixture.entry("temporary"),
                fixture.src.clone(),
                name("document"),
                fixture.entry("document"),
            )
            .unwrap();
        ActualPendingFixture {
            session: fixture.session.clone(),
            parent: fixture.src.clone(),
            temporary_identity: prepared.source_identity(),
            victim_identity: prepared.expected_victim_identity(),
            prepared: Some(prepared),
            fixture,
        }
    }
    /// Actual production native capture/EXCHANGE fixture for connected RPC tests.
    /// Keeps the host authority, source disk and operation journal alive.
    pub(crate) struct ActualReplacementFixture {
        fixture: Fixture,
        pub session: FsSession,
        pub parent: DirectoryGrant,
        pub job: Option<FsJob>,
    }
    impl ActualReplacementFixture {
        pub(crate) fn revoke(&self) -> Result<(), FsError> {
            self.fixture.authority.revoke_directory(&self.fixture.src)
        }
    }
    pub(crate) fn completed_replacement_for_transport() -> ActualReplacementFixture {
        let fixture = Fixture::new();
        fixture.bind_removal();
        fixture.bind_source_key();
        fs::write(fixture.source("document"), b"transport old").unwrap();
        fs::write(fixture.source("temporary"), b"transport new").unwrap();
        let mut job = fixture
            .session
            .start(FsOperation::Replace {
                entry: fixture.entry("temporary"),
                into: fixture.src.clone(),
                name: name("document"),
                victim: fixture.entry("document"),
            })
            .unwrap();
        assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
        ActualReplacementFixture {
            session: fixture.session.clone(),
            parent: fixture.src.clone(),
            job: Some(job),
            fixture,
        }
    }
    struct Fixture {
        authority: NativeGrantAuthority,
        session: FsSession,
        src: DirectoryGrant,
        dst: DirectoryGrant,
        store: Arc<OperationOwner>,
        disk: TempDir,
        meta: TempDir,
    }
    impl Fixture {
        fn new() -> Self {
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
        fn bind_removal(&self) {
            assert_eq!(
                rustix::process::geteuid().as_raw(),
                0,
                "root native namespace fixture required"
            );
            let guard = self.disk.path().join("root-capture-guard");
            fs::DirBuilder::new().mode(0o700).create(&guard).unwrap();
            let leaf = guard.join("entries");
            fs::DirBuilder::new().mode(0o700).create(&leaf).unwrap();
            self.session
                .bind_removal_directory(&self.src, fs::File::open(leaf).unwrap())
                .unwrap();
        }
        #[cfg(target_os = "linux")]
        fn bind_source_key(&self) {
            use std::io::Write;
            use std::os::fd::{AsRawFd, FromRawFd};
            let raw = unsafe {
                libc::memfd_create(
                    c"actual-native-owner-fixture".as_ptr(),
                    libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
                )
            };
            assert!(raw >= 0);
            let mut sealed = unsafe { fs::File::from_raw_fd(raw) };
            sealed.write_all(&[37; 32]).unwrap();
            let seals =
                libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
            assert_eq!(
                unsafe { libc::fcntl(sealed.as_raw_fd(), libc::F_ADD_SEALS, seals) },
                0
            );
            self.session
                .bind_retained_source_owner(&self.src, sealed)
                .unwrap();
        }
        #[cfg(not(target_os = "linux"))]
        fn bind_source_key(&self) {
            panic!("Linux root native source fixture required");
        }
        fn source(&self, n: &str) -> std::path::PathBuf {
            self.disk.path().join("src").join(n)
        }
        fn dest(&self, n: &str) -> std::path::PathBuf {
            self.disk.path().join("dst").join(n)
        }
        fn entry(&self, n: &str) -> GrantedEntry {
            self.session.lookup(&self.src, &name(n)).unwrap()
        }
        fn hooks(&self, h: Hooks) {
            *self.store.hooks.lock().unwrap() = h;
        }
        fn copy(&self, n: &str) -> FsOperation {
            FsOperation::Copy {
                entries: vec![self.entry(n)],
                into: self.dst.clone(),
                conflict: ConflictPolicy::Fail,
            }
        }
        fn moving(&self, n: &str) -> FsOperation {
            FsOperation::Move {
                entries: vec![self.entry(n)],
                into: self.dst.clone(),
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
        // phase: terminal_metadata_room_is_reserved_before_first_effect
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"quota guarded").unwrap();
            f.hooks(Hooks {
                metadata_limit: Some(2048),
                ..Hooks::default()
            });
            assert!(matches!(
                f.session.start(FsOperation::Copy {
                    entries: vec![f.entry("a"); 20],
                    into: f.dst.clone(),
                    conflict: ConflictPolicy::Fail
                }),
                Err(FsError::LimitExceeded)
            ));
            assert!(f.receipt_bytes().is_empty());
            assert_eq!(fs::read_dir(f.dest("")).unwrap().count(), 0);
            assert_eq!(fs::read(f.source("a")).unwrap(), b"quota guarded");
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn uncertain_outcomes_are_acknowledged_and_never_replayed() {
        // phase: uncertain_native_trash_is_not_replayed
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"uncertain").unwrap();
            f.hooks(Hooks {
                unknown_trash: true,
                ..Hooks::default()
            });
            let (id, u) = f.run(FsOperation::Trash {
                entries: vec![f.entry("a")],
            });
            assert_eq!(item(&u).state, ItemState::OutcomeUnknown);
            assert_eq!(
                f.store
                    .submissions
                    .load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            let f = f.reopen();
            assert_eq!(
                f.store
                    .submissions
                    .load(std::sync::atomic::Ordering::SeqCst),
                0
            );
            let r = f.store.recoveries().unwrap().remove(0);
            assert_eq!(r.kind, RecoveryKind::UnknownNativeCommit);
            assert!(matches!(
                f.store.resolve(
                    &r.id,
                    &f.session,
                    RecoveryResolution::AcknowledgeUnknown {
                        note: "x".repeat(1025)
                    }
                ),
                Err(FsError::LimitExceeded)
            ));
            let mut job = f
                .store
                .resolve(
                    &r.id,
                    &f.session,
                    RecoveryResolution::AcknowledgeUnknown {
                        note: "uncertainty accepted".into(),
                    },
                )
                .unwrap();
            drain(&mut job);
            assert_eq!(
                f.store.report(&id).unwrap().state,
                OperationState::OutcomeUnknown
            );
            assert_eq!(fs::read(f.source("a")).unwrap(), b"uncertain");
            assert!(f.store.recoveries().unwrap()[0].acknowledged);
        }
        // phase: uncertain_non_trash_boundary_is_acknowledged_without_replay
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"native rename").unwrap();
            f.hooks(Hooks {
                fail_checkpoint: Some("Published"),
                ..Hooks::default()
            });
            let (id, updates) = f.run(f.moving("a"));
            assert_eq!(item(&updates).state, ItemState::OutcomeUnknown);
            assert!(item(&updates).destination.is_some());
            assert!(!f.source("a").exists());
            assert_eq!(fs::read(f.dest("a")).unwrap(), b"native rename");
            fs::rename(f.dest("a"), f.dest("held")).unwrap();
            let f = f.reopen();
            let recovery = f
                .store
                .recoveries()
                .unwrap()
                .into_iter()
                .find(|r| r.operation == id)
                .unwrap();
            assert_eq!(recovery.kind, RecoveryKind::UnknownNativeCommit);
            assert_eq!(
                f.store.report(&id).unwrap().state,
                OperationState::OutcomeUnknown
            );
            let mut job = f
                .store
                .resolve(
                    &recovery.id,
                    &f.session,
                    RecoveryResolution::AcknowledgeUnknown {
                        note: "cannot establish current rename outcome".into(),
                    },
                )
                .unwrap();
            drain(&mut job);
            assert_eq!(
                f.store.report(&id).unwrap().state,
                OperationState::OutcomeUnknown
            );
            assert!(!f.source("a").exists());
            assert!(!f.dest("a").exists());
            assert_eq!(fs::read(f.dest("held")).unwrap(), b"native rename");
        }
        // phase: acknowledge_report_preserves_retained_source_history
        {
            let f = Fixture::new();
            let (id, _) = retained(&f);
            f.store.acknowledge_report(&id).unwrap();
            let f = f.reopen();
            assert_eq!(
                f.store.report(&id).unwrap().items[0].state,
                ItemState::CopiedSourceRetained
            );
            assert_eq!(
                f.store.report(&id).unwrap().state,
                OperationState::RecoveryRequired
            );
            assert!(f.store.recoveries().unwrap()[0].acknowledged);
            for p in [f.source("a"), f.dest("a")] {
                assert_eq!(fs::read(p).unwrap(), b"both");
            }
        }
        // phase: acknowledge_report_atomically_refuses_mixed_unresolved_recoveries
        {
            for unknown in [false, true] {
                let f = Fixture::new();
                let (id, _) = retained(&f);
                fs::write(f.source("b"), b"blocked recovery").unwrap();
                f.hooks(Hooks {
                    unknown_trash: unknown,
                    stop_after_stage: !unknown,
                    ..Hooks::default()
                });
                let (second, _) = if unknown {
                    f.run(FsOperation::Trash {
                        entries: vec![f.entry("b")],
                    })
                } else {
                    f.run(f.copy("b"))
                };
                f.store.merge_recovery_for_test(&id, &second).unwrap();
                let before = f.receipt_bytes();
                assert!(f.store.acknowledge_report(&id).is_err());
                assert_eq!(f.receipt_bytes(), before);
                assert!(
                    f.store
                        .recoveries()
                        .unwrap()
                        .iter()
                        .all(|r| !r.acknowledged)
                );
                let blocked = f
                    .store
                    .recoveries()
                    .unwrap()
                    .into_iter()
                    .find(|r| r.operation == id && r.kind != RecoveryKind::PublishedSourceRetained)
                    .unwrap();
                f.hooks(Hooks::default());
                let resolution = if unknown {
                    RecoveryResolution::AcknowledgeUnknown {
                        note: "accepted unknown".into(),
                    }
                } else {
                    assert_eq!(fs::read(f.stage()).unwrap(), b"blocked recovery");
                    RecoveryResolution::RestoreKnown {
                        parent: f.dst.clone(),
                        name: name("restored-b"),
                    }
                };
                let mut job = f
                    .store
                    .resolve(&blocked.id, &f.session, resolution)
                    .unwrap();
                assert_eq!(item(&drain(&mut job)).state, ItemState::Completed);
                drop(job);
                f.store.acknowledge_report(&id).unwrap();
                assert!(
                    f.store
                        .recoveries()
                        .unwrap()
                        .iter()
                        .filter(|r| r.operation == id)
                        .all(|r| r.acknowledged)
                );
                for p in [f.source("a"), f.dest("a")] {
                    assert_eq!(fs::read(p).unwrap(), b"both");
                }
            }
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn cancellation_revocation_and_teardown_keep_committed_work() {
        // phase: cancel_after_first_item_retains_committed_item
        {
            let f = Fixture::new();
            for n in ["a", "b"] {
                fs::write(f.source(n), n.as_bytes()).unwrap();
            }
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                barrier: Some(("before-item-two", barrier.clone())),
                ..Hooks::default()
            });
            let mut job = f
                .session
                .start(FsOperation::Copy {
                    entries: vec![f.entry("a"), f.entry("b")],
                    into: f.dst.clone(),
                    conflict: ConflictPolicy::Fail,
                })
                .unwrap();
            let release = Release(barrier.clone());
            barrier.entered();
            assert_eq!(fs::read(f.dest("a")).unwrap(), b"a");
            job.request_cancel();
            drop(release);
            let u = drain(&mut job);
            let states: Vec<_> = u
                .iter()
                .filter_map(|u| {
                    if let FsUpdate::Item(i) = u {
                        Some(i.state)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(states, [ItemState::Completed, ItemState::Cancelled]);
            assert!(matches!(
                u.last(),
                Some(FsUpdate::Finished {
                    completed: 1,
                    failed: 0,
                    cancelled: 1
                })
            ));
            assert!(!f.dest("b").exists());
        }
        // phase: revocation_during_copy_does_not_remove_source
        {
            let f = Fixture::new();
            let bytes = vec![65; 2 * 1024 * 1024];
            fs::write(f.source("a"), &bytes).unwrap();
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                rename_error: Some(rustix::io::Errno::XDEV),
                barrier: Some(("after-chunk", barrier.clone())),
                ..Hooks::default()
            });
            let mut job = f.session.start(f.moving("a")).unwrap();
            let release = Release(barrier.clone());
            barrier.entered();
            f.authority.revoke_directory(&f.src).unwrap();
            drop(release);
            let u = drain(&mut job);
            assert_ne!(item(&u).state, ItemState::Completed);
            assert_eq!(fs::read(f.source("a")).unwrap(), bytes);
            assert!(!f.dest("a").exists());
        }
        // phase: worker_teardown_joins_after_last_owner
        {
            let f = Fixture::new();
            fs::write(f.source("a"), vec![17; 2 * 1024 * 1024]).unwrap();
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                barrier: Some(("after-chunk", barrier.clone())),
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
        // phase: non_droppable_terminal_updates_survive_progress_pressure
        {
            let f = Fixture::new();
            for n in ["a", "b"] {
                fs::write(f.source(n), vec![13; 8 * 1024 * 1024]).unwrap();
            }
            let mut job = f
                .session
                .start(FsOperation::Copy {
                    entries: vec![f.entry("a"), f.entry("b")],
                    into: f.dst.clone(),
                    conflict: ConflictPolicy::Fail,
                })
                .unwrap();
            terminal(&f, &job.operation_id());
            let u = drain(&mut job);
            assert_eq!(
                u.iter().filter(|u| matches!(u, FsUpdate::Item(_))).count(),
                2
            );
            assert_eq!(
                u.iter()
                    .filter(|u| matches!(u, FsUpdate::Finished { .. }))
                    .count(),
                1
            );
            assert!(matches!(
                u.last(),
                Some(FsUpdate::Finished {
                    completed: 2,
                    failed: 0,
                    cancelled: 0
                })
            ));
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn cross_device_moves_copy_then_remove_and_retain_sources_on_failure() {
        // phase: cross_device_copy_remove_stages
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"staged").unwrap();
            f.hooks(Hooks {
                rename_error: Some(rustix::io::Errno::XDEV),
                ..Hooks::default()
            });
            let (id, u) = f.run(f.moving("a"));
            assert_eq!(item(&u).state, ItemState::Completed);
            assert_eq!(fs::read(f.dest("a")).unwrap(), b"staged");
            assert!(!f.source("a").exists());
            let phases: Vec<_> = u
                .iter()
                .filter_map(|u| {
                    if let FsUpdate::Progress { phase, .. } = u {
                        Some(*phase)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(phases, ["copy", "publish", "remove-source"]);
            assert_eq!(
                f.store.checkpoints(&id).unwrap(),
                [
                    "Admitted",
                    "PublishStarted",
                    "Admitted",
                    "Staged",
                    "PublishStarted",
                    "Published",
                    "SourceRemovalStarted",
                    "SourceRemovalCompleted",
                    "Item",
                    "Finished"
                ]
            );
        }
        // phase: non_exdev_rename_failure_never_copies
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"original").unwrap();
            f.hooks(Hooks {
                rename_error: Some(rustix::io::Errno::IO),
                ..Hooks::default()
            });
            let (_, u) = f.run(f.moving("a"));
            assert_eq!(item(&u).state, ItemState::Failed);
            assert_eq!(fs::read(f.source("a")).unwrap(), b"original");
            assert_eq!(fs::read_dir(f.dest("")).unwrap().count(), 0);
            assert!(
                !u.iter()
                    .any(|u| matches!(u, FsUpdate::Progress { phase: "copy", .. }))
            );
        }
        // phase: empty_directory_cross_device_move_reports_all_phases
        {
            let f = Fixture::new();
            fs::create_dir(f.source("empty")).unwrap();
            f.hooks(Hooks {
                rename_error: Some(rustix::io::Errno::XDEV),
                ..Hooks::default()
            });
            let (_, updates) = f.run(f.moving("empty"));
            assert_eq!(item(&updates).state, ItemState::Completed);
            assert!(f.dest("empty").is_dir());
            assert!(!f.source("empty").exists());
            let phases: Vec<_> = updates
                .iter()
                .filter_map(|update| match update {
                    FsUpdate::Progress { phase, .. } => Some(*phase),
                    _ => None,
                })
                .collect();
            assert_eq!(phases, ["copy", "publish", "remove-source"]);
        }
        // phase: copy_eio_after_one_real_chunk_never_publishes
        {
            let f = Fixture::new();
            let bytes = vec![53; 2 * 1024 * 1024];
            fs::write(f.source("a"), &bytes).unwrap();
            f.hooks(Hooks {
                copy_eio: true,
                ..Hooks::default()
            });
            let (_, u) = f.run(f.copy("a"));
            assert_ne!(item(&u).state, ItemState::Completed);
            assert_eq!(fs::read(f.source("a")).unwrap(), bytes);
            assert!(!f.dest("a").exists());
            assert_eq!(fs::read_dir(f.dest("")).unwrap().count(), 0);
        }
        // phase: remove_failure_reports_copied_source_retained
        {
            let f = Fixture::new();
            let (id, u) = retained(&f);
            assert_eq!(item(&u).state, ItemState::CopiedSourceRetained);
            assert!(item(&u).destination.is_some());
            assert_eq!(
                f.store.recoveries().unwrap()[0].kind,
                RecoveryKind::PublishedSourceRetained
            );
            assert_eq!(
                f.store.report(&id).unwrap().state,
                OperationState::RecoveryRequired
            );
            for p in [f.source("a"), f.dest("a")] {
                assert_eq!(fs::read(p).unwrap(), b"both");
            }
            assert!(matches!(
                u.last(),
                Some(FsUpdate::Finished {
                    completed: 0,
                    failed: 1,
                    cancelled: 0
                })
            ));
        }
        // phase: directory_move_retains_new_source_bytes_before_removal
        {
            let f = Fixture::new();
            fs::create_dir(f.source("tree")).unwrap();
            fs::write(f.source("tree/original"), b"copied").unwrap();
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                rename_error: Some(rustix::io::Errno::XDEV),
                barrier: Some(("before-source-removal", barrier.clone())),
                ..Hooks::default()
            });
            let mut job = f.session.start(f.moving("tree")).unwrap();
            let release = Release(barrier.clone());
            barrier.entered();
            fs::write(f.source("tree/new"), b"new user bytes").unwrap();
            drop(release);
            let u = drain(&mut job);
            assert_eq!(item(&u).state, ItemState::CopiedSourceRetained);
            assert_eq!(fs::read(f.source("tree/new")).unwrap(), b"new user bytes");
            assert_eq!(fs::read(f.dest("tree/original")).unwrap(), b"copied");
        }
    }
    fn retained(f: &Fixture) -> (crate::recovery::OperationId, Vec<FsUpdate>) {
        fs::write(f.source("a"), b"both").unwrap();
        f.hooks(Hooks {
            rename_error: Some(rustix::io::Errno::XDEV),
            remove_error: true,
            ..Hooks::default()
        });
        f.run(f.moving("a"))
    }
    #[test]
    fn queue_item_and_candidate_limits_precede_effects() {
        // phase: worker_queue_and_item_limits_precede_effects
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"queued").unwrap();
            assert!(matches!(
                f.session.start(FsOperation::Copy {
                    entries: vec![f.entry("a"); 257],
                    into: f.dst.clone(),
                    conflict: ConflictPolicy::Fail
                }),
                Err(FsError::LimitExceeded)
            ));
            assert!(f.receipt_bytes().is_empty());
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                barrier: Some(("after-chunk", barrier.clone())),
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
                barrier: Some(("after-chunk", barrier.clone())),
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
        // phase: fix1_later_receipt_failure_does_not_hide_durable_item
        {
            let f = Fixture::new();
            fs::write(f.source("a"), b"first").unwrap();
            fs::write(f.source("b"), b"second").unwrap();
            let barrier = Arc::new(Barrier::default());
            f.hooks(Hooks {
                barrier: Some(("before-item-two", barrier.clone())),
                ..Hooks::default()
            });
            let mut job = f
                .session
                .start(FsOperation::Copy {
                    entries: vec![f.entry("a"), f.entry("b")],
                    into: f.dst.clone(),
                    conflict: ConflictPolicy::Fail,
                })
                .unwrap();
            let id = job.operation_id();
            let release = Release(barrier.clone());
            barrier.entered();
            f.hooks(Hooks {
                fail_checkpoint: Some("Item"),
                ..Hooks::default()
            });
            drop(release);
            let deadline = tilo_test_env::hang_deadline();
            while !job.control.results.lock().unwrap().error {
                assert!(Instant::now() < deadline, "receipt error deadline");
                std::thread::yield_now();
            }
            let updates = job.poll().unwrap();
            assert_eq!(item(&updates).item, 0);
            assert_eq!(item(&updates).state, ItemState::Completed);
            assert!(item(&updates).destination.is_some());
            assert!(
                !updates
                    .iter()
                    .any(|u| matches!(u, FsUpdate::Finished { .. }))
            );
            assert!(matches!(job.poll(), Err(FsError::StoreUnavailable)));
            assert_eq!(fs::read(f.dest("a")).unwrap(), b"first");
            let report = f.store.report(&id).unwrap();
            assert_eq!(report.state, OperationState::Running);
            assert_eq!(report.items.len(), 1);
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
    fn changed_after_source_fingerprint(change: &str) {
        let f = Fixture::new();
        fs::create_dir(f.source("tree")).unwrap();
        fs::write(f.source("tree/a"), b"verified bytes").unwrap();
        let barrier = Arc::new(Barrier::default());
        f.hooks(Hooks {
            rename_error: Some(rustix::io::Errno::XDEV),
            barrier: Some(("after-source-fingerprint", barrier.clone())),
            ..Hooks::default()
        });
        let mut job = f.session.start(f.moving("tree")).unwrap();
        let release = Release(barrier.clone());
        barrier.entered();
        match change {
            "added" => fs::write(f.source("tree/new"), b"never copied").unwrap(),
            "replaced" => {
                fs::rename(f.source("tree/a"), f.source("held")).unwrap();
                fs::write(f.source("tree/a"), b"replacement bytes").unwrap();
            }
            "modified" => fs::write(f.source("tree/a"), b"changed in place").unwrap(),
            _ => unreachable!(),
        }
        drop(release);
        let updates = drain(&mut job);
        assert_eq!(item(&updates).state, ItemState::CopiedSourceRetained);
        assert!(item(&updates).destination.is_some());
        assert_eq!(fs::read(f.dest("tree/a")).unwrap(), b"verified bytes");
        match change {
            "added" => assert_eq!(fs::read(f.source("tree/new")).unwrap(), b"never copied"),
            "replaced" => assert_eq!(fs::read(f.source("tree/a")).unwrap(), b"replacement bytes"),
            "modified" => assert_eq!(fs::read(f.source("tree/a")).unwrap(), b"changed in place"),
            _ => unreachable!(),
        }
    }
    #[test]
    fn fix1_a_child_changed_after_the_fingerprint_is_retained() {
        for change in ["added", "replaced", "modified"] {
            eprintln!("case: {change} child after fingerprint");
            changed_after_source_fingerprint(change);
        }
    }
    fn published_interruption(create: bool) {
        let f = Fixture::new();
        fs::write(f.source("a"), b"published bytes").unwrap();
        f.hooks(Hooks {
            fail_checkpoint: Some("Item"),
            ..Hooks::default()
        });
        let operation = if create {
            FsOperation::CreateFile {
                parent: f.dst.clone(),
                name: name("a"),
                bytes: b"published bytes".to_vec(),
                conflict: ConflictPolicy::Fail,
            }
        } else {
            f.copy("a")
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
        if create {
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
        } else {
            assert_eq!(observed.kind, RecoveryKind::PublishedSourceRetained);
        }
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
    fn fix1_an_interrupted_published_copy_or_create_requires_revalidation() {
        for create in [false, true] {
            eprintln!(
                "case: interrupted published {}",
                if create { "create" } else { "copy" }
            );
            published_interruption(create);
        }
    }
    #[test]
    fn queued_directory_effect_refuses_closed_session_or_revoked_parent() {
        for revoke in [false, true] {
            let f = Fixture::new();
            fs::write(f.source("blocking"), b"held chunk").unwrap();
            let barrier = Arc::new(Barrier::default());
            let release = Release(Arc::clone(&barrier));
            f.hooks(Hooks {
                barrier: Some(("after-chunk", Arc::clone(&barrier))),
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
