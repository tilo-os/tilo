//! File-byte preservation. This does not prove a later endpoint displacement.
pub(super) mod directory;
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

// This fact never leaves with_review_baseline_owner's callback. Its borrows
// identify one immutable effective/prospective view, not a reusable cache.
struct CheckedReviewOwner<'a> {
    store: &'a ConnectionStore,
    rows: &'a std::collections::BTreeMap<Vec<u8>, Row>,
    overlay: &'a std::collections::BTreeMap<Vec<u8>, Row>,
    review: &'a format::InitialChainReviewV1,
    connection: &'a format::Connection,
    owner: format::Connection,
    key: format::ReviewKey,
    current: storage::Current,
    current_identity: &'a storage::Metadata,
    log_identity: &'a storage::Metadata,
    sequence: u64,
    digest: [u8; 32],
    lifetime: std::sync::Weak<()>,
    context: Option<&'a super::NativeReadContext<'a>>,
    lease: storage::ReadLease,
}
impl CheckedReviewOwner<'_> {
    fn read(
        &self,
        root: Option<[u8; 32]>,
        count: u32,
        revision: u64,
    ) -> Result<VerifiedBaseline, StoreError> {
        let store = self.store;
        self.lease.check()?;
        store.storage.require_read_lease()?;
        store.check_history_context(self.context)?;
        if store.projection.borrow().is_some()
            || !store.same_native_lifetime(&self.lifetime)
            || !std::ptr::eq(self.overlay, &store.overlay)
            || (store.sequence, store.digest) != (self.sequence, self.digest)
            || (
                store.current.schema,
                &store.current.identity,
                store.current.generation,
                store.current.root,
                store.current.sequence,
                store.current.digest,
                store.current.log,
            ) != (
                self.current.schema,
                &self.current.identity,
                self.current.generation,
                self.current.root,
                self.current.sequence,
                self.current.digest,
                self.current.log,
            )
            || !std::ptr::eq(&store.current_identity, self.current_identity)
            || !std::ptr::eq(&store.log_identity, self.log_identity)
        {
            return Err(StoreError::StaleRevision);
        }
        let Some(Row::Connection(connection)) =
            self.rows.get(&format::key(0, self.key.connection, 0))
        else {
            return Err(StoreError::Corrupt);
        };
        let Some(Row::InitialChainReview(review)) =
            self.rows
                .get(&format::key(7, self.key.connection, self.key.origin))
        else {
            return Err(StoreError::Corrupt);
        };
        if !std::ptr::eq(connection, self.connection)
            || !std::ptr::eq(review, self.review)
            || connection.id != self.owner.id
            || connection.source != self.owner.source
            || connection.destination != self.owner.destination
            || revision > connection.revision
        {
            return Err(StoreError::Corrupt);
        }
        connection.decode()?;
        // Only the repeated main-owner lookup is replaced. CURRENT content,
        // log/namespace identity and every owning baseline read stay fresh.
        store.validate_with_context(self.context)?;
        let baseline = store.read_baseline_rows_with_context(
            connection,
            root,
            count,
            revision,
            self.context,
        )?;
        self.lease.check()?;
        store.check_history_context(self.context)?;
        Ok(baseline)
    }
}

fn object_directory(
    store: &ConnectionStore,
    id: RecoveryId,
    create: bool,
) -> Result<Arc<storage::Directory>, StoreError> {
    object_directory_with_context(store, id, create, None)
}
fn object_directory_with_context(
    store: &ConnectionStore,
    id: RecoveryId,
    create: bool,
    context: Option<&super::NativeReadContext<'_>>,
) -> Result<Arc<storage::Directory>, StoreError> {
    store.validate_with_context(context)?;
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
fn hash_file_with_context(
    file: &mut (impl Read + Seek),
    flag: &AtomicBool,
    expected: u64,
    context: Option<&super::NativeReadContext<'_>>,
) -> Result<([u8; 32], u64), StoreError> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        cancelled(flag)?;
        if let Some(context) = context {
            context.charge(buffer.len() as u64)?;
        }
        let read = file.read(&mut buffer);
        #[cfg(test)]
        storage::physical_trace::named("payload-hash", Some(expected), buffer.len(), &read);
        let count = read?;
        if let Some(context) = context {
            context.check()?;
        }
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
            captured: None,
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
        if self
            .operation(reservation.id.operation)?
            .native_budget
            .is_some()
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.preserve_file_with_context(reservation, source, flag, None)
    }
    pub(super) fn preserve_file_with_context(
        &mut self,
        reservation: RecoveryReservation,
        source: &mut File,
        flag: &AtomicBool,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<DurableAck<PreservedObject>, StoreError> {
        let _read_lease = self.storage.read_lease(false)?;
        self.guard_with_context(context)?;
        self.validate_payload_context(reservation.id.operation, context)?;
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
            let connection = self.connection_with_context(id.operation.connection, context)?;
            if connection.lifecycle != 0 || connection.fence != fence {
                return Err(StoreError::Disconnected);
            }
            let mut entry = match self.row_with_context(&recovery_key(id), context)? {
                Some(Row::Recovery(entry)) if entry.state == 0 => entry,
                _ => return Err(StoreError::StaleRevision),
            };
            cancelled(flag)?;
            let before = Metadata::from_file(source)?;
            if !before.is_file() {
                return Err(StoreError::RecoveryRequired);
            }
            let parent = object_directory_with_context(self, id, true, context)?;
            let object_name = name(id);
            let mut target = storage::open_file(&parent, &object_name, true)?;
            let target_identity = storage::regular(&target)?;
            let (hash, size) = copy_bytes(source, &mut target, flag, &before, context)?;
            storage::sync_file(&target)?;
            storage::validate_file(&parent, &object_name, &target, &target_identity)?;
            // Independently reopen and reread the private object; never reuse the source hash as evidence.
            let mut reread = storage::reread_file(&parent, &object_name)?;
            let reread_identity = storage::regular(&reread)?;
            if storage::file_identity(&reread_identity) != storage::file_identity(&target_identity)
            {
                return Err(StoreError::IdentityChanged);
            }
            let (verified, count) = hash_file_with_context(&mut reread, flag, size, context)?;
            if verified != hash
                || count != size
                || !storage::same_snapshot(&reread_identity, &storage::regular(&reread)?)
            {
                return Err(StoreError::RecoveryRequired);
            }
            storage::validate_file(&parent, &object_name, &target, &target_identity)?;
            storage::sync_directory(&parent)?;
            self.validate_with_context(context)?;
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
            let change = Change::Bytes {
                entry,
                expected: connection.revision,
                fence: connection.fence,
            };
            if let Some(context) = context {
                self.append_native(change, context)?;
            } else {
                self.append(change)?;
            }
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
    pub(super) fn retain_incomplete_recovery(
        &mut self,
        id: RecoveryId,
        expected: u64,
        fence: u64,
    ) -> Result<(), StoreError> {
        self.guard()?;
        self.append(Change::IncompleteRecovery {
            connection: id.operation.connection.0,
            serial: id.operation.serial,
            slot: id.slot,
            expected,
            fence,
        })
    }
    pub(super) fn reduce_incomplete_recovery(
        &self,
        id: RecoveryId,
        expected: u64,
        fence: u64,
    ) -> Result<Vec<Row>, StoreError> {
        let mut connection = self.connection(id.operation.connection)?;
        let mut operation = self.operation(id.operation)?;
        if connection.lifecycle != 0 || connection.fence != fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected || connection.pending != Some(id.operation.serial) {
            return Err(StoreError::StaleRevision);
        }
        let Some(Row::Recovery(mut entry)) = self.row(&recovery_key(id))? else {
            return Err(StoreError::Corrupt);
        };
        if operation.native_budget.is_none()
            || operation.stage > 1
            || !operation.recovery.contains(&id.slot)
            || entry.state != 0
        {
            return Err(StoreError::RecoveryRequired);
        }
        // No object is opened, adopted, verified, replaced or deleted. The exact
        // reserved slot remains pinned and can never become BytesVerified.
        entry.state = 5;
        entry.decode()?;
        connection.revision = format::next(expected)?;
        operation.revision = connection.revision;
        Ok(vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::Recovery(entry),
        ])
    }
    fn validate_payload_context(
        &self,
        id: OperationId,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let _read_lease = self.storage.read_lease(false)?;
        match (
            self.operation_with_context(id, context)?
                .native_budget
                .is_some(),
            context,
        ) {
            (false, None) => Ok(()),
            (true, Some(context))
                if context.id == id
                    && context
                        .lifetime
                        .ptr_eq(&Arc::downgrade(&self.native_lifetime)) =>
            {
                context.check()
            }
            _ => Err(StoreError::RecoveryRequired),
        }
    }
    pub(super) fn verify_recovery(
        &self,
        entry: &format::Recovery,
    ) -> Result<storage::RecoveryReader, StoreError> {
        if self
            .operation(entry.id().operation)?
            .native_budget
            .is_some()
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.verify_recovery_with_context(entry, None)
    }
    pub(super) fn verify_recovery_with_context(
        &self,
        entry: &format::Recovery,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<storage::RecoveryReader, StoreError> {
        let _read_lease = self.storage.read_lease(false)?;
        self.validate_with_context(context)?;
        self.validate_payload_context(entry.id().operation, context)?;
        if let Some(context) = context {
            if context.id != entry.id().operation
                || !context
                    .lifetime
                    .ptr_eq(&Arc::downgrade(&self.native_lifetime))
            {
                return Err(StoreError::StaleRevision);
            }
            context.check()?;
        }
        self.verify_recovery_file(entry, context)
    }
    // Callers must establish the canonical payer before entering this shared
    // byte/identity verifier. Ordinary reads retain their original operation join.
    fn verify_recovery_file(
        &self,
        entry: &format::Recovery,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<storage::RecoveryReader, StoreError> {
        let object = entry.object.as_ref().ok_or(StoreError::RecoveryRequired)?;
        if object.root != self.storage.identity {
            return Err(StoreError::IdentityChanged);
        }
        let parent = object_directory_with_context(self, entry.id(), false, context)?;
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
        let idle = AtomicBool::new(false);
        let flag = context.map_or(&idle, |context| context.cancel);
        let (actual, size) = hash_file_with_context(&mut file, flag, bytes, context)?;
        if actual != sha256
            || size != bytes
            || !storage::same_snapshot(&identity, &storage::recovery_metadata(&file)?)
        {
            return Err(StoreError::RecoveryRequired);
        }
        storage::validate_recovery_reader(&parent, &object_name, &file, &identity)?;
        Ok(file)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    pub(super) fn reduce_native_drain(
        &self,
        change: &Change,
        checks: &mut RecoveryChecks<'_>,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        let (entry, expected, operation_expected, fence, source, fact, selected) = match change {
            Change::NativeDrain {
                entry,
                expected,
                operation_expected,
                fence,
                source,
                fact,
            } => (
                entry,
                expected,
                operation_expected,
                fence,
                source,
                fact,
                false,
            ),
            Change::SelectedDrain {
                entry,
                expected,
                operation_expected,
                fence,
                source,
                fact,
            } => (
                entry,
                expected,
                operation_expected,
                fence,
                source,
                fact,
                true,
            ),
            _ => return Err(StoreError::Corrupt),
        };
        entry.decode()?;
        let mut connection = self.connection(ConnectionId(entry.connection))?;
        let mut operation = self.operation(entry.id().operation)?;
        if connection.revision != *expected
            || connection.fence != *fence
            || operation.revision != *operation_expected
            || connection
                .pending
                .is_some_and(|serial| serial != entry.serial)
        {
            return Err(StoreError::StaleRevision);
        }
        if operation.native_budget.is_none()
            || !matches!(operation.stage, 0 | 1 | 2 | 3 | 7)
            || entry.path != operation.path
            || entry.reason > 1
            || operation.action.is_some() != selected
            || source.outcome.is_some()
            || (!selected
                && ((entry.reason == 0) != matches!(operation.decision, 3 | 4)
                    || !matches!(operation.decision, 1..=4)))
        {
            return Err(StoreError::RecoveryRequired);
        }
        if selected {
            let Some(format::SelectedAction::Restore {
                target,
                replace: true,
                ..
            }) = &operation.action
            else {
                return Err(StoreError::RecoveryRequired);
            };
            if connection.lifecycle == 0
                || entry.reason != 1
                || entry.side != *target
                || operation.native_capture.is_some()
            {
                return Err(StoreError::RecoveryRequired);
            }
            checks.verify_stopped_selected(self, &operation)?;
        }
        if operation.stage == 7 {
            if !matches!(operation.native_boundary, Some(2 | 3))
                || operation
                    .native_boundary_evidence
                    .as_ref()
                    .is_none_or(|recorded| {
                        recorded.attempt != source.attempt || recorded.displaced != source.displaced
                    })
            {
                return Err(StoreError::RecoveryRequired);
            }
        } else if connection.pending != Some(entry.serial) {
            return Err(StoreError::StaleRevision);
        }
        let mut key = format::key(5, entry.connection, entry.serial);
        key.extend(source.attempt.to_be_bytes());
        let Some(Row::NativeRegistration(registration)) = self.row(&key)? else {
            return Err(StoreError::Corrupt);
        };
        registration.validate()?;
        let intent = d::decode(
            registration
                .intent
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        let stage = registration
            .stage
            .as_ref()
            .map(|bytes| d::decode(bytes))
            .transpose()
            .map_err(|_| StoreError::Corrupt)?;
        let displaced = d::decode(
            source
                .displaced
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        displaced
            .joined(&intent, stage.as_ref(), None)
            .map_err(|_| StoreError::Corrupt)?;
        let d::Body::Displaced(body) = &displaced.body else {
            return Err(StoreError::Corrupt);
        };
        if let Some(action) = &operation.action {
            let d::Body::Intent(before) = &intent.body else {
                return Err(StoreError::Corrupt);
            };
            let tombstone = matches!(before.selection, d::Selection::Tombstone { .. });
            if before.selection != self.selected_descriptor(&operation, action)?
                || stage.is_some() == tombstone
                || entry.side != 1 - before.direction
            {
                return Err(StoreError::RecoveryRequired);
            }
        }
        if registration.admission != format::next(operation.expected)?
            || registration.fence > *fence
            || body.barriers != 3
            || body.side != entry.side
            || displaced.header.progress > *expected
        {
            return Err(StoreError::RecoveryRequired);
        }
        let mut fact_row = None;
        match entry.state {
            1 if operation.recovery.contains(&entry.slot) => {
                let Some(Row::Recovery(mut previous)) = self.row(&entry.key())? else {
                    return Err(StoreError::Corrupt);
                };
                if previous.state != 0 {
                    return Err(StoreError::RecoveryRequired);
                }
                previous.state = 1;
                previous.content.clone_from(&entry.content);
                previous.object.clone_from(&entry.object);
                if previous != *entry {
                    return Err(StoreError::Corrupt);
                }
                let fact = fact.as_ref().ok_or(StoreError::RecoveryRequired)?;
                fact.validate()?;
                let bound = d::RecoveryFact::decode(&fact.fact).map_err(|_| StoreError::Corrupt)?;
                let d::Body::Intent(before) = &intent.body else {
                    return Err(StoreError::Corrupt);
                };
                let Some(format::State::File {
                    sha256,
                    bytes,
                    executable,
                }) = &entry.content
                else {
                    return Err(StoreError::Corrupt);
                };
                let object = entry.object.as_ref().ok_or(StoreError::Corrupt)?;
                if fact.connection != entry.connection
                    || fact.serial != entry.serial
                    || fact.slot != entry.slot
                    || fact.attempt != source.attempt
                    || Some(&fact.displaced) != source.displaced.as_ref()
                    || bound.intent != intent.digest
                    || bound.side != entry.side
                    || bound.role != 0
                    || bound.endpoint
                        != *before
                            .endpoints
                            .get(usize::from(entry.side))
                            .ok_or(StoreError::Corrupt)?
                    || bound.object != body.object
                    || bound.artifact != artifact_view(object)
                    || object.root != self.storage.identity
                    || bound.content
                        != (d::Content {
                            digest: *sha256,
                            length: *bytes,
                            executable: *executable,
                        })
                    || bound.content.length != body.stamp.length
                    || bound.content.executable != (body.stamp.mode & 0o111 != 0)
                    || (body.matches_expected
                        && !matches!(before.expected.get(usize::from(entry.side)).ok_or(StoreError::Corrupt)?, d::State::Regular { content, .. } if *content == bound.content))
                    || self.row(&fact.key())?.is_some()
                {
                    return Err(StoreError::RecoveryRequired);
                }
                checks.verify(self, entry)?;
                fact_row = Some(Row::NativeRecoveryFact(fact.clone()));
            }
            0 if fact.is_none()
                && entry.slot as usize == operation.recovery.len()
                && operation.recovery.len() < 128
                && self.row(&entry.key())?.is_none() =>
            {
                operation.recovery.push(entry.slot);
            }
            5 if fact.is_none() && operation.recovery.contains(&entry.slot) => {
                let Some(Row::Recovery(mut previous)) = self.row(&entry.key())? else {
                    return Err(StoreError::Corrupt);
                };
                if previous.state != 0 {
                    return Err(StoreError::RecoveryRequired);
                }
                previous.state = 5;
                if previous != *entry {
                    return Err(StoreError::Corrupt);
                }
            }
            _ => return Err(StoreError::RecoveryRequired),
        }
        connection.revision = format::next(*expected)?;
        operation.revision = connection.revision;
        let mut rows = vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::Recovery(entry.clone()),
        ];
        if let Some(fact) = fact_row {
            rows.push(fact);
        }
        Ok(rows)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    pub(super) fn reduce_recovery(
        &self,
        entry: &format::Recovery,
        expected: u64,
        fence: u64,
        verified: bool,
    ) -> Result<Vec<Row>, StoreError> {
        self.reduce_recovery_with_context(entry, expected, fence, verified, None)
    }
    pub(super) fn reduce_recovery_with_context(
        &self,
        entry: &format::Recovery,
        expected: u64,
        fence: u64,
        verified: bool,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<Vec<Row>, StoreError> {
        entry.decode()?;
        let mut connection =
            self.connection_with_context(ConnectionId(entry.connection), context)?;
        if connection.lifecycle != 0 || connection.fence != fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected || connection.pending != Some(entry.serial) {
            return Err(StoreError::StaleRevision);
        }
        let mut operation = self.operation_with_context(entry.id().operation, context)?;
        if operation.native_capture.is_some() && operation.action.is_some() {
            self.selected_collision_join_with_context(&operation, context)?;
        }
        if let Some(
            action @ format::SelectedAction::Restore {
                target, replace, ..
            },
        ) = &operation.action
            && operation.native_capture.is_none()
        {
            if !replace || entry.side != *target || entry.reason != 1 || entry.captured.is_some() {
                return Err(StoreError::RecoveryRequired);
            }
            let selection = self.selected_descriptor_with_context(&operation, action, context)?;
            let mut prepared = false;
            for row in self.rows_with_context(context)? {
                let Row::NativeRegistration(registration) = row? else {
                    continue;
                };
                if registration.connection != operation.connection
                    || registration.serial != operation.serial
                    || registration.side != *target
                {
                    continue;
                }
                registration.validate()?;
                let tombstone = matches!(
                    selection,
                    super::super::transaction::native::descriptor::Selection::Tombstone { .. }
                );
                if registration.outcome.is_some() || registration.stage.is_some() == tombstone {
                    continue;
                }
                let Some(bytes) = registration.intent else {
                    continue;
                };
                let descriptor = super::super::transaction::native::descriptor::decode(&bytes)
                    .map_err(|_| StoreError::Corrupt)?;
                let super::super::transaction::native::descriptor::Body::Intent(intent) =
                    descriptor.body
                else {
                    return Err(StoreError::Corrupt);
                };
                if intent.selection == selection {
                    prepared = true;
                }
            }
            if !prepared {
                return Err(StoreError::RecoveryRequired);
            }
        }
        if operation.stage > 1 || entry.path != operation.path {
            return Err(StoreError::Corrupt);
        }
        let before = if entry.side == 0 {
            &operation.source
        } else {
            &operation.destination
        };
        let captured = operation
            .native_capture
            .as_deref()
            .map(super::super::transaction::native::descriptor::ConflictSnapshot::decode)
            .transpose()
            .map_err(|_| StoreError::Corrupt)?;
        if let Some(capture) = &captured {
            let origin = entry
                .captured
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?;
            if Some(&origin.snapshot) != operation.native_capture.as_ref()
                || capture
                    .versions
                    .get(usize::from(origin.version))
                    .is_none_or(|version| {
                        version.side != entry.side
                            || !matches!(
                                version.state,
                                super::super::transaction::native::descriptor::State::Regular { .. }
                            )
                    })
            {
                return Err(StoreError::RecoveryRequired);
            }
        } else if entry.captured.is_some() || !matches!(before, Some(format::State::File { .. })) {
            return Err(StoreError::RecoveryRequired);
        }
        if (operation.action.is_none() || captured.is_some())
            && ((entry.reason == 2) != (operation.decision >= 10 || captured.is_some())
                || (entry.reason == 0 && !matches!(operation.decision, 3 | 4)))
        {
            return Err(StoreError::Corrupt);
        }
        if verified {
            let Some(Row::Recovery(old)) = self.row_with_context(&entry.key(), context)? else {
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
                || old.captured != entry.captured
                || entry
                    .object
                    .as_ref()
                    .is_none_or(|o| o.root != self.storage.identity)
            {
                return Err(StoreError::Corrupt);
            }
            if self.all_recovery_verified(&operation, entry, context)? {
                operation.stage = 1;
            }
        } else {
            if entry.state != 0
                || entry.slot as usize != operation.recovery.len()
                || operation.recovery.len() >= 128
                || self.row_with_context(&entry.key(), context)?.is_some()
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
                self.row_with_context(&format::key(3, entry.connection, entry.serial), context)?
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
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<bool, StoreError> {
        if !operation.recovery.contains(&entry.slot) {
            return Err(StoreError::Corrupt);
        }
        // Bytes alone cannot establish which captured object/version was copied.
        // NativeBound supplies that exact join separately.
        if operation.native_capture.is_some() || operation.action.is_some() {
            return Ok(false);
        }
        let mut source = false;
        let mut destination = false;
        for slot in &operation.recovery {
            let candidate = if *slot == entry.slot {
                entry.clone()
            } else {
                let Some(Row::Recovery(value)) = self.row_with_context(
                    &recovery_key(RecoveryId {
                        operation: entry.id().operation,
                        slot: *slot,
                    }),
                    context,
                )?
                else {
                    return Err(StoreError::Corrupt);
                };
                value
            };
            if candidate.state == 5 {
                continue;
            }
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
pub(super) fn recovery_key(id: RecoveryId) -> Vec<u8> {
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
        let context = match &recovery_checks.mode {
            super::VerificationMode::NativeLive(context) => Some(*context),
            _ => None,
        };
        let mut connection = self.connection_with_context(ConnectionId(id), context)?;
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
        let mut operation = self.operation_with_context(
            OperationId {
                connection: ConnectionId(id),
                serial,
            },
            context,
        )?;
        if operation.action.is_some() || operation.native_capture.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
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
            for row in self.rows_with_context(context)? {
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
                        if let Some(Row::Recovery(entry)) = self.row_with_context(
                            &recovery_key(RecoveryId {
                                operation: operation.decode()?.id,
                                slot: *slot,
                            }),
                            context,
                        )? && entry.side == side
                            && entry.state == 1
                        {
                            if let Some(intent) = &native_intent {
                                use super::super::transaction::native::descriptor as d;
                                let mut key = format::key(6, entry.connection, entry.serial);
                                key.extend(entry.slot.to_be_bytes());
                                let Some(Row::NativeRecoveryFact(bound)) =
                                    self.row_with_context(&key, context)?
                                else {
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
        self.read_baseline_with_context(connection, root, count, revision, None)
    }
    pub(super) fn read_baseline_with_context(
        &self,
        connection: &format::Connection,
        root: Option<[u8; 32]>,
        count: u32,
        revision: u64,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<VerifiedBaseline, StoreError> {
        if let Some(context) = context {
            if context.id.connection.0 != connection.id {
                return Err(StoreError::StaleRevision);
            }
            context.check()?;
        }
        self.read_baseline_rows_with_context(connection, root, count, revision, context)
    }
    // Store-wide closure validates an actual historical owner independently of
    // the operation paying for its metadata. Active baseline access above keeps
    // its stricter payer/owner check.
    pub(super) fn read_historical_baseline_with_context(
        &self,
        connection: &format::Connection,
        root: Option<[u8; 32]>,
        count: u32,
        revision: u64,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<VerifiedBaseline, StoreError> {
        self.check_history_context(context)?;
        connection.decode()?;
        let owner = self.connection_with_context(ConnectionId(connection.id), context)?;
        if owner.source != connection.source
            || owner.destination != connection.destination
            || revision > connection.revision
        {
            return Err(StoreError::Corrupt);
        }
        self.read_baseline_rows_with_context(connection, root, count, revision, context)
    }
    pub(super) fn with_review_baseline_owner<'a>(
        &'a self,
        review: &'a format::InitialChainReviewV1,
        rows: &'a std::collections::BTreeMap<Vec<u8>, Row>,
        context: Option<&'a super::NativeReadContext<'a>>,
        validate: impl FnOnce(
            &dyn Fn(Option<[u8; 32]>, u32, u64) -> Result<VerifiedBaseline, StoreError>,
        ) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        // Require the caller's lease before nesting, including the inert path.
        // The nested owner holds the SAME counters/deadline through the callback.
        self.storage.require_read_lease()?;
        self.check_history_context(context)?;
        if self.projection.borrow().is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        let lease = self.storage.read_lease(false)?;
        let Some(Row::Connection(connection)) = rows.get(&format::key(0, review.connection, 0))
        else {
            return Err(StoreError::Corrupt);
        };
        let Some(Row::InitialChainReview(bound_review)) =
            rows.get(&format::key(7, review.connection, review.original_revision))
        else {
            return Err(StoreError::Corrupt);
        };
        if !std::ptr::eq(bound_review, review) {
            return Err(StoreError::Corrupt);
        }
        connection.decode()?;
        let original = review.original_record()?;
        let owner = self.connection_with_context(ConnectionId(review.connection), context)?;
        owner.decode()?;
        if owner.id != connection.id
            || owner.source != connection.source
            || owner.destination != connection.destination
            || original.source != connection.source
            || original.destination != connection.destination
            || owner.source.identity != review.endpoints[0]
            || owner.destination.identity != review.endpoints[1]
        {
            return Err(StoreError::Corrupt);
        }
        let fact = CheckedReviewOwner {
            store: self,
            rows,
            overlay: &self.overlay,
            review,
            connection,
            owner,
            key: format::ReviewKey {
                connection: review.connection,
                review: review.identity()?,
                origin: review.original_revision,
            },
            current: self.current.clone(),
            current_identity: &self.current_identity,
            log_identity: &self.log_identity,
            sequence: self.sequence,
            digest: self.digest,
            lifetime: Arc::downgrade(&self.native_lifetime),
            context,
            lease,
        };
        fact.lease.check()?;
        self.check_history_context(context)?;
        validate(&|root, count, revision| fact.read(root, count, revision))?;
        fact.lease.check()?;
        self.check_history_context(context)
    }
    fn read_baseline_rows_with_context(
        &self,
        connection: &format::Connection,
        root: Option<[u8; 32]>,
        count: u32,
        revision: u64,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<VerifiedBaseline, StoreError> {
        if count > 4096 || (count > 0) != root.is_some() {
            return Err(StoreError::LimitExceeded);
        }
        let mut entries = Vec::new();
        let mut previous = None;
        let mut bytes = 0_usize;
        let meter = |count: usize| {
            context
                .ok_or(StoreError::RecoveryRequired)?
                .charge(count as u64)
        };
        let iterator = if context.is_some() {
            storage::TreeIter::new_metered_baseline(&self.storage, root, count, &meter)?
        } else {
            storage::TreeIter::new_context(&self.storage, root, Some(count), None)?
        };
        for result in iterator {
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
        self.storage.review_charge(0, 0, 0, 0)?;
        if let Some(context) = context {
            context.check()?;
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
    pub(super) fn native_baseline_value(
        &self,
        operation: &format::Operation,
    ) -> Result<Option<super::EntryState>, StoreError> {
        self.native_baseline_value_with_context(operation, None)
    }
    pub(super) fn native_baseline_value_with_context(
        &self,
        operation: &format::Operation,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<Option<super::EntryState>, StoreError> {
        if let Some(context) = context {
            context.check()?;
        }
        if operation.native_kind == Some(format::NativeOperationKind::DirectoryV1) {
            return Self::native_directory_baseline_value_with_rows(
                operation,
                self.rows_with_context(context)?,
            )
            .map(Some);
        }
        if operation.action.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        if operation.decision == 0 {
            return Ok(operation.source.clone().map(Into::into));
        }
        use super::super::transaction::native::descriptor as d;
        let mut latest = None;
        for row in self.rows_with_context(context)? {
            if let Row::NativeRegistration(value) = row? {
                if value.connection == operation.connection
                    && value.serial == operation.serial
                    && latest
                        .as_ref()
                        .is_none_or(|old: &format::NativeRegistration| old.attempt < value.attempt)
                {
                    latest = Some(value);
                }
            }
        }
        let registration = latest.ok_or(StoreError::RecoveryRequired)?;
        registration.validate()?;
        let outcome = d::decode(
            registration
                .outcome
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        let d::Body::Outcome(body) = outcome.body else {
            return Err(StoreError::Corrupt);
        };
        let state = |value: &d::State| match value {
            d::State::Absent => Ok(None),
            d::State::Regular { content, .. } => Ok(Some(super::EntryState::File {
                sha256: content.digest,
                bytes: content.length,
                executable: content.executable,
            })),
            _ => Err(StoreError::RecoveryRequired),
        };
        let source = state(&body.states[0])?;
        if source != state(&body.states[1])? {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(source)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    pub(super) fn native_baseline_patch(
        &self,
        operation: &format::Operation,
        mut patch: std::collections::BTreeMap<RelativePath, super::EntryState>,
        chain_first: bool,
        existing_ancestors: Option<&format::ExistingAncestorPatchV1>,
        directory_ancestors: Option<&format::DirectoryAncestorPatchV1>,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<std::collections::BTreeMap<RelativePath, super::EntryState>, StoreError> {
        let value = self.native_baseline_value_with_context(operation, context)?;
        if operation.native_kind == Some(format::NativeOperationKind::DirectoryV1) {
            if existing_ancestors.is_some()
                || (if chain_first {
                    directory::first_chain_ancestor_patch(operation, &patch)?
                } else {
                    directory::ancestor_patch(operation, &patch)?
                })
                .as_ref()
                    != directory_ancestors
                || value != Some(super::EntryState::Directory)
            {
                return Err(StoreError::RecoveryRequired);
            }
            let requirement =
                super::super::transaction::ExistingAncestorRequirement::from_baseline(
                    &operation.path.to_relative()?,
                    &patch,
                )?;
            for path in requirement.paths() {
                patch.insert(path, super::EntryState::Directory);
            }
        }
        if let Some(receipt) = existing_ancestors {
            let expected = ancestor_patch(operation, &patch)?;
            if expected.as_ref() != Some(receipt)
                || !matches!(value, Some(super::EntryState::File { .. }))
            {
                return Err(StoreError::RecoveryRequired);
            }
            let requirement =
                super::super::transaction::ExistingAncestorRequirement::from_baseline(
                    &operation.path.to_relative()?,
                    &patch,
                )?;
            for path in requirement.paths() {
                patch.insert(path, super::EntryState::Directory);
            }
        }
        let relative_path = operation.path.to_relative()?;
        if let Some(value) = value {
            patch.insert(relative_path, value);
        } else {
            patch.remove(&relative_path);
        }
        Ok(patch)
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
        existing_ancestors: Option<&format::ExistingAncestorPatchV1>,
        directory_ancestors: Option<&format::DirectoryAncestorPatchV1>,
        chain_commit: Option<&format::DirectoryChainCommitV1>,
        first_equality: Option<&super::chain_review::FirstReviewedEquality>,
        recovery_checks: &mut RecoveryChecks,
    ) -> Result<Vec<Row>, StoreError> {
        let context = match &recovery_checks.mode {
            super::VerificationMode::NativeLive(context) => Some(*context),
            _ => None,
        };
        let mut connection = self.connection_with_context(ConnectionId(id), context)?;
        #[cfg(test)]
        let _trace_reduction = storage::physical_trace::reduction(root, connection.baseline_root);
        if connection.revision != expected
            || &connection.source.identity != source
            || &connection.destination.identity != destination
        {
            return Err(StoreError::StaleRevision);
        }
        let chain_first =
            self.validate_chain_baseline(&connection, chain_commit, initial_review, context)?;
        if connection.lifecycle != 0
            || (connection.baseline.is_none()
                && !initial_review
                && !chain_first
                && first_equality.is_none())
        {
            return Err(StoreError::RecoveryRequired);
        }
        let serial = connection.pending.ok_or(StoreError::RecoveryRequired)?;
        let mut operation = self.operation_with_context(
            OperationId {
                connection: ConnectionId(id),
                serial,
            },
            context,
        )?;
        if operation.native_kind.is_none() && directory_ancestors.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        if operation.stage != 3
            || (operation.native_budget.is_some()
                && connection.baseline.is_none()
                && operation.initial_review.is_none()
                && !chain_first
                && !first_equality.is_some_and(|proof| proof.permits(&connection, &operation)))
        {
            return Err(StoreError::RecoveryRequired);
        }
        let revision = format::next(connection.revision)?;
        let candidate =
            self.read_baseline_with_context(&connection, root, count, revision, context)?;
        if existing_ancestors.is_some() && operation.native_budget.is_none() {
            return Err(StoreError::RecoveryRequired);
        }
        if operation.native_budget.is_some() {
            let mut patch = if let Some(old_revision) = connection.baseline {
                self.read_baseline_with_context(
                    &connection,
                    connection.baseline_root,
                    connection.baseline_count,
                    old_revision,
                    context,
                )?
                .entries()
                .cloned()
                .unwrap_or_default()
            } else {
                std::collections::BTreeMap::default()
            };
            patch = self.native_baseline_patch(
                &operation,
                patch,
                chain_first,
                existing_ancestors,
                directory_ancestors,
                context,
            )?;
            if candidate.entries() != Some(&patch) {
                return Err(StoreError::RecoveryRequired);
            }
        }
        let mut rows = Vec::new();
        for mut applied in
            self.selected_attention_with_context(ConnectionId(id), &operation.path, context)?
        {
            recovery_checks.verify_applied_versions(self, &applied, &operation)?;
            if let Some(format::SelectedAction::Restore {
                conflict: Some(selection),
                ..
            }) = &applied.action
            {
                let Some(Row::Conflict(mut conflict)) =
                    self.row_with_context(&format::key(3, id, selection.serial), context)?
                else {
                    return Err(StoreError::RecoveryRequired);
                };
                if conflict.revision != selection.revision
                    || conflict.resolved
                    || conflict.path != operation.path
                {
                    return Err(StoreError::StaleRevision);
                }
                conflict.resolved = true;
                conflict.revision = format::next(conflict.revision)?;
                rows.push(Row::Conflict(conflict));
            }
            applied
                .selected_applied
                .as_mut()
                .ok_or(StoreError::Corrupt)?
                .attention = false;
            rows.push(Row::Operation(applied));
        }
        for slot in &operation.recovery {
            let Some(Row::Recovery(mut entry)) = self.row_with_context(
                &recovery_key(RecoveryId {
                    operation: operation.decode()?.id,
                    slot: *slot,
                }),
                context,
            )?
            else {
                return Err(StoreError::Corrupt);
            };
            if entry.state == 5 {
                continue;
            }
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
        self.complete_chain_baseline(&mut connection, &mut operation, &mut rows, context)?;
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
    context: Option<&super::NativeReadContext<'_>>,
) -> Result<([u8; 32], u64), StoreError> {
    source.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        cancelled(flag)?;
        if let Some(context) = context {
            context.charge(buffer.len() as u64)?;
        }
        let read = source.read(&mut buffer);
        #[cfg(test)]
        storage::physical_trace::named("payload-copy", Some(before.len()), buffer.len(), &read);
        let count = read?;
        if let Some(context) = context {
            context.check()?;
        }
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

impl RecoveryChecks<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    #[cfg_attr(
        not(windows),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "Windows retains verified recovery readers in self through the dependent durable transition."
        )
    )]
    fn verify_applied_versions(
        &mut self,
        store: &ConnectionStore,
        applied: &format::Operation,
        current: &format::Operation,
    ) -> Result<(), StoreError> {
        let context = match &self.mode {
            super::VerificationMode::NativeLive(context) => Some(*context),
            _ => None,
        };
        let Some(action @ format::SelectedAction::Restore { selected, .. }) = &applied.action
        else {
            return Err(StoreError::Corrupt);
        };
        store.selected_descriptor_with_context(applied, action, context)?;
        let content = match selected {
            format::SelectedVersion::Recovery { content, .. } => Some(content),
            format::SelectedVersion::Tombstone { .. } => None,
        };
        if current.action.is_some()
            || current.decision != 0
            || current.native_budget.is_none()
            || applied.path != current.path
            || applied.connection != current.connection
            || current.source.as_ref() != content
            || current.destination.as_ref() != content
            || !applied
                .selected_applied
                .as_ref()
                .is_some_and(|fact| fact.attention)
        {
            return Err(StoreError::RecoveryRequired);
        }
        let payer = OperationId {
            connection: ConnectionId(current.connection),
            serial: current.serial,
        };
        match &self.mode {
            super::VerificationMode::NativeReplay(id) if *id == payer => {}
            super::VerificationMode::NativeLive(read_access)
                if read_access.id == payer
                    && !read_access.recovery_only
                    && !read_access.inspection_only
                    && read_access
                        .lifetime
                        .ptr_eq(&Arc::downgrade(&store.native_lifetime)) =>
            {
                read_access.check()?;
                store.validate_with_context(context)?;
            }
            _ => return Err(StoreError::RecoveryRequired),
        }
        let mut versions = Vec::new();
        if let format::SelectedVersion::Recovery {
            operation: selected_serial,
            slot,
            ..
        } = selected
        {
            versions.push(RecoveryId {
                operation: OperationId {
                    connection: payer.connection,
                    serial: *selected_serial,
                },
                slot: *slot,
            });
        }
        versions.extend(applied.recovery.iter().map(|slot| RecoveryId {
            operation: OperationId {
                connection: payer.connection,
                serial: applied.serial,
            },
            slot: *slot,
        }));
        for id in versions {
            let Some(Row::Recovery(entry)) = store.row_with_context(&recovery_key(id), context)?
            else {
                return Err(StoreError::Corrupt);
            };
            if entry.state == 5 {
                continue;
            }
            if !matches!(entry.state, 1 | 2) || entry.path != applied.path {
                return Err(StoreError::RecoveryRequired);
            }
            entry.decode()?;
            let object = entry.object.as_ref().ok_or(StoreError::RecoveryRequired)?;
            if object.root != store.storage.identity {
                return Err(StoreError::IdentityChanged);
            }
            match &self.mode {
                super::VerificationMode::NativeReplay(id) if *id == payer => {
                    object.file_id.require_platform()?;
                }
                super::VerificationMode::NativeLive(read_access)
                    if read_access.id == payer
                        && !read_access.recovery_only
                        && !read_access.inspection_only
                        && read_access
                            .lifetime
                            .ptr_eq(&Arc::downgrade(&store.native_lifetime)) =>
                {
                    read_access.check()?;
                    store.validate_with_context(context)?;
                    let reader = store.verify_recovery_file(&entry, Some(read_access))?;
                    #[cfg(windows)]
                    self.readers.push(reader);
                    #[cfg(not(windows))]
                    drop(reader);
                }
                _ => return Err(StoreError::RecoveryRequired),
            }
        }
        Ok(())
    }
    #[cfg_attr(
        not(windows),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "Windows retains verified recovery readers in self through the dependent durable transition."
        )
    )]
    pub(super) fn verify_selected(
        &mut self,
        store: &ConnectionStore,
        operation: &format::Operation,
    ) -> Result<(), StoreError> {
        let context = match &self.mode {
            super::VerificationMode::NativeLive(context) => Some(*context),
            _ => None,
        };
        let action = operation
            .action
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        store.selected_descriptor_with_context(operation, action, context)?;
        let format::SelectedAction::Restore { selected, .. } = action;
        let id = OperationId {
            connection: ConnectionId(operation.connection),
            serial: operation.serial,
        };
        if matches!(selected, format::SelectedVersion::Tombstone { .. }) {
            if operation.native_budget.is_none() {
                return Err(StoreError::RecoveryRequired);
            }
            return match &self.mode {
                super::VerificationMode::NativeReplay(replayed) if *replayed == id => Ok(()),
                super::VerificationMode::NativeLive(context)
                    if context.id == id
                        && !context.recovery_only
                        && !context.inspection_only
                        && context
                            .lifetime
                            .ptr_eq(&Arc::downgrade(&store.native_lifetime)) =>
                {
                    store.validate_with_context(Some(context))?;
                    context.check()
                }
                _ => Err(StoreError::RecoveryRequired),
            };
        }
        let format::SelectedVersion::Recovery {
            operation: serial,
            slot,
            ..
        } = selected
        else {
            return Err(StoreError::RecoveryRequired);
        };
        let selected = RecoveryId {
            operation: OperationId {
                connection: id.connection,
                serial: *serial,
            },
            slot: *slot,
        };
        let Some(Row::Recovery(entry)) =
            store.row_with_context(&recovery_key(selected), context)?
        else {
            return Err(StoreError::RecoveryRequired);
        };
        entry.decode()?;
        if operation.native_budget.is_none() {
            return Err(StoreError::RecoveryRequired);
        }
        match &self.mode {
            super::VerificationMode::NativeReplay(replayed) if *replayed == id => {
                entry
                    .object
                    .as_ref()
                    .ok_or(StoreError::RecoveryRequired)?
                    .file_id
                    .require_platform()?;
                Ok(())
            }
            super::VerificationMode::NativeLive(context)
                if context.id == id
                    && !context.recovery_only
                    && !context.inspection_only
                    && context
                        .lifetime
                        .ptr_eq(&Arc::downgrade(&store.native_lifetime)) =>
            {
                store.validate_with_context(Some(context))?;
                context.check()?;
                let reader = store.verify_recovery_file(&entry, Some(context))?;
                #[cfg(windows)]
                self.readers.push(reader);
                #[cfg(not(windows))]
                drop(reader);
                Ok(())
            }
            _ => Err(StoreError::RecoveryRequired),
        }
    }
    #[cfg_attr(
        not(windows),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "Windows retains verified recovery readers in self through the dependent durable transition."
        )
    )]
    pub(super) fn verify(
        &mut self,
        store: &ConnectionStore,
        entry: &format::Recovery,
    ) -> Result<(), StoreError> {
        use super::VerificationMode;
        let context = match &self.mode {
            VerificationMode::NativeLive(context) => Some(*context),
            _ => None,
        };
        let marked = store
            .operation_with_context(entry.id().operation, context)?
            .native_budget
            .is_some();
        let reader = match (&self.mode, marked) {
            (VerificationMode::Legacy, false) => store.verify_recovery(entry)?,
            (VerificationMode::NativeLive(context), true) if context.id == entry.id().operation => {
                store.verify_recovery_with_context(entry, Some(context))?
            }
            (VerificationMode::NativeReplay(id), true) if *id == entry.id().operation => {
                entry.decode()?;
                let object = entry.object.as_ref().ok_or(StoreError::RecoveryRequired)?;
                if object.root != store.storage.identity {
                    return Err(StoreError::IdentityChanged);
                }
                object.file_id.require_platform()?;
                return Ok(());
            }
            _ => return Err(StoreError::RecoveryRequired),
        };
        #[cfg(windows)]
        self.readers.push(reader);
        #[cfg(not(windows))]
        drop(reader);
        Ok(())
    }
}

impl ConnectionStore {
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    pub(super) fn native_recovery_binding(
        &self,
        recovery: &format::Recovery,
        registration: &format::NativeRegistration,
        fact: &format::NativeRecoveryFact,
        committed: bool,
    ) -> Result<
        (
            super::super::transaction::native::descriptor::Intent,
            super::super::transaction::native::descriptor::Displaced,
            super::super::transaction::native::descriptor::RecoveryFact,
        ),
        StoreError,
    > {
        use super::super::transaction::native::descriptor as d;
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
        if !(recovery.state == 1 || committed && recovery.state == 2)
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
        Ok((intent_body, displaced, bound))
    }
    pub(super) fn reduce_native_bound(
        &self,
        fact: &format::NativeRecoveryFact,
        expected: u64,
        fence: u64,
    ) -> Result<Vec<Row>, StoreError> {
        self.reduce_native_bound_with_context(fact, expected, fence, None)
    }
    pub(super) fn reduce_native_bound_with_context(
        &self,
        fact: &format::NativeRecoveryFact,
        expected: u64,
        fence: u64,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        fact.validate()?;
        if fact.captured.is_some() {
            return self.reduce_captured_bound_with_context(fact, expected, fence, context);
        }
        let mut connection =
            self.connection_with_context(ConnectionId(fact.connection), context)?;
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
        let mut operation = self.operation_with_context(id.operation, context)?;
        let Some(Row::Recovery(recovery)) = self.row_with_context(&recovery_key(id), context)?
        else {
            return Err(StoreError::Corrupt);
        };
        let mut registration_key = format::key(5, fact.connection, fact.serial);
        registration_key.extend(fact.attempt.to_be_bytes());
        let Some(Row::NativeRegistration(registration)) =
            self.row_with_context(&registration_key, context)?
        else {
            return Err(StoreError::Corrupt);
        };
        let (intent_body, displaced, bound) =
            self.native_recovery_binding(&recovery, &registration, fact, false)?;
        if self.row_with_context(&fact.key(), context)?.is_some() {
            return Err(StoreError::StaleRevision);
        }
        if let Some(
            action @ format::SelectedAction::Restore {
                target, replace, ..
            },
        ) = &operation.action
        {
            let Some(d::State::Regular {
                object, content, ..
            }) = intent_body.expected.get(usize::from(*target))
            else {
                return Err(StoreError::RecoveryRequired);
            };
            if !replace
                || bound.side != *target
                || !displaced.matches_expected
                || &bound.object != object
                || &bound.content != content
                || recovery.reason != 1
                || intent_body.selection
                    != self.selected_descriptor_with_context(&operation, action, context)?
                || operation.stage > 1
            {
                return Err(StoreError::RecoveryRequired);
            }
            operation.stage = 1;
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

pub(super) fn artifact_view(
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
        if !live.action().is_reconciliation() {
            return Err(StoreError::RecoveryRequired);
        }
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
        source.validate_for(self, live)?;
        source.checkpoint()?;
        let context = self.native_read_context(live, source.deadline(), flag)?;
        let RecoveryReservation { id, session, fence } = reservation;
        if !Arc::ptr_eq(&session, &self.session)
            || id.operation != live.id()
            || fence != live.fence()
        {
            return Err(StoreError::StaleRevision);
        }
        let mut durable = false;
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
            source.checkpoint()?;
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
            source.checkpoint()?;
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
            self.append_native(
                Change::Bytes {
                    entry,
                    expected: live.revision(),
                    fence,
                },
                &context,
            )?;
            durable = true;
            let previous = live.revision;
            live.revision = next;
            source.advance_selected(self, live, previous)?;
            #[cfg(test)]
            super::run_after_native_bytes();
            // A late break retains the durable Bytes artifact, but cannot mint a
            // bound live acknowledgement or replacement authority.
            source.checkpoint()?;
            cancelled(flag)?;
            self.check_native(live)?;
            let next = format::next(live.revision())?;
            self.append_native(
                Change::NativeBound {
                    fact: format::NativeRecoveryFact {
                        captured: None,
                        connection: id.operation.connection.0,
                        serial: id.operation.serial,
                        slot: id.slot,
                        attempt,
                        fact: fact.encode().map_err(|_| StoreError::Corrupt)?,
                        displaced,
                    },
                    expected: live.revision(),
                    fence,
                },
                &context,
            )?;
            let previous = live.revision;
            live.revision = next;
            source.advance_selected(self, live, previous)?;
            source.checkpoint()?;
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
        if (durable && result.is_err())
            || matches!(
                result,
                Err(StoreError::Persistence(_)
                    | StoreError::RecoverySpace
                    | StoreError::IdentityChanged)
            )
        {
            self.poison_native();
        }
        result
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in super::super) fn native_recovery_slots(
        &self,
        live: &super::LiveOperation,
    ) -> Result<Vec<RecoveryEntry>, StoreError> {
        self.check_native(live)?;
        let operation = self.operation(live.id())?;
        let mut entries = Vec::new();
        for slot in operation.recovery {
            let id = RecoveryId {
                operation: live.id(),
                slot,
            };
            let Some(Row::Recovery(entry)) = self.row(&recovery_key(id))? else {
                return Err(StoreError::Corrupt);
            };
            entries.push(entry.decode()?);
        }
        Ok(entries)
    }
    pub(in super::super) fn retain_native_incomplete(
        &mut self,
        live: &mut super::LiveOperation,
        id: RecoveryId,
    ) -> Result<(), StoreError> {
        self.check_native(live)?;
        if id.operation != live.id() {
            return Err(StoreError::StaleRevision);
        }
        self.retain_incomplete_recovery(id, live.revision(), live.fence())?;
        live.revision = format::next(live.revision())?;
        Ok(())
    }
    /// Re-read the exact private artifact and join it to a newly leased actual
    /// quarantine. Existing Bytes/NativeBound rows alone cannot construct this.
    pub(in super::super) fn rebind_native_recovery(
        &mut self,
        live: &mut super::LiveOperation,
        id: RecoveryId,
        source: &mut super::super::transaction::native::NativeRecoveryReader<'_, '_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<super::BoundRecovery>, StoreError> {
        self.guard()?;
        self.check_native(live)?;
        source.validate_for(self, live)?;
        source.checkpoint()?;
        if id.operation != live.id() {
            return Err(StoreError::StaleRevision);
        }
        let context = self.native_read_context(live, source.deadline(), cancel)?;
        let Some(Row::Recovery(entry)) = self.row(&recovery_key(id))? else {
            return Err(StoreError::Corrupt);
        };
        let expected = source.content();
        if entry.state != 1
            || entry.content
                != Some(format::State::File {
                    sha256: expected.digest,
                    bytes: expected.length,
                    executable: expected.executable,
                })
        {
            return Err(StoreError::RecoveryRequired);
        }
        let file = self.verify_recovery_with_context(&entry, Some(&context))?;
        let metadata = storage::recovery_metadata(&file)?;
        let parent = object_directory(self, id, false)?;
        let (attempt, fact, displaced) = source.fact(
            id,
            artifact_view(entry.object.as_ref().ok_or(StoreError::Corrupt)?),
        )?;
        if fact.side != entry.side {
            return Err(StoreError::RecoveryRequired);
        }
        let bound = format::NativeRecoveryFact {
            captured: None,
            connection: id.operation.connection.0,
            serial: id.operation.serial,
            slot: id.slot,
            attempt,
            fact: fact.encode().map_err(|_| StoreError::Corrupt)?,
            displaced,
        };
        source.checkpoint()?;
        match self.row(&bound.key())? {
            Some(Row::NativeRecoveryFact(existing)) if existing == bound => {}
            None => {
                let previous = live.revision();
                self.append_native(
                    Change::NativeBound {
                        fact: bound,
                        expected: live.revision(),
                        fence: live.fence(),
                    },
                    &context,
                )?;
                live.revision = format::next(live.revision())?;
                if let Err(error) = source.advance_selected(self, live, previous) {
                    self.poison_native();
                    return Err(error);
                }
            }
            _ => return Err(StoreError::RecoveryRequired),
        }
        let postwrite = (|| {
            source.checkpoint()?;
            self.check_native(live)?;
            cancelled(cancel)
        })();
        if let Err(error) = postwrite {
            self.poison_native();
            return Err(error);
        }
        let proof = super::BoundRecovery {
            fact,
            preserved: PreservedObject { id, file },
            parent,
            lifetime: live.lifetime().clone(),
            epoch: source.epoch(),
            metadata,
        };
        if let Err(error) = proof.validate(live, source.epoch()) {
            self.poison_native();
            return Err(error);
        }
        Ok(self.ack(proof))
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(in super::super) fn partial_native_recovery_for_test(
    store: &mut ConnectionStore,
    reservation: &RecoveryReservation,
    bytes: &[u8],
) -> Result<(), StoreError> {
    store.guard()?;
    if !Arc::ptr_eq(&reservation.session, &store.session) {
        return Err(StoreError::StaleRevision);
    }
    let parent = object_directory(store, reservation.id, true)?;
    let mut file = storage::open_file(&parent, &name(reservation.id), true)?;
    file.write_all(bytes)?;
    storage::sync_file(&file)?;
    storage::sync_directory(&parent)
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    fn append_native_drain(
        &mut self,
        context: &mut super::RecoveryContext,
        reader: &mut super::super::transaction::native::NativeDrainReader<'_, '_>,
        entry: format::Recovery,
        source: format::BoundaryEvidence,
        fact: Option<format::NativeRecoveryFact>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.check_recovery_context(context)?;
        reader.validate_for(self, context)?;
        let paid = self.recovery_read_context(context, reader.deadline()?, cancel)?;
        let previous = context.revision;
        let change = if reader.is_selected() {
            Change::SelectedDrain {
                entry,
                source,
                fact,
                expected: previous,
                operation_expected: context.operation_revision,
                fence: context.fence,
            }
        } else {
            Change::NativeDrain {
                entry,
                source,
                fact,
                expected: previous,
                operation_expected: context.operation_revision,
                fence: context.fence,
            }
        };
        self.append_native(change, &paid)?;
        context.revision = format::next(previous)?;
        context.operation_revision = context.revision;
        let result = reader
            .advance(previous, context)
            .and_then(|()| reader.validate_for(self, context));
        if result.is_err() {
            self.poison_native();
        }
        result
    }
    /// Existing pre-fence quarantine only. Returns a private preserved reader,
    /// never `LiveOperation`, `StepAdmission`, `BoundRecovery` or publication authority.
    pub(in super::super) fn drain_native_recovery(
        &mut self,
        context: &mut super::RecoveryContext,
        reader: &mut super::super::transaction::native::NativeDrainReader<'_, '_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<PreservedObject>, StoreError> {
        if reader.is_selected() {
            return Err(StoreError::RecoveryRequired);
        }
        self.drain_recovery_bytes(context, reader, cancel)
    }
    pub(in super::super) fn drain_selected_recovery(
        &mut self,
        context: &mut super::RecoveryContext,
        reader: &mut super::super::transaction::native::NativeDrainReader<'_, '_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<PreservedObject>, StoreError> {
        if !reader.is_selected()
            || !matches!(context.action(), super::ExecutionAction::Selected(_))
            || self.connection(context.id().connection)?.lifecycle == 0
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.drain_recovery_bytes(context, reader, cancel)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    fn drain_recovery_bytes(
        &mut self,
        context: &mut super::RecoveryContext,
        reader: &mut super::super::transaction::native::NativeDrainReader<'_, '_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<PreservedObject>, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        if !context.recovery_only {
            return Err(StoreError::RecoveryRequired);
        }
        reader.validate_for(self, context)?;
        let (attempt, displaced, side) = reader.source()?;
        let origin = format::BoundaryEvidence {
            attempt,
            displaced: Some(displaced.clone()),
            outcome: None,
        };
        let expected = reader.content().clone();
        let operation = self.operation(context.id)?;
        let reason = u8::from(reader.is_selected() || !matches!(operation.decision, 3 | 4));
        // A partial object is never reused or overwritten. An already bound
        // complete version can be returned only after another paid private read.
        for slot in &operation.recovery {
            let id = RecoveryId {
                operation: context.id,
                slot: *slot,
            };
            let Some(Row::Recovery(mut entry)) = self.row(&recovery_key(id))? else {
                return Err(StoreError::Corrupt);
            };
            if entry.state == 0 {
                entry.state = 5;
                self.append_native_drain(context, reader, entry, origin.clone(), None, cancel)?;
            } else if entry.state == 1 && entry.side == side {
                let mut key = format::key(6, id.operation.connection.0, id.operation.serial);
                key.extend(id.slot.to_be_bytes());
                if let Some(Row::NativeRecoveryFact(previous)) = self.row(&key)? {
                    let actual = reader.fact(
                        id,
                        artifact_view(entry.object.as_ref().ok_or(StoreError::Corrupt)?),
                    )?;
                    if previous.attempt == attempt
                        && previous.displaced == displaced
                        && previous.fact == actual.encode().map_err(|_| StoreError::Corrupt)?
                    {
                        let paid =
                            self.recovery_read_context(context, reader.deadline()?, cancel)?;
                        let file = self.verify_recovery_with_context(&entry, Some(&paid))?;
                        reader.validate_for(self, context)?;
                        return Ok(self.ack(PreservedObject { id, file }));
                    }
                }
            }
        }
        let operation = self.operation(context.id)?;
        let slot =
            u32::try_from(operation.recovery.len()).map_err(|_| StoreError::LimitExceeded)?;
        let mut entry = format::Recovery {
            captured: None,
            connection: context.id.connection.0,
            serial: context.id.serial,
            slot,
            side,
            path: operation.path,
            reason,
            state: 0,
            content: None,
            object: None,
            retained: 0,
        };
        let id = entry.id();
        self.append_native_drain(context, reader, entry.clone(), origin.clone(), None, cancel)?;
        let paid = self.recovery_read_context(context, reader.deadline()?, cancel)?;
        let parent = object_directory(self, id, true)?;
        let object_name = name(id);
        let mut target = storage::open_file(&parent, &object_name, true)?;
        let target_identity = storage::regular(&target)?;
        let mut buffer = vec![0; 256 * 1024];
        let mut count = 0_u64;
        let mut copied = Sha256::new();
        loop {
            paid.check()?;
            let read = reader.read_chunk(count, &mut buffer)?;
            if read == 0 {
                break;
            }
            count = count
                .checked_add(read as u64)
                .filter(|size| *size <= expected.length)
                .ok_or(StoreError::LimitExceeded)?;
            let bytes = buffer.get(..read).ok_or(StoreError::Corrupt)?;
            target.write_all(bytes)?;
            copied.update(bytes);
        }
        if count != expected.length || <[u8; 32]>::from(copied.finalize()) != expected.digest {
            return Err(StoreError::RecoveryRequired);
        }
        reader.validate_for(self, context)?;
        storage::sync_file(&target)?;
        storage::validate_file(&parent, &object_name, &target, &target_identity)?;
        let mut reread = storage::reread_file(&parent, &object_name)?;
        let metadata = storage::regular(&reread)?;
        if storage::file_identity(&metadata) != storage::file_identity(&target_identity) {
            return Err(StoreError::IdentityChanged);
        }
        let (digest, count) =
            hash_file_with_context(&mut reread, cancel, expected.length, Some(&paid))?;
        if digest != expected.digest
            || count != expected.length
            || !storage::same_snapshot(&metadata, &storage::regular(&reread)?)
        {
            return Err(StoreError::RecoveryRequired);
        }
        storage::validate_recovery_reader(&parent, &object_name, &reread, &metadata)?;
        storage::sync_directory(&parent)?;
        reader.validate_for(self, context)?;
        let object = format::ObjectIdentity {
            root: self.storage.identity.clone(),
            file_id: storage::file_identity(&metadata),
        };
        let fact = reader.fact(id, artifact_view(&object))?;
        let fact = format::NativeRecoveryFact {
            captured: None,
            connection: id.operation.connection.0,
            serial: id.operation.serial,
            slot: id.slot,
            attempt,
            fact: fact.encode().map_err(|_| StoreError::Corrupt)?,
            displaced,
        };
        entry.state = 1;
        entry.content = Some(format::State::File {
            sha256: digest,
            bytes: count,
            executable: expected.executable,
        });
        entry.object = Some(object);
        self.append_native_drain(context, reader, entry, origin, Some(fact), cancel)?;
        storage::validate_recovery_reader(&parent, &object_name, &reread, &metadata)?;
        Ok(self.ack(PreservedObject { id, file: reread }))
    }
}

impl ConnectionStore {
    pub(super) fn reduce_native_capture(
        &self,
        snapshot: &[u8],
        displaced: &[Vec<u8>],
        expected: u64,
        kind: u8,
    ) -> Result<Vec<Row>, StoreError> {
        self.reduce_capture_rows(snapshot, displaced, expected, kind, false)
    }
    pub(super) fn reduce_selected_collision(
        &self,
        snapshot: &[u8],
        expected: u64,
        checks: &mut RecoveryChecks<'_>,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        let capture = d::ConflictSnapshot::decode(snapshot).map_err(|_| StoreError::Corrupt)?;
        let id = OperationId {
            connection: ConnectionId(capture.connection),
            serial: capture.operation,
        };
        let operation = self.operation(id)?;
        let Some(format::SelectedAction::Restore {
            target,
            replace: false,
            ..
        }) = &operation.action
        else {
            return Err(StoreError::RecoveryRequired);
        };
        if operation.stage != 0
            || operation.native_capture.is_some()
            || !operation.recovery.is_empty()
            || capture.versions.len() != 2
            || capture.versions.iter().any(|version| version.role != 0)
            || !capture.versions.iter().any(|version| {
                version.side == *target && matches!(version.state, d::State::Regular { .. })
            })
        {
            return Err(StoreError::RecoveryRequired);
        }
        for attempt in 1_u32..=8 {
            let mut key = format::key(5, id.connection.0, id.serial);
            key.extend(attempt.to_be_bytes());
            if self.row(&key)?.is_some() {
                return Err(StoreError::RecoveryRequired);
            }
        }
        checks.verify_selected(self, &operation)?;
        self.reduce_capture_rows(snapshot, &[], expected, 3, true)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    fn reduce_capture_rows(
        &self,
        snapshot: &[u8],
        displaced: &[Vec<u8>],
        expected: u64,
        kind: u8,
        selected_collision: bool,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        let capture = d::ConflictSnapshot::decode(snapshot).map_err(|_| StoreError::Corrupt)?;
        let mut connection = self.connection(ConnectionId(capture.connection))?;
        let id = OperationId {
            connection: ConnectionId(capture.connection),
            serial: capture.operation,
        };
        let mut operation = self.operation(id)?;
        operation.validate_capture(&capture)?;
        if connection.lifecycle != 0 || connection.fence != capture.fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected
            || operation.revision != expected
            || capture.revision != expected
            || connection.pending != Some(id.serial)
        {
            return Err(StoreError::StaleRevision);
        }
        if kind > 4
            || !matches!(operation.stage, 0..=3 | 6)
            || operation.action.is_some() != selected_collision
        {
            return Err(StoreError::RecoveryRequired);
        }
        // QND is supplied by the owning capture even before any private copy exists.
        // Join it to the registered QNI/QNS, never infer it from a recovery label.
        if displaced.len()
            != capture
                .versions
                .iter()
                .filter(|version| version.displaced.is_some())
                .count()
        {
            return Err(StoreError::Corrupt);
        }
        for version in &capture.versions {
            if version.role == 2 {
                let d::State::Regular { object, .. } = &version.state else {
                    return Err(StoreError::Corrupt);
                };
                let mut matched = false;
                for attempt in 1_u32..=8 {
                    let mut key = format::key(5, capture.connection, capture.operation);
                    key.extend(attempt.to_be_bytes());
                    let Some(Row::NativeRegistration(registration)) = self.row(&key)? else {
                        continue;
                    };
                    if registration.side != version.side {
                        continue;
                    }
                    let Some(stage) = &registration.stage else {
                        continue;
                    };
                    let intent =
                        d::decode(registration.intent.as_deref().ok_or(StoreError::Corrupt)?)
                            .map_err(|_| StoreError::Corrupt)?;
                    let stage = d::decode(stage).map_err(|_| StoreError::Corrupt)?;
                    stage
                        .joined(&intent, None, None)
                        .map_err(|_| StoreError::Corrupt)?;
                    if stage.header.connection == capture.connection
                        && stage.header.operation == capture.operation
                        && stage.header.progress <= expected
                        && matches!(&stage.body, d::Body::Stage(value) if &value.object == object)
                    {
                        if matched {
                            return Err(StoreError::Corrupt);
                        }
                        matched = true;
                    }
                }
                if !matched {
                    return Err(StoreError::RecoveryRequired);
                }
            }
            if let Some(digest) = version.displaced {
                let mut matched = None;
                for bytes in displaced {
                    let clue = d::decode(bytes).map_err(|_| StoreError::Corrupt)?;
                    if clue.digest == digest {
                        if matched.is_some() {
                            return Err(StoreError::Corrupt);
                        }
                        matched = Some(clue);
                    }
                }
                let clue = matched.ok_or(StoreError::RecoveryRequired)?;
                let mut key = format::key(5, capture.connection, capture.operation);
                key.extend(
                    u32::try_from(clue.header.attempt)
                        .map_err(|_| StoreError::Corrupt)?
                        .to_be_bytes(),
                );
                let Some(Row::NativeRegistration(registration)) = self.row(&key)? else {
                    return Err(StoreError::RecoveryRequired);
                };
                let intent = d::decode(
                    registration
                        .intent
                        .as_deref()
                        .ok_or(StoreError::RecoveryRequired)?,
                )
                .map_err(|_| StoreError::Corrupt)?;
                let stage = registration
                    .stage
                    .as_deref()
                    .map(d::decode)
                    .transpose()
                    .map_err(|_| StoreError::Corrupt)?;
                clue.joined(&intent, stage.as_ref(), None)
                    .map_err(|_| StoreError::Corrupt)?;
                let d::Body::Displaced(body) = &clue.body else {
                    return Err(StoreError::Corrupt);
                };
                let (d::State::Regular { object: actual, .. }
                | d::State::Unsupported { object: actual, .. }) = &version.state
                else {
                    return Err(StoreError::Corrupt);
                };
                if clue.header.connection != capture.connection
                    || clue.header.operation != capture.operation
                    || clue.header.progress > expected
                    || body.side != version.side
                    || body.barriers != 3
                    || &body.object != actual
                {
                    return Err(StoreError::Corrupt);
                }
            }
        }
        let revision = format::next(expected)?;
        let mut conflict = match self.row(&format::key(3, capture.connection, capture.operation))? {
            Some(Row::Conflict(conflict)) if !conflict.resolved => conflict,
            None => format::ConflictData {
                native_capture: None,
                connection: capture.connection,
                serial: capture.operation,
                revision,
                kind,
                path: operation.path.clone(),
                source: operation.source.clone(),
                destination: operation.destination.clone(),
                preserved: Vec::new(),
                resolved: false,
            },
            _ => return Err(StoreError::RecoveryRequired),
        };
        conflict.kind = kind;
        conflict.revision = revision;
        conflict.native_capture = Some(snapshot.to_vec());
        operation.native_capture = Some(snapshot.to_vec());
        operation.native_capture_displaced = displaced.to_vec();
        operation.stage = 0;
        operation.revision = revision;
        connection.revision = revision;
        Ok(vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::Conflict(conflict),
        ])
    }
}

impl ConnectionStore {
    fn captured_recovery_ids(
        &self,
        operation: &format::Operation,
        proposed: Option<&format::NativeRecoveryFact>,
    ) -> Result<Option<Vec<RecoveryId>>, StoreError> {
        self.captured_recovery_ids_with_context(operation, proposed, None)
    }
    pub(super) fn captured_recovery_ids_with_context(
        &self,
        operation: &format::Operation,
        proposed: Option<&format::NativeRecoveryFact>,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<Option<Vec<RecoveryId>>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        let capture = d::ConflictSnapshot::decode(
            operation
                .native_capture
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        let mut ids = Vec::new();
        for version in &capture.versions {
            let (object, content) = match &version.state {
                d::State::Absent => continue,
                d::State::Regular {
                    object, content, ..
                } => (object, content),
                _ => return Ok(None),
            };
            let mut found = false;
            for slot in &operation.recovery {
                let mut key = format::key(6, operation.connection, operation.serial);
                key.extend(slot.to_be_bytes());
                let fact = if let Some(fact) = proposed.filter(|fact| fact.slot == *slot) {
                    Some(fact.clone())
                } else {
                    match self.row_with_context(&key, context)? {
                        Some(Row::NativeRecoveryFact(fact)) => Some(fact),
                        _ => None,
                    }
                };
                let Some(fact) = fact else {
                    continue;
                };
                if fact.captured.is_none() {
                    continue;
                }
                fact.validate()?;
                let source =
                    d::RecoveryFact::decode(&fact.fact).map_err(|_| StoreError::Corrupt)?;
                if source.side != version.side
                    || source.role != version.role + 1
                    || &source.object != object
                    || &source.content != content
                {
                    continue;
                }
                let id = RecoveryId {
                    operation: OperationId {
                        connection: ConnectionId(operation.connection),
                        serial: operation.serial,
                    },
                    slot: *slot,
                };
                let Some(Row::Recovery(entry)) =
                    self.row_with_context(&recovery_key(id), context)?
                else {
                    return Err(StoreError::Corrupt);
                };
                if entry.captured.as_ref().is_none_or(|origin| {
                    Some(&origin.snapshot) != fact.captured.as_ref()
                        || d::ConflictSnapshot::decode(&origin.snapshot)
                            .ok()
                            .and_then(|snapshot| {
                                snapshot.versions.get(usize::from(origin.version)).cloned()
                            })
                            .is_none_or(|version| {
                                version.side != source.side || version.role + 1 != source.role
                            })
                }) || entry.state != 1
                    || entry.reason != 2
                    || entry.side != source.side
                    || entry.path != operation.path
                    || entry
                        .object
                        .as_ref()
                        .is_none_or(|identity| artifact_view(identity) != source.artifact)
                    || entry.content
                        != Some(format::State::File {
                            sha256: content.digest,
                            bytes: content.length,
                            executable: content.executable,
                        })
                {
                    return Err(StoreError::Corrupt);
                }
                ids.push(id);
                found = true;
                break;
            }
            if !found {
                return Ok(None);
            }
        }
        Ok(Some(ids))
    }
    fn reduce_captured_bound_with_context(
        &self,
        fact: &format::NativeRecoveryFact,
        expected: u64,
        fence: u64,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        let mut connection =
            self.connection_with_context(ConnectionId(fact.connection), context)?;
        let id = OperationId {
            connection: ConnectionId(fact.connection),
            serial: fact.serial,
        };
        let mut operation = self.operation_with_context(id, context)?;
        if connection.lifecycle != 0 || connection.fence != fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected
            || operation.revision != expected
            || connection.pending != Some(id.serial)
        {
            return Err(StoreError::StaleRevision);
        }
        if operation.action.is_some() {
            self.selected_collision_join_with_context(&operation, context)?;
        }
        if !matches!(operation.stage, 0 | 1)
            || operation.native_capture != fact.captured
            || self.row_with_context(&fact.key(), context)?.is_some()
            || !operation.recovery.contains(&fact.slot)
        {
            return Err(StoreError::RecoveryRequired);
        }
        let source = d::RecoveryFact::decode(&fact.fact).map_err(|_| StoreError::Corrupt)?;
        let Some(Row::Recovery(entry)) = self.row_with_context(
            &recovery_key(RecoveryId {
                operation: id,
                slot: fact.slot,
            }),
            context,
        )?
        else {
            return Err(StoreError::Corrupt);
        };
        if entry.captured.as_ref().is_none_or(|origin| {
            Some(&origin.snapshot) != fact.captured.as_ref()
                || d::ConflictSnapshot::decode(&origin.snapshot)
                    .ok()
                    .and_then(|snapshot| {
                        snapshot.versions.get(usize::from(origin.version)).cloned()
                    })
                    .is_none_or(|version| {
                        version.side != source.side || version.role + 1 != source.role
                    })
        }) || entry.state != 1
            || entry.reason != 2
            || entry.side != source.side
            || entry.path != operation.path
            || entry
                .object
                .as_ref()
                .is_none_or(|identity| artifact_view(identity) != source.artifact)
            || entry.content
                != Some(format::State::File {
                    sha256: source.content.digest,
                    bytes: source.content.length,
                    executable: source.content.executable,
                })
            || (!fact.displaced.is_empty()
                && !operation.native_capture_displaced.contains(&fact.displaced))
        {
            return Err(StoreError::Corrupt);
        }
        operation.stage = u8::from(
            self.captured_recovery_ids_with_context(&operation, Some(fact), context)?
                .is_some(),
        );
        connection.revision = format::next(expected)?;
        operation.revision = connection.revision;
        Ok(vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::NativeRecoveryFact(fact.clone()),
        ])
    }
}

impl ConnectionStore {
    pub(super) fn reduce_native_conflict_complete(
        &self,
        id: OperationId,
        expected: u64,
        fence: u64,
        conflict_expected: u64,
        capture: [u8; 32],
        checks: &mut RecoveryChecks<'_>,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        let mut connection = self.connection(id.connection)?;
        let mut operation = self.operation(id)?;
        if connection.lifecycle != 0 || connection.fence != fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected
            || operation.revision != expected
            || connection.pending != Some(id.serial)
        {
            return Err(StoreError::StaleRevision);
        }
        let bytes = operation
            .native_capture
            .as_deref()
            .ok_or(StoreError::RecoveryRequired)?;
        let snapshot = d::ConflictSnapshot::decode(bytes).map_err(|_| StoreError::Corrupt)?;
        if !matches!(operation.stage, 0 | 1)
            || operation.action.is_some()
            || snapshot.fence != fence
            || snapshot.binding_digest().map_err(|_| StoreError::Corrupt)? != capture
        {
            return Err(StoreError::RecoveryRequired);
        }
        let Some(Row::Conflict(mut conflict)) =
            self.row(&format::key(3, id.connection.0, id.serial))?
        else {
            return Err(StoreError::Corrupt);
        };
        if conflict.resolved
            || conflict.revision != conflict_expected
            || conflict.native_capture.as_deref() != Some(bytes)
        {
            return Err(StoreError::StaleRevision);
        }
        let required = self
            .captured_recovery_ids(&operation, None)?
            .ok_or(StoreError::RecoveryRequired)?;
        for recovery_id in required {
            let Some(Row::Recovery(entry)) = self.row(&recovery_key(recovery_id))? else {
                return Err(StoreError::Corrupt);
            };
            checks.verify(self, &entry)?;
        }
        let revision = format::next(expected)?;
        conflict.revision = revision;
        operation.revision = revision;
        operation.stage = 8;
        connection.revision = revision;
        connection.pending = None;
        Ok(vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::Conflict(conflict),
        ])
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in super::super) fn record_native_capture(
        &mut self,
        live: &mut super::LiveOperation,
        capture: &mut super::super::transaction::native::NativeConflictCapture<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if !live.action().is_reconciliation() {
            return Err(StoreError::RecoveryRequired);
        }
        self.guard()?;
        self.check_native(live)?;
        cancelled(cancel)?;
        capture.validate(live)?;
        let previous = live.revision();
        self.append(Change::NativeCapture {
            snapshot: capture
                .snapshot()
                .encode()
                .map_err(|_| StoreError::Corrupt)?,
            displaced: capture.displaced(),
            expected: previous,
            kind: capture.kind()?,
        })?;
        live.revision = format::next(previous)?;
        live.action = super::ExecutionAction::Captured;
        capture.advance(live, previous)?;
        self.check_native(live)
    }
    pub(in super::super) fn preserve_native_conflict(
        &mut self,
        live: &mut super::LiveOperation,
        capture: &mut super::super::transaction::native::NativeConflictCapture<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if !matches!(live.action(), super::ExecutionAction::Captured) {
            return Err(StoreError::RecoveryRequired);
        }
        self.preserve_captured_versions(live, capture, None, cancel)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    fn preserve_captured_versions(
        &mut self,
        live: &mut super::LiveOperation,
        capture: &mut super::super::transaction::native::NativeConflictCapture<'_>,
        mut selected: Option<&mut super::VerifiedSelection<'_>>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        use super::super::transaction::native::descriptor as d;
        self.guard()?;
        self.check_native(live)?;
        capture.validate(live)?;
        if let Some(reader) = selected.as_deref_mut() {
            reader.validate(self, live)?;
        }
        let snapshot = capture.snapshot().clone();
        let encoded = snapshot.encode().map_err(|_| StoreError::Corrupt)?;
        if self.operation(live.id())?.native_capture.as_ref() != Some(&encoded) {
            return Err(StoreError::StaleRevision);
        }
        for (index, version) in snapshot.versions.iter().enumerate() {
            let d::State::Regular {
                object, content, ..
            } = &version.state
            else {
                continue;
            };
            let (attempt, displaced) = capture.origin(index)?;
            let origin = format::CapturedSlot {
                snapshot: encoded.clone(),
                version: u8::try_from(index).map_err(|_| StoreError::Corrupt)?,
            };
            let mut complete = None;
            for slot in self.operation(live.id())?.recovery {
                let id = RecoveryId {
                    operation: live.id(),
                    slot,
                };
                let Some(Row::Recovery(entry)) = self.row(&recovery_key(id))? else {
                    return Err(StoreError::Corrupt);
                };
                if entry.captured.as_ref() != Some(&origin) {
                    continue;
                }
                if entry.state == 0 {
                    let previous = live.revision();
                    self.retain_native_incomplete(live, id)?;
                    capture.advance(live, previous)?;
                    if let Some(reader) = selected.as_deref_mut() {
                        reader.advance(self, live, previous)?;
                    }
                } else if entry.state == 1 && complete.is_none() {
                    complete = Some(entry);
                }
            }
            if let Some(entry) = complete {
                capture.validate(live)?;
                if let Some(reader) = selected.as_deref_mut() {
                    reader.validate(self, live)?;
                }
                let context = self.native_read_context(live, capture.deadline(), cancel)?;
                let reader = self.verify_recovery_with_context(&entry, Some(&context))?;
                let expected = d::RecoveryFact {
                    intent: snapshot.binding_digest().map_err(|_| StoreError::Corrupt)?,
                    recovery: d::Recovery {
                        connection: entry.connection,
                        operation: entry.serial,
                        slot: entry.slot,
                    },
                    side: version.side,
                    role: version.role + 1,
                    endpoint: *snapshot
                        .endpoints
                        .get(usize::from(version.side))
                        .ok_or(StoreError::Corrupt)?,
                    object: object.clone(),
                    content: content.clone(),
                    artifact: artifact_view(entry.object.as_ref().ok_or(StoreError::Corrupt)?),
                };
                let fact = format::NativeRecoveryFact {
                    captured: Some(encoded.clone()),
                    connection: entry.connection,
                    serial: entry.serial,
                    slot: entry.slot,
                    attempt,
                    fact: expected.encode().map_err(|_| StoreError::Corrupt)?,
                    displaced: displaced.clone(),
                };
                match self.row(&fact.key())? {
                    Some(Row::NativeRecoveryFact(existing)) if existing == fact => {}
                    None => {
                        capture.validate(live)?;
                        if let Some(reader) = selected.as_deref_mut() {
                            reader.validate(self, live)?;
                        }
                        let previous = live.revision();
                        self.append_native(
                            Change::NativeBound {
                                fact,
                                expected: previous,
                                fence: live.fence(),
                            },
                            &context,
                        )?;
                        live.revision = format::next(previous)?;
                        capture.advance(live, previous)?;
                        if let Some(reader) = selected.as_deref_mut() {
                            reader.advance(self, live, previous)?;
                        }
                    }
                    _ => return Err(StoreError::RecoveryRequired),
                }
                capture.validate(live)?;
                if let Some(reader) = selected.as_deref_mut() {
                    reader.validate(self, live)?;
                }
                drop(reader);
                continue;
            }
            let previous = live.revision();
            let reservation = self.reserve_capture_slot(live, &snapshot, index)?;
            capture.advance(live, previous)?;
            if let Some(reader) = selected.as_deref_mut() {
                reader.advance(self, live, previous)?;
            }
            let context = self.native_read_context(live, capture.deadline(), cancel)?;
            let id = reservation.id;
            let Some(Row::Recovery(mut entry)) = self.row(&recovery_key(id))? else {
                return Err(StoreError::Corrupt);
            };
            let parent = object_directory(self, id, true)?;
            let object_name = name(id);
            let mut target = storage::open_file(&parent, &object_name, true)?;
            let identity = storage::regular(&target)?;
            let mut buffer = vec![0; 256 * 1024];
            let mut count = 0_u64;
            let mut hash = Sha256::new();
            loop {
                cancelled(cancel)?;
                if let Some(reader) = selected.as_deref_mut() {
                    reader.retained_check()?;
                }
                let read = capture.read_chunk(index, count, &mut buffer)?;
                if let Some(reader) = selected.as_deref_mut() {
                    reader.retained_check()?;
                }
                if read == 0 {
                    break;
                }
                count = count
                    .checked_add(read as u64)
                    .filter(|count| *count <= content.length)
                    .ok_or(StoreError::LimitExceeded)?;
                let bytes = buffer.get(..read).ok_or(StoreError::Corrupt)?;
                target.write_all(bytes)?;
                hash.update(bytes);
            }
            if count != content.length || <[u8; 32]>::from(hash.finalize()) != content.digest {
                return Err(StoreError::RecoveryRequired);
            }
            capture.validate(live)?;
            if let Some(reader) = selected.as_deref_mut() {
                reader.validate(self, live)?;
            }
            storage::sync_file(&target)?;
            storage::validate_file(&parent, &object_name, &target, &identity)?;
            let mut reread = storage::reread_file(&parent, &object_name)?;
            let reread_identity = storage::regular(&reread)?;
            if storage::file_identity(&identity) != storage::file_identity(&reread_identity) {
                return Err(StoreError::IdentityChanged);
            }
            let (hash, count) =
                hash_file_with_context(&mut reread, cancel, content.length, Some(&context))?;
            if count != content.length
                || hash != content.digest
                || !storage::same_snapshot(&reread_identity, &storage::regular(&reread)?)
            {
                return Err(StoreError::RecoveryRequired);
            }
            storage::validate_recovery_reader(&parent, &object_name, &reread, &reread_identity)?;
            storage::sync_directory(&parent)?;
            capture.validate(live)?;
            if let Some(reader) = selected.as_deref_mut() {
                reader.validate(self, live)?;
            }
            self.check_native(live)?;
            cancelled(cancel)?;
            entry.state = 1;
            entry.content = Some(format::State::File {
                sha256: content.digest,
                bytes: content.length,
                executable: content.executable,
            });
            entry.object = Some(format::ObjectIdentity {
                root: self.storage.identity.clone(),
                file_id: storage::file_identity(&reread_identity),
            });
            let fact = d::RecoveryFact {
                intent: snapshot.binding_digest().map_err(|_| StoreError::Corrupt)?,
                recovery: d::Recovery {
                    connection: id.operation.connection.0,
                    operation: id.operation.serial,
                    slot: id.slot,
                },
                side: version.side,
                role: version.role + 1,
                endpoint: *snapshot
                    .endpoints
                    .get(usize::from(version.side))
                    .ok_or(StoreError::Corrupt)?,
                object: object.clone(),
                content: content.clone(),
                artifact: artifact_view(entry.object.as_ref().ok_or(StoreError::Corrupt)?),
            };
            let previous = live.revision();
            self.append_native(
                Change::Bytes {
                    entry,
                    expected: previous,
                    fence: live.fence(),
                },
                &context,
            )?;
            live.revision = format::next(previous)?;
            #[cfg(test)]
            super::run_after_native_bytes();
            capture.advance(live, previous)?;
            if let Some(reader) = selected.as_deref_mut() {
                reader.advance(self, live, previous)?;
            }
            storage::validate_recovery_reader(&parent, &object_name, &reread, &reread_identity)?;
            let previous = live.revision();
            self.append_native(
                Change::NativeBound {
                    fact: format::NativeRecoveryFact {
                        captured: Some(encoded.clone()),
                        connection: id.operation.connection.0,
                        serial: id.operation.serial,
                        slot: id.slot,
                        attempt,
                        fact: fact.encode().map_err(|_| StoreError::Corrupt)?,
                        displaced,
                    },
                    expected: previous,
                    fence: live.fence(),
                },
                &context,
            )?;
            live.revision = format::next(previous)?;
            capture.advance(live, previous)?;
            if let Some(reader) = selected.as_deref_mut() {
                reader.advance(self, live, previous)?;
            }
        }
        self.check_native(live)
    }
    pub(in super::super) fn record_native_conflict(
        &mut self,
        live: &mut super::LiveOperation,
        capture: &mut super::super::transaction::native::NativeConflictCapture<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<super::ConflictRecord>, StoreError> {
        if !matches!(live.action(), super::ExecutionAction::Captured) {
            return Err(StoreError::RecoveryRequired);
        }
        self.guard()?;
        self.check_native(live)?;
        capture.validate(live)?;
        cancelled(cancel)?;
        let Some(Row::Conflict(conflict)) =
            self.row(&format::key(3, live.id().connection.0, live.id().serial))?
        else {
            return Err(StoreError::RecoveryRequired);
        };
        let context = self.native_read_context(live, capture.deadline(), cancel)?;
        let previous = live.revision();
        self.append_native(
            Change::NativeConflictComplete {
                connection: live.id().connection.0,
                serial: live.id().serial,
                expected: previous,
                fence: live.fence(),
                conflict_expected: conflict.revision,
                capture: capture
                    .snapshot()
                    .binding_digest()
                    .map_err(|_| StoreError::Corrupt)?,
            },
            &context,
        )?;
        live.revision = format::next(previous)?;
        capture.advance(live, previous)?;
        let Some(Row::Conflict(conflict)) =
            self.row(&format::key(3, live.id().connection.0, live.id().serial))?
        else {
            return Err(StoreError::Corrupt);
        };
        Ok(self.ack(conflict.decode()?))
    }
}

impl ConnectionStore {
    pub(super) fn reserve_capture_slot(
        &mut self,
        live: &mut super::LiveOperation,
        snapshot: &super::super::transaction::native::descriptor::ConflictSnapshot,
        index: usize,
    ) -> Result<RecoveryReservation, StoreError> {
        self.guard()?;
        self.check_native(live)?;
        let operation = self.operation(live.id())?;
        let encoded = snapshot.encode().map_err(|_| StoreError::Corrupt)?;
        if operation.native_capture.as_ref() != Some(&encoded) {
            return Err(StoreError::StaleRevision);
        }
        let version = snapshot.versions.get(index).ok_or(StoreError::Corrupt)?;
        let slot =
            u32::try_from(operation.recovery.len()).map_err(|_| StoreError::LimitExceeded)?;
        let entry = format::Recovery {
            captured: Some(format::CapturedSlot {
                snapshot: encoded,
                version: u8::try_from(index).map_err(|_| StoreError::Corrupt)?,
            }),
            connection: live.id().connection.0,
            serial: live.id().serial,
            slot,
            side: version.side,
            path: operation.path,
            reason: 2,
            state: 0,
            content: None,
            object: None,
            retained: 0,
        };
        let id = entry.id();
        self.append(Change::Reserve {
            entry,
            expected: live.revision(),
            fence: live.fence(),
        })?;
        live.revision = format::next(live.revision())?;
        Ok(RecoveryReservation {
            id,
            session: Arc::clone(&self.session),
            fence: live.fence(),
        })
    }
}

impl ConnectionStore {
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    pub(super) fn reduce_captured_drain(
        &self,
        change: &Change,
        checks: &mut RecoveryChecks<'_>,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        let (entry, expected, operation_expected, fence, fact, selected) = match change {
            Change::NativeCaptureDrain {
                entry,
                expected,
                operation_expected,
                fence,
                fact,
            } => (entry, expected, operation_expected, fence, fact, false),
            Change::SelectedCaptureDrain {
                entry,
                expected,
                operation_expected,
                fence,
                fact,
            } => (entry, expected, operation_expected, fence, fact, true),
            _ => return Err(StoreError::Corrupt),
        };
        entry.decode()?;
        let mut connection = self.connection(ConnectionId(entry.connection))?;
        let mut operation = self.operation(entry.id().operation)?;
        if connection.revision != *expected
            || operation.revision != *operation_expected
            || connection.fence != *fence
            || connection.pending != Some(entry.serial)
        {
            return Err(StoreError::StaleRevision);
        }
        if connection.lifecycle == 0
            || !matches!(operation.stage, 0 | 1)
            || operation.action.is_some() != selected
            || entry.reason != 2
            || entry.path != operation.path
        {
            return Err(StoreError::RecoveryRequired);
        }
        if selected {
            self.selected_collision_join(&operation)?;
            checks.verify_stopped_selected(self, &operation)?;
        }
        let origin = entry
            .captured
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        if operation.native_capture.as_ref() != Some(&origin.snapshot) {
            return Err(StoreError::RecoveryRequired);
        }
        let snapshot =
            d::ConflictSnapshot::decode(&origin.snapshot).map_err(|_| StoreError::Corrupt)?;
        operation.validate_capture(&snapshot)?;
        if snapshot.fence >= *fence {
            return Err(StoreError::RecoveryRequired);
        }
        let version = snapshot
            .versions
            .get(usize::from(origin.version))
            .ok_or(StoreError::Corrupt)?;
        if version.side != entry.side || !matches!(version.state, d::State::Regular { .. }) {
            return Err(StoreError::RecoveryRequired);
        }
        let mut bound_row = None;
        let mut conflict_row = None;
        match entry.state {
            1 if operation.recovery.contains(&entry.slot) => {
                let Some(Row::Recovery(mut previous)) = self.row(&entry.key())? else {
                    return Err(StoreError::Corrupt);
                };
                if !matches!(previous.state, 0 | 1) || (previous.state == 1 && previous != *entry) {
                    return Err(StoreError::RecoveryRequired);
                }
                previous.state = 1;
                previous.content.clone_from(&entry.content);
                previous.object.clone_from(&entry.object);
                if previous != *entry {
                    return Err(StoreError::Corrupt);
                }
                let fact = fact.as_ref().ok_or(StoreError::RecoveryRequired)?;
                fact.validate()?;
                let source =
                    d::RecoveryFact::decode(&fact.fact).map_err(|_| StoreError::Corrupt)?;
                let d::State::Regular {
                    object, content, ..
                } = &version.state
                else {
                    return Err(StoreError::Corrupt);
                };
                if fact.connection != entry.connection
                    || fact.serial != entry.serial
                    || fact.slot != entry.slot
                    || fact.captured.as_ref() != Some(&origin.snapshot)
                    || source.side != version.side
                    || source.role != version.role + 1
                    || &source.object != object
                    || &source.content != content
                    || entry.object.as_ref().is_none_or(|artifact| {
                        artifact.root != self.storage.identity
                            || artifact_view(artifact) != source.artifact
                    })
                    || entry.content
                        != Some(format::State::File {
                            sha256: content.digest,
                            bytes: content.length,
                            executable: content.executable,
                        })
                    || (!fact.displaced.is_empty()
                        && !operation.native_capture_displaced.contains(&fact.displaced))
                    || self.row(&fact.key())?.is_some()
                {
                    return Err(StoreError::RecoveryRequired);
                }
                checks.verify(self, entry)?;
                let Some(Row::Conflict(mut conflict)) =
                    self.row(&format::key(3, entry.connection, entry.serial))?
                else {
                    return Err(StoreError::Corrupt);
                };
                if conflict.resolved || conflict.native_capture.as_ref() != Some(&origin.snapshot) {
                    return Err(StoreError::RecoveryRequired);
                }
                if !conflict.preserved.contains(&entry.slot) {
                    conflict.preserved.push(entry.slot);
                    conflict.preserved.sort_unstable();
                }
                conflict.revision = format::next(*expected)?;
                conflict_row = Some(Row::Conflict(conflict));
                bound_row = Some(Row::NativeRecoveryFact(fact.clone()));
            }
            0 if fact.is_none()
                && entry.slot as usize == operation.recovery.len()
                && operation.recovery.len() < 128
                && self.row(&entry.key())?.is_none() =>
            {
                operation.recovery.push(entry.slot);
            }
            5 if fact.is_none() && operation.recovery.contains(&entry.slot) => {
                let Some(Row::Recovery(mut previous)) = self.row(&entry.key())? else {
                    return Err(StoreError::Corrupt);
                };
                if previous.state != 0 {
                    return Err(StoreError::RecoveryRequired);
                }
                previous.state = 5;
                if previous != *entry {
                    return Err(StoreError::Corrupt);
                }
            }
            _ => return Err(StoreError::RecoveryRequired),
        }
        connection.revision = format::next(*expected)?;
        operation.revision = connection.revision;
        // Accounting deliberately preserves stage, pending execution and the fence.
        let mut rows = vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::Recovery(entry.clone()),
        ];
        if let Some(row) = conflict_row {
            rows.push(row);
        }
        if let Some(row) = bound_row {
            rows.push(row);
        }
        Ok(rows)
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    fn append_captured_drain(
        &mut self,
        context: &mut super::RecoveryContext,
        capture: &mut super::super::transaction::native::NativeConflictCapture<'_>,
        mut selected: Option<&mut super::VerifiedSelection<'_>>,
        entry: format::Recovery,
        fact: Option<format::NativeRecoveryFact>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.check_recovery_context(context)?;
        let snapshot = if selected.is_some() {
            self.selected_collision_snapshot(context)?
        } else {
            self.native_capture_snapshot(context)?
        };
        if !context.recovery_only || capture.snapshot() != &snapshot {
            return Err(StoreError::RecoveryRequired);
        }
        capture.validate_recovery(context)?;
        if let Some(reader) = selected.as_deref_mut() {
            reader.validate_recovery(self, context)?;
        }
        let paid = self.recovery_read_context(context, capture.deadline(), cancel)?;
        let previous = context.revision;
        let event = if selected.is_some() {
            Change::SelectedCaptureDrain {
                entry,
                expected: previous,
                operation_expected: context.operation_revision,
                fence: context.fence,
                fact,
            }
        } else {
            Change::NativeCaptureDrain {
                entry,
                expected: previous,
                operation_expected: context.operation_revision,
                fence: context.fence,
                fact,
            }
        };
        self.append_native(event, &paid)?;
        context.revision = format::next(previous)?;
        context.operation_revision = context.revision;
        capture.advance_recovery(context, previous)?;
        if let Some(reader) = selected.as_deref_mut() {
            reader.advance_inspection(previous, context)?;
            reader.validate_recovery(self, context)?;
        }
        self.check_recovery_context(context)
    }
    /// Private accounting of exactly the recorded pre-fence versions. No active
    /// operation, namespace admission, completion or publication token is minted.
    pub(in super::super) fn drain_native_capture(
        &mut self,
        context: &mut super::RecoveryContext,
        capture: &mut super::super::transaction::native::NativeConflictCapture<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if !matches!(context.action(), super::ExecutionAction::Captured) {
            return Err(StoreError::RecoveryRequired);
        }
        self.drain_captured_versions(context, capture, None, cancel)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    fn drain_captured_versions(
        &mut self,
        context: &mut super::RecoveryContext,
        capture: &mut super::super::transaction::native::NativeConflictCapture<'_>,
        mut selected: Option<&mut super::VerifiedSelection<'_>>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        use super::super::transaction::native::descriptor as d;
        self.guard()?;
        self.check_recovery_context(context)?;
        let canonical = if selected.is_some() {
            self.selected_collision_snapshot(context)?
        } else {
            self.native_capture_snapshot(context)?
        };
        if !context.recovery_only
            || capture.snapshot() != &canonical
            || capture.snapshot().fence >= context.fence
        {
            return Err(StoreError::RecoveryRequired);
        }
        capture.validate_recovery(context)?;
        if let Some(reader) = selected.as_deref_mut() {
            reader.validate_recovery(self, context)?;
        }
        let snapshot = capture.snapshot().clone();
        let encoded = snapshot.encode().map_err(|_| StoreError::Corrupt)?;
        for (index, version) in snapshot.versions.iter().enumerate() {
            let d::State::Regular {
                object, content, ..
            } = &version.state
            else {
                continue;
            };
            let (attempt, displaced) = capture.origin(index)?;
            let origin = format::CapturedSlot {
                snapshot: encoded.clone(),
                version: u8::try_from(index).map_err(|_| StoreError::Corrupt)?,
            };
            let mut complete = None;
            for slot in self.operation(context.id)?.recovery {
                let Some(Row::Recovery(mut entry)) = self.row(&recovery_key(RecoveryId {
                    operation: context.id,
                    slot,
                }))?
                else {
                    return Err(StoreError::Corrupt);
                };
                if entry.captured.as_ref() != Some(&origin) {
                    continue;
                }
                if entry.state == 0 {
                    entry.state = 5;
                    self.append_captured_drain(
                        context,
                        capture,
                        selected.as_deref_mut(),
                        entry,
                        None,
                        cancel,
                    )?;
                } else if entry.state == 1 && complete.is_none() {
                    complete = Some(entry);
                }
            }
            let make_fact =
                |entry: &format::Recovery| -> Result<format::NativeRecoveryFact, StoreError> {
                    let fact = d::RecoveryFact {
                        intent: snapshot.binding_digest().map_err(|_| StoreError::Corrupt)?,
                        recovery: d::Recovery {
                            connection: entry.connection,
                            operation: entry.serial,
                            slot: entry.slot,
                        },
                        side: version.side,
                        role: version.role + 1,
                        endpoint: *snapshot
                            .endpoints
                            .get(usize::from(version.side))
                            .ok_or(StoreError::Corrupt)?,
                        object: object.clone(),
                        content: content.clone(),
                        artifact: artifact_view(entry.object.as_ref().ok_or(StoreError::Corrupt)?),
                    };
                    Ok(format::NativeRecoveryFact {
                        captured: Some(encoded.clone()),
                        connection: entry.connection,
                        serial: entry.serial,
                        slot: entry.slot,
                        attempt,
                        displaced: displaced.clone(),
                        fact: fact.encode().map_err(|_| StoreError::Corrupt)?,
                    })
                };
            if let Some(entry) = complete {
                let paid = self.recovery_read_context(context, capture.deadline(), cancel)?;
                let reader = self.verify_recovery_with_context(&entry, Some(&paid))?;
                capture.validate_recovery(context)?;
                if let Some(reader) = selected.as_deref_mut() {
                    reader.validate_recovery(self, context)?;
                }
                let fact = make_fact(&entry)?;
                match self.row(&fact.key())? {
                    Some(Row::NativeRecoveryFact(existing)) if existing == fact => {}
                    None => self.append_captured_drain(
                        context,
                        capture,
                        selected.as_deref_mut(),
                        entry,
                        Some(fact),
                        cancel,
                    )?,
                    _ => return Err(StoreError::RecoveryRequired),
                }
                capture.validate_recovery(context)?;
                if let Some(reader) = selected.as_deref_mut() {
                    reader.validate_recovery(self, context)?;
                }
                drop(reader);
                continue;
            }
            let operation = self.operation(context.id)?;
            let mut entry = format::Recovery {
                captured: Some(origin),
                connection: context.id.connection.0,
                serial: context.id.serial,
                slot: u32::try_from(operation.recovery.len())
                    .map_err(|_| StoreError::LimitExceeded)?,
                side: version.side,
                path: operation.path,
                reason: 2,
                state: 0,
                content: None,
                object: None,
                retained: 0,
            };
            let id = entry.id();
            self.append_captured_drain(
                context,
                capture,
                selected.as_deref_mut(),
                entry.clone(),
                None,
                cancel,
            )?;
            let paid = self.recovery_read_context(context, capture.deadline(), cancel)?;
            let directory = object_directory(self, id, true)?;
            let object_name = name(id);
            let mut target = storage::open_file(&directory, &object_name, true)?;
            let identity = storage::regular(&target)?;
            let mut buffer = vec![0; 256 * 1024];
            let mut count = 0_u64;
            let mut hash = Sha256::new();
            loop {
                paid.check()?;
                if let Some(reader) = selected.as_deref_mut() {
                    reader.retained_check()?;
                }
                let read = capture.read_chunk(index, count, &mut buffer)?;
                if let Some(reader) = selected.as_deref_mut() {
                    reader.retained_check()?;
                }
                if read == 0 {
                    break;
                }
                count = count
                    .checked_add(read as u64)
                    .filter(|size| *size <= content.length)
                    .ok_or(StoreError::LimitExceeded)?;
                let bytes = buffer.get(..read).ok_or(StoreError::Corrupt)?;
                target.write_all(bytes)?;
                hash.update(bytes);
                #[cfg(test)]
                super::run_after_capture_chunk();
            }
            if count != content.length || <[u8; 32]>::from(hash.finalize()) != content.digest {
                return Err(StoreError::RecoveryRequired);
            }
            capture.validate_recovery(context)?;
            if let Some(reader) = selected.as_deref_mut() {
                reader.validate_recovery(self, context)?;
            }
            storage::sync_file(&target)?;
            storage::validate_file(&directory, &object_name, &target, &identity)?;
            let mut reread = storage::reread_file(&directory, &object_name)?;
            let metadata = storage::regular(&reread)?;
            if storage::file_identity(&metadata) != storage::file_identity(&identity) {
                return Err(StoreError::IdentityChanged);
            }
            let (digest, size) =
                hash_file_with_context(&mut reread, cancel, content.length, Some(&paid))?;
            if digest != content.digest
                || size != content.length
                || !storage::same_snapshot(&metadata, &storage::regular(&reread)?)
            {
                return Err(StoreError::RecoveryRequired);
            }
            storage::validate_recovery_reader(&directory, &object_name, &reread, &metadata)?;
            storage::sync_directory(&directory)?;
            capture.validate_recovery(context)?;
            if let Some(reader) = selected.as_deref_mut() {
                reader.validate_recovery(self, context)?;
            }
            entry.state = 1;
            entry.content = Some(format::State::File {
                sha256: digest,
                bytes: size,
                executable: content.executable,
            });
            entry.object = Some(format::ObjectIdentity {
                root: self.storage.identity.clone(),
                file_id: storage::file_identity(&metadata),
            });
            let fact = make_fact(&entry)?;
            self.append_captured_drain(
                context,
                capture,
                selected.as_deref_mut(),
                entry,
                Some(fact),
                cancel,
            )?;
            storage::validate_recovery_reader(&directory, &object_name, &reread, &metadata)?;
        }
        capture.validate_recovery(context)?;
        if let Some(reader) = selected.as_deref_mut() {
            reader.validate_recovery(self, context)?;
        }
        self.check_recovery_context(context)
    }
}

impl ConnectionStore {
    fn selected_recovery_row(
        &self,
        live: &super::LiveOperation,
    ) -> Result<format::Recovery, StoreError> {
        self.check_native(live)?;
        self.selected_recovery_row_for(live.id())
    }
    fn selected_recovery_row_for(
        &self,
        operation_id: OperationId,
    ) -> Result<format::Recovery, StoreError> {
        let operation = self.operation(operation_id)?;
        let Some(format::SelectedAction::Restore { selected, .. }) = &operation.action else {
            return Err(StoreError::RecoveryRequired);
        };
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
                connection: operation_id.connection,
                serial: *serial,
            },
            slot: *slot,
        };
        let Some(Row::Recovery(entry)) = self.row(&recovery_key(id))? else {
            return Err(StoreError::RecoveryRequired);
        };
        entry.decode()?;
        if !matches!(entry.state, 1 | 2)
            || entry.path != operation.path
            || entry.content.as_ref() != Some(content)
            || entry
                .object
                .as_ref()
                .is_none_or(|object| object.root != self.storage.identity)
        {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(entry)
    }
    pub(in super::super) fn open_verified_recovery<'a>(
        &mut self,
        live: &super::LiveOperation,
        deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> Result<super::VerifiedRecoveryRead<'a>, StoreError> {
        self.guard()?;
        let selected = self.selected_recovery_row(live)?;
        let reads = self.native_read_context(live, deadline, cancel)?;
        let parent = object_directory(self, selected.id(), false)?;
        let object = selected.object.as_ref().ok_or(StoreError::Corrupt)?;
        let object_name = name(selected.id());
        #[cfg(not(windows))]
        let file = storage::reread_file(&parent, &object_name)?;
        #[cfg(windows)]
        let file = storage::guarded_reader(
            &parent,
            &object_name,
            object.file_id,
            self.recovery_guards.get(&selected.key()),
        )?;
        let identity = storage::recovery_metadata(&file)?;
        if storage::file_identity(&identity) != object.file_id.require_platform()? {
            return Err(StoreError::IdentityChanged);
        }
        let mut reader = super::VerifiedRecoveryRead {
            failed: false,
            id: live.id(),
            revision: live.revision(),
            fence: live.fence(),
            selected,
            parent,
            file,
            identity,
            reads,
        };
        reader.validate(self, live)?;
        Ok(reader)
    }
}
impl super::VerifiedRecoveryRead<'_> {
    pub(in super::super) fn retained_check(&mut self) -> Result<(), StoreError> {
        let result = self.metadata();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn metadata(&self) -> Result<(), StoreError> {
        if self.failed {
            return Err(StoreError::RecoveryRequired);
        }
        self.reads.check()?;
        if !storage::same_snapshot(&self.identity, &storage::recovery_metadata(&self.file)?) {
            return Err(StoreError::RecoveryRequired);
        }
        storage::validate_recovery_reader(
            &self.parent,
            &name(self.selected.id()),
            &self.file,
            &self.identity,
        )?;
        self.reads.check()
    }
    fn scope(
        &self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
    ) -> Result<(), StoreError> {
        if self.id != live.id()
            || self.revision != live.revision()
            || self.fence != live.fence()
            || !self.reads.lifetime.ptr_eq(live.lifetime())
            || store.selected_recovery_row(live)? != self.selected
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }
    pub(in super::super) fn validate(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
    ) -> Result<(), StoreError> {
        let result = self.validate_inner(store, live);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn validate_inner(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
    ) -> Result<(), StoreError> {
        self.scope(store, live)?;
        self.verify_bytes()?;
        store.check_native(live)
    }
    pub(in super::super) fn advance(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
        previous: u64,
    ) -> Result<(), StoreError> {
        if self.revision != previous || previous.checked_add(1) != Some(live.revision()) {
            self.failed = true;
            return Err(StoreError::StaleRevision);
        }
        self.revision = live.revision();
        self.validate(store, live)
    }
    pub(in super::super) fn read_chunk(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<usize, StoreError> {
        let result = (|| {
            self.scope(store, live)?;
            let Some(format::State::File { bytes, .. }) = self.selected.content else {
                return Err(StoreError::Corrupt);
            };
            if buffer.len() > 256 * 1024 || offset > bytes {
                return Err(StoreError::LimitExceeded);
            }
            self.metadata()?;
            self.file.seek(SeekFrom::Start(offset))?;
            self.reads.charge(buffer.len() as u64)?;
            let read = self.file.read(buffer)?;
            self.metadata()?;
            Ok(read)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub(in super::super) fn binding(
        &self,
    ) -> Result<
        (
            RecoveryId,
            super::super::transaction::native::descriptor::Artifact,
            super::super::transaction::native::descriptor::Content,
        ),
        StoreError,
    > {
        if self.failed {
            return Err(StoreError::RecoveryRequired);
        }
        let Some(format::State::File {
            sha256,
            bytes,
            executable,
        }) = self.selected.content
        else {
            return Err(StoreError::Corrupt);
        };
        Ok((
            self.selected.id(),
            artifact_view(self.selected.object.as_ref().ok_or(StoreError::Corrupt)?),
            super::super::transaction::native::descriptor::Content {
                digest: sha256,
                length: bytes,
                executable,
            },
        ))
    }
}

impl ConnectionStore {
    pub(in super::super) fn open_selected_recovery_reader<'a>(
        &mut self,
        context: &super::RecoveryContext,
        deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> Result<super::VerifiedRecoveryRead<'a>, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        let selected = self.selected_recovery_row_for(context.id())?;
        let reads = self.recovery_read_context(context, deadline, cancel)?;
        let parent = object_directory(self, selected.id(), false)?;
        let object = selected.object.as_ref().ok_or(StoreError::Corrupt)?;
        let object_name = name(selected.id());
        #[cfg(not(windows))]
        let file = storage::reread_file(&parent, &object_name)?;
        #[cfg(windows)]
        let file = storage::guarded_reader(
            &parent,
            &object_name,
            object.file_id,
            self.recovery_guards.get(&selected.key()),
        )?;
        let identity = storage::recovery_metadata(&file)?;
        if storage::file_identity(&identity) != object.file_id.require_platform()? {
            return Err(StoreError::IdentityChanged);
        }
        let mut reader = super::VerifiedRecoveryRead {
            failed: false,
            id: context.id(),
            revision: context.revision(),
            fence: context.fence(),
            selected,
            parent,
            file,
            identity,
            reads,
        };
        reader.validate_recovery(self, context)?;
        Ok(reader)
    }
}
impl super::VerifiedRecoveryRead<'_> {
    pub(in super::super) fn validate_recovery(
        &mut self,
        store: &ConnectionStore,
        context: &super::RecoveryContext,
    ) -> Result<(), StoreError> {
        let result = (|| {
            store.check_recovery_context(context)?;
            if self.id != context.id()
                || self.revision != context.revision()
                || self.fence != context.fence()
                || !self.reads.lifetime.ptr_eq(context.lifetime())
                || store.selected_recovery_row_for(context.id())? != self.selected
            {
                return Err(StoreError::StaleRevision);
            }
            self.verify_bytes()?;
            store.check_recovery_context(context)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn verify_bytes(&mut self) -> Result<(), StoreError> {
        self.metadata()?;
        let Some(format::State::File { sha256, bytes, .. }) = self.selected.content else {
            return Err(StoreError::Corrupt);
        };
        self.file.seek(SeekFrom::Start(0))?;
        let (digest, size) =
            hash_file_with_context(&mut self.file, self.reads.cancel, bytes, Some(&self.reads))?;
        if digest != sha256 || size != bytes {
            return Err(StoreError::RecoveryRequired);
        }
        self.metadata()
    }
    #[cfg(target_os = "linux")]
    pub(in super::super) fn activate(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
    ) -> Result<(), StoreError> {
        self.validate(store, live)?;
        if self.reads.recovery_only {
            self.failed = true;
            return Err(StoreError::RecoveryRequired);
        }
        self.reads.inspection_only = false;
        Ok(())
    }
}

impl ConnectionStore {
    pub(super) fn selected_collision_join(
        &self,
        operation: &format::Operation,
    ) -> Result<format::ConflictData, StoreError> {
        self.selected_collision_join_with_context(operation, None)
    }
    pub(super) fn selected_collision_join_with_context(
        &self,
        operation: &format::Operation,
        context: Option<&super::NativeReadContext<'_>>,
    ) -> Result<format::ConflictData, StoreError> {
        if !matches!(
            operation.decode()?.action,
            super::ExecutionAction::SelectedCollision(_)
        ) {
            return Err(StoreError::RecoveryRequired);
        }
        let Some(action) = &operation.action else {
            return Err(StoreError::Corrupt);
        };
        self.selected_descriptor_with_context(operation, action, context)?;
        let Some(Row::Conflict(conflict)) = self.row_with_context(
            &format::key(3, operation.connection, operation.serial),
            context,
        )?
        else {
            return Err(StoreError::RecoveryRequired);
        };
        conflict.decode()?;
        if conflict.kind != 3
            || conflict.path != operation.path
            || conflict.native_capture != operation.native_capture
        {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(conflict)
    }
}

impl ConnectionStore {
    pub(super) fn reduce_selected_collision_complete(
        &self,
        id: OperationId,
        expected: u64,
        fence: u64,
        conflict_expected: u64,
        digest: [u8; 32],
        checks: &mut RecoveryChecks<'_>,
    ) -> Result<Vec<Row>, StoreError> {
        use super::super::transaction::native::descriptor as d;
        let mut connection = self.connection(id.connection)?;
        let mut operation = self.operation(id)?;
        let mut conflict = self.selected_collision_join(&operation)?;
        let snapshot = d::ConflictSnapshot::decode(
            operation
                .native_capture
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        if connection.lifecycle != 0 || connection.fence != fence || snapshot.fence != fence {
            return Err(StoreError::Disconnected);
        }
        if connection.revision != expected
            || operation.revision != expected
            || connection.pending != Some(id.serial)
            || conflict.revision != conflict_expected
            || conflict.resolved
        {
            return Err(StoreError::StaleRevision);
        }
        if operation.stage != 1
            || snapshot.binding_digest().map_err(|_| StoreError::Corrupt)? != digest
        {
            return Err(StoreError::RecoveryRequired);
        }
        checks.verify_selected(self, &operation)?;
        let required = self
            .captured_recovery_ids(&operation, None)?
            .ok_or(StoreError::RecoveryRequired)?;
        let mut versions = Vec::new();
        for id in required {
            let Some(Row::Recovery(mut entry)) = self.row(&recovery_key(id))? else {
                return Err(StoreError::Corrupt);
            };
            checks.verify(self, &entry)?;
            entry.state = 2;
            entry.retained = 0;
            versions.push(Row::Recovery(entry));
        }
        let selected = if matches!(
            &operation.action,
            Some(format::SelectedAction::Restore {
                selected: format::SelectedVersion::Recovery { .. },
                ..
            })
        ) {
            let mut selected = self.selected_recovery_row_for(id)?;
            selected.retained = 0;
            Some(selected)
        } else {
            None
        };
        let revision = format::next(expected)?;
        operation.revision = revision;
        operation.stage = 10;
        conflict.revision = revision;
        connection.revision = revision;
        connection.pending = None;
        operation.decode()?;
        let mut rows = vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::Conflict(conflict),
        ];
        if let Some(selected) = selected {
            rows.push(Row::Recovery(selected));
        }
        rows.extend(versions);
        Ok(rows)
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in super::super) fn record_selected_collision(
        &mut self,
        live: &mut super::LiveOperation,
        proof: &mut super::super::transaction::native::NativeSelectedCollision<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if !matches!(live.action(), super::ExecutionAction::Selected(_)) {
            return Err(StoreError::RecoveryRequired);
        }
        self.guard()?;
        proof.validate(self, live)?;
        cancelled(cancel)?;
        let (capture, _) = proof.parts();
        let context = self.native_read_context(live, capture.deadline(), cancel)?;
        let previous = live.revision();
        self.append_native(
            Change::NativeSelectedCollision {
                snapshot: capture
                    .snapshot()
                    .encode()
                    .map_err(|_| StoreError::Corrupt)?,
                expected: previous,
            },
            &context,
        )?;
        live.revision = format::next(previous)?;
        live.action = self.operation(live.id())?.decode()?.action;
        proof.captured(self, live, previous)
    }
    pub(in super::super) fn preserve_selected_collision(
        &mut self,
        live: &mut super::LiveOperation,
        proof: &mut super::super::transaction::native::NativeSelectedCollision<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if !matches!(live.action(), super::ExecutionAction::SelectedCollision(_)) {
            return Err(StoreError::RecoveryRequired);
        }
        proof.validate(self, live)?;
        let (capture, reader) = proof.parts();
        self.preserve_captured_versions(live, capture, Some(reader), cancel)
    }
    pub(in super::super) fn complete_selected_collision(
        &mut self,
        live: &mut super::LiveOperation,
        proof: &mut super::super::transaction::native::NativeSelectedCollision<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<super::ConflictRecord>, StoreError> {
        if !matches!(live.action(), super::ExecutionAction::SelectedCollision(_)) {
            return Err(StoreError::RecoveryRequired);
        }
        self.guard()?;
        proof.validate(self, live)?;
        cancelled(cancel)?;
        let conflict = self.selected_collision_join(&self.operation(live.id())?)?;
        let (capture, reader) = proof.parts();
        let context = self.native_read_context(live, capture.deadline(), cancel)?;
        let previous = live.revision();
        self.append_native(
            Change::SelectedCollisionComplete {
                connection: live.id().connection.0,
                serial: live.id().serial,
                expected: previous,
                fence: live.fence(),
                conflict_expected: conflict.revision,
                capture: capture
                    .snapshot()
                    .binding_digest()
                    .map_err(|_| StoreError::Corrupt)?,
            },
            &context,
        )?;
        live.revision = format::next(previous)?;
        capture.advance(live, previous)?;
        reader.retained_check()?;
        Ok(self.ack(self.conflict_record(super::ConflictId {
            connection: live.id().connection,
            serial: live.id().serial,
        })?))
    }
}

impl RecoveryChecks<'_> {
    #[cfg_attr(
        not(windows),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "Windows retains verified recovery readers in self through the dependent durable transition."
        )
    )]
    pub(super) fn verify_stopped_selected(
        &mut self,
        store: &ConnectionStore,
        operation: &format::Operation,
    ) -> Result<(), StoreError> {
        let action = operation
            .action
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        store.selected_descriptor(operation, action)?;
        let id = OperationId {
            connection: ConnectionId(operation.connection),
            serial: operation.serial,
        };
        if operation.native_budget.is_none() {
            return Err(StoreError::RecoveryRequired);
        }
        let format::SelectedAction::Restore { selected, .. } = action;
        if matches!(selected, format::SelectedVersion::Tombstone { .. }) {
            return match &self.mode {
                super::VerificationMode::NativeReplay(replayed) if *replayed == id => Ok(()),
                super::VerificationMode::NativeLive(context)
                    if context.id == id
                        && context.recovery_only
                        && context
                            .lifetime
                            .ptr_eq(&Arc::downgrade(&store.native_lifetime)) =>
                {
                    store.validate()?;
                    context.check()
                }
                _ => Err(StoreError::RecoveryRequired),
            };
        }
        let entry = store.selected_recovery_row_for(id)?;
        match &self.mode {
            super::VerificationMode::NativeReplay(replayed) if *replayed == id => {
                entry
                    .object
                    .as_ref()
                    .ok_or(StoreError::RecoveryRequired)?
                    .file_id
                    .require_platform()?;
                Ok(())
            }
            super::VerificationMode::NativeLive(context)
                if context.id == id
                    && context.recovery_only
                    && context
                        .lifetime
                        .ptr_eq(&Arc::downgrade(&store.native_lifetime)) =>
            {
                store.validate()?;
                context.check()?;
                let reader = store.verify_recovery_file(&entry, Some(context))?;
                #[cfg(windows)]
                self.readers.push(reader);
                #[cfg(not(windows))]
                drop(reader);
                Ok(())
            }
            _ => Err(StoreError::RecoveryRequired),
        }
    }
}

impl super::VerifiedRecoveryRead<'_> {
    #[cfg(target_os = "linux")]
    pub(in super::super) fn advance_inspection(
        &mut self,
        previous: u64,
        context: &super::RecoveryContext,
    ) -> Result<(), StoreError> {
        if !context.recovery_only()
            || self.revision != previous
            || previous.checked_add(1) != Some(context.revision())
            || self.id != context.id()
            || self.fence != context.fence()
            || !self.reads.lifetime.ptr_eq(context.lifetime())
        {
            self.failed = true;
            return Err(StoreError::StaleRevision);
        }
        self.revision = context.revision();
        self.retained_check()
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in super::super) fn drain_selected_capture(
        &mut self,
        context: &mut super::RecoveryContext,
        proof: &mut super::super::transaction::native::NativeSelectedCollision<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if !context.recovery_only
            || !matches!(
                context.action(),
                super::ExecutionAction::SelectedCollision(_)
            )
        {
            return Err(StoreError::RecoveryRequired);
        }
        proof.validate_recovery(self, context)?;
        let (capture, reader) = proof.parts();
        self.drain_captured_versions(context, capture, Some(reader), cancel)
    }
}

impl ConnectionStore {
    pub(in super::super) fn open_verified_tombstone<'a>(
        &mut self,
        live: &super::LiveOperation,
        deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> Result<super::VerifiedSelection<'a>, StoreError> {
        self.guard()?;
        self.check_native(live)?;
        let action = self
            .operation(live.id())?
            .action
            .ok_or(StoreError::RecoveryRequired)?;
        let mut proof = super::VerifiedSelection::Tombstone(super::VerifiedTombstoneRead {
            failed: false,
            revision: live.revision(),
            fence: live.fence(),
            action,
            reads: self.native_read_context(live, deadline, cancel)?,
        });
        proof.validate(self, live)?;
        Ok(proof)
    }
}
impl super::VerifiedTombstoneRead<'_> {
    fn retained_check(&mut self) -> Result<(), StoreError> {
        let result = if self.failed {
            Err(StoreError::RecoveryRequired)
        } else {
            self.reads.check()
        };
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn scope(
        &mut self,
        store: &ConnectionStore,
        id: OperationId,
        revision: u64,
        fence: u64,
        lifetime: &std::sync::Weak<()>,
    ) -> Result<(), StoreError> {
        let result = (|| {
            self.retained_check()?;
            if self.reads.id != id
                || self.revision != revision
                || self.fence != fence
                || !self.reads.lifetime.ptr_eq(lifetime)
            {
                return Err(StoreError::StaleRevision);
            }
            let operation = store.operation(id)?;
            if operation.action.as_ref() != Some(&self.action) {
                return Err(StoreError::StaleRevision);
            }
            store.selected_tombstone_descriptor(&operation, &self.action)?;
            self.retained_check()
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
impl super::VerifiedSelection<'_> {
    pub(in super::super) fn validate(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
    ) -> Result<(), StoreError> {
        match self {
            Self::Recovery(reader) => reader.validate(store, live),
            Self::Tombstone(reader) => {
                let result = store.check_native(live).and_then(|()| {
                    reader.scope(
                        store,
                        live.id(),
                        live.revision(),
                        live.fence(),
                        live.lifetime(),
                    )
                });
                if result.is_err() {
                    reader.failed = true;
                }
                result
            }
        }
    }
    pub(in super::super) fn retained_check(&mut self) -> Result<(), StoreError> {
        match self {
            Self::Recovery(reader) => reader.retained_check(),
            Self::Tombstone(reader) => reader.retained_check(),
        }
    }
    pub(in super::super) fn validate_recovery(
        &mut self,
        store: &ConnectionStore,
        context: &super::RecoveryContext,
    ) -> Result<(), StoreError> {
        match self {
            Self::Recovery(reader) => reader.validate_recovery(store, context),
            Self::Tombstone(reader) => {
                let result = store.check_recovery_context(context).and_then(|()| {
                    reader.scope(
                        store,
                        context.id(),
                        context.revision(),
                        context.fence(),
                        context.lifetime(),
                    )
                });
                if result.is_err() {
                    reader.failed = true;
                }
                result
            }
        }
    }
    #[cfg(target_os = "linux")]
    pub(in super::super) fn activate(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
    ) -> Result<(), StoreError> {
        match self {
            Self::Recovery(reader) => reader.activate(store, live),
            Self::Tombstone(reader) => {
                if reader.reads.recovery_only {
                    reader.failed = true;
                    return Err(StoreError::RecoveryRequired);
                }
                reader.scope(
                    store,
                    live.id(),
                    live.revision(),
                    live.fence(),
                    live.lifetime(),
                )?;
                reader.reads.inspection_only = false;
                store.check_native(live)
            }
        }
    }
    #[cfg(target_os = "linux")]
    pub(in super::super) fn advance(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
        previous: u64,
    ) -> Result<(), StoreError> {
        match self {
            Self::Recovery(reader) => reader.advance(store, live, previous),
            Self::Tombstone(reader) => {
                if reader.revision != previous || previous.checked_add(1) != Some(live.revision()) {
                    reader.failed = true;
                    return Err(StoreError::StaleRevision);
                }
                reader.revision = live.revision();
                self.validate(store, live)
            }
        }
    }
    #[cfg(target_os = "linux")]
    pub(in super::super) fn advance_inspection(
        &mut self,
        previous: u64,
        context: &super::RecoveryContext,
    ) -> Result<(), StoreError> {
        match self {
            Self::Recovery(reader) => reader.advance_inspection(previous, context),
            Self::Tombstone(reader) => {
                if !context.recovery_only()
                    || reader.revision != previous
                    || previous.checked_add(1) != Some(context.revision())
                    || reader.reads.id != context.id()
                    || reader.fence != context.fence()
                    || !reader.reads.lifetime.ptr_eq(context.lifetime())
                {
                    reader.failed = true;
                    return Err(StoreError::StaleRevision);
                }
                reader.revision = context.revision();
                reader.retained_check()
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in super::super) fn open_selected_source<'a>(
        &mut self,
        live: &super::LiveOperation,
        deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> Result<super::VerifiedSelection<'a>, StoreError> {
        let action = self
            .operation(live.id())?
            .action
            .ok_or(StoreError::RecoveryRequired)?;
        match action {
            format::SelectedAction::Restore {
                selected: format::SelectedVersion::Recovery { .. },
                ..
            } => Ok(super::VerifiedSelection::Recovery(
                self.open_verified_recovery(live, deadline, cancel)?,
            )),
            format::SelectedAction::Restore {
                selected: format::SelectedVersion::Tombstone { .. },
                ..
            } => self.open_verified_tombstone(live, deadline, cancel),
        }
    }
    pub(in super::super) fn inspect_selected_source<'a>(
        &mut self,
        context: &super::RecoveryContext,
        deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> Result<super::VerifiedSelection<'a>, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        let action = self
            .operation(context.id())?
            .action
            .ok_or(StoreError::RecoveryRequired)?;
        if matches!(
            action,
            format::SelectedAction::Restore {
                selected: format::SelectedVersion::Recovery { .. },
                ..
            }
        ) {
            return Ok(super::VerifiedSelection::Recovery(
                self.open_selected_recovery_reader(context, deadline, cancel)?,
            ));
        }
        let mut proof = super::VerifiedSelection::Tombstone(super::VerifiedTombstoneRead {
            failed: false,
            revision: context.revision(),
            fence: context.fence(),
            action,
            reads: self.recovery_read_context(context, deadline, cancel)?,
        });
        proof.validate_recovery(self, context)?;
        Ok(proof)
    }
}
#[cfg(target_os = "linux")]
impl super::VerifiedSelection<'_> {
    pub(in super::super) fn binding(
        &self,
    ) -> Result<
        (
            RecoveryId,
            super::super::transaction::native::descriptor::Artifact,
            super::super::transaction::native::descriptor::Content,
        ),
        StoreError,
    > {
        match self {
            Self::Recovery(reader) => reader.binding(),
            Self::Tombstone(_) => Err(StoreError::RecoveryRequired),
        }
    }
    pub(in super::super) fn read_chunk(
        &mut self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<usize, StoreError> {
        match self {
            Self::Recovery(reader) => reader.read_chunk(store, live, offset, buffer),
            Self::Tombstone(_) => Err(StoreError::RecoveryRequired),
        }
    }
}

/// Reconstruct the full missing closure from the verified old baseline. Both
/// live preparation and replay use this exact join, without creating live proof.
pub(super) fn ancestor_patch(
    operation: &format::Operation,
    entries: &std::collections::BTreeMap<super::RelativePath, super::EntryState>,
) -> Result<Option<format::ExistingAncestorPatchV1>, StoreError> {
    let requirement = super::super::transaction::ExistingAncestorRequirement::from_baseline(
        &operation.path.to_relative()?,
        entries,
    )?;
    if requirement.prefixes() == 0 {
        return Ok(None);
    }
    if operation.action.is_some()
        || operation.native_capture.is_some()
        || operation.native_boundary.is_some()
        || operation.selected_applied.is_some()
        || !matches!(operation.decision, 0..=2)
        || ![&operation.source, &operation.destination]
            .into_iter()
            .any(|state| matches!(state, Some(format::State::File { .. })))
    {
        return Err(StoreError::RecoveryRequired);
    }
    let bytes = operation
        .native_identity
        .as_deref()
        .ok_or(StoreError::RecoveryRequired)?;
    let admission = super::super::transaction::native::descriptor::Admission::decode(bytes)
        .map_err(|_| StoreError::Corrupt)?;
    // Operation::decode validates the canonical admission's operation/path,
    // endpoints, direction, decision and original proposal joins.
    operation.decode()?;
    if admission.path != operation.path.to_relative()? {
        return Err(StoreError::Corrupt);
    }
    Ok(Some(format::ExistingAncestorPatchV1 {
        version: 1,
        admission: format::digest(bytes),
        prefixes: requirement.prefixes(),
    }))
}
