//! File-byte preservation. This does not prove a later endpoint displacement.
use super::super::super::{migration::same_snapshot, nofollow::metadata_nofollow};
use super::{
    Arc, AtomicBool, Change, ConnectionId, ConnectionStore, DurableAck, EndpointIdentity, File,
    Lifecycle, OperationId, OperationReceipt, Page, PageCursor, Path, PreservedObject,
    RecoveryChecks, RecoveryEntry, RecoveryId, RecoveryReason, RecoveryReservation, RecoveryState,
    RelativePath, Row, Side, StoreError, VerifiedBaseline, format, storage,
};
use cap_primitives::fs::Metadata;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::Ordering,
    time::Instant,
};

fn object_directory(
    store: &ConnectionStore,
    id: RecoveryId,
    create: bool,
) -> Result<Arc<storage::Directory>, StoreError> {
    store.validate()?;
    let name = format::hex(&id.operation.connection.0);
    let parent = &store.storage.recovery;
    let directory = match metadata_nofollow(&parent.file, Path::new(&name)) {
        Ok(metadata) => parent.open_child(OsStr::new(&name), &metadata)?,
        Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
            let child = parent.create_child(OsStr::new(&name))?;
            storage::sync_directory(&child)?;
            storage::sync_directory(&parent)?;
            child
        }
        Err(error) => return Err(error.into()),
    };
    super::super::super::destination_security::check_private_file(&directory.file)?;
    Ok(directory)
}
fn name(id: RecoveryId) -> String {
    format!("{}-{}", id.operation.serial, id.slot)
}
fn cancelled(flag: &AtomicBool) -> Result<(), StoreError> {
    if flag.load(Ordering::Relaxed) {
        Err(StoreError::Cancelled)
    } else {
        Ok(())
    }
}
fn hash_file(
    file: &mut (impl Read + Seek),
    flag: &AtomicBool,
    expected: u64,
) -> Result<([u8; 32], u64), StoreError> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        cancelled(flag)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or(StoreError::LimitExceeded)?;
        if size > expected {
            return Err(StoreError::RecoveryRequired);
        }
        digest.update(buffer.get(..count).ok_or(StoreError::Corrupt)?);
    }
    if size != expected {
        return Err(StoreError::RecoveryRequired);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok((digest.finalize().into(), size))
}
impl ConnectionStore {
    pub(super) fn operation(&self, id: OperationId) -> Result<format::Operation, StoreError> {
        match self.row(&format::key(1, id.connection.0, id.serial))? {
            Some(Row::Operation(operation)) => Ok(operation),
            _ => Err(StoreError::Corrupt),
        }
    }
    pub(in super::super) fn reserve_recovery(
        &mut self,
        intent: &DurableAck<OperationId>,
        side: Side,
        path: RelativePath,
        reason: RecoveryReason,
    ) -> Result<RecoveryReservation, StoreError> {
        self.guard()?;
        self.check_ack(intent)?;
        let connection = self.connection(intent.value.connection)?;
        if connection.lifecycle != 0 {
            return Err(StoreError::Disconnected);
        }
        let operation = self.operation(intent.value)?;
        let slot =
            u32::try_from(operation.recovery.len()).map_err(|_| StoreError::LimitExceeded)?;
        let reason = match reason {
            RecoveryReason::NormalDeletion => 0,
            RecoveryReason::DisplacedVersion => 1,
            RecoveryReason::Conflict(id)
                if id.connection == intent.value.connection && id.serial == intent.value.serial =>
            {
                2
            }
            RecoveryReason::Conflict(_) => return Err(StoreError::StaleRevision),
        };
        let entry = format::Recovery {
            connection: connection.id,
            serial: intent.value.serial,
            slot,
            side: u8::from(side != Side::Source),
            path: path.into(),
            reason,
            state: 0,
            content: None,
            object: None,
            retained: 0,
        };
        let id = entry.id();
        self.append(Change::Reserve {
            entry,
            expected: connection.revision,
            fence: connection.fence,
        })?;
        Ok(RecoveryReservation {
            id,
            session: Arc::clone(&self.session),
            fence: connection.fence,
        })
    }
    pub(in super::super) fn preserve_file(
        &mut self,
        reservation: RecoveryReservation,
        source: &mut File,
        flag: &AtomicBool,
    ) -> Result<DurableAck<PreservedObject>, StoreError> {
        self.guard()?;
        #[cfg(windows)]
        {
            self.recovery_guards.retain(|_, guard| guard.is_live());
            // Same existing store ceiling as non-connection live keys. Each
            // entry corresponds to one preserved recovery row; no stale growth.
            if self.recovery_guards.len() >= 1_048_576 {
                return Err(StoreError::LimitExceeded);
            }
        }
        let RecoveryReservation { id, session, fence } = reservation;
        let result = (|| {
            if !Arc::ptr_eq(&self.session, &session) {
                return Err(StoreError::StaleRevision);
            }
            let connection = self.connection(id.operation.connection)?;
            if connection.lifecycle != 0 || connection.fence != fence {
                return Err(StoreError::Disconnected);
            }
            let mut entry = match self.row(&recovery_key(id))? {
                Some(Row::Recovery(entry)) if entry.state == 0 => entry,
                _ => return Err(StoreError::StaleRevision),
            };
            cancelled(flag)?;
            let before = Metadata::from_file(source)?;
            if !before.is_file() {
                return Err(StoreError::RecoveryRequired);
            }
            let parent = object_directory(self, id, true)?;
            let object_name = name(id);
            let mut target = storage::open_file(&parent, &object_name, true)?;
            let target_identity = storage::regular(&target)?;
            let (hash, size) = copy_bytes(source, &mut target, flag, &before)?;
            storage::sync_file(&target)?;
            storage::validate_file(&parent, &object_name, &target, &target_identity)?;
            // Independently reopen and reread the private object; never reuse the source hash as evidence.
            let mut reread = storage::reread_file(&parent, &object_name)?;
            let reread_identity = storage::regular(&reread)?;
            if storage::file_identity(&reread_identity) != storage::file_identity(&target_identity)
            {
                return Err(StoreError::IdentityChanged);
            }
            let (verified, count) = hash_file(&mut reread, flag, size)?;
            if verified != hash
                || count != size
                || !storage::same_snapshot(&reread_identity, &storage::regular(&reread)?)
            {
                return Err(StoreError::RecoveryRequired);
            }
            storage::validate_file(&parent, &object_name, &target, &target_identity)?;
            storage::sync_directory(&parent)?;
            self.validate()?;
            entry.state = 1;
            #[cfg(unix)]
            let executable = {
                use cap_primitives::fs::MetadataExt;
                before.mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            entry.content = Some(format::State::File {
                sha256: verified,
                bytes: size,
                executable,
            });
            entry.object = Some(format::ObjectIdentity {
                root: self.storage.identity.clone(),
                file_id: storage::file_identity(&reread_identity),
            });
            #[cfg(windows)]
            let (reread, guard) = storage::preserved_reader(target, reread)?;
            self.append(Change::Bytes {
                entry,
                expected: connection.revision,
                fence: connection.fence,
            })?;
            #[cfg(windows)]
            self.recovery_guards.insert(recovery_key(id), guard);
            Ok(self.ack(PreservedObject { id, file: reread }))
        })();
        if matches!(
            result,
            Err(StoreError::Persistence(_)
                | StoreError::RecoverySpace
                | StoreError::IdentityChanged)
        ) {
            self.poison_native();
        }
        result
    }
    pub(super) fn verify_recovery(
        &self,
        entry: &format::Recovery,
    ) -> Result<storage::RecoveryReader, StoreError> {
        self.validate()?;
        let object = entry.object.as_ref().ok_or(StoreError::RecoveryRequired)?;
        if object.root != self.storage.identity {
            return Err(StoreError::IdentityChanged);
        }
        let parent = object_directory(self, entry.id(), false)?;
        let object_name = name(entry.id());
        #[cfg(not(windows))]
        let mut file = storage::reread_file(&parent, &object_name)?;
        #[cfg(windows)]
        let mut file = storage::guarded_reader(
            &parent,
            &object_name,
            object.file_id,
            self.recovery_guards.get(&recovery_key(entry.id())),
        )?;
        let identity = storage::recovery_metadata(&file)?;
        if storage::file_identity(&identity) != object.file_id.require_platform()? {
            return Err(StoreError::IdentityChanged);
        }
        let Some(format::State::File { sha256, bytes, .. }) = entry.content else {
            return Err(StoreError::RecoveryRequired);
        };
        let (actual, size) = hash_file(&mut file, &AtomicBool::new(false), bytes)?;
        if actual != sha256
            || size != bytes
            || !storage::same_snapshot(&identity, &storage::recovery_metadata(&file)?)
        {
            return Err(StoreError::RecoveryRequired);
        }
        storage::validate_recovery_reader(&parent, &object_name, &file, &identity)?;
        Ok(file)
    }
    pub(super) fn reduce_recovery(
        &self,
        entry: &format::Recovery,
        expected: u64,
        fence: u64,
        verified: bool,
    ) -> Result<Vec<Row>, StoreError> {
        entry.decode()?;
        let mut connection = self.connection(ConnectionId(entry.connection))?;
        if connection.lifecycle != 0 || connection.fence != fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected || connection.pending != Some(entry.serial) {
            return Err(StoreError::StaleRevision);
        }
        let mut operation = self.operation(entry.id().operation)?;
        if operation.stage > 1 || entry.path != operation.path {
            return Err(StoreError::Corrupt);
        }
        let before = if entry.side == 0 {
            &operation.source
        } else {
            &operation.destination
        };
        if !matches!(before, Some(format::State::File { .. })) {
            return Err(StoreError::RecoveryRequired);
        }
        if (entry.reason == 2) != (operation.decision >= 10)
            || (entry.reason == 0 && !matches!(operation.decision, 3 | 4))
        {
            return Err(StoreError::Corrupt);
        }
        if verified {
            let Some(Row::Recovery(old)) = self.row(&entry.key())? else {
                return Err(StoreError::Corrupt);
            };
            if old.state != 0
                || entry.state != 1
                || old.connection != entry.connection
                || old.serial != entry.serial
                || old.slot != entry.slot
                || old.side != entry.side
                || old.path != entry.path
                || old.reason != entry.reason
                || entry
                    .object
                    .as_ref()
                    .is_none_or(|o| o.root != self.storage.identity)
            {
                return Err(StoreError::Corrupt);
            }
            if self.all_recovery_verified(&operation, entry)? {
                operation.stage = 1;
            }
        } else {
            if entry.state != 0
                || entry.slot as usize != operation.recovery.len()
                || operation.recovery.len() >= 128
                || self.row(&entry.key())?.is_some()
            {
                return Err(StoreError::Corrupt);
            }
            operation.recovery.push(entry.slot);
            operation.stage = 0;
        }
        connection.revision = format::next(connection.revision)?;
        operation.revision = connection.revision;
        let mut rows = vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::Recovery(entry.clone()),
        ];
        if entry.reason == 2 && verified {
            let Some(Row::Conflict(mut conflict)) =
                self.row(&format::key(3, entry.connection, entry.serial))?
            else {
                return Err(StoreError::Corrupt);
            };
            if !conflict.preserved.contains(&entry.slot) {
                conflict.preserved.push(entry.slot);
                conflict.preserved.sort_unstable();
            }
            conflict.revision = format::next(conflict.revision)?;
            rows.push(Row::Conflict(conflict));
        }
        Ok(rows)
    }
    fn all_recovery_verified(
        &self,
        operation: &format::Operation,
        entry: &format::Recovery,
    ) -> Result<bool, StoreError> {
        if !operation.recovery.contains(&entry.slot) {
            return Err(StoreError::Corrupt);
        }
        let mut source = false;
        let mut destination = false;
        for slot in &operation.recovery {
            let candidate = if *slot == entry.slot {
                entry.clone()
            } else {
                let Some(Row::Recovery(value)) = self.row(&recovery_key(RecoveryId {
                    operation: entry.id().operation,
                    slot: *slot,
                }))?
                else {
                    return Err(StoreError::Corrupt);
                };
                value
            };
            if candidate.state != 1 {
                return Ok(false);
            }
            if candidate.side == 0 {
                source = true;
            } else {
                destination = true;
            }
        }
        Ok((!requires(operation, 0) || source) && (!requires(operation, 1) || destination))
    }
    pub(in super::super) fn pending_page(
        &self,
        id: ConnectionId,
        cursor: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page<OperationReceipt>, StoreError> {
        let rows = self.table_page(id, 1, cursor, limit)?;
        Ok(Page {
            entries: rows
                .entries
                .into_iter()
                .map(|row| match row {
                    Row::Operation(o) => o.decode(),
                    _ => Err(StoreError::Corrupt),
                })
                .collect::<Result<_, _>>()?,
            next: rows.next,
        })
    }
    pub(in super::super) fn recovery_page(
        &self,
        id: ConnectionId,
        cursor: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page<RecoveryEntry>, StoreError> {
        let rows = self.table_page(id, 2, cursor, limit)?;
        Ok(Page {
            entries: rows
                .entries
                .into_iter()
                .map(|row| match row {
                    Row::Recovery(r) => r.decode(),
                    _ => Err(StoreError::Corrupt),
                })
                .collect::<Result<_, _>>()?,
            next: rows.next,
        })
    }
    pub(super) fn table_page(
        &self,
        id: ConnectionId,
        table: u8,
        cursor: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page<Row>, StoreError> {
        self.connection(id)?;
        if !(1..=128).contains(&limit) {
            return Err(StoreError::LimitExceeded);
        }
        let mut prefix = vec![table];
        prefix.extend(id.0);
        let after = match cursor {
            Some(c)
                if c.checkpoint == self.current.generation
                    && c.sequence == self.sequence
                    && c.last_key.starts_with(&prefix)
                    && c.last_key.len() <= 29 =>
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
            if !key.starts_with(&prefix) || after.as_ref().is_some_and(|after| &key <= after) {
                continue;
            }
            if matches!(&row, Row::Operation(operation) if matches!(operation.stage, 4 | 5)) {
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
            last = Some(key);
            entries.push(row);
        }
        Ok(Page { entries, next })
    }
}
fn recovery_key(id: RecoveryId) -> Vec<u8> {
    let mut key = format::key(2, id.operation.connection.0, id.operation.serial);
    key.extend(id.slot.to_be_bytes());
    key
}

pub(super) fn eligible(entry: &RecoveryEntry, lifecycle: Lifecycle, pinned: bool) -> bool {
    lifecycle == Lifecycle::Active
        && !pinned
        && entry.state == RecoveryState::Attached
        && entry.retained_seconds >= 2_592_000
}
pub(super) fn credit(
    entry: &mut RecoveryEntry,
    since: Instant,
    now: Instant,
    lifecycle: Lifecycle,
    pinned: bool,
) {
    if lifecycle != Lifecycle::Active || pinned || entry.state != RecoveryState::Attached {
        return;
    }
    if let Some(elapsed) = now.checked_duration_since(since)
        && let Some(total) = entry.retained_seconds.checked_add(elapsed.as_secs())
    {
        entry.retained_seconds = total;
    }
}

fn requires(operation: &format::Operation, side: u8) -> bool {
    let extant = if side == 0 {
        operation.source.is_some()
    } else {
        operation.destination.is_some()
    };
    extant
        && match operation.decision {
            1 | 4 => side == 1,
            2 | 3 => side == 0,
            10..=14 => true,
            _ => false,
        }
}
impl ConnectionStore {
    #[expect(
        clippy::too_many_lines,
        reason = "Publication checks retain the same recovery guards through all exact required-version joins."
    )]
    pub(super) fn reduce_outcome(
        &self,
        id: [u8; 16],
        serial: u64,
        expected: u64,
        source: &storage::Identity,
        destination: &storage::Identity,
        verified: bool,
        recovery_checks: &mut RecoveryChecks,
    ) -> Result<Vec<Row>, StoreError> {
        let mut connection = self.connection(ConnectionId(id))?;
        if connection.revision != expected
            || connection.pending != Some(serial)
            || &connection.source.identity != source
            || &connection.destination.identity != destination
        {
            return Err(StoreError::StaleRevision);
        }
        if connection.lifecycle == 2 {
            return Err(StoreError::Disconnected);
        }
        let mut operation = self.operation(OperationId {
            connection: ConnectionId(id),
            serial,
        })?;
        if verified {
            if operation.stage != 2 && !(operation.stage == 0 && operation.decision == 0) {
                return Err(StoreError::RecoveryRequired);
            }
            operation.stage = 3;
        } else {
            if !matches!(operation.stage, 1 | 6) || operation.decision == 0 {
                return Err(StoreError::RecoveryRequired);
            }
            let mut latest_native: Option<format::NativeRegistration> = None;
            for row in self.rows()? {
                if let Row::NativeRegistration(registration) = row? {
                    if registration.connection == operation.connection
                        && registration.serial == operation.serial
                        && latest_native
                            .as_ref()
                            .is_none_or(|old| old.attempt < registration.attempt)
                    {
                        latest_native = Some(registration);
                    }
                }
            }
            let native_intent = latest_native
                .as_ref()
                .map(|registration| {
                    super::super::transaction::native::descriptor::decode(
                        registration
                            .intent
                            .as_ref()
                            .ok_or(StoreError::RecoveryRequired)?,
                    )
                    .map_err(|_| StoreError::Corrupt)
                })
                .transpose()?;
            for side in [0, 1] {
                if requires(&operation, side) {
                    let mut found = false;
                    for slot in &operation.recovery {
                        if let Some(Row::Recovery(entry)) = self.row(&recovery_key(RecoveryId {
                            operation: operation.decode()?.id,
                            slot: *slot,
                        }))? && entry.side == side
                            && entry.state == 1
                        {
                            if let Some(intent) = &native_intent {
                                use super::super::transaction::native::descriptor as d;
                                let mut key = format::key(6, entry.connection, entry.serial);
                                key.extend(entry.slot.to_be_bytes());
                                let Some(Row::NativeRecoveryFact(bound)) = self.row(&key)? else {
                                    continue;
                                };
                                let fact = d::RecoveryFact::decode(&bound.fact)
                                    .map_err(|_| StoreError::Corrupt)?;
                                let displaced =
                                    d::decode(&bound.displaced).map_err(|_| StoreError::Corrupt)?;
                                let d::Body::Intent(body) = &intent.body else {
                                    return Err(StoreError::Corrupt);
                                };
                                let d::State::Regular {
                                    object, content, ..
                                } = body
                                    .expected
                                    .get(usize::from(side))
                                    .ok_or(StoreError::Corrupt)?
                                else {
                                    return Err(StoreError::Corrupt);
                                };
                                if fact.intent != intent.digest
                                    || fact.side != side
                                    || &fact.object != object
                                    || &fact.content != content
                                    || !matches!(
                                        displaced.body,
                                        d::Body::Displaced(d::Displaced {
                                            matches_expected: true,
                                            ..
                                        })
                                    )
                                {
                                    continue;
                                }
                            }
                            recovery_checks.verify(self, &entry)?;
                            found = true;
                        }
                    }
                    if !found {
                        return Err(StoreError::RecoveryRequired);
                    }
                }
            }
            operation.stage = 2;
        }
        connection.revision = format::next(connection.revision)?;
        operation.revision = connection.revision;
        Ok(vec![Row::Connection(connection), Row::Operation(operation)])
    }
    pub(super) fn read_baseline(
        &self,
        connection: &format::Connection,
        root: Option<[u8; 32]>,
        count: u32,
        revision: u64,
    ) -> Result<VerifiedBaseline, StoreError> {
        if count > 4096 || (count > 0) != root.is_some() {
            return Err(StoreError::LimitExceeded);
        }
        let mut entries = Vec::new();
        let mut previous = None;
        let mut bytes = 0_usize;
        for result in storage::TreeIter::new(&self.storage, root)? {
            let Row::Baseline(row) = result? else {
                return Err(StoreError::Corrupt);
            };
            if entries.len() >= 4096
                || row.connection != connection.id
                || row.revision != revision
                || row.ordinal as usize != entries.len()
            {
                return Err(StoreError::Corrupt);
            }
            bytes = bytes
                .checked_add(format::encode(&row)?.len())
                .ok_or(StoreError::LimitExceeded)?;
            if bytes > 32 * 1024 * 1024 {
                return Err(StoreError::LimitExceeded);
            }
            let path = row.path.to_relative()?;
            let logical = path.logical();
            if previous.as_ref().is_some_and(|old| old >= &logical) {
                return Err(StoreError::Corrupt);
            }
            previous = Some(logical);
            entries.push((path, row.state.into()));
        }
        if entries.len() != count as usize {
            return Err(StoreError::Corrupt);
        }
        VerifiedBaseline::from_verified(
            ConnectionId(connection.id),
            EndpointIdentity {
                volume: connection.source.identity.volume,
                root: connection.source.identity.root,
            },
            EndpointIdentity {
                volume: connection.destination.identity.volume,
                root: connection.destination.identity.root,
            },
            entries,
        )
        .map_err(|_| StoreError::Corrupt)
    }
    pub(super) fn reduce_baseline(
        &self,
        id: [u8; 16],
        expected: u64,
        root: Option<[u8; 32]>,
        count: u32,
        source: &storage::Identity,
        destination: &storage::Identity,
        initial_review: bool,
        recovery_checks: &mut RecoveryChecks,
    ) -> Result<Vec<Row>, StoreError> {
        let mut connection = self.connection(ConnectionId(id))?;
        if connection.revision != expected
            || &connection.source.identity != source
            || &connection.destination.identity != destination
        {
            return Err(StoreError::StaleRevision);
        }
        if connection.lifecycle != 0 || (connection.baseline.is_none() && !initial_review) {
            return Err(StoreError::RecoveryRequired);
        }
        let serial = connection.pending.ok_or(StoreError::RecoveryRequired)?;
        let mut operation = self.operation(OperationId {
            connection: ConnectionId(id),
            serial,
        })?;
        if operation.stage != 3 {
            return Err(StoreError::RecoveryRequired);
        }
        let revision = format::next(connection.revision)?;
        self.read_baseline(&connection, root, count, revision)?;
        let mut rows = Vec::new();
        for slot in &operation.recovery {
            let Some(Row::Recovery(mut entry)) = self.row(&recovery_key(RecoveryId {
                operation: operation.decode()?.id,
                slot: *slot,
            }))?
            else {
                return Err(StoreError::Corrupt);
            };
            if entry.state != 1 {
                return Err(StoreError::RecoveryRequired);
            }
            recovery_checks.verify(self, &entry)?;
            entry.state = 2;
            entry.retained = 0;
            rows.push(Row::Recovery(entry));
        }
        connection.baseline = Some(revision);
        connection.baseline_root = root;
        connection.baseline_count = count;
        connection.revision = revision;
        connection.pending = None;
        operation.stage = 4;
        operation.revision = revision;
        rows.push(Row::Connection(connection));
        rows.push(Row::Operation(operation));
        Ok(rows)
    }
}

fn copy_bytes(
    source: &mut File,
    target: &mut File,
    flag: &AtomicBool,
    before: &Metadata,
) -> Result<([u8; 32], u64), StoreError> {
    source.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        cancelled(flag)?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or(StoreError::LimitExceeded)?;
        if size > before.len() {
            return Err(StoreError::RecoveryRequired);
        }
        let chunk = buffer.get(..count).ok_or(StoreError::Corrupt)?;
        target.write_all(chunk)?;
        #[cfg(test)]
        {
            super::tests::after_copy_chunk()?;
            super::tests::fault()?;
        }
        digest.update(chunk);
    }
    if size != before.len() || !same_snapshot(before, &Metadata::from_file(source)?) {
        return Err(StoreError::RecoveryRequired);
    }
    Ok((digest.finalize().into(), size))
}

impl RecoveryChecks {
    #[cfg_attr(
        not(windows),
        expect(
            clippy::unused_self,
            clippy::needless_pass_by_ref_mut,
            reason = "Only Windows retains byte guards; Unix keeps the original immediate reader drop"
        )
    )]
    pub(super) fn verify(
        &mut self,
        store: &ConnectionStore,
        entry: &format::Recovery,
    ) -> Result<(), StoreError> {
        let reader = store.verify_recovery(entry)?;
        #[cfg(windows)]
        self.readers.push(reader);
        #[cfg(not(windows))]
        drop(reader);
        Ok(())
    }
}

impl ConnectionStore {
    pub(super) fn reduce_native_bound(
        &self,
        fact: &format::NativeRecoveryFact,
        expected: u64,
        fence: u64,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        fact.validate()?;
        let mut connection = self.connection(ConnectionId(fact.connection))?;
        if connection.lifecycle != 0 || connection.fence != fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected || connection.pending != Some(fact.serial) {
            return Err(StoreError::StaleRevision);
        }
        let id = RecoveryId {
            operation: OperationId {
                connection: ConnectionId(fact.connection),
                serial: fact.serial,
            },
            slot: fact.slot,
        };
        let mut operation = self.operation(id.operation)?;
        let Some(Row::Recovery(recovery)) = self.row(&recovery_key(id))? else {
            return Err(StoreError::Corrupt);
        };
        let mut registration_key = format::key(5, fact.connection, fact.serial);
        registration_key.extend(fact.attempt.to_be_bytes());
        let Some(Row::NativeRegistration(registration)) = self.row(&registration_key)? else {
            return Err(StoreError::Corrupt);
        };
        let intent = d::decode(registration.intent.as_ref().ok_or(StoreError::Corrupt)?)
            .map_err(|_| StoreError::Corrupt)?;
        let stage = registration
            .stage
            .as_ref()
            .map(|bytes| d::decode(bytes))
            .transpose()
            .map_err(|_| StoreError::Corrupt)?;
        let displaced = d::decode(&fact.displaced).map_err(|_| StoreError::Corrupt)?;
        displaced
            .joined(&intent, stage.as_ref(), None)
            .map_err(|_| StoreError::Corrupt)?;
        let bound = d::RecoveryFact::decode(&fact.fact).map_err(|_| StoreError::Corrupt)?;
        let d::Body::Displaced(displaced) = displaced.body else {
            return Err(StoreError::Corrupt);
        };
        let d::Body::Intent(intent_body) = intent.body else {
            return Err(StoreError::Corrupt);
        };
        let Some(format::State::File {
            sha256,
            bytes,
            executable,
        }) = recovery.content
        else {
            return Err(StoreError::Corrupt);
        };
        if recovery.state != 1
            || bound.intent != intent.digest
            || bound.side != recovery.side
            || bound.role != 0
            || Some(&bound.endpoint) != intent_body.endpoints.get(usize::from(bound.side))
            || bound.object != displaced.object
            || bound.side != displaced.side
            || displaced.barriers != 3
            || bound.content
                != (d::Content {
                    digest: sha256,
                    length: bytes,
                    executable,
                })
            || bound.artifact != artifact_view(recovery.object.as_ref().ok_or(StoreError::Corrupt)?)
        {
            return Err(StoreError::Corrupt);
        }
        if self.row(&fact.key())?.is_some() {
            return Err(StoreError::StaleRevision);
        }
        connection.revision = format::next(connection.revision)?;
        operation.revision = connection.revision;
        Ok(vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::NativeRecoveryFact(fact.clone()),
        ])
    }
}

fn artifact_view(
    value: &format::ObjectIdentity,
) -> super::super::transaction::native::descriptor::Artifact {
    use super::super::transaction::native::descriptor::Artifact;
    let mut endpoint = [0; 64];
    endpoint[..32].copy_from_slice(&value.root.volume);
    endpoint[32..].copy_from_slice(&value.root.root);
    let mut identity = [0; 24];
    let platform = match value.file_id {
        format::FileIdentity::Unix(inode) => {
            identity[..8].copy_from_slice(&inode.to_le_bytes());
            0
        }
        format::FileIdentity::Windows(id) => {
            identity[..8].copy_from_slice(&id.volume_serial.to_le_bytes());
            identity[8..].copy_from_slice(&id.file_id);
            1
        }
    };
    Artifact {
        endpoint,
        platform,
        identity,
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in super::super) fn reserve_native_recovery(
        &mut self,
        live: &mut super::LiveOperation,
        side: Side,
        reason: RecoveryReason,
    ) -> Result<RecoveryReservation, StoreError> {
        self.check_native(live)?;
        let next = format::next(live.revision())?;
        let reservation = self.reserve_recovery(
            &self.ack(live.id()),
            side,
            live.intent().path.clone(),
            reason,
        )?;
        live.revision = next;
        Ok(reservation)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Guarded copy, independent reread, barriers and two durable acknowledgements must remain in order."
    )]
    pub(in super::super) fn preserve_native(
        &mut self,
        live: &mut super::LiveOperation,
        reservation: RecoveryReservation,
        source: &mut super::super::transaction::native::NativeRecoveryReader<'_, '_>,
        flag: &AtomicBool,
    ) -> Result<DurableAck<super::BoundRecovery>, StoreError> {
        self.guard()?;
        self.check_native(live)?;
        source.validate_for(live)?;
        let RecoveryReservation { id, session, fence } = reservation;
        if !Arc::ptr_eq(&session, &self.session)
            || id.operation != live.id()
            || fence != live.fence()
        {
            return Err(StoreError::StaleRevision);
        }
        let result = (|| {
            let mut entry = match self.row(&recovery_key(id))? {
                Some(Row::Recovery(entry)) if entry.state == 0 => entry,
                _ => return Err(StoreError::StaleRevision),
            };
            let expected = source.content().clone();
            let parent = object_directory(self, id, true)?;
            let object_name = name(id);
            let mut target = storage::open_file(&parent, &object_name, true)?;
            let target_identity = storage::regular(&target)?;
            let mut buffer = vec![0; 256 * 1024];
            let mut offset = 0_u64;
            let mut copied = Sha256::new();
            loop {
                cancelled(flag)?;
                let count = source.read_chunk(offset, &mut buffer)?;
                if count == 0 {
                    break;
                }
                offset = offset
                    .checked_add(count as u64)
                    .ok_or(StoreError::LimitExceeded)?;
                if offset > expected.length {
                    return Err(StoreError::RecoveryRequired);
                }
                let bytes = buffer.get(..count).ok_or(StoreError::Corrupt)?;
                target.write_all(bytes)?;
                copied.update(bytes);
            }
            if offset != expected.length || <[u8; 32]>::from(copied.finalize()) != expected.digest {
                return Err(StoreError::RecoveryRequired);
            }
            source.validate()?;
            storage::sync_file(&target)?;
            storage::validate_file(&parent, &object_name, &target, &target_identity)?;
            let mut reread = storage::reread_file(&parent, &object_name)?;
            let reread_identity = storage::regular(&reread)?;
            if storage::file_identity(&reread_identity) != storage::file_identity(&target_identity)
            {
                return Err(StoreError::IdentityChanged);
            }
            let mut digest = Sha256::new();
            let mut verified = 0_u64;
            loop {
                cancelled(flag)?;
                source.charge_store_read(buffer.len() as u64)?;
                let count = reread.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                verified = verified
                    .checked_add(count as u64)
                    .ok_or(StoreError::LimitExceeded)?;
                if verified > expected.length {
                    return Err(StoreError::RecoveryRequired);
                }
                digest.update(buffer.get(..count).ok_or(StoreError::Corrupt)?);
            }
            if verified != expected.length
                || <[u8; 32]>::from(digest.finalize()) != expected.digest
                || !storage::same_snapshot(&reread_identity, &storage::regular(&reread)?)
            {
                return Err(StoreError::RecoveryRequired);
            }
            storage::validate_file(&parent, &object_name, &target, &target_identity)?;
            storage::validate_recovery_reader(&parent, &object_name, &reread, &reread_identity)?;
            storage::sync_directory(&parent)?;
            source.validate()?;
            cancelled(flag)?;
            self.check_native(live)?;
            entry.state = 1;
            entry.content = Some(format::State::File {
                sha256: expected.digest,
                bytes: expected.length,
                executable: expected.executable,
            });
            entry.object = Some(format::ObjectIdentity {
                root: self.storage.identity.clone(),
                file_id: storage::file_identity(&reread_identity),
            });
            let (attempt, fact, displaced) = source.fact(
                id,
                artifact_view(entry.object.as_ref().ok_or(StoreError::Corrupt)?),
            )?;
            if fact.side != entry.side {
                return Err(StoreError::Corrupt);
            }
            let next = format::next(live.revision())?;
            self.append(Change::Bytes {
                entry,
                expected: live.revision(),
                fence,
            })?;
            live.revision = next;
            #[cfg(test)]
            super::run_after_native_bytes();
            // A late break retains the durable Bytes artifact, but cannot mint a
            // bound live acknowledgement or replacement authority.
            source.validate()?;
            cancelled(flag)?;
            self.check_native(live)?;
            let next = format::next(live.revision())?;
            self.append(Change::NativeBound {
                fact: format::NativeRecoveryFact {
                    connection: id.operation.connection.0,
                    serial: id.operation.serial,
                    slot: id.slot,
                    attempt,
                    fact: fact.encode().map_err(|_| StoreError::Corrupt)?,
                    displaced,
                },
                expected: live.revision(),
                fence,
            })?;
            live.revision = next;
            source.validate()?;
            self.check_native(live)?;
            cancelled(flag)?;
            let proof = super::BoundRecovery {
                fact,
                preserved: PreservedObject { id, file: reread },
                parent,
                lifetime: live.lifetime().clone(),
                epoch: source.epoch(),
                metadata: reread_identity,
            };
            proof.validate(live, source.epoch())?;
            Ok(self.ack(proof))
        })();
        if matches!(
            result,
            Err(StoreError::Persistence(_)
                | StoreError::RecoverySpace
                | StoreError::IdentityChanged)
        ) {
            self.poison_native();
        }
        result
    }
}
