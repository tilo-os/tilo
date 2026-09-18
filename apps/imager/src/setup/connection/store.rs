//! Private durable Connected metadata. Persisted records never grant endpoint access.
mod format;
mod recovery;
mod storage;
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
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OperationReceipt {
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
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
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
/// Opaque current-store authority. No serialized row can recreate it.
pub(super) struct LiveOperation {
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
    sequence: u64,
    digest: [u8; 32],
    session: Arc<()>,
    native_lifetime: Arc<()>,
    native_correlation: [u8; 16],
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
        let log_name = format!("log-{}", current.log);
        let mut log = storage::open_file(&storage.root, &log_name, false)?;
        let log_identity = storage::regular(&log)?;
        if log_identity.len() > 16 * 1024 * 1024 {
            return Err(StoreError::LimitExceeded);
        }
        let mut magic = [0; 8];
        log.read_exact(&mut magic)?;
        if &magic != b"QOSCON1\n" {
            return Err(StoreError::Corrupt);
        }
        let header: format::LogHeader = framing::read_record(&mut log, 0)
            .map_err(|_| StoreError::Corrupt)?
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
            sequence,
            digest,
            session: Arc::new(()),
            native_lifetime: Arc::new(()),
            native_correlation: native_correlation()?,
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
        loop {
            let offset = store.log.stream_position()?;
            let event: Option<Event> =
                framing::read_record(&mut store.log, format::next(store.sequence)?)
                    .map_err(|_| StoreError::Corrupt)?;
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
            let mut recovery_checks = RecoveryChecks::default();
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
        store.resume_gc()?;
        Ok(store)
    }
    fn validate(&self) -> Result<(), StoreError> {
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
        if let Some(value) = self.overlay.get(key) {
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
    pub(super) fn begin_intent(
        &mut self,
        intent: OperationIntent,
    ) -> Result<DurableAck<OperationId>, StoreError> {
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
    pub(super) fn begin_native(
        &mut self,
        intent: OperationIntent,
        session: &super::transaction::TransactionSession,
    ) -> Result<LiveOperation, StoreError> {
        session.validate(intent.connection, &self.native_lifetime)?;
        let before = self.load_connection(intent.connection)?;
        let ack = self.begin_intent(intent.clone())?;
        let revision = format::next(before.revision)?;
        Ok(LiveOperation {
            id: ack.value,
            lifetime: Arc::downgrade(&self.native_lifetime),
            revision,
            admission: revision,
            baseline: before.baseline_revision,
            fence: before.fence_generation,
            intent,
            endpoints: [before.source.identity, before.destination.identity],
            correlation: self.native_correlation,
            reads: Arc::new(super::transaction::native::OperationReads::new()),
        })
    }
    pub(super) fn check_native(&self, live: &LiveOperation) -> Result<(), StoreError> {
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
            || operation.expected.checked_add(1) != Some(live.admission)
            || operation.decode()?.intent != live.intent
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(super) fn reserve_native_namespace(
        &mut self,
        live: &mut LiveOperation,
        side: Side,
    ) -> Result<NamespaceReservation, StoreError> {
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
    #[expect(
        clippy::too_many_lines,
        reason = "Reservation and prepared transitions share the same bounded historical registration join."
    )]
    fn reduce_native_registration(
        &self,
        registration: &format::NativeRegistration,
        expected: u64,
        prepared: bool,
    ) -> Result<Vec<Row>, StoreError> {
        registration.validate()?;
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
        if registration.admission != format::next(operation.expected)?
            || registration.baseline != operation.baseline
            || operation.stage > 1
        {
            return Err(StoreError::StaleRevision);
        }
        if (matches!(operation.decision, 1 | 4) && registration.side != 1)
            || (matches!(operation.decision, 2 | 3) && registration.side != 0)
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
                || (matches!(operation.decision, 1 | 2) && registration.stage.is_none())
                || (matches!(operation.decision, 3 | 4) && registration.stage.is_some())
            {
                return Err(StoreError::Corrupt);
            }
            if matches!(
                (
                    operation.decision,
                    &operation.source,
                    &operation.destination
                ),
                (1, Some(format::State::File { .. }), None)
                    | (2, None, Some(format::State::File { .. }))
            ) {
                if !operation.recovery.is_empty() {
                    return Err(StoreError::Corrupt);
                }
                operation.stage = 6;
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
        match &event.change {
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
                matches!(event.change, Change::NativePrepared { .. }),
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
                matches!(event.change, Change::Verified { .. }),
                recovery_checks,
            ),
            Change::BaselineCommit {
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
                matches!(event.change, Change::Bytes { .. }),
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
                if matches!(event.change, Change::Fence { .. }) {
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
    fn reduce_intent(&self, operation: &format::Operation) -> Result<Vec<Row>, StoreError> {
        let receipt = operation.decode()?;
        let mut connection = self.connection(ConnectionId(operation.connection))?;
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
        if connection.pending.is_some() || self.active_intents >= 1024 {
            return Err(StoreError::RecoveryRequired);
        }
        if operation.stage != 0 || !operation.recovery.is_empty() {
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
        connection.pending = Some(operation.serial);
        connection.serial = operation.serial;
        connection.revision = operation.revision;
        let mut rows = vec![
            Row::Connection(connection),
            Row::Operation(operation.clone()),
        ];
        if operation.decision >= 10 {
            rows.push(Row::Conflict(format::ConflictData {
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
    fn append(&mut self, change: Change) -> Result<(), StoreError> {
        // An implicit checkpoint may collect pages before this event is durable.
        self.candidate_pin = match &change {
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
            let mut recovery_checks = RecoveryChecks::default();
            let mut prepared = self.prepare_rows(self.reduce(&event, &mut recovery_checks)?)?;
            let bytes = format::encode(&event)?;
            let control = matches!(
                event.change,
                Change::Fence { .. } | Change::Disconnect { .. }
            );
            if matches!(event.change, Change::Fence { .. }) && bytes.len() + 44 > 512 {
                return Err(StoreError::LimitExceeded);
            }
            if !prepared.fits_overlay() || self.log.metadata()?.len() > 8 * 1024 * 1024 {
                // Drop the prospective rows before page construction; checkpoint
                // can also update retention fields read by the reducer.
                drop(prepared);
                self.checkpoint()?;
                prepared = self.prepare_rows(self.reduce(&event, &mut recovery_checks)?)?;
            }
            if !prepared.fits_overlay() {
                return Err(StoreError::LimitExceeded);
            }
            let bound = if control {
                16 * 1024 * 1024
            } else {
                15 * 1024 * 1024
            };
            if self.log.metadata()?.len() + bytes.len() as u64 + 44 > bound {
                return Err(StoreError::LimitExceeded);
            }
            self.validate()?;
            let committed_sequence = format::next(self.sequence)?;
            let mut sequence = committed_sequence;
            touched = true;
            framing::write_record(&mut self.log, &mut sequence, &event)?;
            storage::sync_file(&self.log)?;
            self.log_identity = storage::regular(&self.log)?;
            self.validate()?;
            self.commit_rows(prepared);
            self.sequence = committed_sequence;
            self.digest = format::digest(&bytes);
            if matches!(event.change, Change::BaselineCommit { .. }) {
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
    pub(super) fn checkpoint(&mut self) -> Result<DurableAck<u64>, StoreError> {
        let result = (|| {
            self.validate()?;
            let sampled = std::time::Instant::now();
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
                        connection.pending.is_some() || entry.reason == 2,
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
#[derive(Default)]
struct RecoveryChecks {
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
        proof.validate(live)?;
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
    pub(super) fn validate(&self, live: &LiveOperation) -> Result<(), StoreError> {
        if !self.lifetime.ptr_eq(live.lifetime())
            || self.lifetime.strong_count() == 0
            || self.registration.connection != live.id.connection.0
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
    pub(super) fn native_namespace_records(
        &self,
        live: &LiveOperation,
    ) -> Result<Vec<NativeNamespaceRecord>, StoreError> {
        self.check_native(live)?;
        let mut records = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.table_page(live.id.connection, 5, cursor, 128)?;
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
                    lifetime: live.lifetime.clone(),
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
