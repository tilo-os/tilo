//! Correlated historical chain facts. No method here creates live authority.
use super::{ConnectionStore, EndpointIdentity, Side, StoreError, format};
use crate::connection::transaction::chain::{DirectoryChainSelectionV1, DirectoryChainTerminalV1};

fn identity(value: &super::storage::Identity) -> EndpointIdentity {
    EndpointIdentity {
        volume: value.volume,
        root: value.root,
    }
}

/// Operation-local first-anchor syntax; store acceptance additionally joins the
/// current connection and retained rows. This never authenticates the selection.
pub(super) fn first_selection(
    operation: &format::Operation,
) -> Result<DirectoryChainSelectionV1, StoreError> {
    let selection = DirectoryChainSelectionV1::decode(
        operation
            .directory_chain_anchor
            .as_deref()
            .ok_or(StoreError::Corrupt)?,
    )?;
    let reference = operation
        .directory_chain_ref
        .as_ref()
        .ok_or(StoreError::Corrupt)?;
    if reference.anchor.connection != operation.connection
        || reference.anchor.serial != operation.serial
        || operation.serial == 0
        || reference.index != 0
        || reference.predecessor.is_some()
        || selection.connection.0 != operation.connection
        || selection.selection_digest != reference.digest
        || selection.review_revision != operation.expected
        || selection.baseline != operation.baseline
    {
        return Err(StoreError::Corrupt);
    }
    member_selection(operation, &selection, 0)?;
    Ok(selection)
}

fn member_selection(
    operation: &format::Operation,
    selection: &DirectoryChainSelectionV1,
    index: u8,
) -> Result<(), StoreError> {
    let member = selection.member(index)?;
    let (state, kind) = match member.terminal {
        DirectoryChainTerminalV1::DirectoryAdd => (
            format::State::Directory,
            Some(format::NativeOperationKind::DirectoryV1),
        ),
        DirectoryChainTerminalV1::RegularAdd { content } => (
            format::State::File {
                sha256: content.sha256,
                bytes: content.bytes,
                executable: content.executable,
            },
            None,
        ),
    };
    let expected = if member.from == Side::Source {
        (Some(state), None, 1)
    } else {
        (None, Some(state), 2)
    };
    if operation.path.to_relative()? != member.path
        || (
            operation.source.clone(),
            operation.destination.clone(),
            operation.decision,
        ) != expected
        || operation.native_kind != kind
        || operation.native_identity.is_none()
        || operation.native_budget.is_none()
        || operation.action.is_some()
        || operation.initial_review.is_some()
        || operation.selected_applied.is_some()
        || operation.native_capture.is_some()
        || !operation.native_capture_displaced.is_empty()
        || !operation.recovery.is_empty()
        || selection.endpoints
            != [
                identity(&operation.source_binding),
                identity(&operation.destination_binding),
            ]
    {
        return Err(StoreError::Corrupt);
    }
    let bytes = operation
        .native_identity
        .as_deref()
        .ok_or(StoreError::Corrupt)?;
    let fence = if kind.is_some() {
        crate::connection::transaction::native::descriptor::directory::HistoricalAdmission::decode(
            bytes,
        )
        .map_err(|_| StoreError::Corrupt)?
        .fence
    } else {
        crate::connection::transaction::native::descriptor::Admission::decode(bytes)
            .map_err(|_| StoreError::Corrupt)?
            .fence
    };
    if fence != selection.original_fence {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

impl ConnectionStore {
    pub(super) fn install_chain_intent(
        &self,
        connection: &mut format::Connection,
        operation: &format::Operation,
    ) -> Result<(), StoreError> {
        if operation.directory_chain_ref.is_none() {
            if connection.directory_chain.is_some()
                || connection.directory_chain_schedule.is_some()
                || operation.directory_chain_anchor.is_some()
                || operation.directory_chain_retired.is_some()
                || operation.directory_chain_stop_boundary.is_some()
            {
                return Err(StoreError::RecoveryRequired);
            }
            return Ok(());
        }
        if let Some(cursor) = connection.directory_chain.as_mut() {
            let reference = operation
                .directory_chain_ref
                .as_ref()
                .ok_or(StoreError::Corrupt)?;
            let schedule = connection
                .directory_chain_schedule
                .as_ref()
                .ok_or(StoreError::Corrupt)?;
            let view = ChainRows {
                store: self,
                replacements: &[],
            };
            let selection = view.member(operation)?;
            if schedule.state != Schedule::Running
                || cursor.current.is_some()
                || operation.directory_chain_anchor.is_some()
                || operation.directory_chain_retired.is_some()
                || operation.directory_chain_stop_boundary.is_some()
                || reference.anchor != cursor.anchor
                || reference.digest != cursor.digest
                || reference.index != cursor.next_index
                || reference.predecessor != cursor.predecessor
                || cursor.next_index >= selection.member_count()?
                || connection.fence != selection.original_fence
                || cursor.progress_revision != connection.revision
                || operation.native_budget.as_ref() != Some(&format::NativeBudget::initial())
            {
                return Err(StoreError::Corrupt);
            }
            let predecessor = view.operation(cursor.predecessor.ok_or(StoreError::Corrupt)?)?;
            if connection.baseline != Some(predecessor.revision) {
                return Err(StoreError::StaleRevision);
            }
            self.native_baseline_value(&predecessor)?;
            cursor.current = Some(format::ChainOperationIdV1 {
                connection: operation.connection,
                serial: operation.serial,
            });
            cursor.progress_revision = operation.revision;
            return Ok(());
        }
        if connection.directory_chain_schedule.is_some()
            || operation.directory_chain_retired.is_some()
            || operation.directory_chain_stop_boundary.is_some()
        {
            return Err(StoreError::RecoveryRequired);
        }
        let selection = first_selection(operation)?;
        if selection.original_fence != connection.fence
            || selection.review_revision != connection.revision
            || selection.baseline != connection.baseline
            || selection.endpoints
                != [
                    identity(&connection.source.identity),
                    identity(&connection.destination.identity),
                ]
            || operation.native_budget.as_ref() != Some(&format::NativeBudget::initial())
        {
            return Err(StoreError::Corrupt);
        }
        let reference = operation
            .directory_chain_ref
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        connection.directory_chain = Some(format::DirectoryChainCursorV1 {
            anchor: reference.anchor,
            digest: reference.digest,
            next_index: 0,
            predecessor: None,
            current: Some(reference.anchor),
            progress_revision: operation.revision,
        });
        connection.directory_chain_schedule = Some(format::DirectoryChainScheduleV1 {
            revision: 1,
            control_sequence: 0,
            state: format::DirectoryChainScheduleState::Running,
        });
        Ok(())
    }
}

use super::{ConnectionId, OperationId, Row};
use format::{
    Change, DirectoryChainControlTransitionV1 as Control, DirectoryChainScheduleState as Schedule,
};

/// Exhaustive routing prevents a future event from silently becoming chain progress.
pub(super) fn change_connection(change: &Change) -> Result<[u8; 16], StoreError> {
    Ok(match change {
        Change::InitialReviewAccepted { review, .. } => review.connection,
        Change::ReviewedIntent { operation, .. } => operation.connection,
        Change::ReviewedChainControl { control, .. } | Change::InitialReviewControl { control } => {
            control.connection
        }
        Change::ReviewedOrdinaryStopBoundary { boundary } => boundary.fact().connection,
        Change::InitialEmptyBaseline { fact } => fact.connection,
        Change::Create { record } => record.id,
        Change::Intent { operation } => operation.connection,
        Change::NativeReserve { registration, .. }
        | Change::NativePrepared { registration, .. }
        | Change::NativeOutcome { registration, .. } => registration.connection,
        Change::NativeBound { fact, .. } => fact.connection,
        Change::Reserve { entry, .. }
        | Change::Bytes { entry, .. }
        | Change::SelectedCaptureDrain { entry, .. }
        | Change::NativeCaptureDrain { entry, .. }
        | Change::NativeDrain { entry, .. }
        | Change::SelectedDrain { entry, .. } => entry.connection,
        Change::NativeCapture { snapshot, .. }
        | Change::NativeSelectedCollision { snapshot, .. } => {
            crate::connection::transaction::native::descriptor::ConflictSnapshot::decode(snapshot)
                .map_err(|_| StoreError::Corrupt)?
                .connection
        }
        Change::DirectoryChainStopBoundary { connection, .. }
        | Change::DirectoryChainControl { connection, .. }
        | Change::SelectedUntouched { connection, .. }
        | Change::SelectedApplied { connection, .. }
        | Change::SelectedAppliedCarryV1 { connection, .. }
        | Change::NativeConflictComplete { connection, .. }
        | Change::SelectedCollisionComplete { connection, .. }
        | Change::DirectoryBoundary { connection, .. }
        | Change::NativeBoundary { connection, .. }
        | Change::SelectedBoundary { connection, .. }
        | Change::IncompleteRecovery { connection, .. }
        | Change::ReadReserve { connection, .. }
        | Change::Fence { connection, .. }
        | Change::Disconnect { connection, .. }
        | Change::Published { connection, .. }
        | Change::Verified { connection, .. }
        | Change::BaselineCommit { connection, .. } => *connection,
    })
}

impl ConnectionStore {
    pub(super) fn chain_event_guard(&self, change: &Change) -> Result<(), StoreError> {
        if matches!(change, Change::Create { .. }) {
            return Ok(());
        }
        let connection = self.connection(ConnectionId(change_connection(change)?))?;
        let Some(cursor) = &connection.directory_chain else {
            return if connection.directory_chain_schedule.is_some() {
                Err(StoreError::Corrupt)
            } else {
                Ok(())
            };
        };
        let schedule = connection
            .directory_chain_schedule
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        if matches!(
            change,
            Change::Fence { .. } | Change::Disconnect { .. } | Change::DirectoryChainControl { .. }
        ) {
            return Ok(());
        }
        let serial = match change {
            Change::Intent { .. }
                if schedule.state == Schedule::Running && cursor.current.is_none() =>
            {
                return Ok(());
            }
            Change::ReadReserve {
                serial,
                recovery_only,
                ..
            } if schedule.state == Schedule::Running || *recovery_only => *serial,
            Change::NativeReserve { registration, .. }
            | Change::NativePrepared { registration, .. }
            | Change::NativeOutcome { registration, .. }
                if schedule.state == Schedule::Running =>
            {
                registration.serial
            }
            Change::DirectoryChainStopBoundary { serial, .. }
                if matches!(schedule.state, Schedule::Stopping { .. }) =>
            {
                *serial
            }
            Change::NativeBoundary { serial, .. } | Change::DirectoryBoundary { serial, .. }
                if matches!(
                    schedule.state,
                    Schedule::Abandoning | Schedule::Disconnected
                ) =>
            {
                *serial
            }
            Change::BaselineCommit { .. } if schedule.state == Schedule::Running => {
                connection.pending.ok_or(StoreError::Corrupt)?
            }
            _ => return Err(StoreError::RecoveryRequired),
        };
        if cursor.current
            != Some(format::ChainOperationIdV1 {
                connection: connection.id,
                serial,
            })
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }

    pub(super) fn update_chain_progress(
        &self,
        change: &Change,
        rows: &mut [Row],
    ) -> Result<(), StoreError> {
        if matches!(
            change,
            Change::Create { .. }
                | Change::Intent { .. }
                | Change::DirectoryChainControl { .. }
                | Change::BaselineCommit { .. }
        ) {
            return Ok(());
        }
        let old = self.connection(ConnectionId(change_connection(change)?))?;
        let Some(old_cursor) = old.directory_chain else {
            return Ok(());
        };
        let old_schedule = old.directory_chain_schedule.ok_or(StoreError::Corrupt)?;
        for row in rows {
            if let Row::Connection(connection) = row {
                if connection.id != old.id {
                    return Err(StoreError::Corrupt);
                }
                if matches!(change, Change::Fence { .. }) {
                    let schedule = connection
                        .directory_chain_schedule
                        .as_mut()
                        .ok_or(StoreError::Corrupt)?;
                    schedule.revision = format::next(schedule.revision)?;
                    schedule.state = Schedule::Disconnected;
                }
                let cursor = connection
                    .directory_chain
                    .as_mut()
                    .ok_or(StoreError::Corrupt)?;
                if matches!(change, Change::Fence { .. } | Change::Disconnect { .. })
                    || old_schedule.state == Schedule::Disconnected
                {
                    cursor.progress_revision = old_cursor.progress_revision;
                } else {
                    cursor.progress_revision = connection.revision;
                }
            }
        }
        Ok(())
    }

    pub(super) fn reduce_chain_control(&self, change: &Change) -> Result<Vec<Row>, StoreError> {
        let Change::DirectoryChainControl {
            connection,
            anchor,
            digest,
            expected_progress,
            expected_schedule,
            control_sequence,
            transition,
        } = change
        else {
            return Err(StoreError::Corrupt);
        };
        let mut connection = self.connection(ConnectionId(*connection))?;
        let cursor = connection
            .directory_chain
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let schedule = connection
            .directory_chain_schedule
            .as_mut()
            .ok_or(StoreError::Corrupt)?;
        let selection = first_selection(&self.operation(cursor.anchor.operation()?)?)?;
        if connection.lifecycle != 0
            || connection.fence != selection.original_fence
            || cursor.anchor != *anchor
            || cursor.digest != *digest
            || cursor.progress_revision != *expected_progress
            || schedule.revision != *expected_schedule
            || format::next(schedule.control_sequence)? != *control_sequence
            || format::next(schedule.revision)? == u64::MAX
        {
            return Err(StoreError::StaleRevision);
        }
        if matches!(transition, Control::Retire) {
            if schedule.state != Schedule::Abandoning || connection.pending.is_some() {
                return Err(StoreError::RecoveryRequired);
            }
            if let Some(current) = cursor.current {
                let operation = self.operation(current.operation()?)?;
                if operation.stage != 7 || operation.revision != cursor.progress_revision {
                    return Err(StoreError::RecoveryRequired);
                }
            }
            let mut anchor = self.operation(cursor.anchor.operation()?)?;
            if anchor.directory_chain_retired.is_some() {
                return Err(StoreError::Corrupt);
            }
            anchor.directory_chain_retired = Some(format::DirectoryChainRetirementV1 {
                reason: format::ChainRetirementReason::Abandoned,
                cursor: cursor.clone(),
            });
            connection.directory_chain = None;
            connection.directory_chain_schedule = None;
            return Ok(vec![Row::Connection(connection), Row::Operation(anchor)]);
        }
        schedule.state = match (&schedule.state, transition) {
            (Schedule::Running, Control::StopRequested { reason }) => {
                Schedule::Stopping { reason: *reason }
            }
            (Schedule::Stopping { reason }, Control::StopAcknowledged) => {
                if let Some(current) = cursor.current {
                    let operation = self.operation(current.operation()?)?;
                    let fact = operation
                        .directory_chain_stop_boundary
                        .as_ref()
                        .ok_or(StoreError::RecoveryRequired)?;
                    if connection.pending != Some(current.serial)
                        || operation.revision != cursor.progress_revision
                        || fact.operation_revision != operation.revision
                        || fact.schedule_revision != schedule.revision
                    {
                        return Err(StoreError::StaleRevision);
                    }
                } else if connection.pending.is_some() {
                    return Err(StoreError::Corrupt);
                }
                Schedule::Stopped { reason: *reason }
            }
            (Schedule::Stopped { .. }, Control::Resume) => Schedule::Running,
            (
                Schedule::Running | Schedule::Stopping { .. } | Schedule::Stopped { .. },
                Control::AbandonRequested,
            ) => Schedule::Abandoning,
            _ => return Err(StoreError::RecoveryRequired),
        };
        schedule.revision = format::next(schedule.revision)?;
        schedule.control_sequence = *control_sequence;
        Ok(vec![Row::Connection(connection)])
    }

    pub(super) fn reduce_chain_stop(&self, change: &Change) -> Result<Vec<Row>, StoreError> {
        let Change::DirectoryChainStopBoundary {
            connection,
            serial,
            expected,
            expected_schedule,
            fence,
            kind,
            evidence,
        } = change
        else {
            return Err(StoreError::Corrupt);
        };
        let id = OperationId {
            connection: ConnectionId(*connection),
            serial: *serial,
        };
        let old = self.connection(id.connection)?;
        let operation = self.operation(id)?;
        let cursor = old
            .directory_chain
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let schedule = old
            .directory_chain_schedule
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        if !matches!(schedule.state, Schedule::Stopping { .. })
            || schedule.revision != *expected_schedule
            || cursor.current != Some(format::ChainOperationIdV1::try_from(id)?)
            || cursor.progress_revision != *expected
            || old.revision != *expected
            || operation.revision != *expected
        {
            return Err(StoreError::StaleRevision);
        }
        let canonical_kind = stop_evidence_kind(*kind, evidence)?;
        let mut rows = match evidence {
            format::DirectoryChainStopEvidenceV1::Regular { evidence } => self
                .reduce_native_boundary(
                    id,
                    *expected,
                    *expected,
                    *fence,
                    canonical_kind,
                    evidence.as_ref(),
                )?,
            format::DirectoryChainStopEvidenceV1::Directory { fact } => {
                self.reduce_directory_boundary(id, *expected, *expected, *fence, fact)?
            }
        };
        for row in &mut rows {
            match row {
                Row::Connection(c) => c.pending = old.pending,
                Row::Operation(o) => {
                    o.stage = operation.stage;
                    o.native_boundary = operation.native_boundary;
                    o.native_boundary_evidence
                        .clone_from(&operation.native_boundary_evidence);
                    o.native_directory_boundary
                        .clone_from(&operation.native_directory_boundary);
                    o.directory_chain_stop_boundary = Some(format::DirectoryChainStopBoundaryV1 {
                        schedule_revision: *expected_schedule,
                        operation_revision: o.revision,
                        kind: *kind,
                        evidence: evidence.clone(),
                    });
                }
                _ => return Err(StoreError::Corrupt),
            }
        }
        Ok(rows)
    }
}

/// The same typed shape is required at append and when a retained fact is reopened.
fn stop_evidence_kind(
    kind: format::ChainStopKind,
    evidence: &format::DirectoryChainStopEvidenceV1,
) -> Result<u8, StoreError> {
    let canonical = match kind {
        format::ChainStopKind::NeverBegun => 0,
        format::ChainStopKind::StagedOnly => 1,
        format::ChainStopKind::PublishedAccounted => 3,
    };
    match evidence {
        format::DirectoryChainStopEvidenceV1::Regular { evidence } => {
            if evidence.as_ref().is_some_and(|e| e.displaced.is_some())
                || match kind {
                    format::ChainStopKind::NeverBegun => evidence.is_some(),
                    format::ChainStopKind::StagedOnly => {
                        evidence.as_ref().is_none_or(|e| e.outcome.is_some())
                    }
                    format::ChainStopKind::PublishedAccounted => {
                        evidence.as_ref().is_none_or(|e| e.outcome.is_none())
                    }
                }
            {
                return Err(StoreError::RecoveryRequired);
            }
        }
        format::DirectoryChainStopEvidenceV1::Directory { fact } => {
            if fact.kind() != canonical
                || matches!(
                    fact,
                    format::DirectoryBoundaryFact::Published { outcome: None, .. }
                )
            {
                return Err(StoreError::RecoveryRequired);
            }
        }
    }
    Ok(canonical)
}

pub(super) fn absent_first_directory(operation: &format::Operation) -> Result<bool, StoreError> {
    if operation.native_kind != Some(format::NativeOperationKind::DirectoryV1)
        || operation.baseline.is_some()
    {
        return Ok(false);
    }
    let selection = first_selection(operation)?;
    if selection.baseline.is_some() {
        return Err(StoreError::Corrupt);
    }
    Ok(true)
}
pub(super) fn directory_descriptor(
    operation: &format::Operation,
    bytes: &[u8],
) -> Result<crate::connection::transaction::native::descriptor::directory::Descriptor, StoreError> {
    use crate::connection::transaction::native::descriptor::directory as d;
    if absent_first_directory(operation)? {
        d::decode_history(bytes)
    } else {
        d::decode(bytes)
    }
    .map_err(|_| StoreError::Corrupt)
}

fn descriptor_progress(operation: &format::Operation, bytes: &[u8]) -> Result<u64, StoreError> {
    if operation.native_kind.is_some() {
        Ok(directory_descriptor(operation, bytes)?.header.progress)
    } else {
        Ok(
            crate::connection::transaction::native::descriptor::decode(bytes)
                .map_err(|_| StoreError::Corrupt)?
                .header
                .progress,
        )
    }
}

impl ConnectionStore {
    pub(super) fn validate_chain_baseline(
        &self,
        connection: &format::Connection,
        commit: Option<&format::DirectoryChainCommitV1>,
        initial_review: bool,
    ) -> Result<bool, StoreError> {
        let operation = self.operation(OperationId {
            connection: ConnectionId(connection.id),
            serial: connection.pending.ok_or(StoreError::RecoveryRequired)?,
        })?;
        let Some(reference) = &operation.directory_chain_ref else {
            return if commit.is_none() && connection.directory_chain.is_none() {
                Ok(false)
            } else {
                Err(StoreError::Corrupt)
            };
        };
        let cursor = connection
            .directory_chain
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        let schedule = connection
            .directory_chain_schedule
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        let commit = commit.ok_or(StoreError::RecoveryRequired)?;
        if initial_review
            || operation.initial_review.is_some()
            || schedule.state != Schedule::Running
            || commit.member != *reference
            || commit.expected_progress != cursor.progress_revision
            || cursor.progress_revision != connection.revision
            || operation.revision != connection.revision
            || cursor.current
                != Some(format::ChainOperationIdV1 {
                    connection: operation.connection,
                    serial: operation.serial,
                })
        {
            return Err(StoreError::StaleRevision);
        }
        if reference.index == 0 {
            let selection = first_selection(&operation)?;
            Ok(selection.baseline.is_none() && operation.baseline.is_none())
        } else {
            Ok(false)
        }
    }

    pub(super) fn complete_chain_baseline(
        &self,
        connection: &mut format::Connection,
        operation: &mut format::Operation,
        rows: &mut Vec<Row>,
    ) -> Result<(), StoreError> {
        let Some(cursor) = connection.directory_chain.as_mut() else {
            return Ok(());
        };
        let reference = operation
            .directory_chain_ref
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        let mut anchor = if reference.anchor.serial == operation.serial {
            operation.clone()
        } else {
            self.operation(reference.anchor.operation()?)?
        };
        let selection = first_selection(&anchor)?;
        cursor.current = None;
        cursor.predecessor = Some(format::ChainOperationIdV1 {
            connection: operation.connection,
            serial: operation.serial,
        });
        cursor.next_index = cursor
            .next_index
            .checked_add(1)
            .ok_or(StoreError::LimitExceeded)?;
        cursor.progress_revision = connection.revision;
        if cursor.next_index == selection.member_count()? {
            let retirement = format::DirectoryChainRetirementV1 {
                reason: format::ChainRetirementReason::Completed,
                cursor: cursor.clone(),
            };
            if reference.anchor.serial == operation.serial {
                operation.directory_chain_retired = Some(retirement);
            } else {
                anchor.directory_chain_retired = Some(retirement);
                rows.push(Row::Operation(anchor));
            }
            connection.directory_chain = None;
            connection.directory_chain_schedule = None;
        }
        Ok(())
    }
}

/// Prospective lookups share the durable row keys with replay and checkpointing.
struct ChainRows<'a> {
    store: &'a ConnectionStore,
    replacements: &'a [Row],
}
impl ChainRows<'_> {
    fn row(&self, key: &[u8]) -> Result<Option<Row>, StoreError> {
        if let Some(row) = self.replacements.iter().find(|row| row.key() == key) {
            return Ok(Some(row.clone()));
        }
        self.store.row(key)
    }
    fn operation(&self, id: format::ChainOperationIdV1) -> Result<format::Operation, StoreError> {
        id.operation()?;
        let Some(Row::Operation(operation)) =
            self.row(&format::key(1, id.connection, id.serial))?
        else {
            return Err(StoreError::Corrupt);
        };
        Ok(operation)
    }
    fn connection(&self, id: [u8; 16]) -> Result<format::Connection, StoreError> {
        let Some(Row::Connection(connection)) = self.row(&format::key(0, id, 0))? else {
            return Err(StoreError::Corrupt);
        };
        Ok(connection)
    }
    fn member(
        &self,
        operation: &format::Operation,
    ) -> Result<DirectoryChainSelectionV1, StoreError> {
        let reference = operation
            .directory_chain_ref
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        if reference.anchor.connection != operation.connection {
            return Err(StoreError::Corrupt);
        }
        let anchor = self.operation(reference.anchor)?;
        let selection = first_selection(&anchor)?;
        if reference.digest != selection.selection_digest {
            return Err(StoreError::Corrupt);
        }
        member_selection(operation, &selection, reference.index)?;
        if reference.index == 0 {
            if operation.serial != anchor.serial
                || operation.directory_chain_anchor != anchor.directory_chain_anchor
            {
                return Err(StoreError::Corrupt);
            }
        } else {
            let predecessor = self.operation(reference.predecessor.ok_or(StoreError::Corrupt)?)?;
            let previous = predecessor
                .directory_chain_ref
                .as_ref()
                .ok_or(StoreError::Corrupt)?;
            if operation.directory_chain_anchor.is_some()
                || operation.directory_chain_retired.is_some()
                || predecessor.connection != operation.connection
                || predecessor.stage != 4
                || previous.anchor != reference.anchor
                || previous.digest != reference.digest
                || previous.index.checked_add(1) != Some(reference.index)
                || operation.baseline != Some(predecessor.revision)
                || operation.expected != predecessor.revision
                || operation.serial != format::next(predecessor.serial)?
            {
                return Err(StoreError::Corrupt);
            }
        }
        Ok(selection)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the complete bounded correlation or future-phase matrix in a single auditable predicate."
    )]
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join differently named fields across operation, cursor, connection and native admission records."
    )]
    fn cursor(
        &self,
        connection: &format::Connection,
        cursor: &format::DirectoryChainCursorV1,
        retirement: Option<format::ChainRetirementReason>,
    ) -> Result<(), StoreError> {
        if cursor.anchor.connection != connection.id
            || cursor.progress_revision == 0
            || cursor.progress_revision > connection.revision
        {
            return Err(StoreError::Corrupt);
        }
        let anchor = self.operation(cursor.anchor)?;
        let selection = first_selection(&anchor)?;
        let count = selection.member_count()?;
        if cursor.digest != selection.selection_digest
            || cursor.next_index > count
            || selection.endpoints
                != [
                    identity(&connection.source.identity),
                    identity(&connection.destination.identity),
                ]
        {
            return Err(StoreError::Corrupt);
        }
        let committed_baseline = if let Some(previous) = cursor.predecessor {
            Some(self.operation(previous)?.revision)
        } else {
            selection.baseline
        };
        if connection.baseline != committed_baseline {
            return Err(StoreError::Corrupt);
        }
        if let Some(revision) = committed_baseline {
            self.store.read_baseline(
                connection,
                connection.baseline_root,
                connection.baseline_count,
                revision,
            )?;
        }
        if let Some(previous) = cursor.predecessor {
            let operation = self.operation(previous)?;
            let reference = operation
                .directory_chain_ref
                .as_ref()
                .ok_or(StoreError::Corrupt)?;
            if previous.connection != connection.id
                || operation.stage != 4
                || reference.anchor != cursor.anchor
                || reference.digest != cursor.digest
                || reference.index.checked_add(1) != Some(cursor.next_index)
                || (cursor.current.is_none() && operation.revision != cursor.progress_revision)
            {
                return Err(StoreError::Corrupt);
            }
        } else if cursor.next_index != 0 {
            return Err(StoreError::Corrupt);
        }
        if let Some(current) = cursor.current {
            let operation = self.operation(current)?;
            let reference = operation
                .directory_chain_ref
                .as_ref()
                .ok_or(StoreError::Corrupt)?;
            if current.connection != connection.id
                || reference.anchor != cursor.anchor
                || reference.digest != cursor.digest
                || reference.index != cursor.next_index
                || reference.predecessor != cursor.predecessor
                || operation.revision < cursor.progress_revision
                || operation.stage == 4
            {
                return Err(StoreError::Corrupt);
            }
            if retirement == Some(format::ChainRetirementReason::Completed) {
                return Err(StoreError::Corrupt);
            }
            if retirement == Some(format::ChainRetirementReason::Abandoned)
                && (operation.stage != 7 || operation.revision != cursor.progress_revision)
            {
                return Err(StoreError::Corrupt);
            }
            if retirement.is_none() {
                let schedule = connection
                    .directory_chain_schedule
                    .as_ref()
                    .ok_or(StoreError::Corrupt)?;
                if schedule.state != Schedule::Disconnected
                    && operation.revision != cursor.progress_revision
                {
                    return Err(StoreError::Corrupt);
                }
                if operation.stage == 7 {
                    if connection.pending.is_some()
                        || !matches!(
                            schedule.state,
                            Schedule::Abandoning | Schedule::Disconnected
                        )
                    {
                        return Err(StoreError::Corrupt);
                    }
                } else if connection.pending != Some(current.serial) {
                    return Err(StoreError::Corrupt);
                }
            }
        } else if retirement.is_none() && connection.pending.is_some() {
            return Err(StoreError::Corrupt);
        }
        match retirement {
            Some(format::ChainRetirementReason::Completed)
                if cursor.next_index != count || cursor.predecessor.is_none() =>
            {
                return Err(StoreError::Corrupt);
            }
            Some(format::ChainRetirementReason::Abandoned) | None if cursor.next_index >= count => {
                return Err(StoreError::Corrupt);
            }
            _ => (),
        }
        Ok(())
    }
}

impl ConnectionStore {
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the complete bounded correlation or future-phase matrix in a single auditable predicate."
    )]
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join differently named fields across operation, cursor, connection and native admission records."
    )]
    pub(super) fn validate_chain_history(&self, replacements: &[Row]) -> Result<(), StoreError> {
        let view = ChainRows {
            store: self,
            replacements,
        };
        let existing = self.rows()?.filter(|row| match row.as_ref() {
            Err(_) => true,
            Ok(row) => !replacements
                .iter()
                .any(|replacement| replacement.key() == row.key()),
        });
        let mut members = std::collections::BTreeMap::<
            ([u8; 16], u64),
            std::collections::BTreeMap<u8, u64>,
        >::new();
        for row in existing.chain(replacements.iter().cloned().map(Ok)) {
            match row? {
                Row::Connection(connection) => {
                    match (
                        &connection.directory_chain,
                        &connection.directory_chain_schedule,
                    ) {
                        (None, None) => (),
                        (Some(cursor), Some(schedule)) => {
                            view.cursor(&connection, cursor, None)?;
                            let anchor = view.operation(cursor.anchor)?;
                            // Every control advances both scalars; Fence alone adds one
                            // further schedule revision and cannot be repeated.
                            let sequence_offset = if schedule.state == Schedule::Disconnected {
                                2
                            } else {
                                1
                            };
                            if anchor.directory_chain_retired.is_some()
                                || schedule.control_sequence.checked_add(sequence_offset)
                                    != Some(schedule.revision)
                            {
                                return Err(StoreError::Corrupt);
                            }
                            if matches!(schedule.state, Schedule::Stopped { .. }) {
                                if let Some(current) = cursor.current {
                                    let operation = view.operation(current)?;
                                    let stop = operation
                                        .directory_chain_stop_boundary
                                        .as_ref()
                                        .ok_or(StoreError::Corrupt)?;
                                    // Ack advanced the schedule once. Paid inspection may
                                    // advance operation/progress afterward without a new stop.
                                    if stop.schedule_revision.checked_add(1)
                                        != Some(schedule.revision)
                                    {
                                        return Err(StoreError::Corrupt);
                                    }
                                }
                            }
                            let selection = first_selection(&anchor)?;
                            if schedule.state == Schedule::Disconnected {
                                if connection.lifecycle == 0
                                    || connection.fence != format::next(selection.original_fence)?
                                {
                                    return Err(StoreError::Corrupt);
                                }
                            } else if connection.lifecycle != 0
                                || connection.fence != selection.original_fence
                                || schedule.revision == u64::MAX
                                || cursor.progress_revision != connection.revision
                            {
                                return Err(StoreError::Corrupt);
                            }
                        }
                        _ => return Err(StoreError::Corrupt),
                    }
                }
                Row::Operation(operation)
                    if operation.directory_chain_ref.is_some()
                        || operation.directory_chain_anchor.is_some()
                        || operation.directory_chain_retired.is_some()
                        || operation.directory_chain_stop_boundary.is_some() =>
                {
                    operation.decode()?;
                    let selection = view.member(&operation)?;
                    view.member_phase(&operation)?;
                    if operation.stage == 4 {
                        let connection = view.connection(operation.connection)?;
                        let baseline = self.read_baseline(
                            &connection,
                            connection.baseline_root,
                            connection.baseline_count,
                            connection.baseline.ok_or(StoreError::Corrupt)?,
                        )?;
                        let expected = if operation.decision == 1 {
                            &operation.source
                        } else {
                            &operation.destination
                        };
                        if baseline
                            .entries()
                            .and_then(|entries| entries.get(&operation.path.to_relative().ok()?))
                            != expected.as_ref().map(|state| state.clone().into()).as_ref()
                        {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    let reference = operation
                        .directory_chain_ref
                        .as_ref()
                        .ok_or(StoreError::Corrupt)?;
                    let connection = view.connection(operation.connection)?;
                    let anchor = view.operation(reference.anchor)?;
                    let cursor = if let Some(retired) = &anchor.directory_chain_retired {
                        if connection
                            .directory_chain
                            .as_ref()
                            .is_some_and(|cursor| cursor.anchor == reference.anchor)
                        {
                            return Err(StoreError::Corrupt);
                        }
                        view.cursor(&connection, &retired.cursor, Some(retired.reason))?;
                        &retired.cursor
                    } else {
                        connection
                            .directory_chain
                            .as_ref()
                            .filter(|cursor| cursor.anchor == reference.anchor)
                            .ok_or(StoreError::Corrupt)?
                    };
                    if !matches!(operation.stage, 0 | 6 | 2 | 3 | 4 | 7)
                        || (reference.index < cursor.next_index && operation.stage != 4)
                        || reference.index > cursor.next_index
                        || (reference.index == cursor.next_index
                            && cursor.current.map(|id| id.serial) != Some(operation.serial))
                    {
                        return Err(StoreError::Corrupt);
                    }
                    if let Some(stop) = &operation.directory_chain_stop_boundary {
                        if stop.operation_revision <= operation.expected
                            || stop.operation_revision > operation.revision
                            || stop.schedule_revision < 2
                            || stop.schedule_revision == u64::MAX
                            || (anchor.directory_chain_retired.is_none()
                                && connection.directory_chain_schedule.as_ref().is_some_and(
                                    |schedule| stop.schedule_revision > schedule.revision,
                                ))
                        {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    if let Some(retired) = &operation.directory_chain_retired {
                        if reference.index != 0
                            || retired.cursor.anchor != reference.anchor
                            || retired.cursor.digest != selection.selection_digest
                        {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    if members
                        .entry((reference.anchor.connection, reference.anchor.serial))
                        .or_default()
                        .insert(reference.index, operation.serial)
                        .is_some()
                    {
                        return Err(StoreError::Corrupt);
                    }
                }
                Row::NativeRegistration(registration) => {
                    let operation = match view.row(&format::key(
                        1,
                        registration.connection,
                        registration.serial,
                    ))? {
                        Some(Row::Operation(operation)) => operation,
                        _ if registration.native_kind
                            == Some(format::NativeOperationKind::DirectoryV1)
                            && registration.baseline.is_none() =>
                        {
                            return Err(StoreError::Corrupt);
                        }
                        _ => continue,
                    };
                    if registration.native_kind == Some(format::NativeOperationKind::DirectoryV1)
                        && registration.baseline.is_none()
                        && !absent_first_directory(&operation)?
                    {
                        return Err(StoreError::Corrupt);
                    }
                    if operation.directory_chain_ref.is_none() {
                        registration.validate()?;
                        continue;
                    }
                    let selection = view.member(&operation)?;
                    if registration.native_kind != operation.native_kind
                        || registration.admission != format::next(operation.expected)?
                        || registration.baseline != operation.baseline
                        || registration.fence != selection.original_fence
                        || registration.side != u8::from(operation.decision == 1)
                        || registration.outcome_displaced.is_some()
                    {
                        return Err(StoreError::Corrupt);
                    }
                    if absent_first_directory(&operation)? {
                        registration.validate_history_candidate()?;
                    } else {
                        registration.validate()?;
                    }
                    for bytes in [
                        &registration.intent,
                        &registration.stage,
                        &registration.outcome,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if descriptor_progress(&operation, bytes)? >= operation.revision {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    if registration.intent.is_some() {
                        if operation.stage == 0 || registration.stage.is_none() {
                            return Err(StoreError::Corrupt);
                        }
                        if operation.native_kind.is_some() {
                            Self::directory_registration_join(&operation, &registration)?;
                        } else {
                            use crate::connection::transaction::native::descriptor as d;
                            let admission = d::Admission::decode(
                                operation
                                    .native_identity
                                    .as_ref()
                                    .ok_or(StoreError::Corrupt)?,
                            )
                            .map_err(|_| StoreError::Corrupt)?;
                            let intent =
                                d::decode(registration.intent.as_ref().ok_or(StoreError::Corrupt)?)
                                    .map_err(|_| StoreError::Corrupt)?;
                            let d::Body::Intent(body) = intent.body else {
                                return Err(StoreError::Corrupt);
                            };
                            if body.path != admission.path
                                || body.endpoints != admission.endpoints
                                || body.expected != admission.states
                                || body.chains != admission.chains
                                || body.direction != admission.direction
                                || body.decision != admission.decision
                                || body.selection != d::Selection::None
                            {
                                return Err(StoreError::Corrupt);
                            }
                        }
                    }
                }
                _ => (),
            }
        }
        for ((connection, serial), members) in members {
            let anchor = view.operation(format::ChainOperationIdV1 { connection, serial })?;
            let c = view.connection(connection)?;
            let cursor = anchor
                .directory_chain_retired
                .as_ref()
                .map(|retired| &retired.cursor)
                .or(c.directory_chain.as_ref())
                .ok_or(StoreError::Corrupt)?;
            let expected = usize::from(cursor.next_index)
                .checked_add(usize::from(cursor.current.is_some()))
                .ok_or(StoreError::Corrupt)?;
            if members.len() != expected
                || members
                    .keys()
                    .copied()
                    .ne(0..u8::try_from(expected).map_err(|_| StoreError::Corrupt)?)
            {
                return Err(StoreError::Corrupt);
            }
        }
        Ok(())
    }
}

impl ConnectionStore {
    pub(super) fn require_no_chain_history(&self, id: ConnectionId) -> Result<(), StoreError> {
        self.require_no_review_or_chain_history(id)
    }
    pub(super) fn validate_live_chain_change(&self, change: &Change) -> Result<(), StoreError> {
        match change {
            Change::Fence { .. } | Change::Disconnect { .. } => return Ok(()),
            Change::Create { record } => {
                return if record.directory_chain.is_none()
                    && record.directory_chain_schedule.is_none()
                {
                    Ok(())
                } else {
                    Err(StoreError::RecoveryRequired)
                };
            }
            Change::InitialReviewAccepted { .. }
            | Change::ReviewedIntent { .. }
            | Change::ReviewedChainControl { .. }
            | Change::InitialReviewControl { .. }
            | Change::ReviewedOrdinaryStopBoundary { .. }
            | Change::DirectoryChainControl { .. }
            | Change::DirectoryChainStopBoundary { .. } => {
                return Err(StoreError::RecoveryRequired);
            }
            Change::Intent { operation }
                if operation.directory_chain_ref.is_some()
                    || operation.directory_chain_anchor.is_some()
                    || operation.directory_chain_retired.is_some()
                    || operation.directory_chain_stop_boundary.is_some() =>
            {
                return Err(StoreError::RecoveryRequired);
            }
            _ => (),
        }
        self.require_no_chain_history(ConnectionId(change_connection(change)?))
    }
}

impl ChainRows<'_> {
    /// Project only descriptor facts already recorded before this accounting event.
    /// An older stop remains valid after resumed work records a later outcome.
    fn retained_boundary(
        &self,
        operation: &format::Operation,
        registrations: &[format::NativeRegistration],
        resulting_revision: u64,
        kind: u8,
        evidence: &format::DirectoryChainStopEvidenceV1,
    ) -> Result<(), StoreError> {
        let cutoff = resulting_revision
            .checked_sub(1)
            .ok_or(StoreError::Corrupt)?;
        let progress = |bytes: &[u8]| descriptor_progress(operation, bytes);
        let mut historical = registrations.to_vec();
        for registration in &mut historical {
            if registration
                .intent
                .as_deref()
                .map(&progress)
                .transpose()?
                .is_some_and(|revision| revision > cutoff)
            {
                registration.intent = None;
                registration.stage = None;
                registration.outcome = None;
            } else if registration
                .outcome
                .as_deref()
                .map(&progress)
                .transpose()?
                .is_some_and(|revision| revision > cutoff)
            {
                registration.outcome = None;
            }
        }
        let mut prior = operation.clone();
        prior.revision = cutoff;
        prior.native_boundary = None;
        prior.native_boundary_evidence = None;
        prior.native_directory_boundary = None;
        prior.directory_chain_stop_boundary = None;
        prior.stage = historical
            .iter()
            .rev()
            .find(|reg| reg.intent.is_some())
            .map_or(0, |reg| if reg.outcome.is_some() { 2 } else { 6 });
        let fence = self.member(operation)?.original_fence;
        match evidence {
            format::DirectoryChainStopEvidenceV1::Regular { evidence } => {
                self.store.validate_regular_boundary(
                    &prior,
                    cutoff,
                    fence,
                    kind,
                    evidence.as_ref(),
                    false,
                    &historical,
                )
            }
            format::DirectoryChainStopEvidenceV1::Directory { fact } if fact.kind() == kind => {
                ConnectionStore::validate_directory_boundary(
                    &prior,
                    cutoff,
                    fence,
                    fact,
                    &historical,
                )
            }
            format::DirectoryChainStopEvidenceV1::Directory { .. } => Err(StoreError::Corrupt),
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the complete bounded correlation or future-phase matrix in a single auditable predicate."
    )]
    fn member_phase(&self, operation: &format::Operation) -> Result<(), StoreError> {
        let mut registrations = Vec::new();
        let mut nonces = std::collections::BTreeSet::new();
        let mut gap = false;
        for attempt in 1u32..=8 {
            let mut key = format::key(5, operation.connection, operation.serial);
            key.extend(attempt.to_be_bytes());
            match self.row(&key)? {
                Some(Row::NativeRegistration(registration)) => {
                    if gap || !nonces.insert(registration.nonce) {
                        return Err(StoreError::Corrupt);
                    }
                    registrations.push(registration);
                }
                None => gap = true,
                _ => return Err(StoreError::Corrupt),
            }
        }
        if let Some(stop) = &operation.directory_chain_stop_boundary {
            let kind = stop_evidence_kind(stop.kind, &stop.evidence)?;
            self.retained_boundary(
                operation,
                &registrations,
                stop.operation_revision,
                kind,
                &stop.evidence,
            )?;
        }
        if operation.stage == 7 {
            let kind = operation.native_boundary.ok_or(StoreError::Corrupt)?;
            let evidence = if operation.native_kind.is_some() {
                format::DirectoryChainStopEvidenceV1::Directory {
                    fact: operation
                        .native_directory_boundary
                        .clone()
                        .ok_or(StoreError::Corrupt)?,
                }
            } else {
                format::DirectoryChainStopEvidenceV1::Regular {
                    evidence: operation.native_boundary_evidence.clone(),
                }
            };
            self.retained_boundary(
                operation,
                &registrations,
                operation.revision,
                kind,
                &evidence,
            )?;
        }
        if matches!(operation.stage, 6 | 2 | 3 | 4) {
            let registration = registrations.last().ok_or(StoreError::Corrupt)?;
            if registration.intent.is_none()
                || registration.stage.is_none()
                || (operation.stage == 6 && registration.outcome.is_some())
                || (matches!(operation.stage, 2 | 3 | 4) && registration.outcome.is_none())
            {
                return Err(StoreError::Corrupt);
            }
            if matches!(operation.stage, 3 | 4) {
                if operation.native_kind.is_some() {
                    let (intent, stage) =
                        ConnectionStore::directory_registration_join(operation, registration)?;
                    let outcome = directory_descriptor(
                        operation,
                        registration.outcome.as_ref().ok_or(StoreError::Corrupt)?,
                    )?;
                    outcome
                        .joined_outcome(&intent, &stage)
                        .map_err(|_| StoreError::Corrupt)?;
                } else {
                    use crate::connection::transaction::native::descriptor as d;
                    registration.validate()?;
                    let outcome =
                        d::decode(registration.outcome.as_ref().ok_or(StoreError::Corrupt)?)
                            .map_err(|_| StoreError::Corrupt)?;
                    let d::Body::Outcome(outcome) = outcome.body else {
                        return Err(StoreError::Corrupt);
                    };
                    let planned = if operation.decision == 1 {
                        &operation.source
                    } else {
                        &operation.destination
                    };
                    if outcome.kind != 0
                        || outcome.sequence != 1
                        || outcome.barriers != 3
                        || !outcome.recovery.is_empty()
                    {
                        return Err(StoreError::Corrupt);
                    }
                    for state in outcome.states {
                        let d::State::Regular { content, .. } = state else {
                            return Err(StoreError::Corrupt);
                        };
                        if planned
                            != &Some(format::State::File {
                                sha256: content.digest,
                                bytes: content.length,
                                executable: content.executable,
                            })
                        {
                            return Err(StoreError::Corrupt);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub(super) const REGULAR_OUTCOME_MAX: usize = 571;

fn singleton_size(row: &Row) -> Result<usize, StoreError> {
    format::encode(row)?;
    Ok(format::encode(&format::PageNode {
        schema: 1,
        node: format::Node::Leaf(vec![row.clone()]),
    })?
    .len())
}
impl ConnectionStore {
    /// Serialization ceilings only: synthetic descriptor bytes never enter rows,
    /// reducers, native decoders, or effect authority. Ordinary Directory callers
    /// retain their existing estimator; this wraps it with chain extension costs.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the complete bounded correlation or future-phase matrix in a single auditable predicate."
    )]
    pub(super) fn preflight_chain_intent(
        &self,
        operation: &format::Operation,
        admitted: &[Row],
    ) -> Result<(), StoreError> {
        if operation.directory_chain_ref.is_none() {
            return Ok(());
        }
        if operation.native_kind.is_some() {
            self.preflight_directory_intent(operation)?;
        }
        for row in admitted {
            singleton_size(row)?;
        }
        if !self.prepare_rows(admitted.to_vec())?.fits_overlay() {
            return Err(StoreError::LimitExceeded);
        }
        let reference = operation
            .directory_chain_ref
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        let mut connection = admitted
            .iter()
            .find_map(|row| {
                if let Row::Connection(c) = row {
                    Some(c.clone())
                } else {
                    None
                }
            })
            .ok_or(StoreError::Corrupt)?;
        let mut cursor = connection
            .directory_chain
            .clone()
            .ok_or(StoreError::Corrupt)?;
        let largest_id = format::ChainOperationIdV1 {
            connection: operation.connection,
            serial: u64::MAX,
        };
        cursor.current = Some(largest_id);
        cursor.predecessor = Some(largest_id);
        cursor.progress_revision = u64::MAX;
        cursor.next_index = 64;
        connection.revision = u64::MAX;
        connection.serial = u64::MAX;
        connection.pending = Some(u64::MAX);
        connection.baseline = Some(u64::MAX);
        connection.baseline_root = Some([255; 32]);
        connection.baseline_count = 4096;
        connection.directory_chain = Some(cursor.clone());
        connection.directory_chain_schedule = Some(format::DirectoryChainScheduleV1 {
            revision: u64::MAX,
            control_sequence: u64::MAX,
            state: Schedule::Stopping {
                reason: format::PauseOrCancel::Cancel,
            },
        });
        let directory = operation.native_kind.is_some();
        // Endpoint-source regular Stage/Outcome bodies have fixed maxima 444/571;
        // Directory375/333 are the existing shared estimator's fixed wire sizes.
        let outcome = vec![255; if directory { 333 } else { REGULAR_OUTCOME_MAX }];
        let registration = format::NativeRegistration {
            native_kind: operation.native_kind,
            connection: operation.connection,
            serial: operation.serial,
            attempt: 8,
            nonce: [255; 16],
            side: 1,
            admission: u64::MAX,
            baseline: Some(u64::MAX),
            fence: u64::MAX,
            intent: Some(vec![
                255;
                if directory {
                    15_369
                } else {
                    crate::connection::transaction::native::descriptor::MAX_BYTES
                }
            ]),
            stage: Some(vec![255; if directory { 375 } else { 444 }]),
            outcome: Some(outcome.clone()),
            outcome_displaced: None,
        };
        let evidence = if directory {
            format::DirectoryChainStopEvidenceV1::Directory {
                fact: format::DirectoryBoundaryFact::Published {
                    attempt: 8,
                    outcome: Some(outcome.clone()),
                },
            }
        } else {
            format::DirectoryChainStopEvidenceV1::Regular {
                evidence: Some(format::BoundaryEvidence {
                    attempt: 8,
                    displaced: None,
                    outcome: Some(outcome.clone()),
                }),
            }
        };
        let mut future = operation.clone();
        future.revision = u64::MAX;
        future.stage = 7;
        if let Some(budget) = &mut future.native_budget {
            budget.sequence = 256;
            budget.reserved = format::READ_LIMIT;
        }
        future.directory_chain_stop_boundary = Some(format::DirectoryChainStopBoundaryV1 {
            schedule_revision: u64::MAX,
            operation_revision: u64::MAX,
            kind: format::ChainStopKind::PublishedAccounted,
            evidence: evidence.clone(),
        });
        future.native_boundary = Some(3);
        if directory {
            future.native_directory_boundary = Some(format::DirectoryBoundaryFact::Published {
                attempt: 8,
                outcome: Some(outcome.clone()),
            });
        } else {
            future.native_boundary_evidence = Some(format::BoundaryEvidence {
                attempt: 8,
                displaced: None,
                outcome: Some(outcome.clone()),
            });
        }
        let retired = format::DirectoryChainRetirementV1 {
            reason: format::ChainRetirementReason::Abandoned,
            cursor,
        };
        let mut future_rows = vec![
            Row::Connection(connection),
            Row::NativeRegistration(registration.clone()),
        ];
        if reference.anchor.serial == operation.serial {
            future.directory_chain_retired = Some(retired);
        } else {
            let mut anchor = self.operation(reference.anchor.operation()?)?;
            anchor.directory_chain_retired = Some(retired);
            future_rows.push(Row::Operation(anchor));
        }
        future_rows.push(Row::Operation(future));
        for row in &future_rows {
            singleton_size(row)?;
        }
        if !self.prepare_rows(future_rows)?.fits_overlay() {
            return Err(StoreError::LimitExceeded);
        }
        let baseline = Change::BaselineCommit {
            connection: operation.connection,
            expected: u64::MAX,
            root: Some([255; 32]),
            count: 4096,
            source: operation.source_binding.clone(),
            destination: operation.destination_binding.clone(),
            initial_review: false,
            existing_ancestors: (!directory).then_some(format::ExistingAncestorPatchV1 {
                version: 1,
                admission: [255; 32],
                prefixes: u64::MAX,
            }),
            directory_ancestors: directory.then_some(format::DirectoryAncestorPatchV1 {
                version: 1,
                admission: [255; 32],
                prefixes: u64::MAX,
            }),
            directory_chain: Some(format::DirectoryChainCommitV1 {
                member: reference.clone(),
                expected_progress: u64::MAX,
            }),
        };
        let stop = Change::DirectoryChainStopBoundary {
            connection: operation.connection,
            serial: operation.serial,
            expected: u64::MAX,
            expected_schedule: u64::MAX,
            fence: u64::MAX,
            kind: format::ChainStopKind::PublishedAccounted,
            evidence,
        };
        let boundary = if directory {
            Change::DirectoryBoundary {
                connection: operation.connection,
                serial: operation.serial,
                expected: u64::MAX,
                operation_expected: u64::MAX,
                fence: u64::MAX,
                fact: format::DirectoryBoundaryFact::Published {
                    attempt: 8,
                    outcome: Some(outcome),
                },
            }
        } else {
            Change::NativeBoundary {
                connection: operation.connection,
                serial: operation.serial,
                expected: u64::MAX,
                operation_expected: u64::MAX,
                fence: u64::MAX,
                kind: 3,
                evidence: Some(format::BoundaryEvidence {
                    attempt: 8,
                    displaced: None,
                    outcome: Some(outcome),
                }),
            }
        };
        for change in [
            Change::Intent {
                operation: operation.clone(),
            },
            Change::NativeOutcome {
                registration,
                expected: u64::MAX,
                verified: true,
                reconstructed: true,
            },
            baseline,
            stop,
            boundary,
        ] {
            let event = format::Event {
                schema: 1,
                predecessor: [255; 32],
                change,
            };
            self.check_event_capacity(&event, format::encode(&event)?.len())?;
        }
        Ok(())
    }
}
