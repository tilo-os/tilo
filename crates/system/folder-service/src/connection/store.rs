//! Private durable Connected metadata. Persisted records never grant endpoint access.
mod chain;
mod chain_review;
#[cfg(test)]
mod chain_tests;
mod directory;
mod format;
mod initial_baseline;
pub(super) use format::InitialEmptyBaselineV1;
#[cfg(test)]
mod initial_baseline_tests;
pub(super) use format::NativeOperationKind;
mod recovery;
mod worker_projection;
#[cfg(test)]
pub(super) use worker_projection::test_support as worker_projection_tests;
mod storage;
#[cfg(test)]
pub(super) use storage::ReadLease as ReviewReadLease;
#[cfg(test)]
mod tests;

use std::path::Path;

#[derive(Debug)]
pub(super) enum StoreError {
    Busy,
    UnsupportedDurability,
    IdentityChanged,
    Corrupt,
    LimitExceeded,
    StaleRevision,
    Disconnected,
    RecoveryRequired,
    LegacyNativePending,
    RecoverySpace,
    Cancelled,
    Persistence(std::io::Error),
}
impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        if matches!(
            error.kind(),
            std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
        ) {
            Self::RecoverySpace
        } else {
            Self::Persistence(error)
        }
    }
}

use super::{
    endpoint::EndpointLocator,
    model::{
        Conflict, ConnectionId, Decision, Encoding, EndpointIdentity, EntryState, NativeName,
        RelativePath, Side, VerifiedBaseline,
    },
};
use std::sync::Arc;
pub(super) type Revision = u64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OperationId {
    pub(super) connection: ConnectionId,
    pub(super) serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RecoveryId {
    pub(super) operation: OperationId,
    pub(super) slot: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ConflictId {
    pub(super) connection: ConnectionId,
    pub(super) serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lifecycle {
    Active,
    Disconnecting,
    Disconnected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ConnectionPolicy {
    pub(super) deletion_retention_seconds: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EndpointBinding {
    pub(super) identity: EndpointIdentity,
    pub(super) locator: EndpointLocator,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConnectionRecord {
    pub(super) id: ConnectionId,
    pub(super) revision: Revision,
    pub(super) source: EndpointBinding,
    pub(super) destination: EndpointBinding,
    pub(super) policy: ConnectionPolicy,
    pub(super) lifecycle: Lifecycle,
    pub(super) baseline_revision: Option<Revision>,
    pub(super) fence_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OperationIntent {
    pub(super) connection: ConnectionId,
    pub(super) expected_revision: Revision,
    pub(super) expected_baseline: Option<Revision>,
    pub(super) path: RelativePath,
    pub(super) source_before: Option<EntryState>,
    pub(super) destination_before: Option<EntryState>,
    pub(super) decision: Decision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReceiptStage {
    Intent,
    Preserved,
    Published,
    Verified,
    BaselineCommitted,
    Aborted,
    PreparedNoDisplacement,
    AccountedPending,
    CapturedConflict,
    SelectedApplied,
    SelectedCollision,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ExecutionAction {
    Reconcile,
    Captured,
    Selected(SelectedExecution),
    SelectedCollision(SelectedExecution),
}
/// Read-only action correlation exposed to native consumers. Serialized action
/// construction remains confined to the store and cannot supply live authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectedExecution {
    action: format::SelectedAction,
}
impl ExecutionAction {
    pub(super) fn is_reconciliation(&self) -> bool {
        matches!(self, Self::Reconcile)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OperationReceipt {
    pub(super) action: ExecutionAction,
    pub(super) id: OperationId,
    pub(super) revision: Revision,
    pub(super) stage: ReceiptStage,
    pub(super) intent: OperationIntent,
    pub(super) recovery: Vec<RecoveryId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryReason {
    NormalDeletion,
    DisplacedVersion,
    Conflict(ConflictId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryState {
    Reserved,
    BytesVerified,
    Attached,
    PurgeIntent,
    Purged,
    RetainedIncomplete,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecoveryEntry {
    pub(super) id: RecoveryId,
    pub(super) original_side: Side,
    pub(super) original_path: RelativePath,
    pub(super) state: RecoveryState,
    pub(super) reason: RecoveryReason,
    pub(super) content: Option<EntryState>,
    pub(super) retained_seconds: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConflictRecord {
    pub(super) captured: Option<super::transaction::native::descriptor::ConflictSnapshot>,
    pub(super) id: ConflictId,
    pub(super) revision: Revision,
    pub(super) kind: Conflict,
    pub(super) path: RelativePath,
    pub(super) source: Option<EntryState>,
    pub(super) destination: Option<EntryState>,
    pub(super) preserved: Vec<RecoveryId>,
    pub(super) resolved: bool,
}
pub(super) struct PageCursor {
    checkpoint: u64,
    sequence: u64,
    last_key: Vec<u8>,
}
pub(super) struct Page<T> {
    pub(super) entries: Vec<T>,
    pub(super) next: Option<PageCursor>,
}
pub(super) struct DurableAck<T> {
    value: T,
    sequence: u64,
    session: Arc<()>,
}
pub(super) struct DisconnectFence {
    connection: ConnectionId,
    generation: u64,
}

use super::super::migration_journal_log as framing;
use format::{Change, Connection, Event, Row};
pub(super) use format::{DirectoryAncestorPatchV1, DirectoryBoundaryFact, ExistingAncestorPatchV1};
#[cfg(test)]
use std::io::Read;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Seek, SeekFrom, Write},
};
// Historical per-open correlation only; process-id reuse can collide across
// processes. This tag is never tested for admission or reconstructed authority.
fn native_correlation() -> Result<[u8; 16], StoreError> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let serial = NEXT
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |value| value.checked_add(1),
        )
        .map_err(|_| StoreError::LimitExceeded)?;
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&u64::from(std::process::id()).to_le_bytes());
    bytes[8..].copy_from_slice(&serial.to_le_bytes());
    Ok(bytes)
}
/// Inspection and paid reads only. This cannot be used for step admission.
pub(super) struct RecoveryContext {
    native_kind: Option<format::NativeOperationKind>,
    native_identity: Option<Vec<u8>>,
    action: ExecutionAction,
    gate: Arc<super::transaction::AdmissionGate>,
    id: OperationId,
    revision: u64,
    operation_revision: u64,
    fence: u64,
    lifetime: std::sync::Weak<()>,
    reads: Arc<super::transaction::native::OperationReads>,
    recovery_only: bool,
    intent: OperationIntent,
    endpoints: [EndpointIdentity; 2],
    admission: u64,
    baseline: Option<u64>,
}
impl RecoveryContext {
    pub(super) fn native_kind(&self) -> Option<format::NativeOperationKind> {
        self.native_kind
    }
    pub(super) fn native_identity(&self) -> Option<&[u8]> {
        self.native_identity.as_deref()
    }
    pub(super) fn action(&self) -> &ExecutionAction {
        &self.action
    }
    pub(super) fn gate(&self) -> Arc<super::transaction::AdmissionGate> {
        Arc::clone(&self.gate)
    }
    pub(super) fn id(&self) -> OperationId {
        self.id
    }
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }
    pub(super) fn fence(&self) -> u64 {
        self.fence
    }
    pub(super) fn lifetime(&self) -> &std::sync::Weak<()> {
        &self.lifetime
    }
    pub(super) fn intent(&self) -> &OperationIntent {
        &self.intent
    }
    pub(super) fn endpoints(&self) -> &[EndpointIdentity; 2] {
        &self.endpoints
    }
    pub(super) fn admission(&self) -> u64 {
        self.admission
    }
    pub(super) fn baseline(&self) -> Option<u64> {
        self.baseline
    }
    pub(super) fn reads(&self) -> &Arc<super::transaction::native::OperationReads> {
        &self.reads
    }
    pub(super) fn recovery_only(&self) -> bool {
        self.recovery_only
    }
}
/// A displayed selection of a retained version. This owns metadata/consent only;
/// its acceptance cannot substitute for a paid current private-byte proof.
pub(super) struct RestorePreview {
    record: ConnectionRecord,
    lifetime: std::sync::Weak<()>,
    intent: OperationIntent,
    selected: ReviewVersion,
    target: Side,
    conflict: Option<format::SelectedConflict>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TombstoneChoice {
    origin: ConflictId,
    revision: u64,
    side: u8,
    snapshot: [u8; 32],
    path: RelativePath,
}
impl TombstoneChoice {
    pub(super) fn side(&self) -> Side {
        if self.side == 0 {
            Side::Source
        } else {
            Side::Destination
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum ReviewVersion {
    Recovery(format::Recovery),
    Tombstone(TombstoneChoice),
}
pub(super) struct ReviewedSelection {
    preview: RestorePreview,
    replace: bool,
}
impl RestorePreview {
    pub(super) fn intent(&self) -> &OperationIntent {
        &self.intent
    }
    pub(super) fn accept(self) -> ReviewedSelection {
        ReviewedSelection {
            preview: self,
            replace: false,
        }
    }
    pub(super) fn accept_replacement(self) -> ReviewedSelection {
        ReviewedSelection {
            preview: self,
            replace: true,
        }
    }
}
impl ReviewedSelection {
    pub(super) fn intent(&self) -> &OperationIntent {
        &self.preview.intent
    }
}
/// Opaque current-store authority. No serialized row can recreate it.
pub(super) struct LiveOperation {
    native_kind: Option<format::NativeOperationKind>,
    native_identity: Option<Vec<u8>>,
    planned_baseline_count: Option<u32>,
    action: ExecutionAction,
    gate: Arc<super::transaction::AdmissionGate>,
    deletion_baseline_checked: bool,
    id: OperationId,
    lifetime: std::sync::Weak<()>,
    revision: Revision,
    admission: Revision,
    baseline: Option<Revision>,
    fence: u64,
    intent: OperationIntent,
    endpoints: [EndpointIdentity; 2],
    correlation: [u8; 16],
    reads: Arc<super::transaction::native::OperationReads>,
}
impl LiveOperation {
    #[cfg(target_os = "linux")]
    pub(super) fn native_session(&self) -> [u8; 16] {
        self.correlation
    }
    pub(super) fn native_kind(&self) -> Option<format::NativeOperationKind> {
        self.native_kind
    }
    pub(super) fn native_identity(&self) -> Option<&[u8]> {
        self.native_identity.as_deref()
    }
    pub(super) fn action(&self) -> &ExecutionAction {
        &self.action
    }
    pub(super) fn gate(&self) -> Arc<super::transaction::AdmissionGate> {
        Arc::clone(&self.gate)
    }
    pub(super) fn deletion_baseline_checked(&self) -> bool {
        self.deletion_baseline_checked
    }
    pub(super) fn id(&self) -> OperationId {
        self.id
    }
    pub(super) fn revision(&self) -> Revision {
        self.revision
    }
    pub(super) fn lifetime(&self) -> &std::sync::Weak<()> {
        &self.lifetime
    }
    pub(super) fn intent(&self) -> &OperationIntent {
        &self.intent
    }
    pub(super) fn endpoints(&self) -> &[EndpointIdentity; 2] {
        &self.endpoints
    }
    pub(super) fn admission(&self) -> Revision {
        self.admission
    }
    pub(super) fn fence(&self) -> u64 {
        self.fence
    }
    pub(super) fn operation_reads(&self) -> &Arc<super::transaction::native::OperationReads> {
        &self.reads
    }
}
pub(super) struct BaselineCandidate {
    existing_ancestors: Option<ExistingAncestorPatchV1>,
    id: OperationId,
    lifetime: std::sync::Weak<()>,
    revision: u64,
    baseline: Option<u64>,
    fence: u64,
    root: Option<[u8; 32]>,
    count: u32,
    value: Option<EntryState>,
}
fn baseline_phase_estimate(old_count: u32, new_count: u32) -> Result<u64, StoreError> {
    let pages = |count: u32| -> Result<u64, StoreError> {
        if count > 4096 {
            return Err(StoreError::LimitExceeded);
        }
        Ok(if count == 0 {
            0
        } else {
            u64::from(count) * 8 + 1
        })
    };
    pages(old_count)?
        .checked_mul(3)
        .and_then(|old| pages(new_count).ok()?.checked_mul(3)?.checked_add(old))
        .and_then(|passes| passes.checked_add(u64::from(new_count) * 8))
        .and_then(|pages| pages.checked_mul(storage::NATIVE_PAGE_CAP))
        .filter(|bytes| *bytes <= format::READ_LIMIT)
        .ok_or(StoreError::LimitExceeded)
}
pub(super) struct NamespaceReservation {
    registration: format::NativeRegistration,
    lifetime: std::sync::Weak<()>,
    revision: u64,
}
impl NamespaceReservation {
    pub(super) fn name(&self) -> String {
        use super::transaction::native::descriptor::Header;
        Header {
            connection: self.registration.connection,
            operation: self.registration.serial,
            attempt: u64::from(self.registration.attempt),
            nonce: self.registration.nonce,
            epoch: [0; 16],
            session: [0; 16],
            admission: self.registration.admission,
            baseline: self.registration.baseline,
            progress: self.revision,
            fence: self.registration.fence,
        }
        .namespace_name()
    }
    pub(super) fn header(
        &self,
        live: &LiveOperation,
        epoch: [u8; 16],
    ) -> Result<super::transaction::native::descriptor::Header, StoreError> {
        if !self.lifetime.ptr_eq(live.lifetime())
            || self.lifetime.strong_count() == 0
            || self.revision != live.revision
            || self.registration.connection != live.id.connection.0
            || self.registration.serial != live.id.serial
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(super::transaction::native::descriptor::Header {
            connection: live.id.connection.0,
            operation: live.id.serial,
            attempt: u64::from(self.registration.attempt),
            nonce: self.registration.nonce,
            epoch,
            session: live.correlation,
            admission: live.admission,
            baseline: live.baseline,
            progress: live.revision,
            fence: live.fence,
        })
    }
}
pub(super) struct ConnectionStore {
    storage: storage::Storage,
    current: storage::Current,
    current_identity: storage::Metadata,
    log: File,
    log_identity: storage::Metadata,
    overlay: BTreeMap<Vec<u8>, Row>,
    overlay_bytes: usize,
    projection: std::cell::RefCell<Option<worker_projection::Budget>>,
    sequence: u64,
    digest: [u8; 32],
    session: Arc<()>,
    native_lifetime: Arc<()>,
    native_correlation: [u8; 16],
    native_reads: BTreeMap<Vec<u8>, Arc<super::transaction::native::OperationReads>>,
    // One sticky admission gate per loaded connection, bounded by the store's
    // 65,536-connection limit. External handle loss cannot undo a requested stop.
    native_gates: BTreeMap<[u8; 16], Arc<super::transaction::AdmissionGate>>,
    poisoned: bool,
    connections: u64,
    active_intents: u64,
    live_keys: u64,
    retention_sample: std::time::Instant,
    gc_scan: Option<storage::GcScan>,
    candidate_pin: Option<[u8; 32]>,
    #[cfg(windows)]
    recovery_guards: BTreeMap<Vec<u8>, storage::RecoveryWriterGuard>,
}
// Prepared against one immutable in-memory state; applying after fsync cannot
// discover a new capacity error or perform another fallible tree read.
struct PreparedRows {
    rows: Vec<Row>,
    overlay_bytes: usize,
    overlay_keys: usize,
    connections: u64,
    active_intents: u64,
    live_keys: u64,
}
impl PreparedRows {
    fn fits_overlay(&self) -> bool {
        self.overlay_keys <= 1024 && self.overlay_bytes <= 8 * 1024 * 1024
    }
}
impl ConnectionStore {
    /// Bounded historical classification; this never issues a live native proof.
    pub(super) fn legacy_native_pending(&self, id: OperationId) -> Result<bool, StoreError> {
        use super::transaction::native::descriptor as d;
        self.validate()?;
        if self.connection(id.connection)?.pending != Some(id.serial) {
            return Ok(false);
        }
        let operation = self.operation(id)?;
        if operation.action.is_some() || !matches!(operation.decision, 1 | 2) {
            return Ok(false);
        }
        let mut legacy = false;
        // Canonical attempts are 1..=8. Unrelated rows never enlarge this scan.
        for attempt in 1_u32..=8 {
            let mut key = format::key(5, id.connection.0, id.serial);
            key.extend(attempt.to_be_bytes());
            let registration = match self.row(&key)? {
                Some(Row::NativeRegistration(registration)) => registration,
                None => continue,
                _ => return Err(StoreError::Corrupt),
            };
            registration.validate()?;
            if registration.key() != key {
                return Err(StoreError::Corrupt);
            }
            if let (Some(intent), Some(stage)) = (&registration.intent, &registration.stage) {
                let intent = d::decode(intent).map_err(|_| StoreError::Corrupt)?;
                let stage = d::decode(stage).map_err(|_| StoreError::Corrupt)?;
                if matches!(
                    intent.body,
                    d::Body::Intent(d::Intent {
                        decision: 1 | 2,
                        selection: d::Selection::None,
                        ..
                    })
                ) && stage.profile == d::Profile::LegacyStage
                {
                    legacy = true;
                }
            }
        }
        Ok(legacy)
    }

    pub(super) fn open_default() -> Result<Self, StoreError> {
        let path = dirs::data_local_dir()
            .ok_or(StoreError::UnsupportedDurability)?
            .join("qos-imager")
            .join("connections-v1");
        Self::open_at(&path)
    }
    pub(super) fn open_at(path: &Path) -> Result<Self, StoreError> {
        let (storage, current) = storage::Storage::open(path)?;
        Self::from_storage(storage, current)
    }
    fn from_storage(
        storage: storage::Storage,
        current: storage::Current,
    ) -> Result<Self, StoreError> {
        let _read_lease = storage.read_lease(false)?;
        let log_name = format!("log-{}", current.log);
        let mut log = storage::open_file(&storage.root, &log_name, false)?;
        let log_identity = storage::regular(&log)?;
        if log_identity.len() > 16 * 1024 * 1024 {
            return Err(StoreError::LimitExceeded);
        }
        storage.read_log_magic(&mut log)?;
        let header: format::LogHeader = storage
            .read_log_record(&mut log, 0)?
            .ok_or(StoreError::Corrupt)?;
        if header.schema != 1
            || header.identity != storage.identity
            || header.generation != current.generation
            || header.first_sequence != format::next(current.sequence)?
            || header.predecessor != current.digest
        {
            return Err(StoreError::Corrupt);
        }
        let current_identity = storage::regular(&storage::read_file(&storage.root, "CURRENT")?)?;
        let sequence = current.sequence;
        let digest = current.digest;
        let mut store = Self {
            storage,
            current,
            current_identity,
            log,
            log_identity,
            overlay: BTreeMap::new(),
            overlay_bytes: 0,
            projection: std::cell::RefCell::new(None),
            sequence,
            digest,
            session: Arc::new(()),
            native_lifetime: Arc::new(()),
            native_correlation: native_correlation()?,
            native_reads: BTreeMap::new(),
            native_gates: BTreeMap::new(),
            poisoned: false,
            connections: 0,
            active_intents: 0,
            live_keys: 0,
            retention_sample: std::time::Instant::now(),
            gc_scan: None,
            candidate_pin: None,
            #[cfg(windows)]
            recovery_guards: BTreeMap::new(),
        };
        for result in storage::TreeIter::new(&store.storage, store.current.root)? {
            store.live_keys = format::next(store.live_keys)?;
            if store.live_keys > 1_114_112 {
                return Err(StoreError::LimitExceeded);
            }
            if let Row::Connection(connection) = result? {
                store.connections = format::next(store.connections)?;
                if connection.pending.is_some() {
                    store.active_intents = format::next(store.active_intents)?;
                }
            }
        }
        if store.connections > 65536 || store.active_intents > 1024 {
            return Err(StoreError::LimitExceeded);
        }
        store.validate_chain_history(&[])?;
        store.validate_review_history(&[])?;
        loop {
            let offset = store.log.stream_position()?;
            let event: Option<Event> = store
                .storage
                .read_log_record(&mut store.log, format::next(store.sequence)?)?;
            let Some(event) = event else {
                if offset != store.log.metadata()?.len() {
                    store.validate()?;
                    store.log.set_len(offset)?;
                    storage::sync_file(&store.log)?;
                    store.log_identity = storage::regular(&store.log)?;
                    store.log.seek(SeekFrom::Start(offset))?;
                }
                break;
            };
            // Fresh replay reads protect only this bookkeeping step. The map
            // starts empty; no persisted row reconstructs a writer guard.
            let mut recovery_checks = store.replay_checks(&event.change)?;
            store.validate_verification_mode(&event.change, &recovery_checks, false)?;
            let rows = store
                .reduce(&event, &mut recovery_checks)
                .map_err(|_| StoreError::Corrupt)?;
            let bytes = format::encode(&event)?;
            let prepared = store.prepare_rows(rows)?;
            if !prepared.fits_overlay() {
                return Err(StoreError::LimitExceeded);
            }
            store.commit_rows(prepared);
            store.sequence = format::next(store.sequence)?;
            store.digest = format::digest(&bytes);
        }
        store.validate_chain_history(&[])?;
        store.validate_review_history(&[])?;
        store.resume_gc()?;
        Ok(store)
    }
    fn validate(&self) -> Result<(), StoreError> {
        if self.projection.borrow().is_some() {
            return self.worker_projection_validate();
        }
        if self.poisoned {
            return Err(StoreError::RecoveryRequired);
        }
        self.storage.validate()?;
        self.storage
            .validate_current(&self.current, &self.current_identity)?;
        self.validate_log()
    }
    fn validate_log(&self) -> Result<(), StoreError> {
        storage::validate_file(
            &self.storage.root,
            &format!("log-{}", self.current.log),
            &self.log,
            &self.log_identity,
        )?;
        if !storage::same_snapshot(&self.log_identity, &storage::regular(&self.log)?) {
            return Err(StoreError::IdentityChanged);
        }
        Ok(())
    }
    fn poison_native(&mut self) {
        self.poisoned = true;
        // Weak live contexts and already-issued step tokens expire immediately,
        // including failures before the store owner itself is dropped.
        self.native_lifetime = Arc::new(());
        for gate in self.native_gates.values() {
            gate.close();
        }
    }
    fn guard(&mut self) -> Result<(), StoreError> {
        let result = self.validate();
        if result.is_err() {
            self.poison_native();
        }
        result
    }
    fn row(&self, key: &[u8]) -> Result<Option<Row>, StoreError> {
        if self.projection.borrow().is_some() {
            return self.worker_projection_row(key);
        }
        if let Some(value) = self.overlay.get(key) {
            self.storage.review_charge(0, 0, 1, 1)?;
            return Ok(Some(value.clone()));
        }
        self.storage.find(self.current.root, key)
    }
    fn connection(&self, id: ConnectionId) -> Result<Connection, StoreError> {
        self.validate()?;
        match self.row(&format::key(0, id.0, 0))? {
            Some(Row::Connection(c)) => Ok(c),
            _ => Err(StoreError::Corrupt),
        }
    }
    /// Read-only bounded facade projection; rows never become native authority.
    pub(super) fn worker_conflicts(
        &self,
        id: ConnectionId,
        cursor: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page<ConflictRecord>, StoreError> {
        let rows = self.worker_table_page(id, 3, cursor, limit)?;
        Ok(Page {
            entries: rows
                .entries
                .into_iter()
                .map(|row| match row {
                    Row::Conflict(conflict) => conflict.decode(),
                    _ => Err(StoreError::Corrupt),
                })
                .collect::<Result<_, _>>()?,
            next: rows.next,
        })
    }
    pub(super) fn load_connection(&self, id: ConnectionId) -> Result<ConnectionRecord, StoreError> {
        self.connection(id)?.decode()
    }
    pub(super) fn load_baseline(&self, id: ConnectionId) -> Result<VerifiedBaseline, StoreError> {
        let record = self.load_connection(id)?;
        let connection = self.connection(id)?;
        if let Some(revision) = record.baseline_revision {
            return self.read_baseline(
                &connection,
                connection.baseline_root,
                connection.baseline_count,
                revision,
            );
        }
        Ok(VerifiedBaseline::absent(
            id,
            record.source.identity,
            record.destination.identity,
        ))
    }
    fn rows(&self) -> Result<impl Iterator<Item = Result<Row, StoreError>> + '_, StoreError> {
        let mut old = storage::TreeIter::new(&self.storage, self.current.root)?.peekable();
        let mut overlay = self.overlay.values().peekable();
        Ok(std::iter::from_fn(move || {
            if let Err(error) = self.storage.review_charge(0, 0, 1, 1) {
                return Some(Err(error));
            }
            match (old.peek(), overlay.peek()) {
                (Some(Err(_)), _) | (Some(_), None) => old.next(),
                (Some(Ok(a)), Some(b)) => match a.key().cmp(&b.key()) {
                    std::cmp::Ordering::Less => old.next(),
                    std::cmp::Ordering::Equal => {
                        old.next();
                        overlay.next().cloned().map(Ok)
                    }
                    std::cmp::Ordering::Greater => overlay.next().cloned().map(Ok),
                },
                (None, Some(_)) => overlay.next().cloned().map(Ok),
                (None, None) => None,
            }
        }))
    }
    pub(super) fn connection_page(
        &self,
        cursor: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page<ConnectionRecord>, StoreError> {
        self.validate()?;
        if !(1..=128).contains(&limit) {
            return Err(StoreError::LimitExceeded);
        }
        let after = match cursor {
            Some(c)
                if c.checkpoint == self.current.generation
                    && c.sequence == self.sequence
                    && c.last_key.len() == 25
                    && c.last_key.first() == Some(&0) =>
            {
                Some(c.last_key)
            }
            Some(_) => return Err(StoreError::StaleRevision),
            None => None,
        };
        let mut entries = Vec::new();
        let mut last = None;
        let mut next = None;
        for result in self.rows()? {
            let row = result?;
            let key = row.key();
            if key.first() != Some(&0) {
                break;
            }
            if after.as_ref().is_some_and(|after| &key <= after) {
                continue;
            }
            if entries.len() == limit {
                next = Some(PageCursor {
                    checkpoint: self.current.generation,
                    sequence: self.sequence,
                    last_key: last.ok_or(StoreError::Corrupt)?,
                });
                break;
            }
            if let Row::Connection(connection) = row {
                entries.push(connection.decode()?);
                last = Some(key);
            }
        }
        Ok(Page { entries, next })
    }
    fn ack<T>(&self, value: T) -> DurableAck<T> {
        DurableAck {
            value,
            sequence: self.sequence,
            session: Arc::clone(&self.session),
        }
    }
    fn check_ack<T>(&self, ack: &DurableAck<T>) -> Result<(), StoreError> {
        if !Arc::ptr_eq(&self.session, &ack.session) || ack.sequence > self.sequence {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }
    pub(super) fn create_connection(
        &mut self,
        id: ConnectionId,
        source: EndpointBinding,
        destination: EndpointBinding,
        policy: ConnectionPolicy,
    ) -> Result<DurableAck<ConnectionRecord>, StoreError> {
        self.guard()?;
        let source_path = source.locator.path();
        let destination_path = destination.locator.path();
        if source_path.starts_with(destination_path)
            || destination_path.starts_with(source_path)
            || self.storage.path.starts_with(source_path)
            || self.storage.path.starts_with(destination_path)
        {
            return Err(StoreError::IdentityChanged);
        }
        let record = Connection::new(id, source, destination, policy)?;
        let value = record.decode()?;
        self.append(Change::Create { record })?;
        Ok(self.ack(value))
    }
    #[cfg(any(windows, test))]
    pub(super) fn begin_intent(
        &mut self,
        intent: OperationIntent,
    ) -> Result<DurableAck<OperationId>, StoreError> {
        self.require_no_chain_history(intent.connection)?;
        self.guard()?;
        let connection = self.connection(intent.connection)?;
        if connection.lifecycle != 0 {
            return Err(StoreError::Disconnected);
        }
        let serial = format::next(connection.serial)?;
        let id = OperationId {
            connection: intent.connection,
            serial,
        };
        self.append(Change::Intent {
            operation: format::Operation::new(intent, serial, &connection)?,
        })?;
        Ok(self.ack(id))
    }
    pub(super) fn transaction_session(
        &mut self,
        id: ConnectionId,
    ) -> Result<super::transaction::TransactionSession, StoreError> {
        self.require_no_chain_history(id)?;
        let record = self.load_connection(id)?;
        if record.lifecycle != Lifecycle::Active {
            return Err(StoreError::Disconnected);
        }
        let gate = Arc::clone(
            self.native_gates
                .entry(id.0)
                .or_insert_with(|| Arc::new(super::transaction::AdmissionGate::new())),
        );
        gate.set_progress(record.revision);
        Ok(super::transaction::TransactionSession::new(
            id,
            Arc::downgrade(&self.native_lifetime),
            gate,
        ))
    }
    pub(super) fn same_native_lifetime(&self, lifetime: &std::sync::Weak<()>) -> bool {
        lifetime.ptr_eq(&Arc::downgrade(&self.native_lifetime))
    }
    pub(super) fn initial_review_basis(
        &self,
        id: ConnectionId,
        source: &super::model::ObservedTree,
        destination: &super::model::ObservedTree,
    ) -> Result<(ConnectionRecord, std::sync::Weak<()>, [u8; 32]), StoreError> {
        use sha2::{Digest, Sha256};
        self.require_no_review_or_chain_history(id)?;
        let record = self.load_connection(id)?;
        if record.lifecycle != Lifecycle::Active
            || record.baseline_revision.is_some()
            || !matches!(source.observation(), super::model::Observation::Complete)
            || !matches!(
                destination.observation(),
                super::model::Observation::Complete
            )
            || source.endpoint().identity != record.source.identity
            || destination.endpoint().identity != record.destination.identity
        {
            return Err(StoreError::RecoveryRequired);
        }
        let mut hash = Sha256::new();
        hash.update(b"Connected explicit initial preview v1");
        hash.update(format::encode(&self.connection(id)?)?);
        for (side, tree) in [source, destination].into_iter().enumerate() {
            hash.update([u8::try_from(side).map_err(|_| StoreError::Corrupt)?]);
            hash.update(
                u64::try_from(tree.entries().len())
                    .map_err(|_| StoreError::LimitExceeded)?
                    .to_le_bytes(),
            );
            for (ordinal, (path, state)) in tree.entries().iter().enumerate() {
                let row = format::BaselineEntry {
                    connection: id.0,
                    revision: record.revision,
                    ordinal: u32::try_from(ordinal).map_err(|_| StoreError::LimitExceeded)?,
                    path: path.clone().into(),
                    state: state.clone().into(),
                };
                let encoded = format::encode(&row)?;
                hash.update(
                    u64::try_from(encoded.len())
                        .map_err(|_| StoreError::LimitExceeded)?
                        .to_le_bytes(),
                );
                hash.update(encoded);
            }
        }
        Ok((
            record,
            Arc::downgrade(&self.native_lifetime),
            hash.finalize().into(),
        ))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn native_admission_basis(
        &self,
        intent: &OperationIntent,
    ) -> Result<(ConnectionRecord, std::sync::Weak<()>, u8), StoreError> {
        self.require_no_review_or_chain_history(intent.connection)?;
        let connection = self.connection(intent.connection)?;
        let record = connection.decode()?;
        if record.lifecycle != Lifecycle::Active
            || record.revision != intent.expected_revision
            || record.baseline_revision != intent.expected_baseline
            || connection.pending.is_some()
        {
            return Err(StoreError::StaleRevision);
        }
        let operation = format::Operation::new(
            intent.clone(),
            format::next(connection.serial)?,
            &connection,
        )?;
        Ok((
            record,
            Arc::downgrade(&self.native_lifetime),
            operation.decision,
        ))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn begin_native(
        &mut self,
        intent: OperationIntent,
        session: &super::transaction::TransactionSession,
        admission: super::transaction::native::NativeAdmission<'_>,
    ) -> Result<LiveOperation, StoreError> {
        if self
            .load_connection(intent.connection)?
            .baseline_revision
            .is_none()
        {
            return Err(StoreError::RecoveryRequired);
        }
        let serial = format::next(self.connection(intent.connection)?.serial)?;
        let identity = admission.consume(self, &intent, serial)?;
        self.begin_native_impl(intent, session, None, Some(identity))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn begin_native_reviewed(
        &mut self,
        intent: OperationIntent,
        session: &super::transaction::TransactionSession,
        reviewed: super::transaction::ReviewedInitial,
        admission: super::transaction::native::NativeAdmission<'_>,
    ) -> Result<LiveOperation, StoreError> {
        let serial = format::next(self.connection(intent.connection)?.serial)?;
        let identity = admission.consume(self, &intent, serial)?;
        self.begin_native_impl(intent, session, Some(reviewed), Some(identity))
    }
    #[cfg(test)]
    fn begin_native_without_identity_for_test(
        &mut self,
        intent: OperationIntent,
        session: &super::transaction::TransactionSession,
    ) -> Result<LiveOperation, StoreError> {
        if self
            .load_connection(intent.connection)?
            .baseline_revision
            .is_none()
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.begin_native_impl(intent, session, None, None)
    }
    #[cfg(test)]
    fn begin_native_reviewed_without_identity_for_test(
        &mut self,
        intent: OperationIntent,
        session: &super::transaction::TransactionSession,
        reviewed: super::transaction::ReviewedInitial,
    ) -> Result<LiveOperation, StoreError> {
        self.begin_native_impl(intent, session, Some(reviewed), None)
    }
    #[cfg(test)]
    fn begin_native_for_test(
        &mut self,
        intent: OperationIntent,
        session: &super::transaction::TransactionSession,
    ) -> Result<LiveOperation, StoreError> {
        self.begin_native_impl(intent, session, None, None)
    }
    fn begin_native_impl(
        &mut self,
        intent: OperationIntent,
        session: &super::transaction::TransactionSession,
        reviewed: Option<super::transaction::ReviewedInitial>,
        identity: Option<Vec<u8>>,
    ) -> Result<LiveOperation, StoreError> {
        self.begin_native_operation(intent, session, reviewed, identity, None, None)
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the one-shot admission or candidate even when validation only borrows its fields."
    )]
    fn begin_native_operation(
        &mut self,
        intent: OperationIntent,
        session: &super::transaction::TransactionSession,
        reviewed: Option<super::transaction::ReviewedInitial>,
        identity: Option<Vec<u8>>,
        action: Option<format::SelectedAction>,
        native_kind: Option<NativeOperationKind>,
    ) -> Result<LiveOperation, StoreError> {
        self.require_no_chain_history(intent.connection)?;
        if native_kind.is_some() && (reviewed.is_some() || action.is_some() || identity.is_none()) {
            return Err(StoreError::RecoveryRequired);
        }
        let planned_baseline_count = if native_kind == Some(NativeOperationKind::DirectoryV1) {
            Some(
                self.preflight_directory_baseline(
                    &intent,
                    &self.load_baseline(intent.connection)?,
                )?,
            )
        } else if identity.is_some()
            && action.is_none()
            && matches!(
                intent.decision,
                Decision::VerifyEqual | Decision::Propagate { .. }
            )
        {
            Some(self.preflight_regular_baseline(&intent, &self.load_baseline(intent.connection)?)?)
        } else {
            None
        };
        let native_identity = identity.clone();
        let review = reviewed
            .as_ref()
            .map(|token| token.validate(self, &intent))
            .transpose()?;
        session.validate(intent.connection, &self.native_lifetime)?;
        self.guard()?;
        let connection = self.connection(intent.connection)?;
        let before = connection.decode()?;
        let serial = format::next(connection.serial)?;
        let mut operation = format::Operation::new(intent.clone(), serial, &connection)?;
        operation.native_budget = Some(format::NativeBudget::initial());
        operation.native_identity = identity;
        operation.native_kind = native_kind;
        operation.action = action;
        let action = operation.decode()?.action;
        if let Some(preview) = review {
            let selection = format::digest(&format::encode(&operation)?);
            operation.initial_review = Some(format::InitialReviewFact {
                preview,
                selection,
                revision: operation.expected,
            });
        }
        let id = OperationId {
            connection: intent.connection,
            serial,
        };
        if native_kind == Some(NativeOperationKind::DirectoryV1) {
            self.preflight_directory_intent(&operation)?;
        }
        self.append(Change::Intent { operation })?;
        let revision = format::next(before.revision)?;
        Ok(LiveOperation {
            native_kind,
            native_identity,
            planned_baseline_count,
            action,
            gate: Arc::clone(
                self.native_gates
                    .get(&id.connection.0)
                    .ok_or(StoreError::StaleRevision)?,
            ),
            deletion_baseline_checked: false,
            id,
            lifetime: Arc::downgrade(&self.native_lifetime),
            revision,
            admission: revision,
            baseline: before.baseline_revision,
            fence: before.fence_generation,
            intent,
            endpoints: [before.source.identity, before.destination.identity],
            correlation: self.native_correlation,
            reads: self.native_read_state(id, 0)?,
        })
    }
    pub(super) fn conflict_record(&self, id: ConflictId) -> Result<ConflictRecord, StoreError> {
        let Some(Row::Conflict(record)) = self.row(&format::key(3, id.connection.0, id.serial))?
        else {
            return Err(StoreError::RecoveryRequired);
        };
        let mut projection = record.decode()?;
        let operation = self.operation(OperationId {
            connection: id.connection,
            serial: id.serial,
        })?;
        if operation.action.is_some() && operation.native_capture.is_some() {
            if self.selected_collision_join(&operation)? != record {
                return Err(StoreError::StaleRevision);
            }
            let Some(format::SelectedAction::Restore { selected, .. }) = operation.action else {
                return Err(StoreError::Corrupt);
            };
            if let format::SelectedVersion::Recovery {
                operation, slot, ..
            } = selected
            {
                projection.preserved.push(RecoveryId {
                    operation: OperationId {
                        connection: id.connection,
                        serial: operation,
                    },
                    slot,
                });
            }
        }
        Ok(projection)
    }
    /// Historical acknowledgement only; supplies no current native authority.
    #[cfg(test)]
    pub(super) fn selected_collision_accounting(
        &self,
        id: OperationId,
    ) -> Result<Option<ConflictRecord>, StoreError> {
        let operation = self.operation(id)?;
        if operation.stage != 10 {
            return Ok(None);
        }
        self.selected_collision_join(&operation)?;
        Ok(Some(self.conflict_record(ConflictId {
            connection: id.connection,
            serial: id.serial,
        })?))
    }
    fn conflict_contains_selection(
        &self,
        conflict: &format::ConflictData,
        selected: RecoveryId,
    ) -> Result<bool, StoreError> {
        if conflict.connection != selected.operation.connection.0 {
            return Ok(false);
        }
        if conflict.serial == selected.operation.serial {
            return Ok(conflict.preserved.contains(&selected.slot));
        }
        let operation = self.operation(OperationId {
            connection: selected.operation.connection,
            serial: conflict.serial,
        })?;
        if operation.stage != 10
            || operation.action.is_none()
            || operation.native_capture.is_none()
            || self.selected_collision_join(&operation)? != *conflict
        {
            return Ok(false);
        }
        let Some(format::SelectedAction::Restore {
            selected: historical,
            ..
        }) = operation.action
        else {
            return Err(StoreError::Corrupt);
        };
        let format::SelectedVersion::Recovery {
            operation, slot, ..
        } = historical
        else {
            return Ok(false);
        };
        Ok(operation == selected.operation.serial && slot == selected.slot)
    }
    pub(super) fn restore_preview(
        &self,
        selected: RecoveryId,
        target: Side,
        conflict: Option<(ConflictId, Revision)>,
        source: super::model::ObservedTree,
        destination: super::model::ObservedTree,
    ) -> Result<RestorePreview, StoreError> {
        let Some(Row::Recovery(entry)) = self.row(&recovery::recovery_key(selected))? else {
            return Err(StoreError::RecoveryRequired);
        };
        let intent = self.selection_preview_intent(
            selected.operation.connection,
            entry.path.to_relative()?,
            source,
            destination,
        )?;
        self.restore_review_basis(intent, selected, target, conflict)
    }
    pub(super) fn tombstone_preview(
        &self,
        selected: TombstoneChoice,
        target: Side,
        conflict: (ConflictId, Revision),
        source: super::model::ObservedTree,
        destination: super::model::ObservedTree,
    ) -> Result<RestorePreview, StoreError> {
        let intent = self.selection_preview_intent(
            selected.origin.connection,
            selected.path.clone(),
            source,
            destination,
        )?;
        self.tombstone_review_basis(intent, selected, target, conflict)
    }
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume complete observation snapshots at review construction so callers must observe again for another proposal."
    )]
    fn selection_preview_intent(
        &self,
        id: ConnectionId,
        path: RelativePath,
        source: super::model::ObservedTree,
        destination: super::model::ObservedTree,
    ) -> Result<OperationIntent, StoreError> {
        let record = self.load_connection(id)?;
        if !matches!(source.observation(), super::model::Observation::Complete)
            || !matches!(
                destination.observation(),
                super::model::Observation::Complete
            )
            || source.endpoint().identity != record.source.identity
            || destination.endpoint().identity != record.destination.identity
        {
            return Err(StoreError::RecoveryRequired);
        }
        let baseline = self.load_baseline(record.id)?;
        let plan = super::reconcile::reconcile(&baseline, &source, &destination);
        if !plan.issues.is_empty() {
            return Err(StoreError::RecoveryRequired);
        }
        let decision = plan
            .proposals
            .iter()
            .find(|proposal| proposal.path == path)
            .map_or_else(
                || {
                    if source.entries().contains_key(&path)
                        || destination.entries().contains_key(&path)
                    {
                        Err(StoreError::RecoveryRequired)
                    } else if record.baseline_revision.is_none() {
                        Ok(Decision::InitialPreview)
                    } else {
                        Ok(Decision::VerifyEqual)
                    }
                },
                |proposal| Ok(proposal.decision.clone()),
            )?;
        if matches!(
            decision,
            Decision::Pending(_)
                | Decision::Conflict(
                    Conflict::FileDirectory
                        | Conflict::NameCollision
                        | Conflict::UnrepresentableName
                )
        ) {
            return Err(StoreError::RecoveryRequired);
        }
        let intent = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: record.baseline_revision,
            source_before: source.entries().get(&path).cloned(),
            destination_before: destination.entries().get(&path).cloned(),
            path,
            decision,
        };
        Ok(intent)
    }
    fn restore_review_basis(
        &self,
        intent: OperationIntent,
        selected: RecoveryId,
        target: Side,
        conflict: Option<(ConflictId, Revision)>,
    ) -> Result<RestorePreview, StoreError> {
        let record = self.load_connection(intent.connection)?;
        let connection = self.connection(intent.connection)?;
        if record.lifecycle != Lifecycle::Active
            || connection.pending.is_some()
            || record.revision != intent.expected_revision
            || record.baseline_revision != intent.expected_baseline
            || selected.operation.connection != record.id
        {
            return Err(StoreError::StaleRevision);
        }
        let Some(Row::Recovery(entry)) = self.row(&recovery::recovery_key(selected))? else {
            return Err(StoreError::RecoveryRequired);
        };
        entry.decode()?;
        if !matches!(entry.state, 1 | 2)
            || entry.path.to_relative()? != intent.path
            || entry.object.is_none()
            || !matches!(entry.content, Some(format::State::File { .. }))
        {
            return Err(StoreError::RecoveryRequired);
        }
        let conflict = conflict
            .map(|(id, revision)| -> Result<_, StoreError> {
                if id.connection != record.id {
                    return Err(StoreError::StaleRevision);
                }
                let Some(Row::Conflict(row)) = self.row(&format::key(3, record.id.0, id.serial))?
                else {
                    return Err(StoreError::RecoveryRequired);
                };
                if row.resolved
                    || row.revision != revision
                    || row.path != entry.path
                    || !self.conflict_contains_selection(&row, selected)?
                {
                    return Err(StoreError::StaleRevision);
                }
                Ok(format::SelectedConflict {
                    serial: id.serial,
                    revision,
                })
            })
            .transpose()?;
        Ok(RestorePreview {
            record,
            lifetime: Arc::downgrade(&self.native_lifetime),
            intent,
            selected: ReviewVersion::Recovery(entry),
            target,
            conflict,
        })
    }
    fn consume_reviewed_selection(
        &self,
        reviewed: ReviewedSelection,
    ) -> Result<(OperationIntent, format::SelectedAction), StoreError> {
        let RestorePreview {
            record,
            lifetime,
            intent,
            selected,
            target,
            conflict,
        } = reviewed.preview;
        if lifetime.strong_count() == 0
            || !self.same_native_lifetime(&lifetime)
            || self.load_connection(record.id)? != record
        {
            return Err(StoreError::StaleRevision);
        }
        let selected = match selected {
            ReviewVersion::Recovery(selected) => {
                if self.row(&selected.key())? != Some(Row::Recovery(selected.clone())) {
                    return Err(StoreError::StaleRevision);
                }
                let checked = self.restore_review_basis(
                    intent.clone(),
                    selected.id(),
                    target,
                    conflict.as_ref().map(|value| {
                        (
                            ConflictId {
                                connection: record.id,
                                serial: value.serial,
                            },
                            value.revision,
                        )
                    }),
                )?;
                if checked.selected != ReviewVersion::Recovery(selected.clone()) {
                    return Err(StoreError::StaleRevision);
                }
                format::SelectedVersion::Recovery {
                    operation: selected.serial,
                    slot: selected.slot,
                    content: selected.content.ok_or(StoreError::RecoveryRequired)?,
                }
            }
            ReviewVersion::Tombstone(selected) => {
                let bound = conflict.as_ref().ok_or(StoreError::RecoveryRequired)?;
                let checked = self.tombstone_review_basis(
                    intent.clone(),
                    selected.clone(),
                    target,
                    (
                        ConflictId {
                            connection: record.id,
                            serial: bound.serial,
                        },
                        bound.revision,
                    ),
                )?;
                if checked.selected != ReviewVersion::Tombstone(selected.clone()) {
                    return Err(StoreError::StaleRevision);
                }
                format::SelectedVersion::Tombstone {
                    operation: selected.origin.serial,
                    revision: selected.revision,
                    side: selected.side,
                    snapshot: selected.snapshot,
                }
            }
        };
        let action = format::SelectedAction::Restore {
            version: 1,
            target: u8::from(target == Side::Destination),
            replace: reviewed.replace,
            revision: record.revision,
            fence: record.fence_generation,
            selected,
            conflict,
        };
        Ok((intent, action))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn begin_native_selected(
        &mut self,
        reviewed: ReviewedSelection,
        session: &super::transaction::TransactionSession,
        admission: super::transaction::native::NativeAdmission<'_>,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        session.require_idle()?;
        let (intent, action) = self.consume_reviewed_selection(reviewed)?;
        let serial = format::next(self.connection(intent.connection)?.serial)?;
        let identity = admission.consume(self, &intent, serial)?;
        self.begin_native_operation(intent, session, None, Some(identity), Some(action), None)
    }
    #[cfg(test)]
    fn begin_native_selected_for_test(
        &mut self,
        reviewed: ReviewedSelection,
        session: &super::transaction::TransactionSession,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        session.require_idle()?;
        let (intent, action) = self.consume_reviewed_selection(reviewed)?;
        self.begin_native_operation(intent, session, None, None, Some(action), None)
    }
    fn native_read_state(
        &mut self,
        id: OperationId,
        sequence: u64,
    ) -> Result<Arc<super::transaction::native::OperationReads>, StoreError> {
        let key = format::key(1, id.connection.0, id.serial);
        if let Some(reads) = self.native_reads.get(&key) {
            return Ok(Arc::clone(reads));
        }
        // Bounded by the existing non-connection row limit, including retained work.
        if self.native_reads.len() >= 1_048_576 {
            return Err(StoreError::LimitExceeded);
        }
        let reads = Arc::new(super::transaction::native::OperationReads::reopened(
            sequence,
        ));
        self.native_reads.insert(key, Arc::clone(&reads));
        Ok(reads)
    }
    pub(super) fn reserve_native_work(
        &mut self,
        live: &mut LiveOperation,
        session: &super::transaction::TransactionSession,
        payload_bound: u64,
        deadline: std::time::Instant,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.check_native(live)?;
        let connection = self.connection(live.id.connection)?;
        let new_count = live.planned_baseline_count.unwrap_or(
            connection
                .baseline_count
                .checked_add(1)
                .ok_or(StoreError::LimitExceeded)?,
        );
        let bytes = baseline_phase_estimate(connection.baseline_count, new_count)?
            .checked_add(payload_bound)
            .ok_or(StoreError::LimitExceeded)?;
        self.reserve_native_reads(live, session, bytes, deadline, cancel)?;
        self.verify_native_deletion_baseline(live, deadline, cancel)
    }
    pub(super) fn verify_native_deletion_baseline(
        &self,
        live: &mut LiveOperation,
        deadline: std::time::Instant,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.check_native(live)?;
        let surviving = match live.intent.decision {
            Decision::RecoverDeletion { at: Side::Source } => live.intent.source_before.as_ref(),
            Decision::RecoverDeletion {
                at: Side::Destination,
            } => live.intent.destination_before.as_ref(),
            _ => return Ok(()),
        }
        .ok_or(StoreError::RecoveryRequired)?;
        let context = self.native_read_context(live, deadline, cancel)?;
        let connection = self.connection(live.id.connection)?;
        let revision = connection.baseline.ok_or(StoreError::RecoveryRequired)?;
        let baseline = self.read_baseline_with_context(
            &connection,
            connection.baseline_root,
            connection.baseline_count,
            revision,
            Some(&context),
        )?;
        if baseline
            .entries()
            .and_then(|entries| entries.get(&live.intent.path))
            != Some(surviving)
        {
            return Err(StoreError::RecoveryRequired);
        }
        context.check()?;
        live.deletion_baseline_checked = true;
        Ok(())
    }
    pub(super) fn reserve_native_reads(
        &mut self,
        live: &mut LiveOperation,
        session: &super::transaction::TransactionSession,
        bytes: u64,
        deadline: std::time::Instant,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), StoreError> {
        self.guard()?;
        self.check_native(live)?;
        session.validate(live.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        if cancel.load(std::sync::atomic::Ordering::Acquire)
            || std::time::Instant::now() >= deadline
        {
            return Err(StoreError::Cancelled);
        }
        if bytes == 0 {
            return Err(StoreError::LimitExceeded);
        }
        let previous = self
            .operation(live.id)?
            .native_budget
            .ok_or(StoreError::RecoveryRequired)?;
        let charge = bytes
            .checked_add(format::READ_QUANTUM - 1)
            .and_then(|n| (n / format::READ_QUANTUM).checked_mul(format::READ_QUANTUM))
            .ok_or(StoreError::LimitExceeded)?;
        let next = format::NativeBudget {
            version: 1,
            reserved: previous
                .reserved
                .checked_add(charge)
                .filter(|n| *n <= format::READ_LIMIT)
                .ok_or(StoreError::LimitExceeded)?,
            sequence: format::next(previous.sequence)?,
        };
        next.validate()?;
        self.append(Change::ReadReserve {
            recovery_only: false,
            operation_expected: Some(live.revision),
            connection: live.id.connection.0,
            serial: live.id.serial,
            expected: live.revision,
            fence: live.fence,
            previous,
            next: next.clone(),
        })?;
        live.revision = format::next(live.revision)?;
        let stopped = session.require_idle().is_err()
            || cancel.load(std::sync::atomic::Ordering::Acquire)
            || std::time::Instant::now() >= deadline;
        if live
            .reads
            .grant(next.sequence, if stopped { 0 } else { charge })
            .is_err()
        {
            self.poison_native();
            return Err(StoreError::RecoveryRequired);
        }
        if stopped {
            return Err(StoreError::Cancelled);
        }
        Ok(())
    }
    pub(super) fn open_recovery_context(
        &mut self,
        id: OperationId,
        expected_revision: u64,
    ) -> Result<RecoveryContext, StoreError> {
        self.require_no_chain_history(id.connection)?;
        self.guard()?;
        let connection = self.connection(id.connection)?;
        let operation = self.operation(id)?;
        let budget = operation
            .native_budget
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        if connection.revision != expected_revision {
            return Err(StoreError::StaleRevision);
        }
        let gate = self
            .native_gates
            .entry(id.connection.0)
            .or_insert_with(|| Arc::new(super::transaction::AdmissionGate::new()));
        if connection.lifecycle != 0 {
            gate.close();
        }
        gate.require_quiescent()?;
        gate.set_progress(connection.revision);
        let recovery_gate = Arc::clone(gate);
        let recovery_only = gate.stopped().load(std::sync::atomic::Ordering::Acquire)
            || connection.pending != Some(id.serial)
            || matches!(operation.stage, 4 | 5 | 7);
        let record = connection.decode()?;
        let receipt = operation.decode()?;
        let intent = receipt.intent;
        Ok(RecoveryContext {
            native_kind: operation.native_kind,
            native_identity: operation.native_identity.clone(),
            action: receipt.action,
            gate: recovery_gate,
            id,
            revision: connection.revision,
            operation_revision: operation.revision,
            fence: connection.fence,
            lifetime: Arc::downgrade(&self.native_lifetime),
            reads: self.native_read_state(id, budget.sequence)?,
            recovery_only,
            intent,
            endpoints: [record.source.identity, record.destination.identity],
            admission: format::next(operation.expected)?,
            baseline: operation.baseline,
        })
    }
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    pub(super) fn check_recovery_context(
        &self,
        context: &RecoveryContext,
    ) -> Result<(), StoreError> {
        self.require_no_chain_history(context.id.connection)?;
        if !context
            .lifetime
            .ptr_eq(&Arc::downgrade(&self.native_lifetime))
            || context.lifetime.strong_count() == 0
        {
            return Err(StoreError::StaleRevision);
        }
        let connection = self.connection(context.id.connection)?;
        let operation = self.operation(context.id)?;
        if connection.revision != context.revision
            || connection.fence != context.fence
            || operation.revision != context.operation_revision
            || operation.native_kind != context.native_kind
            || operation.native_identity != context.native_identity
            || operation.native_budget.is_none()
        {
            return Err(StoreError::StaleRevision);
        }
        let gate = self
            .native_gates
            .get(&context.id.connection.0)
            .ok_or(StoreError::StaleRevision)?;
        gate.require_quiescent()?;
        if !context.recovery_only
            && (connection.lifecycle != 0
                || gate.stopped().load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(StoreError::Cancelled);
        }
        Ok(())
    }
    pub(super) fn reserve_recovery_reads(
        &mut self,
        context: &mut RecoveryContext,
        bytes: u64,
        deadline: std::time::Instant,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        if cancel.load(std::sync::atomic::Ordering::Acquire)
            || std::time::Instant::now() >= deadline
        {
            return Err(StoreError::Cancelled);
        }
        if bytes == 0 {
            return Err(StoreError::LimitExceeded);
        }
        let previous = self
            .operation(context.id)?
            .native_budget
            .ok_or(StoreError::RecoveryRequired)?;
        let charge = bytes
            .checked_add(format::READ_QUANTUM - 1)
            .and_then(|n| (n / format::READ_QUANTUM).checked_mul(format::READ_QUANTUM))
            .ok_or(StoreError::LimitExceeded)?;
        let next = format::NativeBudget {
            version: 1,
            sequence: format::next(previous.sequence)?,
            reserved: previous
                .reserved
                .checked_add(charge)
                .filter(|n| *n <= format::READ_LIMIT)
                .ok_or(StoreError::LimitExceeded)?,
        };
        next.validate()?;
        self.append(Change::ReadReserve {
            recovery_only: context.recovery_only,
            operation_expected: Some(context.operation_revision),
            connection: context.id.connection.0,
            serial: context.id.serial,
            expected: context.revision,
            fence: context.fence,
            previous,
            next: next.clone(),
        })?;
        context.revision = format::next(context.revision)?;
        context.operation_revision = context.revision;
        let stopped = self.check_recovery_context(context).is_err()
            || cancel.load(std::sync::atomic::Ordering::Acquire)
            || std::time::Instant::now() >= deadline;
        if context
            .reads
            .grant(next.sequence, if stopped { 0 } else { charge })
            .is_err()
        {
            self.poison_native();
            return Err(StoreError::RecoveryRequired);
        }
        if stopped {
            return Err(StoreError::Cancelled);
        }
        Ok(())
    }
    #[cfg(test)]
    fn resume_native_for_test(
        &mut self,
        id: OperationId,
        session: &super::transaction::TransactionSession,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        session.validate(id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        let connection = self.connection(id.connection)?;
        let operation = self.operation(id)?;
        let budget = operation
            .native_budget
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let receipt = operation.decode()?;
        if !receipt.action.is_reconciliation() {
            return Err(StoreError::RecoveryRequired);
        }
        let before = connection.decode()?;
        let live = LiveOperation {
            native_kind: operation.native_kind,
            native_identity: operation.native_identity.clone(),
            planned_baseline_count: None,
            action: receipt.action,
            gate: Arc::clone(
                self.native_gates
                    .get(&id.connection.0)
                    .ok_or(StoreError::StaleRevision)?,
            ),
            deletion_baseline_checked: false,
            id,
            lifetime: Arc::downgrade(&self.native_lifetime),
            revision: operation.revision,
            admission: format::next(operation.expected)?,
            baseline: operation.baseline,
            fence: connection.fence,
            intent: receipt.intent,
            endpoints: [before.source.identity, before.destination.identity],
            correlation: self.native_correlation,
            reads: self.native_read_state(id, budget.sequence)?,
        };
        self.check_native(&live)?;
        Ok(live)
    }
    // Verified records equality, not endpoint mutation. Only this exact ordinary
    // stage-3 shape can reuse the fresh owning untouched inspection; the old
    // Verified row supplies no live authority and never replaces paid rereads.
    fn verified_equality_is_untouched(
        &self,
        operation: &format::Operation,
    ) -> Result<bool, StoreError> {
        if operation.stage != 3
            || operation.action.is_some()
            || operation.native_capture.is_some()
            || !operation.native_capture_displaced.is_empty()
            || !operation.recovery.is_empty()
            || operation.native_identity.is_none()
            || operation.native_budget.is_none()
            || operation.native_boundary.is_some()
        {
            return Ok(false);
        }
        let receipt = operation.decode()?;
        if !receipt.action.is_reconciliation()
            || receipt.intent.decision != Decision::VerifyEqual
            || receipt.intent.source_before != receipt.intent.destination_before
        {
            return Ok(false);
        }
        for row in self.rows()? {
            if let Row::NativeRegistration(registration) = row? {
                if registration.connection == operation.connection
                    && registration.serial == operation.serial
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn never_begun_identity(
        &self,
        context: &RecoveryContext,
    ) -> Result<super::transaction::native::descriptor::Admission, StoreError> {
        self.check_recovery_context(context)?;
        let connection = self.connection(context.id.connection)?;
        let operation = self.operation(context.id)?;
        if connection.pending != Some(context.id.serial)
            || (operation.stage != 0 && !self.verified_equality_is_untouched(&operation)?)
            || !operation.recovery.is_empty()
        {
            return Err(StoreError::RecoveryRequired);
        }
        super::transaction::native::descriptor::Admission::decode(
            operation
                .native_identity
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn resume_native_untouched(
        &self,
        context: &RecoveryContext,
        evidence: &mut super::transaction::native::NeverBegun<'_>,
        session: &super::transaction::TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<LiveOperation, StoreError> {
        self.check_recovery_context(context)?;
        session.validate(context.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        if context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        evidence.validate(self, context)?;
        let mut live = LiveOperation {
            native_kind: context.native_kind,
            native_identity: context.native_identity.clone(),
            planned_baseline_count: None,
            action: context.action.clone(),
            gate: context.gate(),
            deletion_baseline_checked: false,
            id: context.id,
            lifetime: context.lifetime.clone(),
            revision: context.revision,
            admission: context.admission,
            baseline: context.baseline,
            fence: context.fence,
            intent: context.intent.clone(),
            endpoints: context.endpoints,
            correlation: self.native_correlation,
            reads: Arc::clone(&context.reads),
        };
        if matches!(live.intent.decision, Decision::RecoverDeletion { .. }) {
            self.verify_native_deletion_baseline(&mut live, evidence.deadline(), cancel)?;
        }
        self.check_native(&live)?;
        evidence.validate(self, context)?;
        session.require_idle()?;
        Ok(live)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn record_boundary(
        &mut self,
        context: &RecoveryContext,
        evidence: &mut super::transaction::native::NativeSafeBoundary<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<OperationReceipt>, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let (kind, evidence) = evidence.validate(self, context)?;
        let evidence = evidence.map(|(attempt, displaced, outcome)| format::BoundaryEvidence {
            attempt,
            displaced,
            outcome,
        });
        self.append(Change::NativeBoundary {
            connection: context.id.connection.0,
            serial: context.id.serial,
            expected: context.revision,
            operation_expected: context.operation_revision,
            fence: context.fence,
            kind,
            evidence,
        })?;
        Ok(self.ack(self.operation(context.id)?.decode()?))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn record_never_begun(
        &mut self,
        context: &RecoveryContext,
        evidence: &mut super::transaction::native::NeverBegun<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<OperationReceipt>, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        evidence.validate(self, context)?;
        self.append(Change::NativeBoundary {
            connection: context.id.connection.0,
            serial: context.id.serial,
            expected: context.revision,
            operation_expected: context.operation_revision,
            fence: context.fence,
            kind: 0,
            evidence: None,
        })?;
        // The proof retains leases and handles through durable accounting. No
        // payload/publication/baseline transition follows this acknowledged boundary.
        Ok(self.ack(self.operation(context.id)?.decode()?))
    }
    pub(super) fn native_capture_snapshot(
        &self,
        context: &RecoveryContext,
    ) -> Result<super::transaction::native::descriptor::ConflictSnapshot, StoreError> {
        self.check_recovery_context(context)?;
        if !matches!(context.action, ExecutionAction::Captured) {
            return Err(StoreError::RecoveryRequired);
        }
        let operation = self.operation(context.id)?;
        let snapshot = super::transaction::native::descriptor::ConflictSnapshot::decode(
            operation
                .native_capture
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        operation.validate_capture(&snapshot)?;
        Ok(snapshot)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn resume_native_conflict(
        &mut self,
        context: &RecoveryContext,
        evidence: &mut super::transaction::native::NativeConflictCapture<'_>,
        session: &super::transaction::TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        session.validate(context.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        if context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let operation = self.operation(context.id)?;
        if !matches!(operation.stage, 0 | 1)
            || self.connection(context.id.connection)?.pending != Some(context.id.serial)
            || evidence.snapshot() != &self.native_capture_snapshot(context)?
        {
            return Err(StoreError::RecoveryRequired);
        }
        evidence.validate_recovery(context)?;
        let live = LiveOperation {
            native_kind: context.native_kind,
            native_identity: context.native_identity.clone(),
            planned_baseline_count: None,
            action: ExecutionAction::Captured,
            gate: context.gate(),
            deletion_baseline_checked: false,
            id: context.id,
            lifetime: context.lifetime.clone(),
            revision: context.revision,
            admission: context.admission,
            baseline: context.baseline,
            fence: context.fence,
            intent: context.intent.clone(),
            endpoints: context.endpoints,
            correlation: self.native_correlation,
            reads: Arc::clone(&context.reads),
        };
        self.check_native(&live)?;
        evidence.validate(&live)?;
        session.require_idle()?;
        Ok(live)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn resume_native(
        &mut self,
        context: &RecoveryContext,
        evidence: &mut super::transaction::native::RecoveredEvidence<'_>,
        session: &super::transaction::TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        session.validate(context.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        if context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let connection = self.connection(context.id.connection)?;
        let operation = self.operation(context.id)?;
        if connection.pending != Some(context.id.serial)
            || connection.baseline != context.baseline
            || !matches!(operation.stage, 0 | 1 | 2 | 3 | 6)
        {
            return Err(StoreError::RecoveryRequired);
        }
        evidence.validate(context)?;
        self.check_recovery_context(context)?;
        session.require_idle()?;
        let live = LiveOperation {
            native_kind: context.native_kind,
            native_identity: context.native_identity.clone(),
            planned_baseline_count: None,
            action: context.action.clone(),
            gate: context.gate(),
            deletion_baseline_checked: false,
            id: context.id,
            lifetime: context.lifetime.clone(),
            revision: context.revision,
            admission: context.admission,
            baseline: context.baseline,
            fence: context.fence,
            intent: context.intent.clone(),
            endpoints: context.endpoints,
            correlation: self.native_correlation,
            reads: Arc::clone(&context.reads),
        };
        self.check_native(&live)?;
        evidence.validate(context)?;
        Ok(live)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn resume_native_deletion(
        &mut self,
        context: &RecoveryContext,
        evidence: &mut super::transaction::native::RecoveredDeletion<'_>,
        session: &super::transaction::TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        session.validate(context.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        if context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let connection = self.connection(context.id.connection)?;
        let operation = self.operation(context.id)?;
        if connection.pending != Some(context.id.serial)
            || connection.baseline != context.baseline
            || !matches!(operation.stage, 0..=3)
            || !matches!(operation.decision, 3 | 4)
        {
            return Err(StoreError::RecoveryRequired);
        }
        evidence.validate(context)?;
        let mut live = LiveOperation {
            native_kind: context.native_kind,
            native_identity: context.native_identity.clone(),
            planned_baseline_count: None,
            action: context.action.clone(),
            gate: context.gate(),
            deletion_baseline_checked: false,
            id: context.id,
            lifetime: context.lifetime.clone(),
            revision: context.revision,
            admission: context.admission,
            baseline: context.baseline,
            fence: context.fence,
            intent: context.intent.clone(),
            endpoints: context.endpoints,
            correlation: self.native_correlation,
            reads: Arc::clone(&context.reads),
        };
        self.verify_native_deletion_baseline(&mut live, evidence.deadline(), cancel)?;
        evidence.validate(context)?;
        self.check_recovery_context(context)?;
        session.require_idle()?;
        Ok(live)
    }
    fn reduce_native_boundary(
        &self,
        id: OperationId,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        kind: u8,
        evidence: Option<&format::BoundaryEvidence>,
    ) -> Result<Vec<Row>, StoreError> {
        self.reduce_boundary_rows(
            id,
            expected,
            operation_expected,
            fence,
            kind,
            evidence,
            false,
        )
    }
    fn reduce_boundary_rows(
        &self,
        id: OperationId,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        kind: u8,
        evidence: Option<&format::BoundaryEvidence>,
        selected: bool,
    ) -> Result<Vec<Row>, StoreError> {
        let mut connection = self.connection(id.connection)?;
        let mut operation = self.operation(id)?;
        if connection.revision != expected
            || connection.fence != fence
            || connection.pending != Some(id.serial)
            || operation.revision != operation_expected
        {
            return Err(StoreError::StaleRevision);
        }
        if connection.lifecycle > 1
            || kind > 3
            || operation.native_budget.is_none()
            || operation.native_boundary.is_some()
            || operation.action.is_some() != selected
            || (selected && (connection.lifecycle != 1 || operation.native_capture.is_some()))
        {
            return Err(StoreError::RecoveryRequired);
        }
        let registrations = self.operation_registrations(&operation)?;
        self.validate_regular_boundary(
            &operation,
            expected,
            fence,
            kind,
            evidence,
            selected,
            &registrations,
        )?;
        connection.pending = None;
        connection.revision = format::next(connection.revision)?;
        operation.revision = connection.revision;
        operation.stage = 7;
        operation.native_boundary = Some(kind);
        operation.native_boundary_evidence = evidence.cloned();
        operation.decode()?;
        Ok(vec![Row::Connection(connection), Row::Operation(operation)])
    }
    /// Canonical typed accounting check shared by event reduction and inert history closure.
    #[expect(
        clippy::too_many_lines,
        reason = "One canonical typed accounting predicate is shared by ordinary reduction and historical closure."
    )]
    fn validate_regular_boundary(
        &self,
        operation: &format::Operation,
        expected: u64,
        fence: u64,
        kind: u8,
        evidence: Option<&format::BoundaryEvidence>,
        selected: bool,
        registrations: &[format::NativeRegistration],
    ) -> Result<(), StoreError> {
        if operation.native_kind.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        operation.decode()?;
        if kind == 0 {
            if (operation.stage != 0 && !self.verified_equality_is_untouched(operation)?)
                || !operation.recovery.is_empty()
                || operation.native_identity.is_none()
                || evidence.is_some()
            {
                return Err(StoreError::RecoveryRequired);
            }
            if registrations.iter().any(|registration| {
                registration.intent.is_some()
                    || registration.stage.is_some()
                    || registration.outcome.is_some()
            }) {
                return Err(StoreError::RecoveryRequired);
            }
        } else {
            use super::transaction::native::descriptor as d;
            let evidence = evidence.ok_or(StoreError::RecoveryRequired)?;
            let Some(registration) = registrations
                .iter()
                .find(|registration| registration.attempt == evidence.attempt)
            else {
                return Err(StoreError::RecoveryRequired);
            };
            registration.validate()?;
            let intent = d::decode(
                registration
                    .intent
                    .as_deref()
                    .ok_or(StoreError::RecoveryRequired)?,
            )
            .map_err(|_| StoreError::Corrupt)?;
            let d::Body::Intent(body) = &intent.body else {
                return Err(StoreError::Corrupt);
            };
            if registration.admission != format::next(operation.expected)?
                || registration.baseline != operation.baseline
                || registration.fence > fence
                || body.path != operation.path.to_relative()?
                || body.decision != operation.decision
            {
                return Err(StoreError::StaleRevision);
            }
            if let Some(action) = &operation.action {
                if body.selection != self.selected_descriptor(operation, action)?
                    || registration.side
                        != 1u8.checked_sub(body.direction).ok_or(StoreError::Corrupt)?
                {
                    return Err(StoreError::RecoveryRequired);
                }
            }
            let parse = |bytes: &Option<Vec<u8>>| {
                bytes
                    .as_ref()
                    .map(|bytes| d::decode(bytes).map_err(|_| StoreError::Corrupt))
                    .transpose()
            };
            let stage = parse(&registration.stage)?;
            let displaced = parse(&evidence.displaced)?;
            let outcome = parse(&evidence.outcome)?;
            let tombstone = matches!(body.selection, d::Selection::Tombstone { .. });
            if (selected && stage.is_some() == tombstone)
                || (!selected && matches!(operation.decision, 1 | 2) && stage.is_none())
            {
                return Err(StoreError::RecoveryRequired);
            }
            if let Some(displaced) = &displaced {
                displaced
                    .joined(&intent, stage.as_ref(), None)
                    .map_err(|_| StoreError::Corrupt)?;
                if !matches!(&displaced.body, d::Body::Displaced(body) if body.barriers == 3)
                    || displaced.header.progress > expected
                {
                    return Err(StoreError::RecoveryRequired);
                }
            }
            if let Some(outcome) = &outcome {
                outcome
                    .joined(&intent, stage.as_ref(), displaced.as_ref())
                    .map_err(|_| StoreError::Corrupt)?;
                if !matches!(&outcome.body, d::Body::Outcome(body) if body.barriers == 3)
                    || outcome.header.progress > expected
                {
                    return Err(StoreError::RecoveryRequired);
                }
            }
            if registration
                .outcome
                .as_ref()
                .is_some_and(|recorded| Some(recorded) != evidence.outcome.as_ref())
                || registration
                    .outcome_displaced
                    .as_ref()
                    .is_some_and(|recorded| Some(recorded) != evidence.displaced.as_ref())
            {
                return Err(StoreError::RecoveryRequired);
            }
            match kind {
                1 if matches!(operation.stage, 0 | 1 | 6)
                    && displaced.is_none()
                    && outcome.is_none() => {}
                2 if matches!(operation.stage, 0 | 1)
                    && displaced.is_some()
                    && outcome.is_none() => {}
                3 if matches!(operation.stage, 0 | 1 | 2 | 3 | 6)
                    && (matches!(
                        body.expected
                            .get(
                                1usize
                                    .checked_sub(usize::from(body.direction))
                                    .ok_or(StoreError::Corrupt)?
                            )
                            .ok_or(StoreError::Corrupt)?,
                        d::State::Absent
                    ) || displaced.is_some())
                    && (selected || !matches!(operation.decision, 3 | 4) || outcome.is_some()) => {}
                _ => return Err(StoreError::RecoveryRequired),
            }
        }
        Ok(())
    }
    fn operation_registrations(
        &self,
        operation: &format::Operation,
    ) -> Result<Vec<format::NativeRegistration>, StoreError> {
        self.rows()?
            .filter_map(|row| match row {
                Ok(Row::NativeRegistration(reg))
                    if reg.connection == operation.connection && reg.serial == operation.serial =>
                {
                    Some(Ok(reg))
                }
                Err(error) => Some(Err(error)),
                _ => None,
            })
            .collect()
    }
    fn reduce_read_reserve(
        &self,
        id: OperationId,
        expected: u64,
        fence: u64,
        previous: &format::NativeBudget,
        next: &format::NativeBudget,
        recovery_only: bool,
        operation_expected: Option<u64>,
    ) -> Result<Vec<Row>, StoreError> {
        let mut connection = self.connection(id.connection)?;
        let mut operation = self.operation(id)?;
        if (!recovery_only && connection.lifecycle != 0) || connection.fence != fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected
            || operation.revision != operation_expected.unwrap_or(expected)
            || (!recovery_only && connection.pending != Some(id.serial))
            || operation.native_budget.as_ref() != Some(previous)
        {
            return Err(StoreError::StaleRevision);
        }
        previous.validate()?;
        next.validate()?;
        if next.sequence != format::next(previous.sequence)? || next.reserved <= previous.reserved {
            return Err(StoreError::Corrupt);
        }
        let revision = format::next(expected)?;
        connection.revision = revision;
        operation.revision = revision;
        operation.native_budget = Some(next.clone());
        Ok(vec![Row::Connection(connection), Row::Operation(operation)])
    }
    pub(super) fn check_native(&self, live: &LiveOperation) -> Result<(), StoreError> {
        self.require_no_chain_history(live.id.connection)?;
        if !live.lifetime.ptr_eq(&Arc::downgrade(&self.native_lifetime))
            || live.lifetime.strong_count() == 0
        {
            return Err(StoreError::StaleRevision);
        }
        let connection = self.connection(live.id.connection)?;
        if connection.lifecycle != 0 || connection.fence != live.fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != live.revision
            || connection.pending != Some(live.id.serial)
            || connection.baseline != live.baseline
        {
            return Err(StoreError::StaleRevision);
        }
        let operation = self.operation(live.id)?;
        if operation.revision != live.revision
            || operation.native_kind != live.native_kind
            || operation.native_identity != live.native_identity
            || operation.expected.checked_add(1) != Some(live.admission)
            || operation.decode()?.action != live.action
            || operation.decode()?.intent != live.intent
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(super) fn selected_native_identity(
        &self,
        live: &LiveOperation,
    ) -> Result<super::transaction::native::descriptor::Admission, StoreError> {
        self.check_native(live)?;
        let operation = self.operation(live.id())?;
        if operation.action.is_none() {
            return Err(StoreError::RecoveryRequired);
        }
        let fact = super::transaction::native::descriptor::Admission::decode(
            operation
                .native_identity
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        if fact.connection != live.id.connection.0
            || fact.operation != live.id.serial
            || fact.revision.checked_add(1) != Some(live.admission)
            || fact.path != live.intent.path
            || fact.baseline != live.baseline
            || fact.fence != live.fence
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(fact)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn selected_native_scope(
        &self,
        live: &LiveOperation,
    ) -> Result<
        (
            u8,
            bool,
            u8,
            super::transaction::native::descriptor::Selection,
        ),
        StoreError,
    > {
        self.check_native(live)?;
        if !matches!(live.action(), ExecutionAction::Selected(_)) {
            return Err(StoreError::RecoveryRequired);
        }
        let operation = self.operation(live.id())?;
        let Some(
            action @ format::SelectedAction::Restore {
                target, replace, ..
            },
        ) = &operation.action
        else {
            return Err(StoreError::RecoveryRequired);
        };
        Ok((
            *target,
            *replace,
            operation.decision,
            self.selected_descriptor(&operation, action)?,
        ))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn reserve_selected_namespace(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut super::transaction::native::NativeRestore<'_>,
    ) -> Result<NamespaceReservation, StoreError> {
        proof.validate(self, live)?;
        let (target, _, _, _) = self.selected_native_scope(live)?;
        let existing = self
            .native_namespace_records(live)?
            .into_iter()
            .filter(|record| record.operation() == live.id())
            .collect::<Vec<_>>();
        if !existing.is_empty() {
            if existing.len() != 1 || self.operation(live.id())?.stage != 0 {
                return Err(StoreError::RecoveryRequired);
            }
            let record = existing
                .into_iter()
                .next()
                .ok_or(StoreError::RecoveryRequired)?;
            let registration = &record.registration;
            if registration.side != target
                || registration.admission != live.admission
                || registration.baseline != live.baseline
                || registration.fence != live.fence
                || registration.intent.is_some()
                || registration.stage.is_some()
                || registration.outcome.is_some()
            {
                return Err(StoreError::RecoveryRequired);
            }
            proof.require_uncreated_namespace(self, live, &record)?;
            return Ok(NamespaceReservation {
                registration: record.registration,
                lifetime: live.lifetime.clone(),
                revision: live.revision,
            });
        }
        let previous = live.revision();
        let nonce = super::transaction::native::namespace_nonce()?;
        let reserved = self.reserve_native_namespace_with_nonce(
            live,
            if target == 0 {
                Side::Source
            } else {
                Side::Destination
            },
            nonce,
        )?;
        proof.advance(self, live, previous)?;
        Ok(reserved)
    }
    #[cfg(target_os = "linux")]
    fn selected_registration(
        &self,
        live: &LiveOperation,
        proof: &super::transaction::native::NativeRestore<'_>,
    ) -> Result<format::NativeRegistration, StoreError> {
        let (intent, stage) = proof.prepared_descriptors()?;
        let descriptor = super::transaction::native::descriptor::decode(&intent)
            .map_err(|_| StoreError::Corrupt)?;
        let mut key = format::key(5, live.id.connection.0, live.id.serial);
        key.extend(
            u32::try_from(descriptor.header.attempt)
                .map_err(|_| StoreError::Corrupt)?
                .to_be_bytes(),
        );
        let Some(Row::NativeRegistration(registration)) = self.row(&key)? else {
            return Err(StoreError::RecoveryRequired);
        };
        if registration.intent.as_ref() != Some(&intent) || registration.stage != stage {
            return Err(StoreError::RecoveryRequired);
        }
        registration.validate()?;
        Ok(registration)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn check_selected_displace_prepared(
        &self,
        live: &LiveOperation,
        proof: &super::transaction::native::NativeRestore<'_>,
    ) -> Result<(), StoreError> {
        let (_, replace, _, _) = self.selected_native_scope(live)?;
        let registration = self.selected_registration(live, proof)?;
        if !replace || registration.outcome.is_some() || self.operation(live.id())?.stage != 0 {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(super) fn reserve_selected_recovery(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut super::transaction::native::NativeRestore<'_>,
    ) -> Result<RecoveryReservation, StoreError> {
        proof.validate(self, live)?;
        let (target, replace, _, _) = self.selected_native_scope(live)?;
        if !replace {
            return Err(StoreError::RecoveryRequired);
        }
        let previous = live.revision();
        let reserved = self.reserve_recovery(
            &self.ack(live.id()),
            if target == 0 {
                Side::Source
            } else {
                Side::Destination
            },
            live.intent().path.clone(),
            RecoveryReason::DisplacedVersion,
        )?;
        live.revision = format::next(previous)?;
        proof.advance(self, live, previous)?;
        Ok(reserved)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn check_selected_prepared(
        &self,
        live: &LiveOperation,
        proof: &super::transaction::native::NativeRestore<'_>,
    ) -> Result<(), StoreError> {
        self.check_native(live)?;
        let registration = self.selected_registration(live, proof)?;
        if registration.outcome.is_some() || !matches!(self.operation(live.id())?.stage, 1 | 6) {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(super) fn finish_selected(
        &mut self,
        live: &mut LiveOperation,
        mut proof: super::transaction::native::NativeRestore<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<OperationReceipt>, StoreError> {
        self.guard()?;
        proof.validate(self, live)?;
        let registration = self.selected_registration(live, &proof)?;
        let outcome = super::transaction::native::descriptor::decode(&proof.outcome_descriptor()?)
            .map_err(|_| StoreError::Corrupt)?;
        let context = self.native_read_context(live, proof.deadline(), cancel)?;
        let previous = live.revision();
        let prior = self.selected_attention_carry(&self.operation(live.id())?, previous)?;
        let change = if prior.is_empty() {
            Change::SelectedApplied {
                connection: live.id.connection.0,
                serial: live.id.serial,
                expected: previous,
                fence: live.fence,
                attempt: registration.attempt,
                outcome: outcome.digest,
            }
        } else {
            Change::SelectedAppliedCarryV1 {
                connection: live.id.connection.0,
                serial: live.id.serial,
                expected: previous,
                fence: live.fence,
                attempt: registration.attempt,
                outcome: outcome.digest,
                carry: format::SelectedAttentionCarryV1 { version: 1, prior },
            }
        };
        self.append_native(change, &context)?;
        live.revision = format::next(previous)?;
        proof.after_applied(live, previous)?;
        let receipt = self.operation(live.id())?.decode()?;
        Ok(self.ack(receipt))
    }
    /// Durable accounting only. This query supplies no resumed live authority.
    #[cfg(test)]
    pub(super) fn selected_application(
        &self,
        id: OperationId,
    ) -> Result<Option<OperationReceipt>, StoreError> {
        let operation = self.operation(id)?;
        if operation.stage != 9 {
            return Ok(None);
        }
        Ok(Some(operation.decode()?))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn record_selected_outcome(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut super::transaction::native::NativeRestore<'_>,
        verified: bool,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.guard()?;
        proof.validate(self, live)?;
        let mut registration = self.selected_registration(live, proof)?;
        registration.outcome = Some(proof.outcome_descriptor()?);
        registration.outcome_displaced = proof.outcome_displaced()?;
        let context = self.native_read_context(live, proof.deadline(), cancel)?;
        let previous = live.revision();
        self.append_native(
            Change::NativeOutcome {
                registration,
                expected: previous,
                verified,
                reconstructed: false,
            },
            &context,
        )?;
        live.revision = format::next(previous)?;
        proof.advance(self, live, previous)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn record_selected_prepared(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut super::transaction::native::NativeRestore<'_>,
        reserved: NamespaceReservation,
    ) -> Result<(), StoreError> {
        proof.validate(self, live)?;
        if !reserved.lifetime.ptr_eq(live.lifetime()) || reserved.revision != live.revision() {
            return Err(StoreError::StaleRevision);
        }
        let (intent, stage) = proof.prepared_descriptors()?;
        let mut registration = reserved.registration;
        registration.intent = Some(intent);
        registration.stage = stage;
        let previous = live.revision();
        self.append(Change::NativePrepared {
            registration,
            expected: previous,
        })?;
        live.revision = format::next(previous)?;
        proof.advance(self, live, previous)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn reserve_native_namespace(
        &mut self,
        live: &mut LiveOperation,
        side: Side,
    ) -> Result<NamespaceReservation, StoreError> {
        if !live.action.is_reconciliation() {
            return Err(StoreError::RecoveryRequired);
        }
        let nonce = super::transaction::native::namespace_nonce()?;
        self.reserve_native_namespace_with_nonce(live, side, nonce)
    }
    // Shared durable reducer input; outside the store only the native random
    // producer above can select a new attempt label.
    fn reserve_native_namespace_with_nonce(
        &mut self,
        live: &mut LiveOperation,
        side: Side,
        nonce: [u8; 16],
    ) -> Result<NamespaceReservation, StoreError> {
        self.check_native(live)?;
        let mut count = 0_u32;
        let mut attempts = 0_u32;
        for row in self.rows()? {
            if let Row::NativeRegistration(row) = row? {
                if row.connection != live.id.connection.0 {
                    continue;
                }
                count = count.checked_add(1).ok_or(StoreError::LimitExceeded)?;
                if row.serial == live.id.serial {
                    if row.nonce == nonce {
                        return Err(StoreError::StaleRevision);
                    }
                    attempts = attempts.checked_add(1).ok_or(StoreError::LimitExceeded)?;
                }
            }
        }
        if count >= 128 || attempts >= 8 {
            return Err(StoreError::LimitExceeded);
        }
        let registration = format::NativeRegistration {
            native_kind: self.operation(live.id)?.native_kind,
            connection: live.id.connection.0,
            serial: live.id.serial,
            attempt: attempts + 1,
            nonce,
            side: u8::from(side != Side::Source),
            admission: live.admission,
            baseline: live.baseline,
            fence: live.fence,
            intent: None,
            stage: None,
            outcome: None,
            outcome_displaced: None,
        };
        let revision = format::next(live.revision)?;
        self.append(Change::NativeReserve {
            registration: registration.clone(),
            expected: live.revision,
        })?;
        live.revision = revision;
        Ok(NamespaceReservation {
            registration,
            lifetime: live.lifetime.clone(),
            revision,
        })
    }
    fn reduce_native_outcome(
        &self,
        registration: &format::NativeRegistration,
        expected: u64,
        verified: bool,
        reconstructed: bool,
        recovery_checks: &mut RecoveryChecks<'_>,
    ) -> Result<Vec<Row>, StoreError> {
        self.reduce_native_outcome_inner(
            registration,
            expected,
            verified,
            reconstructed,
            false,
            recovery_checks,
        )
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    fn reduce_native_outcome_inner(
        &self,
        registration: &format::NativeRegistration,
        expected: u64,
        verified: bool,
        reconstructed: bool,
        complete_selected: bool,
        recovery_checks: &mut RecoveryChecks<'_>,
    ) -> Result<Vec<Row>, StoreError> {
        if registration.native_kind == Some(format::NativeOperationKind::DirectoryV1) {
            if complete_selected {
                return Err(StoreError::Corrupt);
            }
            return self.reduce_directory_outcome(registration, expected, verified, reconstructed);
        }
        use super::transaction::native::descriptor as d;
        registration.validate()?;
        let connection = self.connection(ConnectionId(registration.connection))?;
        if connection.lifecycle != 0 || connection.fence != registration.fence {
            return Err(StoreError::Disconnected);
        }
        let Some(Row::NativeRegistration(mut previous)) = self.row(&registration.key())? else {
            return Err(StoreError::Corrupt);
        };
        if verified {
            if previous != *registration || previous.outcome.is_none() {
                return Err(StoreError::StaleRevision);
            }
        } else {
            if previous.outcome.is_some() {
                return Err(StoreError::StaleRevision);
            }
            previous.outcome.clone_from(&registration.outcome);
            previous
                .outcome_displaced
                .clone_from(&registration.outcome_displaced);
            if previous != *registration {
                return Err(StoreError::Corrupt);
            }
        }
        let outcome = d::decode(
            registration
                .outcome
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        let intent = d::decode(registration.intent.as_ref().ok_or(StoreError::Corrupt)?)
            .map_err(|_| StoreError::Corrupt)?;
        let d::Body::Outcome(body) = &outcome.body else {
            return Err(StoreError::Corrupt);
        };
        let d::Body::Intent(before) = &intent.body else {
            return Err(StoreError::Corrupt);
        };
        let historical_progress = reconstructed
            && self
                .operation(OperationId {
                    connection: ConnectionId(registration.connection),
                    serial: registration.serial,
                })?
                .native_budget
                .is_some()
            && outcome.header.progress >= registration.admission
            && outcome.header.progress <= expected;
        if (!verified && outcome.header.progress != expected && !historical_progress)
            || (reconstructed && !historical_progress)
            || body.kind > 2
            || body.sequence != 1
            || (body.kind < 2
                && body.states.get(usize::from(before.direction))
                    != before.expected.get(usize::from(before.direction)))
        {
            return Err(StoreError::Corrupt);
        }
        let displaced = registration
            .outcome_displaced
            .as_ref()
            .map(|bytes| d::decode(bytes).map_err(|_| StoreError::Corrupt))
            .transpose()?;
        let mut accounted_displaced = displaced.is_none();
        for reference in &body.recovery {
            let mut key = format::key(6, registration.connection, registration.serial);
            key.extend(reference.slot.to_be_bytes());
            let Some(Row::NativeRecoveryFact(bound)) = self.row(&key)? else {
                return Err(StoreError::RecoveryRequired);
            };
            let fact = d::RecoveryFact::decode(&bound.fact).map_err(|_| StoreError::Corrupt)?;
            if bound.attempt != registration.attempt
                || fact.intent != intent.digest
                || fact.side != reference.side
                || fact.role != reference.role
                || fact.binding_digest().map_err(|_| StoreError::Corrupt)? != reference.binding
            {
                return Err(StoreError::Corrupt);
            }
            let mut recovery_key = format::key(2, registration.connection, registration.serial);
            recovery_key.extend(reference.slot.to_be_bytes());
            let Some(Row::Recovery(entry)) = self.row(&recovery_key)? else {
                return Err(StoreError::Corrupt);
            };
            if entry.state != 1 {
                return Err(StoreError::RecoveryRequired);
            }
            recovery_checks.verify(self, &entry)?;
            if let Some(d::Descriptor {
                body: d::Body::Displaced(actual),
                ..
            }) = &displaced
            {
                if fact.role == 0
                    && fact.object == actual.object
                    && fact.side == actual.side
                    && registration.outcome_displaced.as_ref() == Some(&bound.displaced)
                {
                    accounted_displaced = true;
                }
            }
        }
        if !accounted_displaced {
            return Err(StoreError::RecoveryRequired);
        }
        let operation = self.operation(OperationId {
            connection: ConnectionId(registration.connection),
            serial: registration.serial,
        })?;
        let mut rows = if let Some(action) = &operation.action {
            if before.selection != self.selected_descriptor(&operation, action)?
                || connection.revision != expected
                || connection.pending != Some(registration.serial)
                || (verified && operation.stage != if complete_selected { 3 } else { 2 })
                || (!verified && !matches!(operation.stage, 1 | 6))
            {
                return Err(StoreError::RecoveryRequired);
            }
            let format::SelectedAction::Restore {
                target, replace, ..
            } = action;
            let tombstone = matches!(before.selection, d::Selection::Tombstone { .. });
            if *target != registration.side
                || usize::from(*target) != 1 - usize::from(before.direction)
                || ((body.kind == 1 || displaced.is_some()) && !replace)
                || (body.kind == 2) != tombstone
                || (tombstone
                    && (body.states.get(usize::from(*target)) != Some(&d::State::Absent)
                        || body.states.get(usize::from(before.direction))
                            != before.expected.get(usize::from(before.direction))))
            {
                return Err(StoreError::RecoveryRequired);
            }
            recovery_checks.verify_selected(self, &operation)?;
            let mut connection = connection;
            let mut operation = operation;
            connection.revision = format::next(expected)?;
            operation.revision = connection.revision;
            operation.stage = if verified { 3 } else { 2 };
            vec![Row::Connection(connection), Row::Operation(operation)]
        } else {
            if complete_selected {
                return Err(StoreError::RecoveryRequired);
            }
            self.reduce_outcome(
                registration.connection,
                registration.serial,
                expected,
                &connection.source.identity,
                &connection.destination.identity,
                verified,
                recovery_checks,
            )?
        };
        rows.push(Row::NativeRegistration(registration.clone()));
        Ok(rows)
    }
    fn selected_attention(
        &self,
        id: ConnectionId,
        path: &format::NativePath,
    ) -> Result<Vec<format::Operation>, StoreError> {
        let mut pending = Vec::new();
        for row in self.rows()? {
            let Row::Operation(operation) = row? else {
                continue;
            };
            if operation.connection == id.0
                && &operation.path == path
                && operation
                    .selected_applied
                    .as_ref()
                    .is_some_and(|fact| fact.attention)
            {
                operation.decode()?;
                if pending.len() >= 128 {
                    return Err(StoreError::LimitExceeded);
                }
                pending.push(operation);
            }
        }
        Ok(pending)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    fn reduce_selected_applied(
        &self,
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        fence: u64,
        attempt: u32,
        outcome: [u8; 32],
        carry: Option<&format::SelectedAttentionCarryV1>,
        checks: &mut RecoveryChecks<'_>,
    ) -> Result<Vec<Row>, StoreError> {
        let id = OperationId {
            connection: ConnectionId(connection),
            serial,
        };
        let operation = self.operation(id)?;
        if let Some(carry) = carry {
            self.validate_selected_attention_carry(&operation, expected, carry)?;
        }
        let Some(format::SelectedAction::Restore { conflict, .. }) = &operation.action else {
            return Err(StoreError::RecoveryRequired);
        };
        if operation.selected_applied.is_some() || self.connection(id.connection)?.fence != fence {
            return Err(StoreError::StaleRevision);
        }
        if let Some(expected) = conflict {
            let Some(Row::Conflict(current)) =
                self.row(&format::key(3, connection, expected.serial))?
            else {
                return Err(StoreError::RecoveryRequired);
            };
            if current.revision != expected.revision || current.resolved {
                return Err(StoreError::StaleRevision);
            }
        }
        let mut key = format::key(5, connection, serial);
        key.extend(attempt.to_be_bytes());
        let Some(Row::NativeRegistration(registration)) = self.row(&key)? else {
            return Err(StoreError::RecoveryRequired);
        };
        let descriptor = super::transaction::native::descriptor::decode(
            registration
                .outcome
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        if descriptor.digest != outcome {
            return Err(StoreError::RecoveryRequired);
        }
        let mut rows =
            self.reduce_native_outcome_inner(&registration, expected, true, false, true, checks)?;
        for row in &mut rows {
            match row {
                Row::Connection(value) => value.pending = None,
                Row::Operation(value) => {
                    value.stage = 9;
                    value.selected_applied = Some(format::SelectedAppliedFact {
                        revision: value.revision,
                        fence,
                        outcome,
                        attention: true,
                    });
                }
                _ => {}
            }
        }
        // Completion starts retention at zero, but the per-path attention marker
        // keeps both selected and displaced versions pinned until it is cleared.
        let Some(format::SelectedAction::Restore { selected, .. }) = &operation.action else {
            return Err(StoreError::Corrupt);
        };
        if let format::SelectedVersion::Recovery {
            operation: selected_serial,
            slot,
            ..
        } = selected
        {
            let selected_id = RecoveryId {
                operation: OperationId {
                    connection: id.connection,
                    serial: *selected_serial,
                },
                slot: *slot,
            };
            let Some(Row::Recovery(mut selected_entry)) =
                self.row(&recovery::recovery_key(selected_id))?
            else {
                return Err(StoreError::Corrupt);
            };
            selected_entry.retained = 0;
            rows.push(Row::Recovery(selected_entry));
        }
        for slot in &operation.recovery {
            let id = RecoveryId {
                operation: id,
                slot: *slot,
            };
            let Some(Row::Recovery(mut entry)) = self.row(&recovery::recovery_key(id))? else {
                return Err(StoreError::Corrupt);
            };
            if entry.state == 5 {
                continue;
            }
            if entry.state != 1 {
                return Err(StoreError::RecoveryRequired);
            }
            checks.verify(self, &entry)?;
            entry.state = 2;
            entry.retained = 0;
            rows.push(Row::Recovery(entry));
        }
        // Legacy events unconditionally supersede all earlier markers. Only an
        // already validated versioned event retains the exact named true facts.
        rows.extend(self.superseded_attention_rows(id.connection, &operation.path, carry)?);
        Ok(rows)
    }
    fn superseded_attention_rows(
        &self,
        connection: ConnectionId,
        path: &format::NativePath,
        carry: Option<&format::SelectedAttentionCarryV1>,
    ) -> Result<Vec<Row>, StoreError> {
        let mut rows = Vec::new();
        for mut previous in self.selected_attention(connection, path)? {
            if carry.is_some_and(|carry| {
                carry
                    .prior
                    .iter()
                    .any(|fact| fact.operation == previous.serial)
            }) {
                continue;
            }
            previous
                .selected_applied
                .as_mut()
                .ok_or(StoreError::Corrupt)?
                .attention = false;
            rows.push(Row::Operation(previous));
        }
        Ok(rows)
    }
    fn selected_descriptor(
        &self,
        operation: &format::Operation,
        action: &format::SelectedAction,
    ) -> Result<super::transaction::native::descriptor::Selection, StoreError> {
        use super::transaction::native::descriptor as d;
        let format::SelectedAction::Restore {
            selected, conflict, ..
        } = action;
        if matches!(selected, format::SelectedVersion::Tombstone { .. }) {
            return self.selected_tombstone_descriptor(operation, action);
        }
        let format::SelectedVersion::Recovery {
            operation: serial,
            slot,
            content,
        } = selected
        else {
            return Err(StoreError::RecoveryRequired);
        };
        let id = RecoveryId {
            operation: OperationId {
                connection: ConnectionId(operation.connection),
                serial: *serial,
            },
            slot: *slot,
        };
        let Some(Row::Recovery(entry)) = self.row(&recovery::recovery_key(id))? else {
            return Err(StoreError::RecoveryRequired);
        };
        if !matches!(entry.state, 1 | 2)
            || entry.path != operation.path
            || entry.content.as_ref() != Some(content)
        {
            return Err(StoreError::RecoveryRequired);
        }
        let object = entry.object.as_ref().ok_or(StoreError::RecoveryRequired)?;
        if object.root != self.storage.identity {
            return Err(StoreError::IdentityChanged);
        }
        let format::State::File {
            sha256,
            bytes,
            executable,
        } = content
        else {
            return Err(StoreError::Corrupt);
        };
        let recovery = d::Recovery {
            connection: operation.connection,
            operation: *serial,
            slot: *slot,
        };
        let artifact = recovery::artifact_view(object);
        let content = d::Content {
            digest: *sha256,
            length: *bytes,
            executable: *executable,
        };
        Ok(if let Some(conflict) = conflict {
            d::Selection::Resolve {
                conflict: (operation.connection, conflict.serial),
                revision: conflict.revision,
                recovery,
                artifact,
                content,
            }
        } else {
            d::Selection::Restore {
                recovery,
                artifact,
                content,
            }
        })
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    fn reduce_native_registration(
        &self,
        registration: &format::NativeRegistration,
        expected: u64,
        prepared: bool,
    ) -> Result<Vec<Row>, StoreError> {
        if registration.native_kind == Some(format::NativeOperationKind::DirectoryV1)
            && registration.baseline.is_none()
        {
            let operation = self.operation(OperationId {
                connection: ConnectionId(registration.connection),
                serial: registration.serial,
            })?;
            if !chain::absent_first_directory(&operation)? {
                return Err(StoreError::Corrupt);
            }
            registration.validate_history_candidate()?;
        } else {
            registration.validate()?;
        }
        let mut connection = self.connection(ConnectionId(registration.connection))?;
        if connection.lifecycle != 0 || connection.fence != registration.fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected || connection.pending != Some(registration.serial) {
            return Err(StoreError::StaleRevision);
        }
        let mut operation = self.operation(OperationId {
            connection: ConnectionId(registration.connection),
            serial: registration.serial,
        })?;
        if registration.native_kind != operation.native_kind {
            return Err(StoreError::Corrupt);
        }
        let selected = operation.action.as_ref();
        if let Some(format::SelectedAction::Restore {
            target, replace, ..
        }) = selected
        {
            let target_before = if *target == 0 {
                &operation.source
            } else {
                &operation.destination
            };
            if registration.side != *target || (!replace && target_before.is_some()) {
                return Err(StoreError::RecoveryRequired);
            }
        }
        if registration.admission != format::next(operation.expected)?
            || registration.baseline != operation.baseline
            || operation.stage > 1
        {
            return Err(StoreError::StaleRevision);
        }
        if selected.is_none()
            && ((matches!(operation.decision, 1 | 4) && registration.side != 1)
                || (matches!(operation.decision, 2 | 3) && registration.side != 0))
        {
            return Err(StoreError::Corrupt);
        }
        if prepared {
            let Some(Row::NativeRegistration(mut previous)) = self.row(&registration.key())? else {
                return Err(StoreError::Corrupt);
            };
            if previous.intent.is_some() || registration.intent.is_none() {
                return Err(StoreError::StaleRevision);
            }
            previous.intent.clone_from(&registration.intent);
            previous.stage.clone_from(&registration.stage);
            if previous != *registration {
                return Err(StoreError::Corrupt);
            }
            if operation.native_kind == Some(format::NativeOperationKind::DirectoryV1) {
                let (intent, _) = Self::directory_registration_join(&operation, registration)?;
                if intent.header.progress != expected {
                    return Err(StoreError::Corrupt);
                }
                operation.stage = 6;
            } else {
                use super::transaction::native::descriptor::{self, Body, State};
                let decoded =
                    descriptor::decode(registration.intent.as_ref().ok_or(StoreError::Corrupt)?)
                        .map_err(|_| StoreError::Corrupt)?;
                let Body::Intent(intent) = &decoded.body else {
                    return Err(StoreError::Corrupt);
                };
                let identity = |v: &storage::Identity| {
                    let mut bytes = [0; 64];
                    bytes[..32].copy_from_slice(&v.volume);
                    bytes[32..].copy_from_slice(&v.root);
                    bytes
                };
                let state = |v: &State| -> Result<Option<format::State>, StoreError> {
                    match v {
                        State::Absent => Ok(None),
                        State::Regular { content, .. } => Ok(Some(format::State::File {
                            sha256: content.digest,
                            bytes: content.length,
                            executable: content.executable,
                        })),
                        _ => Err(StoreError::Corrupt),
                    }
                };
                if decoded.header.progress != expected
                    || intent.endpoints
                        != [
                            identity(&connection.source.identity),
                            identity(&connection.destination.identity),
                        ]
                    || intent.path != operation.path.to_relative()?
                    || intent.decision != operation.decision
                    || state(&intent.expected[0])? != operation.source
                    || state(&intent.expected[1])? != operation.destination
                    || (selected.is_none()
                        && matches!(operation.decision, 1 | 2)
                        && registration.stage.is_none())
                    || (selected.is_none()
                        && matches!(operation.decision, 3 | 4)
                        && registration.stage.is_some())
                {
                    return Err(StoreError::Corrupt);
                }
                if let Some(action) = selected {
                    let tombstone =
                        matches!(intent.selection, descriptor::Selection::Tombstone { .. });
                    if intent.selection != self.selected_descriptor(&operation, action)?
                        || intent.direction != 1 - registration.side
                        || registration.stage.is_some() == tombstone
                    {
                        return Err(StoreError::Corrupt);
                    }
                } else if intent.selection != descriptor::Selection::None {
                    return Err(StoreError::Corrupt);
                }
                if selected.is_some()
                    && intent.expected.get(usize::from(registration.side)) == Some(&State::Absent)
                {
                    if !operation.recovery.is_empty() {
                        return Err(StoreError::Corrupt);
                    }
                    operation.stage = 6;
                }
                if selected.is_none()
                    && matches!(
                        (
                            operation.decision,
                            &operation.source,
                            &operation.destination
                        ),
                        (1, Some(format::State::File { .. }), None)
                            | (2, None, Some(format::State::File { .. }))
                    )
                {
                    if !operation.recovery.is_empty() {
                        return Err(StoreError::Corrupt);
                    }
                    operation.stage = 6;
                }
            }
        } else {
            if registration.intent.is_some()
                || registration.stage.is_some()
                || self.row(&registration.key())?.is_some()
            {
                return Err(StoreError::Corrupt);
            }
            let mut count = 0_u32;
            let mut attempts = 0_u32;
            for row in self.rows()? {
                if let Row::NativeRegistration(row) = row? {
                    if row.connection != registration.connection {
                        continue;
                    }
                    count = count.checked_add(1).ok_or(StoreError::LimitExceeded)?;
                    if row.serial == registration.serial {
                        if row.nonce == registration.nonce {
                            return Err(StoreError::StaleRevision);
                        }
                        attempts = attempts.checked_add(1).ok_or(StoreError::LimitExceeded)?;
                    }
                }
            }
            if count >= 128 || attempts >= 8 || registration.attempt != attempts + 1 {
                return Err(StoreError::LimitExceeded);
            }
        }
        connection.revision = format::next(connection.revision)?;
        operation.revision = connection.revision;
        Ok(vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::NativeRegistration(registration.clone()),
        ])
    }
    pub(super) fn request_disconnect(
        &mut self,
        id: ConnectionId,
        expected: Revision,
    ) -> Result<DurableAck<DisconnectFence>, StoreError> {
        if let Some(gate) = self.native_gates.get(&id.0) {
            gate.close();
        }
        self.guard()?;
        let connection = self.connection(id)?;
        if connection.revision != expected {
            return Err(StoreError::StaleRevision);
        }
        if connection.lifecycle == 2 {
            return Err(StoreError::Disconnected);
        }
        let generation = if connection.lifecycle == 1 {
            connection.fence
        } else {
            let generation = format::next(connection.fence)?;
            self.append(Change::Fence {
                connection: id.0,
                expected,
                generation,
            })?;
            generation
        };
        Ok(self.ack(DisconnectFence {
            connection: id,
            generation,
        }))
    }
    pub(super) fn finish_disconnect(
        &mut self,
        fence: &DurableAck<DisconnectFence>,
    ) -> Result<DurableAck<ConnectionRecord>, StoreError> {
        self.guard()?;
        self.check_ack(fence)?;
        let connection = self.connection(fence.value.connection)?;
        if connection.lifecycle != 1 || connection.fence != fence.value.generation {
            return Err(StoreError::StaleRevision);
        }
        if connection.pending.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        self.append(Change::Disconnect {
            connection: connection.id,
            expected: connection.revision,
            generation: connection.fence,
        })?;
        Ok(self.ack(self.load_connection(fence.value.connection)?))
    }
    #[expect(
        clippy::too_many_lines,
        reason = "One exhaustive event dispatch preserves the shared preconditions and recovery guard owner."
    )]
    fn reduce(
        &self,
        event: &Event,
        recovery_checks: &mut RecoveryChecks,
    ) -> Result<Vec<Row>, StoreError> {
        if event.schema != 1 || event.predecessor != self.digest {
            return Err(StoreError::Corrupt);
        }
        self.review_event_guard(&event.change)?;
        let canonical = chain_review::canonical_component(&event.change);
        let change = canonical.as_ref().unwrap_or(&event.change);
        self.chain_event_guard(change)?;
        let mut rows = self.reduce_rows(change, recovery_checks)?;
        self.update_chain_progress(change, &mut rows)?;
        if let Change::Intent { operation } = change {
            self.preflight_chain_intent(operation, &rows)?;
        }
        self.complete_review_rows(&event.change, &mut rows)?;
        self.validate_chain_history(&rows)?;
        self.validate_review_history(&rows)?;
        Ok(rows)
    }
    // Canonical row derivation only. Wrappers add their complete outer rows before
    // the single common prospective-history epilogue above.
    fn reduce_rows(
        &self,
        change: &Change,
        recovery_checks: &mut RecoveryChecks,
    ) -> Result<Vec<Row>, StoreError> {
        match change {
            Change::InitialReviewAccepted { .. }
            | Change::ReviewedIntent { .. }
            | Change::ReviewedChainControl { .. }
            | Change::InitialReviewControl { .. }
            | Change::ReviewedOrdinaryStopBoundary { .. } => self.reduce_review_change(change),
            Change::InitialEmptyBaseline { fact } => self.reduce_initial_empty(fact),
            Change::DirectoryChainControl { .. } => self.reduce_chain_control(change),
            Change::DirectoryChainStopBoundary { .. } => self.reduce_chain_stop(change),
            Change::NativeConflictComplete {
                connection,
                serial,
                expected,
                fence,
                conflict_expected,
                capture,
            } => self.reduce_native_conflict_complete(
                OperationId {
                    connection: ConnectionId(*connection),
                    serial: *serial,
                },
                *expected,
                *fence,
                *conflict_expected,
                *capture,
                recovery_checks,
            ),
            Change::SelectedCollisionComplete {
                connection,
                serial,
                expected,
                fence,
                conflict_expected,
                capture,
            } => self.reduce_selected_collision_complete(
                OperationId {
                    connection: ConnectionId(*connection),
                    serial: *serial,
                },
                *expected,
                *fence,
                *conflict_expected,
                *capture,
                recovery_checks,
            ),
            Change::NativeSelectedCollision { snapshot, expected } => {
                self.reduce_selected_collision(snapshot, *expected, recovery_checks)
            }
            Change::NativeCapture {
                snapshot,
                displaced,
                expected,
                kind,
            } => self.reduce_native_capture(snapshot, displaced, *expected, *kind),
            Change::NativeDrain { .. } | Change::SelectedDrain { .. } => {
                self.reduce_native_drain(change, recovery_checks)
            }
            Change::SelectedCaptureDrain { .. } | Change::NativeCaptureDrain { .. } => {
                self.reduce_captured_drain(change, recovery_checks)
            }
            Change::DirectoryBoundary {
                connection,
                serial,
                expected,
                operation_expected,
                fence,
                fact,
            } => self.reduce_directory_boundary(
                OperationId {
                    connection: ConnectionId(*connection),
                    serial: *serial,
                },
                *expected,
                *operation_expected,
                *fence,
                fact,
            ),
            Change::NativeBoundary {
                connection,
                serial,
                expected,
                operation_expected,
                fence,
                kind,
                evidence,
            } => self.reduce_native_boundary(
                OperationId {
                    connection: ConnectionId(*connection),
                    serial: *serial,
                },
                *expected,
                *operation_expected,
                *fence,
                *kind,
                evidence.as_ref(),
            ),
            Change::SelectedUntouched {
                connection,
                serial,
                expected,
                operation_expected,
                fence,
            } => {
                let id = OperationId {
                    connection: ConnectionId(*connection),
                    serial: *serial,
                };
                recovery_checks.verify_stopped_selected(self, &self.operation(id)?)?;
                self.reduce_boundary_rows(id, *expected, *operation_expected, *fence, 0, None, true)
            }
            Change::SelectedBoundary {
                connection,
                serial,
                expected,
                operation_expected,
                fence,
                kind,
                evidence,
            } => {
                let id = OperationId {
                    connection: ConnectionId(*connection),
                    serial: *serial,
                };
                let operation = self.operation(id)?;
                recovery_checks.verify_stopped_selected(self, &operation)?;
                self.reduce_boundary_rows(
                    id,
                    *expected,
                    *operation_expected,
                    *fence,
                    *kind,
                    Some(evidence),
                    true,
                )
            }
            Change::SelectedApplied {
                connection,
                serial,
                expected,
                fence,
                attempt,
                outcome,
            } => self.reduce_selected_applied(
                *connection,
                *serial,
                *expected,
                *fence,
                *attempt,
                *outcome,
                None,
                recovery_checks,
            ),
            Change::SelectedAppliedCarryV1 {
                connection,
                serial,
                expected,
                fence,
                attempt,
                outcome,
                carry,
            } => self.reduce_selected_applied(
                *connection,
                *serial,
                *expected,
                *fence,
                *attempt,
                *outcome,
                Some(carry),
                recovery_checks,
            ),
            Change::NativeOutcome {
                registration,
                expected,
                verified,
                reconstructed,
            } => self.reduce_native_outcome(
                registration,
                *expected,
                *verified,
                *reconstructed,
                recovery_checks,
            ),
            Change::IncompleteRecovery {
                connection,
                serial,
                slot,
                expected,
                fence,
            } => self.reduce_incomplete_recovery(
                RecoveryId {
                    operation: OperationId {
                        connection: ConnectionId(*connection),
                        serial: *serial,
                    },
                    slot: *slot,
                },
                *expected,
                *fence,
            ),
            Change::ReadReserve {
                connection,
                serial,
                expected,
                fence,
                previous,
                next,
                recovery_only,
                operation_expected,
            } => self.reduce_read_reserve(
                OperationId {
                    connection: ConnectionId(*connection),
                    serial: *serial,
                },
                *expected,
                *fence,
                previous,
                next,
                *recovery_only,
                *operation_expected,
            ),
            Change::NativeBound {
                fact,
                expected,
                fence,
            } => self.reduce_native_bound(fact, *expected, *fence),
            Change::NativeReserve {
                registration,
                expected,
            }
            | Change::NativePrepared {
                registration,
                expected,
            } => self.reduce_native_registration(
                registration,
                *expected,
                matches!(*change, Change::NativePrepared { .. }),
            ),
            Change::Published {
                connection,
                serial,
                expected,
                source,
                destination,
            }
            | Change::Verified {
                connection,
                serial,
                expected,
                source,
                destination,
            } => self.reduce_outcome(
                *connection,
                *serial,
                *expected,
                source,
                destination,
                matches!(*change, Change::Verified { .. }),
                recovery_checks,
            ),
            Change::BaselineCommit {
                directory_chain,
                directory_ancestors,
                existing_ancestors,
                connection,
                expected,
                root,
                count,
                source,
                destination,
                initial_review,
            } => self.reduce_baseline(
                *connection,
                *expected,
                *root,
                *count,
                source,
                destination,
                *initial_review,
                existing_ancestors.as_ref(),
                directory_ancestors.as_ref(),
                directory_chain.as_ref(),
                recovery_checks,
            ),
            Change::Create { record } => self.reduce_create(record),
            Change::Intent { operation } => self.reduce_intent(operation),
            Change::Reserve {
                entry,
                expected,
                fence,
            }
            | Change::Bytes {
                entry,
                expected,
                fence,
            } => self.reduce_recovery(
                entry,
                *expected,
                *fence,
                matches!(*change, Change::Bytes { .. }),
            ),
            Change::Fence {
                connection,
                expected,
                generation,
            }
            | Change::Disconnect {
                connection,
                expected,
                generation,
            } => {
                let mut record = self.connection(ConnectionId(*connection))?;
                if record.revision != *expected {
                    return Err(StoreError::StaleRevision);
                }
                if matches!(*change, Change::Fence { .. }) {
                    if record.lifecycle != 0 || *generation != format::next(record.fence)? {
                        return Err(StoreError::Disconnected);
                    }
                    record.lifecycle = 1;
                    record.fence = *generation;
                } else {
                    if record.lifecycle != 1
                        || record.fence != *generation
                        || record.pending.is_some()
                    {
                        return Err(StoreError::RecoveryRequired);
                    }
                    record.lifecycle = 2;
                }
                record.revision = format::next(record.revision)?;
                Ok(vec![Row::Connection(record)])
            }
        }
    }

    fn reduce_create(&self, record: &format::Connection) -> Result<Vec<Row>, StoreError> {
        record.decode()?;
        if record.revision != 1
            || record.serial != 0
            || record.pending.is_some()
            || record.baseline.is_some()
            || record.lifecycle != 0
            || self.connections >= 65536
        {
            return Err(StoreError::LimitExceeded);
        }
        if self.row(&format::key(0, record.id, 0))?.is_some() {
            return Err(StoreError::StaleRevision);
        }
        Ok(vec![Row::Connection(record.clone())])
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    fn reduce_intent(&self, operation: &format::Operation) -> Result<Vec<Row>, StoreError> {
        let receipt = operation.decode()?;
        if let Some(review) = &operation.initial_review {
            let mut selected = operation.clone();
            selected.initial_review = None;
            if operation.native_budget.is_none()
                || operation.baseline.is_some()
                || review.revision != operation.expected
                || review.preview == [0; 32]
                || review.selection != format::digest(&format::encode(&selected)?)
            {
                return Err(StoreError::Corrupt);
            }
        }
        let mut connection = self.connection(ConnectionId(operation.connection))?;
        if let Some(bytes) = &operation.native_identity {
            let fence = match operation.native_kind {
                Some(format::NativeOperationKind::DirectoryV1) => {
                    super::transaction::native::descriptor::directory::HistoricalAdmission::decode(
                        bytes,
                    )
                    .map_err(|_| StoreError::Corrupt)?
                    .fence
                }
                None => {
                    super::transaction::native::descriptor::Admission::decode(bytes)
                        .map_err(|_| StoreError::Corrupt)?
                        .fence
                }
            };
            if fence != connection.fence {
                return Err(StoreError::StaleRevision);
            }
        }
        if connection.lifecycle != 0 {
            return Err(StoreError::Disconnected);
        }
        if operation.source_binding != connection.source.identity
            || operation.destination_binding != connection.destination.identity
        {
            return Err(StoreError::IdentityChanged);
        }
        if operation.expected != connection.revision {
            return Err(StoreError::StaleRevision);
        }
        if connection.baseline != operation.baseline
            || operation.serial != format::next(connection.serial)?
            || operation.revision != format::next(connection.revision)?
        {
            return Err(StoreError::StaleRevision);
        }
        if operation.action.is_none() {
            for row in self.rows()? {
                let Row::Operation(previous) = row? else {
                    continue;
                };
                if previous.connection == operation.connection
                    && previous.path == operation.path
                    && previous.action.is_some()
                    && previous.native_capture.is_some()
                {
                    let conflict = self.selected_collision_join(&previous)?;
                    if !conflict.resolved {
                        let mut equality = operation.clone();
                        equality.stage = 3;
                        let mut accepted = false;
                        for applied in self.selected_attention(
                            ConnectionId(operation.connection),
                            &operation.path,
                        )? {
                            accepted |= self.joined_selected_equality_conflict_matches(
                                &equality, &applied, &conflict,
                            )?;
                        }
                        if !accepted {
                            return Err(StoreError::RecoveryRequired);
                        }
                    }
                }
            }
            for previous in
                self.selected_attention(ConnectionId(operation.connection), &operation.path)?
            {
                let Some(format::SelectedAction::Restore { selected, .. }) = previous.action else {
                    return Err(StoreError::Corrupt);
                };
                let content = match selected {
                    format::SelectedVersion::Recovery { content, .. } => Some(content),
                    format::SelectedVersion::Tombstone { .. } => None,
                };
                if operation.decision != 0
                    || operation.native_budget.is_none()
                    || operation.source != content
                    || operation.destination != content
                {
                    return Err(StoreError::RecoveryRequired);
                }
            }
        }
        if connection.pending.is_some() || self.active_intents >= 1024 {
            return Err(StoreError::RecoveryRequired);
        }
        if operation.stage != 0
            || !operation.recovery.is_empty()
            || operation
                .native_budget
                .as_ref()
                .is_some_and(|budget| budget != &format::NativeBudget::initial())
        {
            return Err(StoreError::Corrupt);
        }
        match receipt.intent.decision {
            Decision::VerifyEqual if operation.source != operation.destination => {
                return Err(StoreError::Corrupt);
            }
            Decision::Propagate { from: Side::Source } if operation.source.is_none() => {
                return Err(StoreError::Corrupt);
            }
            Decision::Propagate {
                from: Side::Destination,
            } if operation.destination.is_none() => return Err(StoreError::Corrupt),
            Decision::RecoverDeletion { at: Side::Source }
                if operation.source.is_none() || operation.destination.is_some() =>
            {
                return Err(StoreError::Corrupt);
            }
            Decision::RecoverDeletion {
                at: Side::Destination,
            } if operation.destination.is_none() || operation.source.is_some() => {
                return Err(StoreError::Corrupt);
            }
            _ => (),
        }
        if let Some(action) = &operation.action {
            let format::SelectedAction::Restore {
                fence,
                selected,
                conflict,
                ..
            } = action;
            if *fence != connection.fence {
                return Err(StoreError::StaleRevision);
            }
            self.selected_descriptor(operation, action)?;
            let selected_id = match selected {
                format::SelectedVersion::Recovery {
                    operation: serial,
                    slot,
                    ..
                } => Some(RecoveryId {
                    operation: OperationId {
                        connection: ConnectionId(operation.connection),
                        serial: *serial,
                    },
                    slot: *slot,
                }),
                format::SelectedVersion::Tombstone {
                    operation: serial, ..
                } => {
                    if !matches!(
                        self.operation(OperationId {
                            connection: ConnectionId(operation.connection),
                            serial: *serial
                        })?
                        .stage,
                        8 | 10
                    ) {
                        return Err(StoreError::RecoveryRequired);
                    }
                    None
                }
            };
            if let Some(selection) = conflict {
                let Some(Row::Conflict(captured)) =
                    self.row(&format::key(3, operation.connection, selection.serial))?
                else {
                    return Err(StoreError::RecoveryRequired);
                };
                if captured.resolved
                    || captured.revision != selection.revision
                    || captured.path != operation.path
                    || selected_id
                        .map(|id| self.conflict_contains_selection(&captured, id))
                        .transpose()?
                        .is_some_and(|valid| !valid)
                {
                    return Err(StoreError::StaleRevision);
                }
            }
        }
        if operation.native_budget.is_some()
            && matches!(operation.decision, 3 | 4)
            && connection.baseline.is_none()
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.install_chain_intent(&mut connection, operation)?;
        connection.pending = Some(operation.serial);
        connection.serial = operation.serial;
        connection.revision = operation.revision;
        let mut rows = vec![
            Row::Connection(connection),
            Row::Operation(operation.clone()),
        ];
        if operation.action.is_none() && operation.decision >= 10 {
            rows.push(Row::Conflict(format::ConflictData {
                native_capture: None,
                connection: operation.connection,
                serial: operation.serial,
                revision: operation.revision,
                kind: operation.decision - 10,
                path: operation.path.clone(),
                source: operation.source.clone(),
                destination: operation.destination.clone(),
                preserved: Vec::new(),
                resolved: false,
            }));
        }
        Ok(rows)
    }
    /// Exact encoded framing and real log length, shared with future-phase preflight.
    fn check_event_capacity(&self, event: &Event, encoded: usize) -> Result<(), StoreError> {
        let framed = u64::try_from(encoded)
            .map_err(|_| StoreError::LimitExceeded)?
            .checked_add(44)
            .ok_or(StoreError::LimitExceeded)?;
        if matches!(event.change, Change::Fence { .. }) && framed > 512 {
            return Err(StoreError::LimitExceeded);
        }
        let reviewed_control = match &event.change {
            Change::InitialReviewControl { control }
            | Change::ReviewedChainControl { control, .. } => {
                control.validate()?;
                if let Change::ReviewedChainControl { inner, .. } = &event.change {
                    if inner.connection != control.connection {
                        return Err(StoreError::Corrupt);
                    }
                    inner.anchor.operation()?;
                }
                true
            }
            Change::ReviewedOrdinaryStopBoundary { boundary } => {
                boundary.validate()?;
                true
            }
            _ => false,
        };
        if reviewed_control && framed > 4096 {
            return Err(StoreError::LimitExceeded);
        }
        let control = reviewed_control
            || matches!(
                event.change,
                Change::Fence { .. }
                    | Change::Disconnect { .. }
                    | Change::DirectoryChainControl { .. }
            );
        let bound = if control {
            16 * 1024 * 1024
        } else {
            15 * 1024 * 1024
        };
        if self
            .log
            .metadata()?
            .len()
            .checked_add(framed)
            .is_none_or(|length| length > bound)
        {
            return Err(StoreError::LimitExceeded);
        }
        Ok(())
    }
    fn prepare_rows(&self, rows: Vec<Row>) -> Result<PreparedRows, StoreError> {
        // Exact replacement deltas, including rows reconstructed by tiny events.
        // No state or persistence mutation is allowed during admission.
        let mut bytes = self.overlay_bytes;
        let mut keys = self.overlay.len();
        let mut seen = std::collections::BTreeSet::new();
        for row in &rows {
            if !seen.insert(row.key()) {
                return Err(StoreError::Corrupt);
            }
            if let Some(old) = self.overlay.get(&row.key()) {
                bytes = bytes
                    .checked_sub(format::encode(old)?.len())
                    .ok_or(StoreError::Corrupt)?;
            } else {
                keys = keys.checked_add(1).ok_or(StoreError::LimitExceeded)?;
            }
            bytes = bytes
                .checked_add(format::encode(row)?.len())
                .ok_or(StoreError::LimitExceeded)?;
        }
        let mut connections = self.connections;
        let mut active = self.active_intents;
        let mut live = self.live_keys;
        for row in &rows {
            let old = self.row(&row.key())?;
            if old.is_none() {
                live = format::next(live)?;
            }
            if let Row::Connection(connection) = row {
                if old.is_none() {
                    connections = format::next(connections)?;
                }
                let old_pending = matches!(old,Some(Row::Connection(c)) if c.pending.is_some());
                if connection.pending.is_some() && !old_pending {
                    active = format::next(active)?;
                }
                if connection.pending.is_none() && old_pending {
                    active = active.checked_sub(1).ok_or(StoreError::Corrupt)?;
                }
            }
        }
        if connections > 65536
            || active > 1024
            || live.checked_sub(connections).ok_or(StoreError::Corrupt)? > 1_048_576
        {
            return Err(StoreError::LimitExceeded);
        }
        Ok(PreparedRows {
            rows,
            overlay_bytes: bytes,
            overlay_keys: keys,
            connections,
            active_intents: active,
            live_keys: live,
        })
    }
    fn commit_rows(&mut self, prepared: PreparedRows) {
        for row in prepared.rows {
            if let Row::Connection(record) = &row {
                if let Some(gate) = self.native_gates.get(&record.id) {
                    gate.set_progress(record.revision);
                }
            }
            self.overlay.insert(row.key(), row);
        }
        self.connections = prepared.connections;
        self.active_intents = prepared.active_intents;
        self.live_keys = prepared.live_keys;
        self.overlay_bytes = prepared.overlay_bytes;
    }
    fn native_read_context<'a>(
        &self,
        live: &LiveOperation,
        deadline: std::time::Instant,
        cancel: &'a AtomicBool,
    ) -> Result<NativeReadContext<'a>, StoreError> {
        self.check_native(live)?;
        if self.operation(live.id)?.native_budget.is_none() {
            return Err(StoreError::RecoveryRequired);
        }
        let gate = self
            .native_gates
            .get(&live.id.connection.0)
            .ok_or(StoreError::StaleRevision)?;
        let context = NativeReadContext {
            id: live.id,
            reads: Arc::clone(&live.reads),
            lifetime: live.lifetime.clone(),
            gate: Arc::clone(gate),
            deadline: deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(30)),
            cancel,
            inspection_only: false,
            recovery_only: false,
        };
        context.check()?;
        Ok(context)
    }
    fn recovery_read_context<'a>(
        &self,
        recovery: &RecoveryContext,
        deadline: std::time::Instant,
        cancel: &'a AtomicBool,
    ) -> Result<NativeReadContext<'a>, StoreError> {
        self.check_recovery_context(recovery)?;
        let gate = self
            .native_gates
            .get(&recovery.id.connection.0)
            .ok_or(StoreError::StaleRevision)?;
        let context = NativeReadContext {
            id: recovery.id,
            reads: Arc::clone(&recovery.reads),
            lifetime: recovery.lifetime.clone(),
            gate: Arc::clone(gate),
            deadline: deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(30)),
            cancel,
            inspection_only: true,
            recovery_only: recovery.recovery_only,
        };
        context.check()?;
        Ok(context)
    }
    pub(super) fn check_final_context(&self, recovery: &RecoveryContext) -> Result<(), StoreError> {
        if self.final_context_ready(recovery)? {
            Ok(())
        } else {
            Err(StoreError::RecoveryRequired)
        }
    }
    /// False is a valid current completion blocked by durable attention. Broken
    /// admission remains an error; callers must never turn it into mere pending.
    pub(super) fn final_context_ready(
        &self,
        recovery: &RecoveryContext,
    ) -> Result<bool, StoreError> {
        self.check_recovery_context(recovery)?;
        let connection = self.connection(recovery.id.connection)?;
        if connection.lifecycle != 0
            || connection.pending.is_some()
            || connection.baseline.is_none()
            || self
                .native_gates
                .get(&connection.id)
                .is_none_or(|gate| gate.stopped().load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(StoreError::RecoveryRequired);
        }
        for row in self.rows()? {
            match row? {
                Row::Conflict(conflict)
                    if conflict.connection == connection.id && !conflict.resolved =>
                {
                    return Ok(false);
                }
                Row::Operation(operation)
                    if operation.connection == connection.id
                        && operation
                            .selected_applied
                            .as_ref()
                            .is_some_and(|fact| fact.attention) =>
                {
                    return Ok(false);
                }
                _ => {}
            }
        }
        Ok(true)
    }
    pub(super) fn load_recovery_baseline(
        &self,
        recovery: &RecoveryContext,
        deadline: std::time::Instant,
        cancel: &AtomicBool,
    ) -> Result<VerifiedBaseline, StoreError> {
        let context = self.recovery_read_context(recovery, deadline, cancel)?;
        let connection = self.connection(recovery.id.connection)?;
        let revision = connection.baseline.ok_or(StoreError::RecoveryRequired)?;
        self.read_baseline_with_context(
            &connection,
            connection.baseline_root,
            connection.baseline_count,
            revision,
            Some(&context),
        )
    }
    fn verification_operation(&self, change: &Change) -> Result<Option<OperationId>, StoreError> {
        Ok(match change {
            Change::NativeSelectedCollision { snapshot, .. } => {
                let capture =
                    super::transaction::native::descriptor::ConflictSnapshot::decode(snapshot)
                        .map_err(|_| StoreError::Corrupt)?;
                Some(OperationId {
                    connection: ConnectionId(capture.connection),
                    serial: capture.operation,
                })
            }
            Change::SelectedUntouched {
                connection, serial, ..
            }
            | Change::SelectedBoundary {
                connection, serial, ..
            }
            | Change::SelectedCollisionComplete {
                connection, serial, ..
            }
            | Change::SelectedApplied {
                connection, serial, ..
            }
            | Change::SelectedAppliedCarryV1 {
                connection, serial, ..
            }
            | Change::NativeConflictComplete {
                connection, serial, ..
            }
            | Change::Published {
                connection, serial, ..
            }
            | Change::Verified {
                connection, serial, ..
            } => Some(OperationId {
                connection: ConnectionId(*connection),
                serial: *serial,
            }),
            Change::NativeOutcome { registration, .. } => Some(OperationId {
                connection: ConnectionId(registration.connection),
                serial: registration.serial,
            }),
            Change::BaselineCommit { connection, .. } => Some(OperationId {
                connection: ConnectionId(*connection),
                serial: self
                    .connection(ConnectionId(*connection))?
                    .pending
                    .ok_or(StoreError::RecoveryRequired)?,
            }),
            Change::Bytes { entry, .. }
            | Change::SelectedCaptureDrain { entry, .. }
            | Change::SelectedDrain { entry, .. } => Some(entry.id().operation),
            Change::NativeDrain { entry, .. } | Change::NativeCaptureDrain { entry, .. }
                if entry.state == 1 =>
            {
                Some(entry.id().operation)
            }
            Change::NativeBound { fact, .. } => Some(OperationId {
                connection: ConnectionId(fact.connection),
                serial: fact.serial,
            }),
            _ => None,
        })
    }
    // Only from_storage calls this; replay can validate history but never lends
    // its mode to a live append, reader or native proof constructor.
    fn replay_checks(&self, change: &Change) -> Result<RecoveryChecks<'static>, StoreError> {
        let mode = match self.verification_operation(change)? {
            Some(id) if self.operation(id)?.native_budget.is_some() => {
                VerificationMode::NativeReplay(id)
            }
            _ => VerificationMode::Legacy,
        };
        Ok(RecoveryChecks {
            mode,
            ..RecoveryChecks::default()
        })
    }
    fn validate_verification_mode(
        &self,
        change: &Change,
        checks: &RecoveryChecks<'_>,
        live_append: bool,
    ) -> Result<(), StoreError> {
        if live_append {
            self.validate_live_chain_change(change)?;
        }
        let Some(id) = self.verification_operation(change)? else {
            return Ok(());
        };
        let marked = self.operation(id)?.native_budget.is_some();
        match (&checks.mode, marked) {
            (VerificationMode::Legacy, false) => Ok(()),
            (VerificationMode::NativeReplay(replayed), true) if !live_append && *replayed == id => {
                Ok(())
            }
            (VerificationMode::NativeLive(context), true)
                if (!context.inspection_only
                    || (context.recovery_only
                        && (matches!(
                            change,
                            Change::SelectedCaptureDrain { .. }
                                | Change::SelectedUntouched { .. }
                                | Change::SelectedBoundary { .. }
                                | Change::SelectedDrain { .. }
                        ) || matches!(change, Change::NativeDrain { entry, .. } | Change::NativeCaptureDrain { entry, .. } if entry.state == 1))))
                    && context.id == id
                    && context
                        .lifetime
                        .ptr_eq(&Arc::downgrade(&self.native_lifetime)) =>
            {
                context.check()
            }
            _ => Err(StoreError::RecoveryRequired),
        }
    }
    fn append_native(
        &mut self,
        change: Change,
        context: &NativeReadContext<'_>,
    ) -> Result<(), StoreError> {
        self.append_checked(
            change,
            RecoveryChecks {
                mode: VerificationMode::NativeLive(context),
                ..RecoveryChecks::default()
            },
        )
    }
    fn append(&mut self, change: Change) -> Result<(), StoreError> {
        self.append_checked(change, RecoveryChecks::default())
    }
    fn append_checked(
        &mut self,
        change: Change,
        recovery_checks: RecoveryChecks<'_>,
    ) -> Result<(), StoreError> {
        self.append_checked_with_baseline(change, recovery_checks, BaselineAppendCheck::none())
    }
    fn append_checked_with_baseline(
        &mut self,
        change: Change,
        mut recovery_checks: RecoveryChecks<'_>,
        mut baseline: BaselineAppendCheck<'_, '_>,
    ) -> Result<(), StoreError> {
        let _read_lease = self.storage.read_lease(false)?;
        let live_append = baseline.live_append();
        #[cfg(test)]
        let consent = matches!(&baseline, BaselineAppendCheck::InitialReview(_));
        #[cfg(not(test))]
        let consent = false;
        if matches!(change, Change::InitialReviewAccepted { .. }) && live_append && !consent {
            return Err(StoreError::RecoveryRequired);
        }
        if matches!(change, Change::InitialEmptyBaseline { .. }) && !baseline.initial_empty() {
            return Err(StoreError::RecoveryRequired);
        }
        // An implicit checkpoint may collect pages before this event is durable.
        self.candidate_pin = match &change {
            Change::InitialReviewAccepted { review, .. } => Some(review.root),
            Change::BaselineCommit { root, .. } => *root,
            _ => None,
        };
        let mut touched = false;
        let result = (|| {
            self.validate()?;
            let event = Event {
                schema: 1,
                predecessor: self.digest,
                change,
            };
            // Keep actual byte guards through checkpoint, refreshed reduction,
            // append, flush and in-memory commit. At most two reductions each
            // verify the operation's bounded (128) recovery slots.
            if consent {
                baseline.validate(self, &event.change)?;
            }
            self.validate_verification_mode(
                &event.change,
                &recovery_checks,
                live_append && !consent,
            )?;
            let mut prepared = self.prepare_rows(self.reduce(&event, &mut recovery_checks)?)?;
            let bytes = format::encode(&event)?;
            if matches!(event.change, Change::Fence { .. }) && bytes.len() + 44 > 512 {
                return Err(StoreError::LimitExceeded);
            }
            if !prepared.fits_overlay() || self.log.metadata()?.len() > 8 * 1024 * 1024 {
                // Drop the prospective rows before page construction; checkpoint
                // can also update retention fields read by the reducer.
                drop(prepared);
                self.checkpoint()?;
                if consent {
                    baseline.validate(self, &event.change)?;
                }
                self.validate_verification_mode(
                    &event.change,
                    &recovery_checks,
                    live_append && !consent,
                )?;
                prepared = self.prepare_rows(self.reduce(&event, &mut recovery_checks)?)?;
            }
            if !prepared.fits_overlay() {
                return Err(StoreError::LimitExceeded);
            }
            self.check_event_capacity(&event, bytes.len())?;
            self.validate()?;
            if consent {
                baseline.validate(self, &event.change)?;
            }
            self.validate_verification_mode(
                &event.change,
                &recovery_checks,
                live_append && !consent,
            )?;
            let committed_sequence = format::next(self.sequence)?;
            let mut sequence = committed_sequence;
            // All page/recovery rereads and any checkpoint/second reduction are
            // finished. A read-budget context alone is not endpoint authority.
            #[cfg(all(test, target_os = "linux"))]
            baseline.before_write_for_test();
            baseline.validate(self, &event.change)?;
            touched = true;
            framing::write_record(&mut self.log, &mut sequence, &event)?;
            storage::sync_file(&self.log)?;
            self.log_identity = storage::regular(&self.log)?;
            self.validate()?;
            if consent {
                baseline.validate(self, &event.change)?;
            }
            self.validate_verification_mode(
                &event.change,
                &recovery_checks,
                live_append && !consent,
            )?;
            // Keep the actual native proof and Windows recovery readers alive
            // through fsync. A failure here poisons the owner: reopen may show
            // the already durable event; returning Err cannot roll it back.
            baseline.validate(self, &event.change)?;
            self.commit_rows(prepared);
            self.sequence = committed_sequence;
            self.digest = format::digest(&bytes);
            if matches!(
                event.change,
                Change::BaselineCommit { .. }
                    | Change::SelectedApplied { .. }
                    | Change::SelectedAppliedCarryV1 { .. }
                    | Change::SelectedCollisionComplete { .. }
            ) {
                self.retention_sample = std::time::Instant::now();
            }
            Ok(())
        })();
        self.candidate_pin = None;
        if (touched && result.is_err())
            || matches!(
                result,
                Err(StoreError::Persistence(_)
                    | StoreError::IdentityChanged
                    | StoreError::RecoverySpace)
            )
        {
            self.poison_native();
        }
        result
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Checkpoint selection, native replay validation, pins and durable root publication must remain ordered."
    )]
    pub(super) fn checkpoint(&mut self) -> Result<DurableAck<u64>, StoreError> {
        let _read_lease = self.storage.read_lease(false)?;
        let result = (|| {
            self.validate()?;
            self.validate_chain_history(&[])?;
            self.validate_review_history(&[])?;
            let sampled = std::time::Instant::now();
            let mut selected_pins = std::collections::BTreeSet::new();
            for row in self.rows()? {
                let Row::Operation(operation) = row? else {
                    continue;
                };
                let selected_collision = operation.action.is_some()
                    && operation.native_capture.is_some()
                    && !self.selected_collision_join(&operation)?.resolved;
                if !(selected_collision
                    || operation.action.is_some() && operation.native_boundary.is_some()
                    || operation
                        .selected_applied
                        .as_ref()
                        .is_some_and(|fact| fact.attention))
                {
                    continue;
                }
                let Some(format::SelectedAction::Restore { selected, .. }) = operation.action
                else {
                    return Err(StoreError::Corrupt);
                };
                if let format::SelectedVersion::Recovery {
                    operation: selected,
                    slot,
                    ..
                } = selected
                {
                    selected_pins.insert(recovery::recovery_key(RecoveryId {
                        operation: OperationId {
                            connection: ConnectionId(operation.connection),
                            serial: selected,
                        },
                        slot,
                    }));
                }
                for slot in operation.recovery {
                    selected_pins.insert(recovery::recovery_key(RecoveryId {
                        operation: OperationId {
                            connection: ConnectionId(operation.connection),
                            serial: operation.serial,
                        },
                        slot,
                    }));
                }
                if selected_pins.len() > 1_048_576 {
                    return Err(StoreError::LimitExceeded);
                }
            }
            let rows = self.rows()?.map(|result| {
                let mut row = result?;
                if let Row::Recovery(entry) = &mut row {
                    let connection = self.connection(ConnectionId(entry.connection))?;
                    let mut value = entry.decode()?;
                    recovery::credit(
                        &mut value,
                        self.retention_sample,
                        sampled,
                        connection.decode()?.lifecycle,
                        connection.pending.is_some()
                            || entry.reason == 2
                            || selected_pins.contains(&entry.key()),
                    );
                    entry.retained = value.retained_seconds;
                }
                Ok(row)
            });
            let root = storage::build_pages(&self.storage, rows)?;
            let generation = format::next(self.current.generation)?;
            let mut next = self.current.clone();
            next.generation = generation;
            next.root = root;
            next.sequence = self.sequence;
            next.digest = self.digest;
            next.log = generation;
            let mut log =
                storage::open_file(&self.storage.root, &format!("log-{generation}"), true)
                    .map_err(|error| match error {
                        StoreError::Persistence(ref io)
                            if io.kind() == std::io::ErrorKind::AlreadyExists =>
                        {
                            StoreError::RecoveryRequired
                        }
                        other => other,
                    })?;
            log.write_all(b"QOSCON1\n")?;
            let header = format::LogHeader {
                schema: 1,
                identity: self.storage.identity.clone(),
                generation,
                first_sequence: format::next(self.sequence)?,
                predecessor: self.digest,
            };
            format::encode(&header)?;
            framing::write_record(&mut log, &mut 0, &header)?;
            let log_identity = storage::regular(&log)?;
            storage::sync_file(&log)?;
            storage::sync_directory(&self.storage.root)?;
            let current_identity = self.publish_current(&next, &log, &log_identity)?;
            // Adopt only the identities validated through publication, never a
            // fresh snapshot that could hide a change during persistence.
            self.current_identity = current_identity;
            self.log_identity = log_identity;
            self.log = log;
            self.current = next;
            self.overlay.clear();
            self.overlay_bytes = 0;
            self.retention_sample = sampled;
            self.gc_step()?;
            Ok(self.ack(generation))
        })();
        if result.is_err() {
            self.poison_native();
        }
        result
    }
}

use std::sync::atomic::AtomicBool;
pub(super) struct RecoveryReservation {
    id: RecoveryId,
    session: Arc<()>,
    fence: u64,
}
pub(super) struct PreservedObject {
    id: RecoveryId,
    file: storage::RecoveryReader,
}
/// Owning selected private-artifact reader. Only the locked store can construct
/// it after paying for a fresh read under the selected operation's budget.
pub(super) struct VerifiedRecoveryRead<'a> {
    failed: bool,
    id: OperationId,
    revision: u64,
    fence: u64,
    selected: format::Recovery,
    parent: Arc<storage::Directory>,
    file: storage::RecoveryReader,
    identity: storage::Metadata,
    reads: NativeReadContext<'a>,
}
/// Current selected-source correlation. Tombstones retain checked historical
/// metadata; only separate paid native guards establish current endpoint authority.
pub(super) enum VerifiedSelection<'a> {
    Recovery(VerifiedRecoveryRead<'a>),
    Tombstone(VerifiedTombstoneRead<'a>),
}
pub(super) struct VerifiedTombstoneRead<'a> {
    failed: bool,
    revision: u64,
    fence: u64,
    action: format::SelectedAction,
    reads: NativeReadContext<'a>,
}
struct NativeReadContext<'a> {
    id: OperationId,
    reads: Arc<super::transaction::native::OperationReads>,
    lifetime: std::sync::Weak<()>,
    gate: Arc<super::transaction::AdmissionGate>,
    deadline: std::time::Instant,
    cancel: &'a AtomicBool,
    inspection_only: bool,
    recovery_only: bool,
}
impl NativeReadContext<'_> {
    fn check(&self) -> Result<(), StoreError> {
        if self.lifetime.strong_count() == 0 {
            return Err(StoreError::StaleRevision);
        }
        if self.cancel.load(std::sync::atomic::Ordering::Acquire)
            || (!self.recovery_only
                && self
                    .gate
                    .stopped()
                    .load(std::sync::atomic::Ordering::Acquire))
            || std::time::Instant::now() >= self.deadline
        {
            return Err(StoreError::Cancelled);
        }
        Ok(())
    }
    fn charge(&self, count: u64) -> Result<(), StoreError> {
        self.check()?;
        self.reads
            .charge(count)
            .map_err(|()| StoreError::LimitExceeded)
    }
}
#[derive(Default)]
enum VerificationMode<'a> {
    #[default]
    Legacy,
    NativeLive(&'a NativeReadContext<'a>),
    NativeReplay(OperationId),
}
// The host-only variant probes shared append ordering, never native validity.
enum BaselineAppendCheck<'a, 'proof> {
    #[cfg(test)]
    InitialReview(&'a super::transaction::ReviewedInitialSet),
    None(std::marker::PhantomData<&'a &'proof ()>),
    #[cfg(target_os = "linux")]
    InitialEmpty(&'a mut super::transaction::InitialEmptyProof<'proof>),
    #[cfg(target_os = "linux")]
    Native {
        live: &'a LiveOperation,
        comparison: &'a mut super::transaction::native::VerifiedComparison<'proof>,
        cancel: &'a AtomicBool,
    },
    #[cfg(target_os = "linux")]
    Directory {
        live: &'a LiveOperation,
        comparison: &'a mut super::transaction::native::VerifiedDirectoryComparison<'proof>,
        cancel: &'a AtomicBool,
    },
    #[cfg(test)]
    Host(&'a tests::BaselineAppendProbe),
    #[cfg(test)]
    Historical(&'a chain_tests::HistoricalAppendProbe),
}
impl BaselineAppendCheck<'_, '_> {
    fn initial_empty(&self) -> bool {
        #[cfg(target_os = "linux")]
        if matches!(self, Self::InitialEmpty(_)) {
            return true;
        }
        false
    }
    fn live_append(&self) -> bool {
        #[cfg(test)]
        if matches!(self, Self::Historical(_)) {
            return false;
        }
        true
    }
    fn none() -> Self {
        Self::None(std::marker::PhantomData)
    }
    #[cfg(all(test, target_os = "linux"))]
    fn before_write_for_test(&mut self) {
        if let Self::InitialEmpty(proof) = self {
            BEFORE_NATIVE_BASELINE_WRITE.with(|slot| {
                if let Some(hook) = slot.borrow_mut().take() {
                    (hook.action)();
                    if hook.revoke {
                        proof.revoke_for_test();
                    }
                }
            });
        }
        if let Self::Directory { comparison, .. } = self {
            BEFORE_NATIVE_BASELINE_WRITE.with(|slot| {
                if let Some(hook) = slot.borrow_mut().take() {
                    (hook.action)();
                    if hook.revoke {
                        comparison.revoke_for_test();
                    }
                }
            });
        }
        if let Self::Native { comparison, .. } = self {
            BEFORE_NATIVE_BASELINE_WRITE.with(|slot| {
                if let Some(hook) = slot.borrow_mut().take() {
                    (hook.action)();
                    if hook.revoke {
                        comparison.revoke_for_test();
                    }
                }
            });
        }
    }
    #[cfg_attr(
        not(any(test, target_os = "linux")),
        expect(
            clippy::unnecessary_wraps,
            clippy::needless_pass_by_ref_mut,
            reason = "The shared append interface is fallible and mutates owning native proof on Linux; non-Linux production has only its legacy no-op variant."
        )
    )]
    fn validate(&mut self, store: &ConnectionStore, change: &Change) -> Result<(), StoreError> {
        // Non-Linux production builds have only the no-op legacy variant.
        let _ = (store, change);
        match self {
            #[cfg(test)]
            Self::InitialReview(owner) => {
                let Change::InitialReviewAccepted { review, .. } = change else {
                    return Err(StoreError::StaleRevision);
                };
                owner.validate(store)?;
                if review.original.as_slice() != owner.original()
                    || review.preview != owner.preview()
                    || review.connection != owner.record().id.0
                {
                    return Err(StoreError::StaleRevision);
                }
                Ok(())
            }
            Self::None(_) => Ok(()),
            #[cfg(target_os = "linux")]
            Self::InitialEmpty(proof) => {
                let Change::InitialEmptyBaseline { fact } = change else {
                    return Err(StoreError::StaleRevision);
                };
                if proof.fact(store)? != *fact {
                    return Err(StoreError::StaleRevision);
                }
                Ok(())
            }
            #[cfg(test)]
            Self::Historical(probe) => probe.validate(store),
            #[cfg(target_os = "linux")]
            Self::Native {
                live,
                comparison,
                cancel,
            } => {
                if live.native_kind.is_some()
                    || !matches!(change, Change::BaselineCommit { connection, expected, existing_ancestors, directory_ancestors:None, .. }
                    if *connection == live.id.connection.0 && *expected == live.revision
                        && existing_ancestors.as_ref() == comparison.existing_ancestors())
                {
                    return Err(StoreError::StaleRevision);
                }
                store.check_native(live)?;
                comparison.validate(store, live, cancel)
            }
            #[cfg(target_os = "linux")]
            Self::Directory {
                live,
                comparison,
                cancel,
            } => {
                if live.native_kind != Some(NativeOperationKind::DirectoryV1)
                    || !matches!(change, Change::BaselineCommit {connection,expected,directory_ancestors,existing_ancestors:None,source,destination,initial_review:false,..}
                        if *connection == live.id.connection.0 && *expected == live.revision
                            && directory_ancestors.as_ref() == comparison.receipt()
                            && *source == storage::Identity::from(live.endpoints[0])
                            && *destination == storage::Identity::from(live.endpoints[1]))
                {
                    return Err(StoreError::StaleRevision);
                }
                store.check_native(live)?;
                comparison.validate(store, live, cancel)
            }
            #[cfg(test)]
            Self::Host(probe) => {
                if !matches!(change, Change::BaselineCommit { .. }) {
                    return Err(StoreError::RecoveryRequired);
                }
                probe.validate(store)
            }
        }
    }
}
#[derive(Default)]
struct RecoveryChecks<'a> {
    mode: VerificationMode<'a>,
    #[cfg(windows)]
    readers: Vec<storage::RecoveryReader>,
}

impl ConnectionStore {
    fn gc_step(&mut self) -> Result<usize, StoreError> {
        self.guard()?;
        let result = self.collect_garbage();
        if result.is_err() {
            self.poison_native();
        }
        result
    }
}

impl<T> DurableAck<T> {
    pub(super) fn value(&self) -> &T {
        &self.value
    }
}

#[cfg(windows)]
pub(super) use format::WindowsFileId;

#[cfg(windows)]
pub(super) use storage::{CurrentReplacement, GcDeletion};

#[cfg(target_os = "linux")]
pub(super) struct PreparedNative {
    registration: format::NativeRegistration,
    lifetime: std::sync::Weak<()>,
    revision: u64,
}
#[cfg(target_os = "linux")]
impl PreparedNative {
    pub(super) fn validate(
        &self,
        live: &LiveOperation,
        header: &super::transaction::native::descriptor::Header,
    ) -> Result<(), StoreError> {
        if !self.lifetime.ptr_eq(live.lifetime())
            || self.lifetime.strong_count() == 0
            || self.revision != live.revision
            || self.registration.connection != live.id.connection.0
            || self.registration.serial != live.id.serial
            || u64::from(self.registration.attempt) != header.attempt
            || self.registration.nonce != header.nonce
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }
    pub(super) fn descriptors(&self) -> (&[u8], Option<&[u8]>) {
        (
            self.registration.intent.as_deref().unwrap_or(&[]),
            self.registration.stage.as_deref(),
        )
    }
}
#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(super) fn recovered_prepared(
        &self,
        live: &LiveOperation,
        proof: &mut super::transaction::native::RecoveredEvidence<'_>,
    ) -> Result<DurableAck<PreparedNative>, StoreError> {
        self.check_native(live)?;
        proof.validate_live(live)?;
        let (attempt, intent, stage) = proof.prepared_facts()?;
        let mut key = format::key(5, live.id.connection.0, live.id.serial);
        key.extend(attempt.to_be_bytes());
        let Some(Row::NativeRegistration(registration)) = self.row(&key)? else {
            return Err(StoreError::Corrupt);
        };
        if registration.intent.as_ref() != Some(&intent)
            || registration.stage.as_ref() != Some(&stage)
            || registration.outcome.is_some()
            || self.operation(live.id)?.stage > 1
        {
            return Err(StoreError::RecoveryRequired);
        }
        proof.validate_live(live)?;
        Ok(self.ack(PreparedNative {
            registration,
            lifetime: live.lifetime.clone(),
            revision: live.revision,
        }))
    }
    pub(super) fn recovered_deletion_prepared(
        &self,
        live: &LiveOperation,
        proof: &mut super::transaction::native::RecoveredDeletion<'_>,
    ) -> Result<DurableAck<PreparedNative>, StoreError> {
        self.check_native(live)?;
        proof.validate_live(live)?;
        let (attempt, intent) = proof.prepared_facts()?;
        let mut key = format::key(5, live.id.connection.0, live.id.serial);
        key.extend(attempt.to_be_bytes());
        let Some(Row::NativeRegistration(registration)) = self.row(&key)? else {
            return Err(StoreError::Corrupt);
        };
        if registration.intent.as_ref() != Some(&intent)
            || registration.stage.is_some()
            || registration.outcome.is_some()
            || self.operation(live.id)?.stage > 1
        {
            return Err(StoreError::RecoveryRequired);
        }
        proof.validate_live(live)?;
        Ok(self.ack(PreparedNative {
            registration,
            lifetime: live.lifetime.clone(),
            revision: live.revision,
        }))
    }
    pub(super) fn native_ancestor_patch(
        &self,
        live: &LiveOperation,
        deadline: std::time::Instant,
        cancel: &AtomicBool,
    ) -> Result<Option<ExistingAncestorPatchV1>, StoreError> {
        self.check_native(live)?;
        let context = self.native_read_context(live, deadline, cancel)?;
        let connection = self.connection(live.id.connection)?;
        let entries = if let Some(revision) = connection.baseline {
            self.read_baseline_with_context(
                &connection,
                connection.baseline_root,
                connection.baseline_count,
                revision,
                Some(&context),
            )?
            .entries()
            .cloned()
            .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        recovery::ancestor_patch(&self.operation(live.id)?, &entries)
    }
    pub(super) fn prepare_baseline(
        &mut self,
        live: &LiveOperation,
        comparison: &mut super::transaction::native::VerifiedComparison<'_>,
        cancel: &AtomicBool,
    ) -> Result<BaselineCandidate, StoreError> {
        self.guard()?;
        self.check_native(live)?;
        let operation = self.operation(live.id)?;
        if operation.stage != 3 {
            return Err(StoreError::RecoveryRequired);
        }
        comparison.validate(self, live, cancel)?;
        let context = self.native_read_context(live, comparison.deadline(), cancel)?;
        let connection = self.connection(live.id.connection)?;
        if connection.baseline.is_none() && operation.initial_review.is_none() {
            return Err(StoreError::RecoveryRequired);
        }
        let selected = self.selected_attention(live.id.connection, &operation.path)?;
        for row in self.rows()? {
            if let Row::Conflict(conflict) = row?
                && conflict.connection == connection.id
                && !conflict.resolved
                && conflict.path == operation.path
            {
                self.attention_conflict_origin(&conflict)?;
                let mut accepted = false;
                for applied in &selected {
                    accepted |= self.joined_selected_equality_conflict_matches(
                        &operation, applied, &conflict,
                    )?;
                }
                if !accepted {
                    return Err(StoreError::RecoveryRequired);
                }
            }
        }
        let mut entries = if let Some(revision) = connection.baseline {
            self.read_baseline_with_context(
                &connection,
                connection.baseline_root,
                connection.baseline_count,
                revision,
                Some(&context),
            )?
            .entries()
            .cloned()
            .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        let existing_ancestors = recovery::ancestor_patch(&operation, &entries)?;
        if existing_ancestors.as_ref() != comparison.existing_ancestors() {
            return Err(StoreError::RecoveryRequired);
        }
        let requirement = super::transaction::ExistingAncestorRequirement::from_baseline(
            &live.intent.path,
            &entries,
        )?;
        for path in requirement.paths() {
            entries.insert(path, EntryState::Directory);
        }
        if comparison.value() != self.native_baseline_value(&operation)?.as_ref() {
            return Err(StoreError::RecoveryRequired);
        }
        if let Some(value) = comparison.value() {
            entries.insert(live.intent.path.clone(), value.clone());
        } else {
            entries.remove(&live.intent.path);
        }
        if entries.len() > 4096 {
            return Err(StoreError::LimitExceeded);
        }
        let count = u32::try_from(entries.len()).map_err(|_| StoreError::LimitExceeded)?;
        live.reads
            .require_credit(baseline_phase_estimate(connection.baseline_count, count)?)
            .map_err(|()| StoreError::LimitExceeded)?;
        let revision = format::next(live.revision)?;
        let mut entries: Vec<_> = entries.into_iter().collect();
        entries.sort_by_key(|(path, _)| path.logical());
        let rows = entries
            .into_iter()
            .enumerate()
            .map(|(ordinal, (path, state))| {
                Ok(Row::Baseline(format::BaselineEntry {
                    connection: connection.id,
                    revision,
                    ordinal: u32::try_from(ordinal).map_err(|_| StoreError::LimitExceeded)?,
                    path: path.into(),
                    state: state.into(),
                }))
            });
        context.check()?;
        let root = storage::build_pages_with(
            &self.storage,
            rows,
            Some(&|count| context.charge(count as u64)),
        )?;
        self.candidate_pin = root;
        self.read_baseline_with_context(&connection, root, count, revision, Some(&context))?;
        comparison.validate(self, live, cancel)?;
        Ok(BaselineCandidate {
            existing_ancestors,
            id: live.id,
            lifetime: live.lifetime.clone(),
            revision: live.revision,
            baseline: live.baseline,
            fence: live.fence,
            root,
            count,
            value: comparison.value().cloned(),
        })
    }
    pub(super) fn commit_baseline(
        &mut self,
        live: &mut LiveOperation,
        candidate: BaselineCandidate,
        comparison: &mut super::transaction::native::VerifiedComparison<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<Revision>, StoreError> {
        self.guard()?;
        self.check_native(live)?;
        if candidate.id != live.id
            || candidate.revision != live.revision
            || candidate.baseline != live.baseline
            || candidate.fence != live.fence
            || !candidate.lifetime.ptr_eq(live.lifetime())
            || candidate.lifetime.strong_count() == 0
            || candidate.root != self.candidate_pin
            || candidate.value.as_ref() != comparison.value()
            || candidate.existing_ancestors.as_ref() != comparison.existing_ancestors()
        {
            return Err(StoreError::StaleRevision);
        }
        comparison.validate(self, live, cancel)?;
        let context = self.native_read_context(live, comparison.deadline(), cancel)?;
        let connection = self.connection(live.id.connection)?;
        let initial_review = self.operation(live.id)?.initial_review.is_some();
        if connection.baseline.is_none() && !initial_review {
            return Err(StoreError::RecoveryRequired);
        }
        comparison.validate(self, live, cancel)?;
        self.append_checked_with_baseline(
            Change::BaselineCommit {
                directory_chain: None,
                directory_ancestors: None,
                existing_ancestors: candidate.existing_ancestors,
                connection: connection.id,
                expected: live.revision,
                root: candidate.root,
                count: candidate.count,
                source: connection.source.identity,
                destination: connection.destination.identity,
                initial_review,
            },
            RecoveryChecks {
                mode: VerificationMode::NativeLive(&context),
                ..RecoveryChecks::default()
            },
            BaselineAppendCheck::Native {
                live,
                comparison,
                cancel,
            },
        )?;
        live.revision = format::next(live.revision)?;
        Ok(self.ack(live.revision))
    }
    pub(super) fn record_native_equal(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut super::transaction::native::NativeEquality<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.guard()?;
        self.check_native(live)?;
        if live.intent.decision != Decision::VerifyEqual {
            return Err(StoreError::RecoveryRequired);
        }
        proof.validate(live)?;
        let context = self.native_read_context(live, proof.deadline(), cancel)?;
        let connection = self.connection(live.id.connection)?;
        self.append_native(
            Change::Verified {
                connection: connection.id,
                serial: live.id.serial,
                expected: live.revision,
                source: connection.source.identity,
                destination: connection.destination.identity,
            },
            &context,
        )?;
        live.revision = format::next(live.revision)?;
        proof.validate(live)?;
        Ok(())
    }
    pub(super) fn record_native_outcome(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut super::transaction::native::NativeOutcome<'_, '_>,
        verified: bool,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.guard()?;
        self.check_native(live)?;
        proof.validate(live)?;
        let (attempt, outcome, displaced) = proof.facts()?;
        let mut key = format::key(5, live.id.connection.0, live.id.serial);
        key.extend(attempt.to_be_bytes());
        let Some(Row::NativeRegistration(mut registration)) = self.row(&key)? else {
            return Err(StoreError::Corrupt);
        };
        registration.outcome = Some(outcome);
        registration.outcome_displaced = displaced;
        let context = self.native_read_context(live, proof.deadline(), cancel)?;
        proof.validate(live)?;
        self.append_native(
            Change::NativeOutcome {
                registration,
                expected: live.revision,
                verified,
                reconstructed: proof.reconstructed(),
            },
            &context,
        )?;
        live.revision = format::next(live.revision)?;
        if let Err(error) = proof.validate(live) {
            self.poison_native();
            return Err(error);
        }
        Ok(())
    }
    pub(super) fn record_native_deletion(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut super::transaction::native::NativeDeletion<'_, '_>,
        verified: bool,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.guard()?;
        self.check_native(live)?;
        proof.validate(live)?;
        let (attempt, outcome, displaced) = proof.facts()?;
        let mut key = format::key(5, live.id.connection.0, live.id.serial);
        key.extend(attempt.to_be_bytes());
        let Some(Row::NativeRegistration(mut registration)) = self.row(&key)? else {
            return Err(StoreError::Corrupt);
        };
        registration.outcome = Some(outcome);
        registration.outcome_displaced = displaced;
        let context = self.native_read_context(live, proof.deadline(), cancel)?;
        proof.validate(live)?;
        self.append_native(
            Change::NativeOutcome {
                registration,
                expected: live.revision,
                verified,
                reconstructed: proof.reconstructed(),
            },
            &context,
        )?;
        live.revision = format::next(live.revision)?;
        proof.validate(live)?;
        Ok(())
    }
    pub(super) fn prepare_native(
        &mut self,
        live: &mut LiveOperation,
        reservation: NamespaceReservation,
        mut proof: super::transaction::native::NativePrepared<'_, '_>,
    ) -> Result<DurableAck<PreparedNative>, StoreError> {
        self.check_native(live)?;
        reservation.header(live, [0; 16])?;
        proof.validate(live)?;
        let (intent, stage) = proof.descriptors()?;
        let mut registration = reservation.registration;
        registration.intent = Some(intent);
        registration.stage = stage;
        let revision = format::next(live.revision)?;
        proof.validate(live)?;
        self.append(Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })?;
        live.revision = revision;
        if let Err(error) = proof.validate(live) {
            self.poison_native();
            return Err(error);
        }
        Ok(self.ack(PreparedNative {
            registration,
            lifetime: live.lifetime.clone(),
            revision,
        }))
    }
}

#[cfg(target_os = "linux")]
pub(super) struct BoundRecovery {
    fact: super::transaction::native::descriptor::RecoveryFact,
    preserved: PreservedObject,
    parent: Arc<storage::Directory>,
    lifetime: std::sync::Weak<()>,
    epoch: [u8; 16],
    metadata: cap_primitives::fs::Metadata,
}
#[cfg(target_os = "linux")]
impl BoundRecovery {
    pub(super) fn validate(
        &self,
        live: &LiveOperation,
        epoch: [u8; 16],
    ) -> Result<&super::transaction::native::descriptor::RecoveryFact, StoreError> {
        if !self.lifetime.ptr_eq(live.lifetime())
            || self.lifetime.strong_count() == 0
            || self.epoch != epoch
            || self.preserved.id.operation != live.id
        {
            return Err(StoreError::StaleRevision);
        }
        let metadata = storage::recovery_metadata(&self.preserved.file)?;
        if !storage::same_snapshot(&self.metadata, &metadata) {
            return Err(StoreError::IdentityChanged);
        }
        storage::validate_recovery_reader(
            &self.parent,
            &format!(
                "{}-{}",
                self.preserved.id.operation.serial, self.preserved.id.slot
            ),
            &self.preserved.file,
            &self.metadata,
        )?;
        Ok(&self.fact)
    }
}

/// Read-only durable ownership clues. Construction is confined to the locked
/// store; native inspection must still validate every current namespace object.
#[cfg(target_os = "linux")]
pub(super) struct NativeNamespaceRecord {
    registration: format::NativeRegistration,
    lifetime: std::sync::Weak<()>,
}
#[cfg(target_os = "linux")]
impl NativeNamespaceRecord {
    pub(super) fn native_kind(&self) -> Option<NativeOperationKind> {
        self.registration.native_kind
    }
    pub(super) fn operation(&self) -> OperationId {
        OperationId {
            connection: ConnectionId(self.registration.connection),
            serial: self.registration.serial,
        }
    }
    pub(super) fn validate(&self, live: &LiveOperation) -> Result<(), StoreError> {
        if !self.lifetime.ptr_eq(live.lifetime())
            || self.lifetime.strong_count() == 0
            || self.registration.connection != live.id.connection.0
        {
            return Err(StoreError::StaleRevision);
        }
        self.registration.validate()
    }
    pub(super) fn validate_recovery(&self, context: &RecoveryContext) -> Result<(), StoreError> {
        if !self.lifetime.ptr_eq(&context.lifetime)
            || self.lifetime.strong_count() == 0
            || self.registration.connection != context.id.connection.0
        {
            return Err(StoreError::StaleRevision);
        }
        self.registration.validate()
    }
    pub(super) fn side(&self) -> u8 {
        self.registration.side
    }
    pub(super) fn name(&self) -> String {
        NamespaceReservation {
            registration: self.registration.clone(),
            lifetime: self.lifetime.clone(),
            revision: 0,
        }
        .name()
    }
    pub(super) fn uncreated_for(&self, context: &RecoveryContext) -> Result<bool, StoreError> {
        self.validate_recovery(context)?;
        if self.registration.serial != context.id.serial {
            return Ok(false);
        }
        if self.registration.intent.is_some()
            || self.registration.stage.is_some()
            || self.registration.outcome.is_some()
            || self.registration.admission != context.admission
            || self.registration.baseline != context.baseline
        {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(true)
    }
    pub(super) fn outcome(&self) -> Option<&[u8]> {
        self.registration.outcome.as_deref()
    }
    pub(super) fn descriptors(&self) -> Result<(&[u8], Option<&[u8]>), StoreError> {
        Ok((
            self.registration
                .intent
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
            self.registration.stage.as_deref(),
        ))
    }
}
#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(super) fn recovery_namespace_records(
        &self,
        context: &RecoveryContext,
    ) -> Result<Vec<NativeNamespaceRecord>, StoreError> {
        self.check_recovery_context(context)?;
        self.namespace_records_for(context.id.connection, &context.lifetime)
    }
    pub(super) fn native_namespace_records(
        &self,
        live: &LiveOperation,
    ) -> Result<Vec<NativeNamespaceRecord>, StoreError> {
        self.check_native(live)?;
        self.namespace_records_for(live.id.connection, &live.lifetime)
    }
    fn namespace_records_for(
        &self,
        connection: ConnectionId,
        lifetime: &std::sync::Weak<()>,
    ) -> Result<Vec<NativeNamespaceRecord>, StoreError> {
        let mut records = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.table_page(connection, 5, cursor, 128)?;
            for row in page.entries {
                let Row::NativeRegistration(registration) = row else {
                    return Err(StoreError::Corrupt);
                };
                if records.len() >= 128 {
                    return Err(StoreError::LimitExceeded);
                }
                registration.validate()?;
                records.push(NativeNamespaceRecord {
                    registration,
                    lifetime: lifetime.clone(),
                });
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(records)
    }
}

#[cfg(all(test, target_os = "linux"))]
thread_local! { static AFTER_NATIVE_BYTES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
#[cfg(all(test, target_os = "linux"))]
pub(super) fn after_native_bytes_for_test(hook: impl FnOnce() + 'static) {
    AFTER_NATIVE_BYTES.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
#[cfg(all(test, target_os = "linux"))]
fn run_after_native_bytes() {
    AFTER_NATIVE_BYTES.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(all(test, target_os = "linux"))]
pub(super) use recovery::partial_native_recovery_for_test;

#[cfg(all(test, target_os = "linux"))]
thread_local! { static AFTER_CAPTURE_CHUNK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
#[cfg(all(test, target_os = "linux"))]
pub(super) fn after_capture_chunk_for_test(hook: impl FnOnce() + 'static) {
    AFTER_CAPTURE_CHUNK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
#[cfg(all(test, target_os = "linux"))]
fn run_after_capture_chunk() {
    AFTER_CAPTURE_CHUNK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(super) fn selected_recovery_scope(
        &self,
        context: &RecoveryContext,
    ) -> Result<
        (
            u8,
            bool,
            u8,
            super::transaction::native::descriptor::Selection,
        ),
        StoreError,
    > {
        self.check_recovery_context(context)?;
        if !matches!(context.action(), ExecutionAction::Selected(_)) {
            return Err(StoreError::RecoveryRequired);
        }
        let operation = self.operation(context.id())?;
        let Some(
            action @ format::SelectedAction::Restore {
                target, replace, ..
            },
        ) = &operation.action
        else {
            return Err(StoreError::RecoveryRequired);
        };
        if context.action
            != ExecutionAction::Selected(SelectedExecution {
                action: action.clone(),
            })
        {
            return Err(StoreError::StaleRevision);
        }
        Ok((
            *target,
            *replace,
            operation.decision,
            self.selected_descriptor(&operation, action)?,
        ))
    }
    pub(super) fn resume_native_selected(
        &mut self,
        context: &RecoveryContext,
        evidence: &mut super::transaction::native::NativeRestore<'_>,
        session: &super::transaction::TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        session.validate(context.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        if context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let operation = self.operation(context.id)?;
        if !matches!(operation.stage, 0 | 1 | 2 | 3 | 6)
            || self.connection(context.id.connection)?.pending != Some(context.id.serial)
        {
            return Err(StoreError::RecoveryRequired);
        }
        evidence.validate_recovery(self, context)?;
        let live = LiveOperation {
            native_kind: context.native_kind,
            native_identity: context.native_identity.clone(),
            planned_baseline_count: None,
            action: context.action.clone(),
            gate: context.gate(),
            deletion_baseline_checked: false,
            id: context.id,
            lifetime: context.lifetime.clone(),
            revision: context.revision,
            admission: context.admission,
            baseline: context.baseline,
            fence: context.fence,
            intent: context.intent.clone(),
            endpoints: context.endpoints,
            correlation: self.native_correlation,
            reads: Arc::clone(&context.reads),
        };
        self.check_native(&live)?;
        evidence.activate(self, &live)?;
        session.require_idle()?;
        Ok(live)
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(super) fn record_recovered_selected_outcome(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut super::transaction::native::NativeRestore<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.guard()?;
        proof.validate(self, live)?;
        let mut registration = self.selected_registration(live, proof)?;
        registration.outcome = Some(proof.outcome_descriptor()?);
        registration.outcome_displaced = proof.outcome_displaced()?;
        if matches!(self.operation(live.id())?.stage, 1 | 6) {
            let context = self.native_read_context(live, proof.deadline(), cancel)?;
            let previous = live.revision();
            self.append_native(
                Change::NativeOutcome {
                    registration: registration.clone(),
                    expected: previous,
                    verified: false,
                    reconstructed: true,
                },
                &context,
            )?;
            live.revision = format::next(previous)?;
            proof.advance(self, live, previous)?;
        }
        if self.operation(live.id())?.stage == 2 {
            self.record_selected_outcome(live, proof, true, cancel)?;
        }
        if self.operation(live.id())?.stage != 3
            || self.selected_registration(live, proof)? != registration
        {
            return Err(StoreError::RecoveryRequired);
        }
        proof.validate(self, live)
    }
}

// Metadata exception only: the caller must already hold and validate the fresh
// owning comparison, and commit still independently pays for retained versions.
impl ConnectionStore {
    /// Canonical ordinary conflict metadata, including independently typed capture.
    /// Neither this join nor a checkpoint row provides current native evidence.
    fn ordinary_conflict_join(
        &self,
        conflict: &format::ConflictData,
    ) -> Result<format::Operation, StoreError> {
        conflict.decode()?;
        let operation = self.operation(OperationId {
            connection: ConnectionId(conflict.connection),
            serial: conflict.serial,
        })?;
        operation.decode()?;
        if operation.connection != conflict.connection
            || operation.serial != conflict.serial
            || operation.action.is_some()
            || operation.path != conflict.path
            || operation.source != conflict.source
            || operation.destination != conflict.destination
            || conflict.revision < format::next(operation.expected)?
            || conflict.preserved.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(StoreError::Corrupt);
        }
        match (&operation.native_capture, &conflict.native_capture) {
            (None, None)
                if matches!(operation.decision, 10..=14)
                    && conflict.kind == operation.decision - 10 => {}
            (Some(recorded), Some(captured)) if recorded == captured => {
                let snapshot =
                    super::transaction::native::descriptor::ConflictSnapshot::decode(recorded)
                        .map_err(|_| StoreError::Corrupt)?;
                operation.validate_capture(&snapshot)?;
                if !matches!(operation.stage, 0 | 1 | 7 | 8)
                    || (operation.stage == 8
                        && conflict.revision
                            > operation
                                .revision
                                .saturating_add(u64::from(conflict.resolved)))
                {
                    return Err(StoreError::Corrupt);
                }
            }
            _ => return Err(StoreError::Corrupt),
        }
        for slot in &conflict.preserved {
            if !operation.recovery.contains(slot) {
                return Err(StoreError::Corrupt);
            }
            let id = RecoveryId {
                operation: OperationId {
                    connection: ConnectionId(conflict.connection),
                    serial: conflict.serial,
                },
                slot: *slot,
            };
            let Some(Row::Recovery(entry)) = self.row(&recovery::recovery_key(id))? else {
                return Err(StoreError::Corrupt);
            };
            entry.decode()?;
            if entry.id() != id
                || entry.path != conflict.path
                || entry.reason != 2
                || !matches!(entry.state, 1..=4)
            {
                return Err(StoreError::Corrupt);
            }
            if let Some(origin) = &entry.captured {
                let snapshot = super::transaction::native::descriptor::ConflictSnapshot::decode(
                    &origin.snapshot,
                )
                .map_err(|_| StoreError::Corrupt)?;
                operation.validate_capture(&snapshot)?;
                let version = snapshot
                    .versions
                    .get(usize::from(origin.version))
                    .ok_or(StoreError::Corrupt)?;
                let super::transaction::native::descriptor::State::Regular { content, .. } =
                    &version.state
                else {
                    return Err(StoreError::Corrupt);
                };
                if entry.content
                    != Some(format::State::File {
                        sha256: content.digest,
                        bytes: content.length,
                        executable: content.executable,
                    })
                {
                    return Err(StoreError::Corrupt);
                }
            }
        }
        Ok(operation)
    }
    fn attention_conflict_origin(
        &self,
        conflict: &format::ConflictData,
    ) -> Result<format::Operation, StoreError> {
        let operation = self.operation(OperationId {
            connection: ConnectionId(conflict.connection),
            serial: conflict.serial,
        })?;
        if operation.action.is_some() {
            if self.selected_collision_join(&operation)? != *conflict {
                return Err(StoreError::Corrupt);
            }
            Ok(operation)
        } else {
            self.ordinary_conflict_join(conflict)
        }
    }
    fn attention_selection_join(
        &self,
        applied: &format::Operation,
        conflict: &format::ConflictData,
    ) -> Result<bool, StoreError> {
        applied.decode()?;
        let origin = self.attention_conflict_origin(conflict)?;
        let Some(
            action @ format::SelectedAction::Restore {
                selected,
                conflict: Some(bound),
                ..
            },
        ) = &applied.action
        else {
            return Err(StoreError::Corrupt);
        };
        if applied.connection != conflict.connection
            || bound.serial != conflict.serial
            || applied.path != origin.path
            || applied.source_binding != origin.source_binding
            || applied.destination_binding != origin.destination_binding
        {
            return Err(StoreError::Corrupt);
        }
        self.selected_descriptor(applied, action)?;
        if let format::SelectedVersion::Recovery {
            operation, slot, ..
        } = selected
        {
            if !self.conflict_contains_selection(
                conflict,
                RecoveryId {
                    operation: OperationId {
                        connection: ConnectionId(applied.connection),
                        serial: *operation,
                    },
                    slot: *slot,
                },
            )? {
                return Err(StoreError::Corrupt);
            }
        }
        Ok(origin.native_capture.is_none()
            || (origin.action.is_none() && origin.stage == 8)
            || (origin.action.is_some() && origin.stage == 10))
    }
    fn validate_selected_attention_carry(
        &self,
        operation: &format::Operation,
        expected: u64,
        carry: &format::SelectedAttentionCarryV1,
    ) -> Result<(), StoreError> {
        carry.validate()?;
        if operation.native_budget.is_none()
            || self.selected_attention_carry(operation, expected)? != carry.prior
        {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(())
    }
    fn selected_attention_carry(
        &self,
        operation: &format::Operation,
        expected: u64,
    ) -> Result<Vec<format::SelectedAttentionRefV1>, StoreError> {
        operation.decode()?;
        let Some(format::SelectedAction::Restore {
            selected,
            conflict: Some(bound),
            ..
        }) = &operation.action
        else {
            return Ok(Vec::new());
        };
        if operation.native_budget.is_none() || operation.native_kind.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        let Some(Row::Conflict(current)) =
            self.row(&format::key(3, operation.connection, bound.serial))?
        else {
            return Err(StoreError::Corrupt);
        };
        if !self.attention_selection_join(operation, &current)?
            || current.resolved
            || current.revision != bound.revision
        {
            return Err(StoreError::StaleRevision);
        }
        let content = |selected: &format::SelectedVersion| match selected {
            format::SelectedVersion::Recovery { content, .. } => Some(content.clone()),
            format::SelectedVersion::Tombstone { .. } => None,
        };
        let mut prior = Vec::new();
        let mut conflicts = std::collections::BTreeSet::new();
        for previous in
            self.selected_attention(ConnectionId(operation.connection), &operation.path)?
        {
            let fact = previous
                .selected_applied
                .as_ref()
                .ok_or(StoreError::Corrupt)?;
            let Some(format::SelectedAction::Restore {
                selected: earlier,
                conflict: Some(old_bound),
                ..
            }) = &previous.action
            else {
                continue;
            };
            if previous.native_budget.is_none() {
                return Err(StoreError::Corrupt);
            }
            let Some(Row::Conflict(old_conflict)) =
                self.row(&format::key(3, operation.connection, old_bound.serial))?
            else {
                return Err(StoreError::Corrupt);
            };
            // Structural validity is checked even for subsequently superseded consent.
            let complete = self.attention_selection_join(&previous, &old_conflict)?;
            if !complete || old_conflict.resolved || old_conflict.revision != old_bound.revision {
                continue;
            }
            if !conflicts.insert(old_bound.serial) {
                return Err(StoreError::Corrupt);
            }
            if old_bound.serial == bound.serial
                || content(earlier) != content(selected)
                || previous.source_binding != operation.source_binding
                || previous.destination_binding != operation.destination_binding
            {
                continue;
            }
            if previous.serial >= operation.serial
                || previous.revision > expected
                || fact.revision > expected
            {
                return Err(StoreError::Corrupt);
            }
            if prior.len() >= 127 {
                return Err(StoreError::LimitExceeded);
            }
            prior.push(format::SelectedAttentionRefV1 {
                operation: previous.serial,
                operation_revision: previous.revision,
                applied_revision: fact.revision,
                fence: fact.fence,
                outcome: fact.outcome,
                conflict: old_bound.serial,
                conflict_revision: old_bound.revision,
            });
        }
        prior.sort_by_key(|fact| fact.operation);
        Ok(prior)
    }
    fn joined_selected_equality_conflict_matches(
        &self,
        comparison: &format::Operation,
        applied: &format::Operation,
        conflict: &format::ConflictData,
    ) -> Result<bool, StoreError> {
        self.attention_conflict_origin(conflict)?;
        let Some(format::SelectedAction::Restore {
            conflict: Some(bound),
            ..
        }) = &applied.action
        else {
            return Ok(false);
        };
        if bound.serial != conflict.serial || applied.connection != conflict.connection {
            return Ok(false);
        }
        Ok(self.attention_selection_join(applied, conflict)?
            && selected_equality_conflict_matches(comparison, applied, conflict))
    }
}

fn selected_equality_conflict_matches(
    comparison: &format::Operation,
    applied: &format::Operation,
    conflict: &format::ConflictData,
) -> bool {
    if comparison.decode().is_err()
        || applied.decode().is_err()
        || conflict.decode().is_err()
        || comparison.action.is_some()
        || comparison.native_budget.is_none()
        || comparison.stage != 3
        || comparison.decision != 0
        || comparison.serial <= applied.serial
        || comparison.expected < applied.revision
        || comparison.connection != applied.connection
        || comparison.path != applied.path
        || comparison.source_binding != applied.source_binding
        || comparison.destination_binding != applied.destination_binding
        || !applied
            .selected_applied
            .as_ref()
            .is_some_and(|fact| fact.attention)
        || conflict.resolved
        || conflict.connection != applied.connection
        || conflict.path != applied.path
    {
        return false;
    }
    let Some(format::SelectedAction::Restore {
        selected,
        conflict: Some(bound),
        ..
    }) = &applied.action
    else {
        return false;
    };
    let content = match selected {
        format::SelectedVersion::Recovery { content, .. } => Some(content),
        format::SelectedVersion::Tombstone { .. } => None,
    };
    bound.serial == conflict.serial
        && bound.revision == conflict.revision
        && comparison.source.as_ref() == content
        && comparison.destination.as_ref() == content
}

impl ConnectionStore {
    pub(super) fn selected_collision_snapshot(
        &self,
        context: &RecoveryContext,
    ) -> Result<super::transaction::native::descriptor::ConflictSnapshot, StoreError> {
        self.check_recovery_context(context)?;
        if !matches!(context.action, ExecutionAction::SelectedCollision(_)) {
            return Err(StoreError::RecoveryRequired);
        }
        let operation = self.operation(context.id)?;
        self.selected_collision_join(&operation)?;
        let snapshot = super::transaction::native::descriptor::ConflictSnapshot::decode(
            operation
                .native_capture
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        operation.validate_capture(&snapshot)?;
        Ok(snapshot)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn resume_selected_collision(
        &mut self,
        context: &RecoveryContext,
        evidence: &mut super::transaction::native::NativeSelectedCollision<'_>,
        session: &super::transaction::TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        session.validate(context.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        if context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let operation = self.operation(context.id)?;
        if !matches!(operation.stage, 0 | 1)
            || self.connection(context.id.connection)?.pending != Some(context.id.serial)
            || evidence.parts().0.snapshot() != &self.selected_collision_snapshot(context)?
        {
            return Err(StoreError::RecoveryRequired);
        }
        evidence.validate_recovery(self, context)?;
        let live = LiveOperation {
            native_kind: context.native_kind,
            native_identity: context.native_identity.clone(),
            planned_baseline_count: None,
            action: context.action.clone(),
            gate: context.gate(),
            deletion_baseline_checked: false,
            id: context.id,
            lifetime: context.lifetime.clone(),
            revision: context.revision,
            admission: context.admission,
            baseline: context.baseline,
            fence: context.fence,
            intent: context.intent.clone(),
            endpoints: context.endpoints,
            correlation: self.native_correlation,
            reads: Arc::clone(&context.reads),
        };
        evidence.activate(self, &live)?;
        self.check_native(&live)?;
        session.require_idle()?;
        Ok(live)
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(super) fn record_selected_boundary(
        &mut self,
        context: &RecoveryContext,
        proof: &mut super::transaction::native::NativeSelectedBoundary<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<OperationReceipt>, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        if !context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let (kind, attempt, displaced, outcome) = proof.validate(self, context)?;
        let reads = self.recovery_read_context(context, proof.deadline(), cancel)?;
        self.append_native(
            Change::SelectedBoundary {
                connection: context.id.connection.0,
                serial: context.id.serial,
                expected: context.revision,
                operation_expected: context.operation_revision,
                fence: context.fence,
                kind,
                evidence: format::BoundaryEvidence {
                    attempt,
                    displaced,
                    outcome,
                },
            },
            &reads,
        )?;
        proof.retained_check(context)?;
        Ok(self.ack(self.operation(context.id)?.decode()?))
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(super) fn record_selected_untouched(
        &mut self,
        context: &RecoveryContext,
        proof: &mut super::transaction::native::SelectedNeverBegun<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<OperationReceipt>, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        if !context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        proof.validate(self, context)?;
        let reads = self.recovery_read_context(context, proof.deadline(), cancel)?;
        self.append_native(
            Change::SelectedUntouched {
                connection: context.id.connection.0,
                serial: context.id.serial,
                expected: context.revision,
                operation_expected: context.operation_revision,
                fence: context.fence,
            },
            &reads,
        )?;
        proof.retained_check(context)?;
        Ok(self.ack(self.operation(context.id)?.decode()?))
    }
    pub(super) fn resume_selected_untouched(
        &mut self,
        context: &RecoveryContext,
        proof: &mut super::transaction::native::SelectedNeverBegun<'_>,
        session: &super::transaction::TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        session.validate(context.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        if context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        proof.validate(self, context)?;
        let live = LiveOperation {
            native_kind: context.native_kind,
            native_identity: context.native_identity.clone(),
            planned_baseline_count: None,
            action: context.action.clone(),
            gate: context.gate(),
            deletion_baseline_checked: false,
            id: context.id,
            lifetime: context.lifetime.clone(),
            revision: context.revision,
            admission: context.admission,
            baseline: context.baseline,
            fence: context.fence,
            intent: context.intent.clone(),
            endpoints: context.endpoints,
            correlation: self.native_correlation,
            reads: Arc::clone(&context.reads),
        };
        self.check_native(&live)?;
        proof.activate(self, &live)?;
        proof.retained_check(context)?;
        session.require_idle()?;
        Ok(live)
    }
}

impl ConnectionStore {
    /// Checks the immutable recorded absence and lineage. This produces descriptor
    /// metadata only; native current absence/displacement still requires paid guards.
    fn selected_tombstone_descriptor(
        &self,
        operation: &format::Operation,
        action: &format::SelectedAction,
    ) -> Result<super::transaction::native::descriptor::Selection, StoreError> {
        use super::transaction::native::descriptor as d;
        let format::SelectedAction::Restore {
            selected:
                format::SelectedVersion::Tombstone {
                    operation: serial,
                    revision: origin_revision,
                    side,
                    snapshot,
                },
            conflict: Some(selected_conflict),
            ..
        } = action
        else {
            return Err(StoreError::RecoveryRequired);
        };
        if *side > 1 {
            return Err(StoreError::Corrupt);
        }
        let source = self.operation(OperationId {
            connection: ConnectionId(operation.connection),
            serial: *serial,
        })?;
        let encoded = source
            .native_capture
            .as_deref()
            .ok_or(StoreError::RecoveryRequired)?;
        let capture = d::ConflictSnapshot::decode(encoded).map_err(|_| StoreError::Corrupt)?;
        source.validate_capture(&capture)?;
        let Some(Row::Conflict(conflict)) =
            self.row(&format::key(3, operation.connection, *serial))?
        else {
            return Err(StoreError::RecoveryRequired);
        };
        conflict.decode()?;
        if source.path != operation.path
            || source.source_binding != operation.source_binding
            || source.destination_binding != operation.destination_binding
            || conflict.path != operation.path
            || conflict.revision < *origin_revision
            || *origin_revision < capture.revision
            || conflict.native_capture.as_deref() != Some(encoded)
            || capture.binding_digest().map_err(|_| StoreError::Corrupt)? != *snapshot
            || !capture.versions.iter().any(|version| {
                version.side == *side && version.role == 0 && version.state == d::State::Absent
            })
        {
            return Err(StoreError::RecoveryRequired);
        }
        if selected_conflict.serial != *serial {
            let Some(Row::Conflict(current)) = self.row(&format::key(
                3,
                operation.connection,
                selected_conflict.serial,
            ))?
            else {
                return Err(StoreError::RecoveryRequired);
            };
            let format::SelectedAction::Restore { selected, .. } = action;
            if current.revision < selected_conflict.revision
                || !self.tombstone_collision_contains(&current, selected, operation)?
            {
                return Err(StoreError::RecoveryRequired);
            }
        }
        Ok(d::Selection::Tombstone {
            conflict: (operation.connection, *serial),
            revision: *origin_revision,
        })
    }
}

impl ConnectionStore {
    /// Read-only projection of positively recorded absent versions; not a native proof.
    pub(super) fn tombstone_choices(
        &self,
        id: ConflictId,
    ) -> Result<Vec<TombstoneChoice>, StoreError> {
        use super::transaction::native::descriptor as d;
        let Some(Row::Conflict(conflict)) =
            self.row(&format::key(3, id.connection.0, id.serial))?
        else {
            return Err(StoreError::RecoveryRequired);
        };
        conflict.decode()?;
        let operation = self.operation(OperationId {
            connection: id.connection,
            serial: id.serial,
        })?;
        let encoded = conflict
            .native_capture
            .as_deref()
            .ok_or(StoreError::RecoveryRequired)?;
        if operation.native_capture.as_deref() != Some(encoded) || operation.path != conflict.path {
            return Err(StoreError::RecoveryRequired);
        }
        let snapshot = d::ConflictSnapshot::decode(encoded).map_err(|_| StoreError::Corrupt)?;
        operation.validate_capture(&snapshot)?;
        let digest = snapshot.binding_digest().map_err(|_| StoreError::Corrupt)?;
        let mut choices: Vec<_> = snapshot
            .versions
            .iter()
            .filter(|version| version.role == 0 && version.state == d::State::Absent)
            .map(|version| TombstoneChoice {
                origin: id,
                revision: conflict.revision,
                side: version.side,
                snapshot: digest,
                path: snapshot.path.clone(),
            })
            .collect();
        if let Some(format::SelectedAction::Restore {
            selected:
                format::SelectedVersion::Tombstone {
                    operation: origin,
                    revision,
                    side,
                    snapshot,
                },
            ..
        }) = &operation.action
        {
            if self.selected_collision_join(&operation)? != conflict {
                return Err(StoreError::RecoveryRequired);
            }
            let chosen = TombstoneChoice {
                origin: ConflictId {
                    connection: id.connection,
                    serial: *origin,
                },
                revision: *revision,
                side: *side,
                snapshot: *snapshot,
                path: operation.path.to_relative()?,
            };
            if !choices.contains(&chosen) {
                choices.push(chosen);
            }
        }
        Ok(choices)
    }
    fn tombstone_review_basis(
        &self,
        intent: OperationIntent,
        selected: TombstoneChoice,
        target: Side,
        conflict: (ConflictId, Revision),
    ) -> Result<RestorePreview, StoreError> {
        let record = self.load_connection(intent.connection)?;
        let connection = self.connection(record.id)?;
        if record.lifecycle != Lifecycle::Active
            || connection.pending.is_some()
            || record.revision != intent.expected_revision
            || record.baseline_revision != intent.expected_baseline
            || selected.origin.connection != record.id
            || selected.path != intent.path
            || selected.origin.connection != conflict.0.connection
            || !self.tombstone_choices(conflict.0)?.contains(&selected)
        {
            return Err(StoreError::StaleRevision);
        }
        let Some(Row::Conflict(row)) = self.row(&format::key(3, record.id.0, conflict.0.serial))?
        else {
            return Err(StoreError::RecoveryRequired);
        };
        if row.resolved || row.revision != conflict.1 {
            return Err(StoreError::StaleRevision);
        }
        Ok(RestorePreview {
            record,
            lifetime: Arc::downgrade(&self.native_lifetime),
            intent,
            selected: ReviewVersion::Tombstone(selected),
            target,
            conflict: Some(format::SelectedConflict {
                serial: conflict.0.serial,
                revision: conflict.1,
            }),
        })
    }
}

impl ConnectionStore {
    // A later conflict offers an older absence only through its own canonical
    // selected collision action. No path-only or same-slot fallback is permitted.
    fn tombstone_collision_contains(
        &self,
        conflict: &format::ConflictData,
        selected: &format::SelectedVersion,
        current: &format::Operation,
    ) -> Result<bool, StoreError> {
        conflict.decode()?;
        let prior = self.operation(OperationId {
            connection: ConnectionId(conflict.connection),
            serial: conflict.serial,
        })?;
        prior.decode()?;
        if prior.stage != 10
            || conflict.kind != 3
            || prior.path != conflict.path
            || prior.path != current.path
            || prior.connection != current.connection
            || prior.source_binding != current.source_binding
            || prior.destination_binding != current.destination_binding
            || prior.native_capture.is_none()
            || prior.native_capture != conflict.native_capture
        {
            return Ok(false);
        }
        let Some(format::SelectedAction::Restore {
            selected: original, ..
        }) = &prior.action
        else {
            return Ok(false);
        };
        if original != selected {
            return Ok(false);
        }
        let snapshot = super::transaction::native::descriptor::ConflictSnapshot::decode(
            prior.native_capture.as_ref().ok_or(StoreError::Corrupt)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        prior.validate_capture(&snapshot)?;
        Ok(true)
    }
}

impl ConnectionStore {
    /// Serialized owner navigation only; neither identifier admits continuation.
    pub(super) fn executor_position(
        &self,
        id: ConnectionId,
    ) -> Result<(Option<OperationId>, Option<OperationId>), StoreError> {
        let connection = self.connection(id)?;
        Ok((
            connection.pending.map(|serial| OperationId {
                connection: id,
                serial,
            }),
            (connection.serial != 0).then_some(OperationId {
                connection: id,
                serial: connection.serial,
            }),
        ))
    }
    /// Metadata-only pre-Intent feasibility. This is a requirement, never proof.
    pub(super) fn preflight_regular_baseline(
        &self,
        intent: &OperationIntent,
        baseline: &VerifiedBaseline,
    ) -> Result<u32, StoreError> {
        self.preflight_baseline(intent, baseline)
    }
    fn preflight_baseline(
        &self,
        intent: &OperationIntent,
        baseline: &VerifiedBaseline,
    ) -> Result<u32, StoreError> {
        let record = self.load_connection(intent.connection)?;
        if record.revision != intent.expected_revision
            || record.baseline_revision != intent.expected_baseline
            || baseline.connection != record.id
            || baseline.source != record.source.identity
            || baseline.destination != record.destination.identity
        {
            return Err(StoreError::StaleRevision);
        }
        let mut entries = baseline.entries().cloned().unwrap_or_default();
        let requirement =
            super::transaction::ExistingAncestorRequirement::from_baseline(&intent.path, &entries)?;
        if requirement.prefixes() != 0
            && !matches!(
                intent.decision,
                Decision::VerifyEqual | Decision::Propagate { .. }
            )
        {
            return Err(StoreError::RecoveryRequired);
        }
        for path in requirement.paths() {
            entries.insert(path, EntryState::Directory);
        }
        if let Some(value) = intent
            .source_before
            .as_ref()
            .or(intent.destination_before.as_ref())
        {
            entries.insert(intent.path.clone(), value.clone());
        }
        let count = u32::try_from(entries.len()).map_err(|_| StoreError::LimitExceeded)?;
        let old_count = u32::try_from(baseline.entries().map_or(0, BTreeMap::len))
            .map_err(|_| StoreError::LimitExceeded)?;
        baseline_phase_estimate(old_count, count)?
            .checked_add(64 * 1024 * 1024)
            .filter(|bytes| *bytes <= format::READ_LIMIT)
            .ok_or(StoreError::LimitExceeded)?;
        let mut bytes = 0_usize;
        for (ordinal, (path, state)) in entries.into_iter().enumerate() {
            bytes = bytes
                .checked_add(
                    format::encode(&format::BaselineEntry {
                        connection: intent.connection.0,
                        revision: format::next(intent.expected_revision)?,
                        ordinal: u32::try_from(ordinal).map_err(|_| StoreError::LimitExceeded)?,
                        path: path.into(),
                        state: state.into(),
                    })?
                    .len(),
                )
                .ok_or(StoreError::LimitExceeded)?;
            if bytes > 32 * 1024 * 1024 {
                return Err(StoreError::LimitExceeded);
            }
        }
        Ok(count)
    }
    pub(super) fn executor_candidate_allowed(
        &self,
        intent: &OperationIntent,
    ) -> Result<bool, StoreError> {
        let connection = self.connection(intent.connection)?;
        let mut proposed = format::Operation::new(
            intent.clone(),
            format::next(connection.serial)?,
            &connection,
        )?;
        proposed.native_budget = Some(format::NativeBudget::initial());
        if self.reduce_intent(&proposed).is_err() {
            return Ok(false);
        }
        for row in self.rows()? {
            let Row::Conflict(conflict) = row? else {
                continue;
            };
            if conflict.connection != intent.connection.0
                || conflict.resolved
                || !conflict.path.to_relative()?.overlaps(&intent.path)
            {
                continue;
            }
            let mut equality = proposed.clone();
            equality.stage = 3;
            let selected = self.selected_attention(intent.connection, &proposed.path)?;
            self.attention_conflict_origin(&conflict)?;
            let mut accepted = false;
            for applied in &selected {
                accepted |=
                    self.joined_selected_equality_conflict_matches(&equality, applied, &conflict)?;
            }
            if !accepted {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl ConnectionStore {
    pub(super) fn executor_receipt(&self, id: OperationId) -> Result<OperationReceipt, StoreError> {
        self.operation(id)?.decode()
    }
    pub(super) fn executor_session(
        &self,
        id: ConnectionId,
        session: &super::transaction::TransactionSession,
    ) -> Result<(), StoreError> {
        self.require_no_review_or_chain_history(id)?;
        session.validate(id, &self.native_lifetime)
    }
}

impl ConnectionStore {
    /// Reservation metadata only. Exclusive native creation still refuses any
    /// existing object at the exact name; this never adopts a created directory.
    pub(super) fn reusable_native_namespace(
        &self,
        live: &LiveOperation,
        side: Side,
    ) -> Result<Option<NamespaceReservation>, StoreError> {
        self.check_native(live)?;
        if !live.action.is_reconciliation() {
            return Err(StoreError::RecoveryRequired);
        }
        let operation = self.operation(live.id())?;
        if operation.stage != 0 || !operation.recovery.is_empty() {
            return Err(StoreError::RecoveryRequired);
        }
        let mut existing = None;
        for row in self.rows()? {
            let Row::NativeRegistration(registration) = row? else {
                continue;
            };
            if registration.connection != live.id.connection.0
                || registration.serial != live.id.serial
            {
                continue;
            }
            registration.validate()?;
            if existing.is_some()
                || registration.side != u8::from(side == Side::Destination)
                || registration.admission != live.admission
                || registration.baseline != live.baseline
                || registration.fence != live.fence
                || registration.intent.is_some()
                || registration.stage.is_some()
                || registration.outcome.is_some()
            {
                return Err(StoreError::RecoveryRequired);
            }
            existing = Some(NamespaceReservation {
                registration,
                lifetime: live.lifetime.clone(),
                revision: live.revision,
            });
        }
        Ok(existing)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn reserve_executor_namespace(
        &mut self,
        live: &mut LiveOperation,
        side: Side,
    ) -> Result<NamespaceReservation, StoreError> {
        if let Some(reservation) = self.reusable_native_namespace(live, side)? {
            return Ok(reservation);
        }
        self.reserve_native_namespace(live, side)
    }
}

#[cfg(all(test, target_os = "linux"))]
struct NativeBaselineWriteHook {
    revoke: bool,
    action: Box<dyn FnOnce()>,
}
#[cfg(all(test, target_os = "linux"))]
thread_local! {
    static BEFORE_NATIVE_BASELINE_WRITE: std::cell::RefCell<Option<NativeBaselineWriteHook>> = const { std::cell::RefCell::new(None) };
}
#[cfg(all(test, target_os = "linux"))]
pub(super) fn before_native_baseline_write_for_test(revoke: bool, hook: impl FnOnce() + 'static) {
    BEFORE_NATIVE_BASELINE_WRITE.with(|slot| {
        *slot.borrow_mut() = Some(NativeBaselineWriteHook {
            revoke,
            action: Box::new(hook),
        });
    });
}
#[cfg(all(test, target_os = "linux"))]
impl ConnectionStore {
    pub(super) fn selected_attention_snapshot_for_test(
        &self,
        id: ConnectionId,
    ) -> Result<Vec<Vec<u8>>, StoreError> {
        let mut result = Vec::new();
        for row in self.rows()? {
            if let Row::Operation(operation) = row?
                && operation.connection == id.0
                && operation
                    .selected_applied
                    .as_ref()
                    .is_some_and(|fact| fact.attention)
            {
                result.push(format::encode(&operation)?);
            }
        }
        Ok(result)
    }
    pub(super) fn baseline_checkpoint_generation_for_test(&self) -> u64 {
        self.current.generation
    }
    pub(super) fn force_baseline_checkpoint_for_test(&mut self) {
        self.overlay_bytes = 9 * 1024 * 1024;
    }
}

#[cfg(all(test, target_os = "linux"))]
impl ConnectionStore {
    pub(super) fn native_budget_for_test(
        &self,
        id: OperationId,
    ) -> Result<(u8, u64, u64), StoreError> {
        let budget = self
            .operation(id)?
            .native_budget
            .ok_or(StoreError::RecoveryRequired)?;
        Ok((budget.version, budget.reserved, budget.sequence))
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn clear_native_baseline_write_for_test() {
    BEFORE_NATIVE_BASELINE_WRITE.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn storage_fault_for_test(point: Option<usize>) {
    tests::FAULT.with(|slot| slot.set(point.map(|point| (point, false))));
}
