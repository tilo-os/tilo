//! Typed Directory history is reduced separately from regular-file evidence.
use super::super::transaction::native::descriptor::directory as d;
use super::{
    ConnectionId, ConnectionStore, EntryState, OperationId, OperationIntent, Row, StoreError,
    VerifiedBaseline, format,
};

impl ConnectionStore {
    pub(super) fn directory_registration_join(
        operation: &format::Operation,
        registration: &format::NativeRegistration,
    ) -> Result<(d::Descriptor, d::Descriptor), StoreError> {
        operation.decode()?;
        let historical = super::chain::absent_first_directory(operation)?;
        if historical {
            registration.validate_history_candidate()?;
        } else {
            registration.validate()?;
        }
        if operation.native_kind != Some(format::NativeOperationKind::DirectoryV1)
            || registration.native_kind != operation.native_kind
            || registration.connection != operation.connection
            || registration.serial != operation.serial
            || registration.admission != format::next(operation.expected)?
            || registration.baseline != operation.baseline
        {
            return Err(StoreError::Corrupt);
        }
        let bytes = operation
            .native_identity
            .as_deref()
            .ok_or(StoreError::Corrupt)?;
        if !historical {
            d::Admission::decode(bytes).map_err(|_| StoreError::Corrupt)?;
        }
        let admission = d::HistoricalAdmission::decode(bytes).map_err(|_| StoreError::Corrupt)?;
        let intent = super::chain::directory_descriptor(
            operation,
            registration.intent.as_deref().ok_or(StoreError::Corrupt)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        admission
            .joined(bytes, &intent)
            .map_err(|_| StoreError::Corrupt)?;
        let stage = super::chain::directory_descriptor(
            operation,
            registration.stage.as_deref().ok_or(StoreError::Corrupt)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        stage.joined(&intent).map_err(|_| StoreError::Corrupt)?;
        Ok((intent, stage))
    }

    pub(super) fn reduce_directory_outcome(
        &self,
        registration: &format::NativeRegistration,
        expected: u64,
        verified: bool,
        reconstructed: bool,
    ) -> Result<Vec<Row>, StoreError> {
        let mut connection = self.connection(ConnectionId(registration.connection))?;
        let mut operation = self.operation(OperationId {
            connection: ConnectionId(registration.connection),
            serial: registration.serial,
        })?;
        let (intent, stage) = Self::directory_registration_join(&operation, registration)?;
        if connection.lifecycle != 0 || connection.fence != registration.fence {
            return Err(StoreError::Disconnected);
        }
        if connection.pending != Some(operation.serial)
            || connection.revision != expected
            || operation.revision != expected
            || operation.stage != if verified { 2 } else { 6 }
        {
            return Err(StoreError::StaleRevision);
        }
        let Some(Row::NativeRegistration(mut previous)) = self.row(&registration.key())? else {
            return Err(StoreError::Corrupt);
        };
        if !verified {
            if previous.outcome.is_some() {
                return Err(StoreError::StaleRevision);
            }
            previous.outcome.clone_from(&registration.outcome);
        }
        if previous != *registration {
            return Err(StoreError::Corrupt);
        }
        let outcome = super::chain::directory_descriptor(
            &operation,
            registration.outcome.as_deref().ok_or(StoreError::Corrupt)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        outcome
            .joined_outcome(&intent, &stage)
            .map_err(|_| StoreError::Corrupt)?;
        let d::Body::Outcome(body) = &outcome.body else {
            return Err(StoreError::Corrupt);
        };
        if body.sequence != 1
            || outcome.header.progress > expected
            || (!verified && !reconstructed && outcome.header.progress != expected)
        {
            return Err(StoreError::Corrupt);
        }
        connection.revision = format::next(expected)?;
        operation.revision = connection.revision;
        operation.stage = if verified { 3 } else { 2 };
        Ok(vec![
            Row::Connection(connection),
            Row::Operation(operation),
            Row::NativeRegistration(registration.clone()),
        ])
    }

    pub(super) fn native_directory_baseline_value(
        &self,
        operation: &format::Operation,
    ) -> Result<EntryState, StoreError> {
        let mut latest: Option<format::NativeRegistration> = None;
        for row in self.rows()? {
            if let Row::NativeRegistration(value) = row? {
                if value.connection == operation.connection
                    && value.serial == operation.serial
                    && latest
                        .as_ref()
                        .is_none_or(|old| old.attempt < value.attempt)
                {
                    latest = Some(value);
                }
            }
        }
        let registration = latest.ok_or(StoreError::RecoveryRequired)?;
        let (intent, stage) = Self::directory_registration_join(operation, &registration)?;
        let outcome = super::chain::directory_descriptor(
            operation,
            registration
                .outcome
                .as_deref()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        outcome
            .joined_outcome(&intent, &stage)
            .map_err(|_| StoreError::Corrupt)?;
        Ok(EntryState::Directory)
    }
}

impl ConnectionStore {
    pub(in crate::connection) fn preflight_directory_baseline(
        &self,
        intent: &OperationIntent,
        baseline: &VerifiedBaseline,
    ) -> Result<u32, StoreError> {
        super::super::transaction::directory::addition_side(intent)?;
        let entries = baseline.entries().ok_or(StoreError::RecoveryRequired)?;
        if intent.expected_baseline.is_none()
            || entries
                .keys()
                .any(|path| path.logical() == intent.path.logical())
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.preflight_baseline(intent, baseline)
    }
}

impl ConnectionStore {
    #[cfg(target_os = "linux")]
    pub(in crate::connection) fn begin_native_directory(
        &mut self,
        intent: OperationIntent,
        session: &super::super::transaction::TransactionSession,
        admission: super::super::transaction::native::NativeDirectoryAdmission<'_>,
    ) -> Result<super::LiveOperation, StoreError> {
        let serial = format::next(self.connection(intent.connection)?.serial)?;
        let identity = admission.consume(self, &intent, serial)?;
        self.begin_native_operation(
            intent,
            session,
            None,
            Some(identity),
            None,
            Some(format::NativeOperationKind::DirectoryV1),
        )
    }

    pub(super) fn preflight_directory_intent(
        &self,
        operation: &format::Operation,
    ) -> Result<(), StoreError> {
        operation.decode()?;
        if operation.native_kind != Some(format::NativeOperationKind::DirectoryV1) {
            return Err(StoreError::Corrupt);
        }
        let rows = self.reduce_intent(operation)?;
        for row in &rows {
            directory_singleton(row)?;
        }
        if !self.prepare_rows(rows)?.fits_overlay() {
            return Err(StoreError::LimitExceeded);
        }
        let event = format::Event {
            schema: 1,
            predecessor: self.digest,
            change: format::Change::Intent {
                operation: operation.clone(),
            },
        };
        let length = format::encode(&event)?.len() as u64;
        if self
            .log
            .metadata()?
            .len()
            .checked_add(length + 44)
            .is_none_or(|n| n > 15 * 1024 * 1024)
        {
            return Err(StoreError::LimitExceeded);
        }
        self.preflight_directory_future(operation)
    }
    fn preflight_directory_future(&self, operation: &format::Operation) -> Result<(), StoreError> {
        // Ceiling-only serialization, never a registration, proof or reserved space.
        // Every unknown wire byte costs at most four JSON bytes including its comma.
        let registration = format::NativeRegistration {
            native_kind: operation.native_kind,
            connection: operation.connection,
            serial: operation.serial,
            admission: format::next(operation.expected)?,
            baseline: operation.baseline,
            attempt: 8,
            nonce: [255; 16],
            fence: u64::MAX,
            side: 1,
            intent: Some(vec![255; 15_369]),
            stage: Some(vec![255; 375]),
            outcome: Some(vec![255; 333]),
            outcome_displaced: None,
        };
        directory_singleton(&Row::NativeRegistration(registration.clone()))?;
        format::encode(&format::Event {
            schema: 1,
            predecessor: [255; 32],
            change: format::Change::NativeOutcome {
                registration: registration.clone(),
                expected: u64::MAX,
                verified: false,
                reconstructed: false,
            },
        })?;
        let mut future = operation.clone();
        future.revision = u64::MAX;
        future.stage = 3;
        if let Some(budget) = &mut future.native_budget {
            budget.sequence = u64::MAX;
            budget.reserved = format::READ_LIMIT;
        }
        let mut connection = self.connection(ConnectionId(operation.connection))?;
        connection.revision = u64::MAX;
        connection.serial = operation.serial;
        connection.baseline = Some(u64::MAX);
        connection.pending = None;
        directory_singleton(&Row::Connection(connection.clone()))?;
        let fact = format::DirectoryBoundaryFact::Published {
            attempt: 8,
            outcome: Some(vec![255; 333]),
        };
        future.stage = 7;
        future.native_boundary = Some(3);
        future.native_directory_boundary = Some(fact.clone());
        directory_singleton(&Row::Operation(future.clone()))?;
        let boundary = format::Event {
            schema: 1,
            predecessor: [255; 32],
            change: format::Change::DirectoryBoundary {
                connection: operation.connection,
                serial: operation.serial,
                expected: u64::MAX,
                operation_expected: u64::MAX,
                fence: u64::MAX,
                fact,
            },
        };
        for event in [
            boundary,
            format::Event {
                schema: 1,
                predecessor: [255; 32],
                change: format::Change::NativeOutcome {
                    registration: registration.clone(),
                    expected: u64::MAX,
                    verified: true,
                    reconstructed: true,
                },
            },
        ] {
            let length = format::encode(&event)?.len() as u64;
            if self
                .log
                .metadata()?
                .len()
                .checked_add(length + 44)
                .is_none_or(|n| n > 15 * 1024 * 1024)
            {
                return Err(StoreError::LimitExceeded);
            }
        }
        if !self
            .prepare_rows(vec![
                Row::Operation(future),
                Row::Connection(connection),
                Row::NativeRegistration(registration),
            ])?
            .fits_overlay()
        {
            return Err(StoreError::LimitExceeded);
        }
        Ok(())
    }
}
fn directory_singleton(row: &Row) -> Result<usize, StoreError> {
    format::encode(row)?;
    format::encode(&format::PageNode {
        schema: 1,
        node: format::Node::Leaf(vec![row.clone()]),
    })
    .map(|bytes| bytes.len())
}

#[cfg(target_os = "linux")]
use super::super::transaction::{
    TransactionSession,
    native::{
        DirectoryBaselineValue, DirectoryContinuation, DirectoryOutcome, DirectoryPrepared,
        VerifiedDirectoryComparison,
    },
};
#[cfg(target_os = "linux")]
use super::{
    Arc, AtomicBool, BaselineAppendCheck, DirectoryAncestorPatchV1, DurableAck, LiveOperation,
    NamespaceReservation, PreparedNative, RecoveryChecks, RecoveryContext, Revision,
    VerificationMode, baseline_phase_estimate, recovery, storage,
};

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in crate::connection) fn prepare_native_directory(
        &mut self,
        live: &mut LiveOperation,
        reservation: NamespaceReservation,
        mut proof: DirectoryPrepared<'_>,
    ) -> Result<DurableAck<PreparedNative>, StoreError> {
        self.guard()?;
        self.check_native(live)?;
        if live.native_kind != Some(format::NativeOperationKind::DirectoryV1) {
            return Err(StoreError::RecoveryRequired);
        }
        reservation.header(live, [0; 16])?;
        proof.validate(live)?;
        let (intent, stage) = proof.descriptors();
        let mut registration = reservation.registration;
        registration.intent = Some(intent);
        registration.stage = Some(stage);
        registration.validate()?;
        proof.validate(live)?;
        self.append(format::Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })?;
        live.revision = format::next(live.revision)?;
        if let Err(error) = proof.validate(live) {
            self.poison_native();
            return Err(error);
        }
        Ok(self.ack(PreparedNative {
            registration,
            lifetime: live.lifetime.clone(),
            revision: live.revision,
        }))
    }
    pub(in crate::connection) fn resume_native_directory(
        &mut self,
        context: &RecoveryContext,
        proof: &mut DirectoryContinuation<'_>,
        session: &TransactionSession,
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
        if context.native_kind != Some(format::NativeOperationKind::DirectoryV1)
            || !matches!(operation.stage, 6 | 2 | 3)
            || self.connection(context.id.connection)?.pending != Some(context.id.serial)
        {
            return Err(StoreError::RecoveryRequired);
        }
        proof.validate(context)?;
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
        proof.validate(context)?;
        Ok(live)
    }
    pub(in crate::connection) fn record_directory_outcome(
        &mut self,
        live: &mut LiveOperation,
        proof: &mut DirectoryOutcome<'_>,
        verified: bool,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.guard()?;
        self.check_native(live)?;
        proof.validate(live)?;
        let (attempt, outcome) = proof.facts()?;
        let mut key = format::key(5, live.id.connection.0, live.id.serial);
        key.extend(attempt.to_be_bytes());
        let Some(Row::NativeRegistration(mut registration)) = self.row(&key)? else {
            return Err(StoreError::Corrupt);
        };
        if registration.native_kind != Some(format::NativeOperationKind::DirectoryV1) {
            return Err(StoreError::Corrupt);
        }
        registration.outcome = Some(outcome);
        let context = self.native_read_context(live, proof.deadline(), cancel)?;
        proof.validate(live)?;
        self.append_native(
            format::Change::NativeOutcome {
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
    pub(in crate::connection) fn native_directory_ancestor_patch(
        &self,
        live: &LiveOperation,
        deadline: std::time::Instant,
        cancel: &AtomicBool,
    ) -> Result<Option<DirectoryAncestorPatchV1>, StoreError> {
        self.check_native(live)?;
        let context = self.native_read_context(live, deadline, cancel)?;
        let connection = self.connection(live.id.connection)?;
        let entries = self
            .read_baseline_with_context(
                &connection,
                connection.baseline_root,
                connection.baseline_count,
                connection.baseline.ok_or(StoreError::RecoveryRequired)?,
                Some(&context),
            )?
            .entries()
            .cloned()
            .ok_or(StoreError::RecoveryRequired)?;
        recovery::directory::ancestor_patch(&self.operation(live.id)?, &entries)
    }
}

#[cfg(target_os = "linux")]
pub(in crate::connection) struct DirectoryBaselineCandidate {
    receipt: Option<DirectoryAncestorPatchV1>,
    id: OperationId,
    lifetime: std::sync::Weak<()>,
    revision: u64,
    baseline: Option<u64>,
    fence: u64,
    root: Option<[u8; 32]>,
    count: u32,
    value: DirectoryBaselineValue,
}
#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in crate::connection) fn prepare_directory_baseline(
        &mut self,
        live: &LiveOperation,
        comparison: &mut VerifiedDirectoryComparison<'_>,
        cancel: &AtomicBool,
    ) -> Result<DirectoryBaselineCandidate, StoreError> {
        self.guard()?;
        self.check_native(live)?;
        let operation = self.operation(live.id)?;
        if operation.native_kind != Some(format::NativeOperationKind::DirectoryV1)
            || operation.stage != 3
        {
            return Err(StoreError::RecoveryRequired);
        }
        comparison.validate(self, live, cancel)?;
        let context = self.native_read_context(live, comparison.deadline(), cancel)?;
        let connection = self.connection(live.id.connection)?;
        for row in self.rows()? {
            if let Row::Conflict(conflict) = row? {
                if conflict.connection == connection.id
                    && !conflict.resolved
                    && conflict.path.to_relative()?.overlaps(&live.intent.path)
                {
                    return Err(StoreError::RecoveryRequired);
                }
            }
        }
        let mut entries = self
            .read_baseline_with_context(
                &connection,
                connection.baseline_root,
                connection.baseline_count,
                connection.baseline.ok_or(StoreError::RecoveryRequired)?,
                Some(&context),
            )?
            .entries()
            .cloned()
            .ok_or(StoreError::RecoveryRequired)?;
        let receipt = recovery::directory::ancestor_patch(&operation, &entries)?;
        if receipt.as_ref() != comparison.receipt()
            || self.native_directory_baseline_value(&operation)? != EntryState::Directory
        {
            return Err(StoreError::RecoveryRequired);
        }
        for path in super::super::transaction::ExistingAncestorRequirement::from_baseline(
            &live.intent.path,
            &entries,
        )?
        .paths()
        {
            entries.insert(path, EntryState::Directory);
        }
        entries.insert(live.intent.path.clone(), EntryState::Directory);
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
        Ok(DirectoryBaselineCandidate {
            receipt,
            id: live.id,
            lifetime: live.lifetime.clone(),
            revision: live.revision,
            baseline: live.baseline,
            fence: live.fence,
            root,
            count,
            value: comparison.value(),
        })
    }
    pub(in crate::connection) fn commit_directory_baseline(
        &mut self,
        live: &mut LiveOperation,
        candidate: DirectoryBaselineCandidate,
        comparison: &mut VerifiedDirectoryComparison<'_>,
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
            || candidate.value != comparison.value()
            || candidate.receipt.as_ref() != comparison.receipt()
        {
            return Err(StoreError::StaleRevision);
        }
        comparison.validate(self, live, cancel)?;
        let context = self.native_read_context(live, comparison.deadline(), cancel)?;
        let connection = self.connection(live.id.connection)?;
        comparison.validate(self, live, cancel)?;
        self.append_checked_with_baseline(
            format::Change::BaselineCommit {
                directory_chain: None,
                directory_ancestors: candidate.receipt,
                existing_ancestors: None,
                connection: connection.id,
                expected: live.revision,
                root: candidate.root,
                count: candidate.count,
                source: connection.source.identity,
                destination: connection.destination.identity,
                initial_review: false,
            },
            RecoveryChecks {
                mode: VerificationMode::NativeLive(&context),
                ..RecoveryChecks::default()
            },
            BaselineAppendCheck::Directory {
                live,
                comparison,
                cancel,
            },
        )?;
        live.revision = format::next(live.revision)?;
        Ok(self.ack(live.revision))
    }
}

impl ConnectionStore {
    pub(super) fn reduce_directory_boundary(
        &self,
        id: OperationId,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        fact: &format::DirectoryBoundaryFact,
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
        operation.decode()?;
        if super::chain::absent_first_directory(&operation)? {
            fact.validate_history_candidate()?;
        } else {
            fact.validate()?;
        }
        if operation.native_kind != Some(format::NativeOperationKind::DirectoryV1)
            || connection.lifecycle > 1
            || operation.native_boundary.is_some()
            || operation.native_directory_boundary.is_some()
        {
            return Err(StoreError::RecoveryRequired);
        }
        let registrations = self.operation_registrations(&operation)?;
        Self::validate_directory_boundary(&operation, expected, fence, fact, &registrations)?;
        connection.pending = None;
        connection.revision = format::next(expected)?;
        operation.revision = connection.revision;
        operation.stage = 7;
        operation.native_boundary = Some(fact.kind());
        operation.native_directory_boundary = Some(fact.clone());
        operation.decode()?;
        Ok(vec![Row::Connection(connection), Row::Operation(operation)])
    }
    pub(super) fn validate_directory_boundary(
        operation: &format::Operation,
        expected: u64,
        fence: u64,
        fact: &format::DirectoryBoundaryFact,
        registrations: &[format::NativeRegistration],
    ) -> Result<(), StoreError> {
        if operation.native_kind != Some(format::NativeOperationKind::DirectoryV1) {
            return Err(StoreError::RecoveryRequired);
        }
        if super::chain::absent_first_directory(operation)? {
            fact.validate_history_candidate()?;
        } else {
            fact.validate()?;
        }
        match fact {
            format::DirectoryBoundaryFact::Untouched => {
                if operation.stage != 0 {
                    return Err(StoreError::RecoveryRequired);
                }
                if registrations.iter().any(|reg| {
                    reg.native_kind != operation.native_kind
                        || reg.intent.is_some()
                        || reg.stage.is_some()
                        || reg.outcome.is_some()
                }) {
                    return Err(StoreError::RecoveryRequired);
                }
            }
            format::DirectoryBoundaryFact::Staged { attempt }
            | format::DirectoryBoundaryFact::Published { attempt, .. } => {
                let Some(reg) = registrations.iter().find(|reg| reg.attempt == *attempt) else {
                    return Err(StoreError::RecoveryRequired);
                };
                let (intent, stage) = Self::directory_registration_join(operation, reg)?;
                if reg.fence > fence
                    || intent.header.progress > expected
                    || stage.header.progress > expected
                {
                    return Err(StoreError::StaleRevision);
                }
                match fact {
                    format::DirectoryBoundaryFact::Staged { .. }
                        if operation.stage == 6 && reg.outcome.is_none() =>
                    {
                        ();
                    }
                    format::DirectoryBoundaryFact::Published { outcome, .. }
                        if matches!(operation.stage, 6 | 2 | 3) =>
                    {
                        if reg
                            .outcome
                            .as_ref()
                            .is_some_and(|bytes| Some(bytes) != outcome.as_ref())
                        {
                            return Err(StoreError::RecoveryRequired);
                        }
                        if let Some(bytes) = outcome {
                            let value = super::chain::directory_descriptor(operation, bytes)
                                .map_err(|_| StoreError::Corrupt)?;
                            value
                                .joined_outcome(&intent, &stage)
                                .map_err(|_| StoreError::Corrupt)?;
                            if value.header.progress > expected {
                                return Err(StoreError::StaleRevision);
                            }
                        }
                    }
                    _ => return Err(StoreError::RecoveryRequired),
                }
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl ConnectionStore {
    pub(in crate::connection) fn preflight_directory_effects(
        &self,
        live: &LiveOperation,
    ) -> Result<(), StoreError> {
        self.check_native(live)?;
        let operation = self.operation(live.id)?;
        if operation.native_kind != Some(format::NativeOperationKind::DirectoryV1)
            || operation.stage != 0
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.preflight_directory_future(&operation)
    }
    pub(in crate::connection) fn resume_directory_untouched(
        &mut self,
        context: &RecoveryContext,
        proof: &mut super::super::transaction::native::DirectoryUntouched<'_>,
        session: &TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<LiveOperation, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        session.validate(context.id.connection, &self.native_lifetime)?;
        session.require_idle()?;
        let operation = self.operation(context.id)?;
        if context.recovery_only || cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        if operation.stage != 0
            || context.native_kind != Some(format::NativeOperationKind::DirectoryV1)
            || self.connection(context.id.connection)?.pending != Some(context.id.serial)
        {
            return Err(StoreError::RecoveryRequired);
        }
        proof.validate(context)?;
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
        proof.validate(context)?;
        Ok(live)
    }
    pub(in crate::connection) fn record_directory_boundary(
        &mut self,
        context: &RecoveryContext,
        proof: &mut super::super::transaction::native::DirectoryBoundary<'_>,
        cancel: &AtomicBool,
    ) -> Result<DurableAck<super::OperationReceipt>, StoreError> {
        self.guard()?;
        self.check_recovery_context(context)?;
        if !context.recovery_only
            || context.native_kind != Some(format::NativeOperationKind::DirectoryV1)
        {
            return Err(StoreError::RecoveryRequired);
        }
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let fact = proof.validate(context)?;
        self.append(format::Change::DirectoryBoundary {
            connection: context.id.connection.0,
            serial: context.id.serial,
            expected: context.revision,
            operation_expected: context.operation_revision,
            fence: context.fence,
            fact: fact.clone(),
        })?;
        match proof.validate(context) {
            Ok(after) if after == fact => (),
            _ => {
                self.poison_native();
                return Err(StoreError::RecoveryRequired);
            }
        }
        Ok(self.ack(self.operation(context.id)?.decode()?))
    }
}
