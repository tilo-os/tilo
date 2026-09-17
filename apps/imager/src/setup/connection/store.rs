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
pub(super) struct ConnectionStore {
    storage: storage::Storage,
    current: storage::Current,
    current_identity: cap_primitives::fs::Metadata,
    log: File,
    log_identity: cap_primitives::fs::Metadata,
    overlay: BTreeMap<Vec<u8>, Row>,
    overlay_bytes: usize,
    sequence: u64,
    digest: [u8; 32],
    session: Arc<()>,
    poisoned: bool,
    connections: u64,
    active_intents: u64,
    live_keys: u64,
    retention_sample: std::time::Instant,
    gc_scan: Option<storage::GcScan>,
    candidate_pin: Option<[u8; 32]>,
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
        let current_identity =
            storage::regular(&storage::open_file(&storage.root, "CURRENT", false)?)?;
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
            poisoned: false,
            connections: 0,
            active_intents: 0,
            live_keys: 0,
            retention_sample: std::time::Instant::now(),
            gc_scan: None,
            candidate_pin: None,
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
            let rows = store.reduce(&event).map_err(|_| StoreError::Corrupt)?;
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
        if !super::super::migration::same_snapshot(
            &self.log_identity,
            &storage::regular(&self.log)?,
        ) {
            return Err(StoreError::IdentityChanged);
        }
        Ok(())
    }
    fn guard(&mut self) -> Result<(), StoreError> {
        let result = self.validate();
        if result.is_err() {
            self.poisoned = true;
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
    pub(super) fn request_disconnect(
        &mut self,
        id: ConnectionId,
        expected: Revision,
    ) -> Result<DurableAck<DisconnectFence>, StoreError> {
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
    fn reduce(&self, event: &Event) -> Result<Vec<Row>, StoreError> {
        if event.schema != 1 || event.predecessor != self.digest {
            return Err(StoreError::Corrupt);
        }
        match &event.change {
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
            let mut prepared = self.prepare_rows(self.reduce(&event)?)?;
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
                prepared = self.prepare_rows(self.reduce(&event)?)?;
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
            self.poisoned = true;
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
            storage::sync_directory(&self.storage.root.file)?;
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
            self.poisoned = true;
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
    file: File,
}
impl ConnectionStore {
    fn gc_step(&mut self) -> Result<usize, StoreError> {
        self.guard()?;
        let result = self.collect_garbage();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

impl<T> DurableAck<T> {
    pub(super) fn value(&self) -> &T {
        &self.value
    }
}
