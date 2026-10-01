//! Host-owned bounded durable mutation receipts. Stored locators are metadata,
//! never grants; recovery always requires newly supplied live authority.
use crate::native::{NativeLocator, io_error, lock};
use crate::operations::{FsJob, ItemOutcome, ItemState};
use crate::{DirectoryGrant, EntryName, FsError, FsOwner, FsSession};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use std::os::unix::ffi::OsStringExt;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Read,
    sync::{Arc, Mutex},
};

const MAX_REPORTS: usize = 4096;
const MAX_REPORT_BYTES: usize = 64 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationId(uuid::Uuid);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryId(uuid::Uuid);
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RecoveryKind {
    NeedsRevalidation,
    KnownStaged,
    UnknownNativeCommit,
    PublishedSourceRetained,
}
#[derive(Clone, Debug)]
pub struct RecoveryInfo {
    pub id: RecoveryId,
    pub operation: OperationId,
    pub kind: RecoveryKind,
    pub original_name: OsString,
    pub acknowledged: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OperationState {
    Running,
    Finished,
    RecoveryRequired,
    OutcomeUnknown,
}
#[derive(Clone, Debug)]
pub struct RecordedItemOutcome {
    pub item: usize,
    pub state: ItemState,
    pub recoveries: Vec<RecoveryId>,
    pub error_code: Option<String>,
    pub message: Option<String>,
}
#[derive(Clone, Debug)]
pub struct OperationReport {
    pub id: OperationId,
    pub owner: FsOwner,
    pub originating_mount: uuid::Uuid,
    pub state: OperationState,
    pub items: Vec<RecordedItemOutcome>,
}
pub enum RecoveryResolution {
    RestoreKnown {
        parent: DirectoryGrant,
        name: EntryName,
    },
    AcknowledgeUnknown {
        note: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Fingerprint {
    pub identity: crate::native::NativeIdentity,
    pub length: u64,
    pub digest: [u8; 32],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct RecoveryRecord {
    pub id: uuid::Uuid,
    pub kind: RecoveryKind,
    pub original_name: Vec<u8>,
    pub acknowledged: bool,
    pub note: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Step {
    pub item: usize,
    pub phase: String,
    pub stage: Option<NativeLocator>,
    pub destination: Option<NativeLocator>,
    pub source: Option<NativeLocator>,
    pub fingerprint: Option<Fingerprint>,
    pub recovery: Option<RecoveryRecord>,
}
impl Step {
    pub(super) fn new(item: usize) -> Self {
        Self {
            item,
            phase: "Admitted".into(),
            stage: None,
            destination: None,
            source: None,
            fingerprint: None,
            recovery: None,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct StoredItem {
    item: usize,
    state: ItemState,
    recoveries: Vec<uuid::Uuid>,
    error_code: Option<String>,
    message: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    version: u32,
    id: uuid::Uuid,
    workspace: String,
    app_id: String,
    mount: uuid::Uuid,
    state: OperationState,
    item_count: usize,
    items: Vec<StoredItem>,
    steps: Vec<Step>,
    checkpoints: Vec<String>,
    acknowledged: bool,
}
#[derive(Default)]
struct Store {
    receipts: BTreeMap<uuid::Uuid, Receipt>,
    poisoned: bool,
}

/// The trusted host retains this owner. Opening takes an already-authorized
/// private directory and an exclusive writer lock; no ambient directory is consulted.
/// The host must provision a supported private journal root and maintain its
/// permissions and cooperative child namespace. Granted user files have their
/// own policies; admission never relocates a journal to another filesystem.
pub struct OperationOwner {
    storage: tilo_files::native::atomic::StorageDirectory,
    _writer: advisory_lock::FileLock,
    owner: FsOwner,
    metadata_limit: usize,
    state: Mutex<Store>,
    #[cfg(test)]
    pub(super) hooks: Mutex<crate::operations::tests::Hooks>,
    #[cfg(test)]
    pub(super) submissions: std::sync::atomic::AtomicUsize,
}
impl OperationId {
    pub(super) fn native(&self) -> uuid::Uuid {
        self.0
    }
}
impl OperationOwner {
    /// # Errors
    /// Returns `Busy` for a second writer, and `StoreUnavailable` for invalid
    /// or oversized receipts or owner mismatch. Privacy denial returns `Denied`;
    /// unsupported native permission profiles remain explicit I/O errors.
    /// Reconciliation publication failures retain their typed phase and source.
    pub fn open(opened_storage: std::fs::File, owner: FsOwner) -> Result<Arc<Self>, FsError> {
        if !opened_storage.metadata().map_err(io_error)?.is_dir() {
            return Err(FsError::StoreUnavailable);
        }
        // Duplicate only the incoming directory, never the lock-bearing writer.
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(
            opened_storage.try_clone().map_err(io_error)?,
        )
        .map_err(io_error)?;
        storage
            .validate_privacy(tilo_files::native::atomic::Privacy::OwnerOnly)
            .map_err(io_error)?;
        let directory = Dir::from_std_file(opened_storage);
        let writer = rustix::fs::openat(
            &directory,
            ".tilo-operation-writer",
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|e| io_error(e.into()))?;
        let writer = advisory_lock::FileLock::try_exclusive(std::fs::File::from(writer))
            .map_err(|e| io_error(e.into()))?;
        let mut store = Store::default();
        let mut aggregate = 0_usize;
        for entry in directory.entries().map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let native = entry.file_name();
            let Some(name) = native.to_str() else {
                continue;
            };
            if !name.ends_with(".receipt") {
                continue;
            }
            if store.receipts.len() == MAX_REPORTS {
                return Err(FsError::LimitExceeded);
            }
            let descriptor = rustix::fs::openat(
                &directory,
                &native,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|e| io_error(e.into()))?;
            let mut bytes = vec![];
            std::fs::File::from(descriptor)
                .take((MAX_REPORT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(io_error)?;
            aggregate = aggregate
                .checked_add(bytes.len())
                .ok_or(FsError::LimitExceeded)?;
            if bytes.len() > MAX_REPORT_BYTES || aggregate > MAX_METADATA_BYTES {
                return Err(FsError::LimitExceeded);
            }
            let mut receipt: Receipt =
                postcard::from_bytes(&bytes).map_err(|_| FsError::StoreUnavailable)?;
            if receipt.version != 1
                || receipt.workspace != owner.workspace
                || receipt.app_id != owner.app_id
                || name != format!("{}.receipt", receipt.id)
                || receipt.item_count > 256
            {
                return Err(FsError::StoreUnavailable);
            }
            mark_interrupted(&mut receipt)?;
            store.receipts.insert(receipt.id, receipt);
        }
        let result = Arc::new(Self {
            storage,
            _writer: writer,
            owner,
            metadata_limit: MAX_REPORT_BYTES,
            state: Mutex::new(store),
            #[cfg(test)]
            hooks: Mutex::new(crate::operations::tests::Hooks::default()),
            #[cfg(test)]
            submissions: std::sync::atomic::AtomicUsize::new(0),
        });
        // Reconciliation metadata is durable; user files remain untouched.
        for receipt in lock(&result.state)?.receipts.values() {
            result.write(receipt)?;
        }
        Ok(result)
    }
    pub(super) fn owner(&self) -> &FsOwner {
        &self.owner
    }
    fn report_byte_limit(&self) -> usize {
        #[cfg(test)]
        {
            self.hooks.lock().map_or(0, |hooks| {
                hooks
                    .metadata_limit
                    .unwrap_or(self.metadata_limit)
                    .min(self.metadata_limit)
            })
        }
        #[cfg(not(test))]
        {
            self.metadata_limit
        }
    }
    fn write(&self, receipt: &Receipt) -> Result<(), FsError> {
        let bytes = postcard::to_stdvec(receipt).map_err(|_| FsError::StoreUnavailable)?;
        if bytes.len() > self.report_byte_limit() {
            return Err(FsError::LimitExceeded);
        }
        let final_name = format!("{}.receipt", receipt.id);
        self.storage
            .publish(
                std::ffi::OsStr::new(&final_name),
                &bytes,
                tilo_files::native::atomic::PublishMode::Replace,
                tilo_files::native::atomic::Privacy::OwnerOnly,
            )
            .map_err(FsError::Publication)
    }

    fn reserve_terminal_metadata(
        &self,
        receipt: &Receipt,
        checkpoint: &str,
    ) -> Result<(), FsError> {
        // A terminal item has one receipt ID, a bounded code and a 64-byte
        // diagnostic. Reserve all remaining outcomes before permitting effects.
        let bytes = postcard::to_stdvec(receipt)
            .map_err(|_| FsError::StoreUnavailable)?
            .len();
        let remaining = receipt.item_count.saturating_sub(receipt.items.len());
        let reserve = remaining
            .checked_mul(160)
            .and_then(|n| {
                n.checked_add(
                    if matches!(
                        checkpoint,
                        "Admitted" | "PublishStarted" | "NativeTrashSubmitted"
                    ) {
                        1024
                    } else {
                        0
                    },
                )
            })
            .ok_or(FsError::LimitExceeded)?;
        if bytes.checked_add(reserve).ok_or(FsError::LimitExceeded)? > self.report_byte_limit() {
            return Err(FsError::LimitExceeded);
        }
        Ok(())
    }
    fn update(
        &self,
        id: &OperationId,
        checkpoint: &str,
        change: impl FnOnce(&mut Receipt) -> Result<(), FsError>,
    ) -> Result<(), FsError> {
        let mut store = lock(&self.state)?;
        if store.poisoned {
            return Err(FsError::StoreUnavailable);
        }
        let mut receipt = store.receipts.get(&id.0).ok_or(FsError::NotFound)?.clone();
        change(&mut receipt)?;
        if receipt
            .checkpoints
            .last()
            .is_none_or(|previous| previous != checkpoint)
        {
            receipt.checkpoints.push(checkpoint.into());
        }
        self.reserve_terminal_metadata(&receipt, checkpoint)?;
        let aggregate = store.receipts.values().try_fold(0_usize, |sum, stored| {
            let stored = if stored.id == receipt.id {
                &receipt
            } else {
                stored
            };
            let bytes = if stored.state == OperationState::Running {
                MAX_REPORT_BYTES
            } else {
                postcard::to_stdvec(stored)
                    .map_err(|_| FsError::StoreUnavailable)?
                    .len()
            };
            sum.checked_add(bytes).ok_or(FsError::LimitExceeded)
        })?;
        if aggregate > MAX_METADATA_BYTES {
            return Err(FsError::LimitExceeded);
        }
        #[cfg(test)]
        if lock(&self.hooks)?.fail_checkpoint == Some(checkpoint) {
            return Err(FsError::StoreUnavailable);
        }
        #[cfg(test)]
        let publication_failure = lock(&self.hooks)?.publication_failure;
        #[cfg(test)]
        let written = match publication_failure {
            Some((phase, fault)) if phase == checkpoint => {
                tilo_files::native::atomic::test_support::with_failure(fault, || {
                    self.write(&receipt)
                })
            }
            _ => self.write(&receipt),
        };
        #[cfg(not(test))]
        let written = self.write(&receipt);
        if let Err(error) = written {
            store.poisoned = true;
            return Err(error);
        }
        store.receipts.insert(id.0, receipt);
        drop(store);
        Ok(())
    }
    pub(super) fn admit(&self, mount: uuid::Uuid, count: usize) -> Result<OperationId, FsError> {
        let mut store = lock(&self.state)?;
        if store.poisoned {
            return Err(FsError::StoreUnavailable);
        }
        let limit = MAX_REPORTS;
        #[cfg(test)]
        let limit = lock(&self.hooks)?.report_limit.unwrap_or(limit);
        let reserved = store.receipts.values().try_fold(0_usize, |sum, r| {
            let size = if r.state == OperationState::Running {
                MAX_REPORT_BYTES
            } else {
                postcard::to_stdvec(r)
                    .map_err(|_| FsError::StoreUnavailable)?
                    .len()
            };
            sum.checked_add(size).ok_or(FsError::LimitExceeded)
        })?;
        if store.receipts.len() >= limit {
            return Err(FsError::Busy);
        }
        if reserved > MAX_METADATA_BYTES - MAX_REPORT_BYTES {
            return Err(FsError::LimitExceeded);
        }
        #[cfg(test)]
        if lock(&self.hooks)?.fail_checkpoint == Some("Admitted") {
            return Err(FsError::StoreUnavailable);
        }
        let id = uuid::Uuid::new_v4();
        let receipt = Receipt {
            version: 1,
            id,
            workspace: self.owner.workspace.clone(),
            app_id: self.owner.app_id.clone(),
            mount,
            state: OperationState::Running,
            item_count: count,
            items: vec![],
            steps: (0..count).map(Step::new).collect(),
            checkpoints: vec!["Admitted".into()],
            acknowledged: false,
        };
        self.reserve_terminal_metadata(&receipt, "Admitted")?;
        if let Err(error) = self.write(&receipt) {
            if matches!(&error, FsError::Publication(publication) if publication.state() != tilo_files::native::atomic::PublicationState::NotPublished)
            {
                store.poisoned = true;
            }
            return Err(error);
        }
        store.receipts.insert(id, receipt);
        drop(store);
        Ok(OperationId(id))
    }
    pub(super) fn checkpoint(&self, id: &OperationId, step: &Step) -> Result<(), FsError> {
        self.update(id, &step.phase, |r| {
            let slot = r.steps.get_mut(step.item).ok_or(FsError::LimitExceeded)?;
            *slot = step.clone();
            Ok(())
        })
    }
    pub(super) fn step(&self, id: &OperationId, item: usize) -> Result<Step, FsError> {
        lock(&self.state)?
            .receipts
            .get(&id.0)
            .and_then(|r| r.steps.get(item))
            .cloned()
            .ok_or(FsError::NotFound)
    }
    pub(super) fn item(&self, id: &OperationId, item: &ItemOutcome) -> Result<(), FsError> {
        self.update(id, "Item", |r| {
            if r.items.iter().any(|i| i.item == item.item) {
                return Err(FsError::Conflict);
            }
            let code = item.error.as_ref().map(error_code);
            let message = item.error.as_ref().map(|e| bounded(&e.to_string(), 64));
            r.items.push(StoredItem {
                item: item.item,
                state: item.state,
                recoveries: item.recoveries.iter().map(|id| id.0).collect(),
                error_code: code,
                message,
            });
            Ok(())
        })
    }
    pub(super) fn finish(&self, id: &OperationId) -> Result<(), FsError> {
        self.update(id, "Finished", |r| {
            if r.items.len() != r.item_count {
                return Err(FsError::Busy);
            }
            r.state = fold(r.items.iter().map(|i| i.state));
            Ok(())
        })
    }
    pub(super) fn reconcile(&self, current: &FsSession) -> Result<(), FsError> {
        let pending = {
            let store = lock(&self.state)?;
            store
                .receipts
                .values()
                .flat_map(|r| {
                    r.steps
                        .iter()
                        .filter(|s| {
                            s.recovery.as_ref().is_some_and(|v| {
                                !v.acknowledged && v.kind == RecoveryKind::NeedsRevalidation
                            })
                        })
                        .map(|s| (OperationId(r.id), s.clone()))
                })
                .collect::<Vec<_>>()
        };
        for (operation, step) in pending {
            let Some(kind) = classify(current, &step)? else {
                continue;
            };
            self.update(&operation, "Revalidated", |receipt| {
                let target = receipt.steps.get_mut(step.item).ok_or(FsError::NotFound)?;
                let recovery = target.recovery.as_mut().ok_or(FsError::NotFound)?;
                recovery.kind = kind;
                let state = match kind {
                    RecoveryKind::KnownStaged | RecoveryKind::NeedsRevalidation => {
                        ItemState::RecoveryRequired
                    }
                    RecoveryKind::PublishedSourceRetained => ItemState::CopiedSourceRetained,
                    RecoveryKind::UnknownNativeCommit => ItemState::OutcomeUnknown,
                };
                if let Some(item) = receipt.items.iter_mut().find(|item| item.item == step.item)
                    && item.state != ItemState::OutcomeUnknown
                {
                    item.state = state;
                }
                receipt.state = fold(receipt.items.iter().map(|item| item.state));
                Ok(())
            })?;
        }
        Ok(())
    }
    /// # Errors
    /// Returns `NotFound` for IDs outside this explicitly scoped owner.
    pub fn report(&self, id: &OperationId) -> Result<OperationReport, FsError> {
        let store = lock(&self.state)?;
        let r = store.receipts.get(&id.0).ok_or(FsError::NotFound)?;
        let report = OperationReport {
            id: id.clone(),
            owner: self.owner.clone(),
            originating_mount: r.mount,
            state: r.state,
            items: r
                .items
                .iter()
                .map(|i| RecordedItemOutcome {
                    item: i.item,
                    state: i.state,
                    recoveries: i.recoveries.iter().copied().map(RecoveryId).collect(),
                    error_code: i.error_code.clone(),
                    message: i.message.clone(),
                })
                .collect(),
        };
        drop(store);
        Ok(report)
    }
    /// # Errors
    /// Returns `StoreUnavailable` if the scoped owner cannot be read.
    pub fn recoveries(&self) -> Result<Vec<RecoveryInfo>, FsError> {
        let store = lock(&self.state)?;
        Ok(store
            .receipts
            .values()
            .flat_map(|r| {
                r.steps.iter().filter_map(|s| {
                    s.recovery.as_ref().map(|recovery| RecoveryInfo {
                        id: RecoveryId(recovery.id),
                        operation: OperationId(r.id),
                        kind: recovery.kind,
                        original_name: OsString::from_vec(recovery.original_name.clone()),
                        acknowledged: recovery.acknowledged,
                    })
                })
            })
            .collect())
    }
    pub(super) fn find_recovery(&self, id: &RecoveryId) -> Result<(OperationId, Step), FsError> {
        let store = lock(&self.state)?;
        for r in store.receipts.values() {
            for s in &r.steps {
                if s.recovery.as_ref().is_some_and(|v| v.id == id.0) {
                    return Ok((OperationId(r.id), s.clone()));
                }
            }
        }
        drop(store);
        Err(FsError::NotFound)
    }
    pub(super) fn acknowledge_recovery(
        &self,
        id: &RecoveryId,
        note: Option<String>,
    ) -> Result<(), FsError> {
        let (operation, step) = self.find_recovery(id)?;
        self.update(&operation, "RecoveryResolved", |r| {
            let recovery = r
                .steps
                .get_mut(step.item)
                .and_then(|s| s.recovery.as_mut())
                .ok_or(FsError::NotFound)?;
            recovery.acknowledged = true;
            recovery.note = note;
            Ok(())
        })
    }
    /// Start an explicit new recovery operation using only current live grants.
    /// # Errors
    /// Returns scope, identity, collision, note-length and admission failures;
    /// an unsuccessful restore always retains the original recovery receipt.
    pub fn resolve(
        &self,
        id: &RecoveryId,
        current: &FsSession,
        resolution: RecoveryResolution,
    ) -> Result<FsJob, FsError> {
        let attached = current.operation_store()?;
        if !std::ptr::eq(self, attached.as_ref()) || current.owner() != self.owner() {
            return Err(FsError::Denied);
        }
        self.reconcile(current)?;
        let (_, step) = self.find_recovery(id)?;
        let recovery = step.recovery.as_ref().ok_or(FsError::NotFound)?;
        if recovery.acknowledged {
            return Err(FsError::Conflict);
        }
        match resolution {
            RecoveryResolution::RestoreKnown { parent, name } => {
                if recovery.kind != RecoveryKind::KnownStaged {
                    return Err(FsError::Denied);
                }
                current.mutation_directory(&parent)?;
                let stage = step.stage.as_ref().ok_or(FsError::StaleEntry)?;
                let _ = current.recovery_source(stage)?;
                crate::operations::start_restore(current, id.clone(), step, parent, name)
            }
            RecoveryResolution::AcknowledgeUnknown { note } => {
                if recovery.kind != RecoveryKind::UnknownNativeCommit {
                    return Err(FsError::Denied);
                }
                if note.len() > 1024 {
                    return Err(FsError::LimitExceeded);
                }
                crate::operations::start_acknowledgment(current, id.clone(), note)
            }
        }
    }
    /// Explicit host acknowledgment never mutates user bytes or rewrites history.
    /// # Errors
    /// Returns `Busy` for active jobs or any unresolved known/unknown recovery.
    pub fn acknowledge_report(&self, id: &OperationId) -> Result<(), FsError> {
        self.update(id, "ReportAcknowledged", |r| {
            if r.state == OperationState::Running
                || r.steps
                    .iter()
                    .filter_map(|s| s.recovery.as_ref())
                    .any(|recovery| {
                        !recovery.acknowledged
                            && recovery.kind != RecoveryKind::PublishedSourceRetained
                    })
            {
                return Err(FsError::Busy);
            }
            for recovery in r.steps.iter_mut().filter_map(|s| s.recovery.as_mut()) {
                recovery.acknowledged = true;
            }
            r.acknowledged = true;
            Ok(())
        })
    }
    #[cfg(test)]
    pub(super) fn merge_recovery_for_test(
        &self,
        into: &OperationId,
        from: &OperationId,
    ) -> Result<(), FsError> {
        // Fixture-only composition of two real terminal syscall/checkpoint results.
        // Move the receipt's recovery ownership, never manufacture staged bytes.
        let (mut step, mut item) = {
            let store = lock(&self.state)?;
            let r = store.receipts.get(&from.0).ok_or(FsError::NotFound)?;
            let pair = (r.steps[0].clone(), r.items[0].clone());
            drop(store);
            pair
        };
        self.update(from, "FixtureTransferred", |r| {
            r.steps[0].recovery = None;
            r.items[0].recoveries.clear();
            Ok(())
        })?;
        self.update(into, "FixtureMixed", |r| {
            let index = r.item_count;
            r.item_count = r.item_count.checked_add(1).ok_or(FsError::LimitExceeded)?;
            step.item = index;
            item.item = index;
            r.steps.push(step);
            r.items.push(item);
            r.state = fold(r.items.iter().map(|i| i.state));
            Ok(())
        })
    }
    #[cfg(test)]
    pub(super) fn checkpoints(&self, id: &OperationId) -> Result<Vec<String>, FsError> {
        Ok(lock(&self.state)?
            .receipts
            .get(&id.0)
            .ok_or(FsError::NotFound)?
            .checkpoints
            .clone())
    }
}
pub(super) fn ensure_recovery(step: &mut Step, kind: RecoveryKind) -> RecoveryId {
    let record = step.recovery.get_or_insert_with(|| RecoveryRecord {
        id: uuid::Uuid::new_v4(),
        kind,
        original_name: step
            .source
            .as_ref()
            .or(step.destination.as_ref())
            .map_or_else(Vec::new, |l| l.name.clone()),
        acknowledged: false,
        note: None,
    });
    record.kind = kind;
    if kind == RecoveryKind::NeedsRevalidation {
        record.acknowledged = false;
    }
    RecoveryId(record.id)
}
fn fold(states: impl Iterator<Item = ItemState>) -> OperationState {
    let mut result = OperationState::Finished;
    for state in states {
        match state {
            ItemState::OutcomeUnknown => return OperationState::OutcomeUnknown,
            ItemState::RecoveryRequired | ItemState::CopiedSourceRetained => {
                result = OperationState::RecoveryRequired;
            }
            _ => {}
        }
    }
    result
}
fn bounded(message: &str, max: usize) -> String {
    let mut end = message.len().min(max);
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    message.get(..end).unwrap_or_default().to_owned()
}
fn error_code(error: &FsError) -> String {
    match error {
        FsError::Publication(_) => "StoreUnavailable".into(),
        FsError::Io(_) => "Io".into(),
        FsError::Unsupported(_) => "Unsupported".into(),
        FsError::Transport(_) => "Transport".into(),
        other => format!("{other:?}"),
    }
}

fn classify(current: &FsSession, step: &Step) -> Result<Option<RecoveryKind>, FsError> {
    let mut missing_authority = false;
    let mut observe = |locator: Option<&NativeLocator>| -> Result<Option<Fingerprint>, FsError> {
        let Some(locator) = locator else {
            return Ok(None);
        };
        match current.observe_receipt(locator) {
            Ok(fingerprint) => Ok(Some(fingerprint)),
            Err(FsError::Denied | FsError::Revoked | FsError::Closed) => {
                missing_authority = true;
                Ok(None)
            }
            Err(FsError::NotFound | FsError::StaleEntry) => Ok(None),
            Err(error) => Err(error),
        }
    };
    let staged = observe(step.stage.as_ref())?;
    if let (Some(staged), Some(expected)) = (staged, step.fingerprint.as_ref())
        && staged.identity == expected.identity
        && staged.length == expected.length
        && staged.digest == expected.digest
    {
        return Ok(Some(RecoveryKind::KnownStaged));
    }
    let destination = observe(step.destination.as_ref())?;
    let source = observe(step.source.as_ref())?;
    if missing_authority {
        return Ok(None);
    }
    if let (Some(destination), Some(source)) = (destination, source) {
        let content_matches = step.fingerprint.as_ref().is_none_or(|expected| {
            destination.identity == expected.identity
                && destination.length == expected.length
                && destination.digest == expected.digest
                && source.length == expected.length
                && source.digest == expected.digest
        });
        if content_matches {
            return Ok(Some(RecoveryKind::PublishedSourceRetained));
        }
    }
    if matches!(
        step.phase.as_str(),
        "PublishStarted" | "Published" | "SourceRemovalStarted" | "SourceRemovalCompleted"
    ) {
        Ok(Some(RecoveryKind::UnknownNativeCommit))
    } else {
        Ok(None)
    }
}

fn mark_interrupted(receipt: &mut Receipt) -> Result<(), FsError> {
    // Reopening never submits, removes, publishes, overwrites or cleans up.
    // Interrupted checkpoints conservatively become durable unresolved outcomes.
    if receipt.state == OperationState::Running {
        for index in 0..receipt.item_count {
            if receipt.items.iter().any(|i| i.item == index) {
                continue;
            }
            let step = receipt
                .steps
                .iter_mut()
                .find(|s| s.item == index)
                .ok_or(FsError::StoreUnavailable)?;
            let state = match step.phase.as_str() {
                "NativeTrashSubmitted" => {
                    ensure_recovery(step, RecoveryKind::UnknownNativeCommit);
                    ItemState::OutcomeUnknown
                }
                "Published" | "SourceRemovalStarted" | "Staged" | "PublishStarted" => {
                    ensure_recovery(step, RecoveryKind::NeedsRevalidation);
                    ItemState::RecoveryRequired
                }
                "SourceRemovalCompleted" | "NativeTrashCompleted" => ItemState::Completed,
                _ if step.stage.is_some() => {
                    ensure_recovery(step, RecoveryKind::NeedsRevalidation);
                    ItemState::RecoveryRequired
                }
                _ => ItemState::Failed,
            };
            receipt.items.push(StoredItem {
                item: index,
                state,
                recoveries: step.recovery.iter().map(|r| r.id).collect(),
                error_code: Some("Interrupted".into()),
                message: Some("Host stopped before terminal durability".into()),
            });
        }
        receipt.items.sort_by_key(|i| i.item);
        receipt.state = fold(receipt.items.iter().map(|i| i.state));
    }
    for step in &mut receipt.steps {
        if let Some(recovery) = step.recovery.as_mut()
            && !recovery.acknowledged
            && recovery.kind != RecoveryKind::UnknownNativeCommit
        {
            recovery.kind = RecoveryKind::NeedsRevalidation;
        }
    }

    Ok(())
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    fn fixture() -> Receipt {
        let identity = crate::native::NativeIdentity {
            device: 0x1234,
            inode: 0x00ab_cdef,
        };
        Receipt {
            version: 1,
            id: uuid::Uuid::from_u128(0x1234),
            workspace: "fixture-room".into(),
            app_id: "fixture-app".into(),
            mount: uuid::Uuid::from_u128(0x5678),
            state: OperationState::RecoveryRequired,
            item_count: 1,
            items: vec![StoredItem {
                item: 0,
                state: ItemState::CopiedSourceRetained,
                recoveries: vec![uuid::Uuid::from_u128(0x9876)],
                error_code: Some("StoreUnavailable".into()),
                message: Some("retained".into()),
            }],
            steps: vec![Step {
                item: 0,
                phase: "Published".into(),
                stage: None,
                source: Some(NativeLocator {
                    root: identity,
                    parents: vec![b"parent".to_vec()],
                    name: vec![0x66, 0x80],
                    identity: Some(identity),
                }),
                destination: None,
                fingerprint: Some(Fingerprint {
                    identity,
                    length: 3,
                    digest: [0x42; 32],
                }),
                recovery: Some(RecoveryRecord {
                    id: uuid::Uuid::from_u128(0x9876),
                    kind: RecoveryKind::PublishedSourceRetained,
                    original_name: vec![0x66, 0x80],
                    acknowledged: false,
                    note: None,
                }),
            }],
            checkpoints: vec!["Admitted".into(), "Published".into()],
            acknowledged: false,
        }
    }
    fn private_root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .tempdir()
            .unwrap()
    }
    fn open(root: &tempfile::TempDir) -> Arc<OperationOwner> {
        OperationOwner::open(
            std::fs::File::open(root.path()).unwrap(),
            FsOwner {
                workspace: "fixture-room".into(),
                app_id: "fixture-app".into(),
            },
        )
        .unwrap()
    }
    #[test]
    fn receipt_publication_admission_memory_disk_and_poisoning() {
        use tilo_files::native::atomic::{
            PublicationState as State,
            test_support::{Checkpoint, with_failure},
        };
        for (fault, state, has_target) in [
            (Checkpoint::BeforeDataFlush, State::NotPublished, false),
            (Checkpoint::BeforePublish, State::NotPublished, false),
            (
                Checkpoint::PublishSubmitted,
                State::PublicationUnknown,
                false,
            ),
            (Checkpoint::AfterPublish, State::PublicationUnknown, true),
            (
                Checkpoint::BeforeCommit,
                State::PublishedDurabilityUnknown,
                true,
            ),
        ] {
            let root = private_root();
            let owner = open(&root);
            let error =
                with_failure(fault, || owner.admit(uuid::Uuid::from_u128(9), 1)).unwrap_err();
            let FsError::Publication(publication) = &error else {
                panic!("typed phase missing: {error}")
            };
            assert_eq!(publication.state(), state);
            assert_eq!(error_code(&error), "StoreUnavailable");
            let store = owner.state.lock().unwrap();
            assert!(store.receipts.is_empty());
            assert_eq!(store.poisoned, state != State::NotPublished);
            drop(store);
            let targets: Vec<_> = std::fs::read_dir(root.path())
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|e| e == "receipt"))
                .collect();
            assert_eq!(targets.len(), usize::from(has_target));
            if has_target {
                let bytes = std::fs::read(&targets[0]).unwrap();
                let receipt: Receipt = postcard::from_bytes(&bytes).unwrap();
                assert_eq!(receipt.version, 1);
                assert_eq!(receipt.mount, uuid::Uuid::from_u128(9));
                assert_eq!(receipt.checkpoints, ["Admitted"]);
            }
            if state == State::NotPublished {
                owner.admit(uuid::Uuid::nil(), 1).unwrap();
            } else {
                assert!(matches!(
                    owner.admit(uuid::Uuid::nil(), 1),
                    Err(FsError::StoreUnavailable)
                ));
            }
        }
    }

    #[test]
    fn receipt_publication_commit_guard_never_owns_writer_exclusion_or_repairs_memory() {
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
        let root = private_root();
        let owner = open(&root);
        let error = with_failure(Checkpoint::BeforeCommit, || {
            owner.admit(uuid::Uuid::nil(), 1)
        })
        .unwrap_err();
        let FsError::Publication(publication) = error else {
            panic!("publication source")
        };
        assert!(matches!(
            OperationOwner::open(
                std::fs::File::open(root.path()).unwrap(),
                owner.owner.clone()
            ),
            Err(FsError::Busy)
        ));
        publication.retry_commit().unwrap();
        assert!(owner.state.lock().unwrap().receipts.is_empty());
        assert!(matches!(
            owner.admit(uuid::Uuid::nil(), 1),
            Err(FsError::StoreUnavailable)
        ));
        drop(owner);
        let owner = open(&root);
        assert_eq!(owner.state.lock().unwrap().receipts.len(), 1);
        drop(owner);
        let owner = open(&root);
        let error = with_failure(Checkpoint::BeforeCommit, || {
            owner.admit(uuid::Uuid::nil(), 1)
        })
        .unwrap_err();
        drop(owner);
        // Retaining the error retains publication handles, not the writer lock.
        let reopened = open(&root);
        assert_eq!(reopened.state.lock().unwrap().receipts.len(), 2);
        drop(error);
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|e| e == "receipt"))
                .count(),
            2
        );
    }

    #[test]
    fn receipt_publication_reconciliation_failure_retains_original_phase_and_disk() {
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
        let root = private_root();
        let owner = open(&root);
        let id = owner.admit(uuid::Uuid::nil(), 1).unwrap();
        drop(owner);
        let error = with_failure(Checkpoint::BeforeCommit, || {
            OperationOwner::open(
                std::fs::File::open(root.path()).unwrap(),
                FsOwner {
                    workspace: "fixture-room".into(),
                    app_id: "fixture-app".into(),
                },
            )
        })
        .err()
        .unwrap();
        let FsError::Publication(publication) = &error else {
            panic!("phase lost")
        };
        assert_eq!(
            publication.state(),
            tilo_files::native::atomic::PublicationState::PublishedDurabilityUnknown
        );
        drop(error);
        let reopened = open(&root);
        assert_eq!(
            reopened.report(&id).unwrap().items[0].error_code.as_deref(),
            Some("Interrupted")
        );
        assert_eq!(
            reopened.report(&id).unwrap().state,
            OperationState::Finished
        );
    }

    #[test]
    fn receipt_v1_fixed_writer_bytes() {
        // Captured from the pre-migration production writer, 2026-09-18.
        const V1: &[u8] = &[
            1, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 18, 52, 12, 102, 105, 120, 116, 117,
            114, 101, 45, 114, 111, 111, 109, 11, 102, 105, 120, 116, 117, 114, 101, 45, 97, 112,
            112, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 86, 120, 2, 1, 1, 0, 3, 1, 16, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 152, 118, 1, 16, 83, 116, 111, 114, 101, 85, 110,
            97, 118, 97, 105, 108, 97, 98, 108, 101, 1, 8, 114, 101, 116, 97, 105, 110, 101, 100,
            1, 0, 9, 80, 117, 98, 108, 105, 115, 104, 101, 100, 0, 0, 1, 180, 36, 239, 155, 175, 5,
            1, 6, 112, 97, 114, 101, 110, 116, 2, 102, 128, 1, 180, 36, 239, 155, 175, 5, 1, 180,
            36, 239, 155, 175, 5, 3, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66,
            66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 1, 16, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 152, 118, 3, 2, 102, 128, 0, 0, 2, 8, 65, 100, 109, 105,
            116, 116, 101, 100, 9, 80, 117, 98, 108, 105, 115, 104, 101, 100, 0,
        ];
        let root = tempfile::Builder::new()
            .permissions(
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .tempdir()
            .unwrap();
        let owner = OperationOwner::open(
            std::fs::File::open(root.path()).unwrap(),
            FsOwner {
                workspace: "fixture-room".into(),
                app_id: "fixture-app".into(),
            },
        )
        .unwrap();
        let receipt = fixture();
        let historical_pending = root.path().join(format!("{}.pending", receipt.id));
        std::fs::write(&historical_pending, b"historical pending evidence").unwrap();
        owner.write(&receipt).unwrap();
        assert_eq!(
            std::fs::read(&historical_pending).unwrap(),
            b"historical pending evidence"
        );
        let bytes = std::fs::read(root.path().join(format!("{}.receipt", receipt.id))).unwrap();
        assert_eq!(bytes, V1);
        let decoded: Receipt = postcard::from_bytes(V1).unwrap();
        assert_eq!(postcard::to_stdvec(&decoded).unwrap(), V1);
        assert_eq!(decoded.steps[0].source.as_ref().unwrap().name, [0x66, 0x80]);
        assert_eq!(
            decoded.steps[0].source.as_ref().unwrap().root.device,
            0x1234
        );
        assert_eq!(
            decoded.steps[0].source.as_ref().unwrap().root.inode,
            0x00ab_cdef
        );
    }
}
