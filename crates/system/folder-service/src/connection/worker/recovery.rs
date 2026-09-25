//! Bounded recovery commands. Durable metadata never supplies native authority.
use super::super::{model::EntryState, store};
use super::*;
use crate::connected::{
    ConflictChoice, ConflictInfo, RecoveryAvailability, RecoveryId, RecoveryInfo, RecoveryPage,
    RecoveryReason, RetentionEligibility, SelectionPreview, SelectionReviewId,
};
use std::time::{Duration, Instant};

const REVIEW_SECONDS: u64 = 300;

/// A positively recorded absent version. Its private correlation is metadata,
/// and must be joined to a current conflict revision before use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TombstoneVersion(pub(super) store::TombstoneChoice);
impl TombstoneVersion {
    #[must_use]
    pub fn original_side(&self) -> Side {
        self.0.side()
    }
}

#[derive(Clone)]
enum Candidate {
    Recovery(store::RecoveryId),
    Tombstone(store::TombstoneChoice),
}
pub(super) struct SelectionReview {
    token: SelectionReviewId,
    deadline: Instant,
    preview: store::RestorePreview,
    candidate: Candidate,
    target: Side,
    conflict: Option<(store::ConflictId, u64)>,
}

fn recovery_id(id: RecoveryId) -> store::RecoveryId {
    store::RecoveryId {
        operation: store::OperationId {
            connection: id.connection,
            serial: id.operation,
        },
        slot: id.slot,
    }
}
fn recovery_info(entry: store::RecoveryEntry, record: &ConnectionRecord) -> RecoveryInfo {
    let availability = match entry.state {
        store::RecoveryState::Purged => RecoveryAvailability::Purged,
        store::RecoveryState::Reserved
        | store::RecoveryState::RetainedIncomplete
        | store::RecoveryState::PurgeIntent => RecoveryAvailability::Incomplete,
        _ if entry.original_path.components().len() != 1
            || !matches!(entry.content, Some(EntryState::File { .. })) =>
        {
            RecoveryAvailability::Structural
        }
        _ if record.lifecycle != Lifecycle::Active => RecoveryAvailability::Disconnected,
        _ if !cfg!(target_os = "linux") => RecoveryAvailability::UnsupportedHost,
        _ => RecoveryAvailability::RequiresNativeReview,
    };
    // Age is durable accounting, not wall-clock age. Pins/attention may still
    // protect a version even after this interval; this service never purges.
    let retention = if record.lifecycle != Lifecycle::Active
        || entry.state != store::RecoveryState::Attached
        || matches!(entry.reason, store::RecoveryReason::Conflict(_))
    {
        RetentionEligibility::Protected
    } else if entry.retained_seconds < record.policy.deletion_retention_seconds {
        RetentionEligibility::Retaining
    } else {
        RetentionEligibility::RequiresEvaluation
    };
    RecoveryInfo {
        id: RecoveryId {
            connection: entry.id.operation.connection,
            operation: entry.id.operation.serial,
            slot: entry.id.slot,
        },
        original_side: entry.original_side,
        original_path: entry.original_path,
        content: entry.content,
        reason: match entry.reason {
            store::RecoveryReason::NormalDeletion => RecoveryReason::Deletion,
            store::RecoveryReason::DisplacedVersion => RecoveryReason::DisplacedVersion,
            store::RecoveryReason::Conflict(id) => RecoveryReason::Conflict { serial: id.serial },
        },
        availability,
        retained_seconds: entry.retained_seconds,
        retention,
    }
}

pub(super) fn conflict_info(
    store: &ConnectionStore,
    record: store::ConflictRecord,
) -> Result<ConflictInfo, ConnectionError> {
    // A selected collision can carry a candidate from an older operation. The
    // canonical join, rather than the raw conflict row, supplies that choice.
    let record = store.conflict_record(record.id).map_err(store_error)?;
    let connection = store
        .load_connection(record.id.connection)
        .map_err(store_error)?;
    let mut choices = Vec::new();
    if !record.resolved && record.captured.is_some() && record.path.components().len() == 1 {
        if record.preserved.len() > PAGE_LIMIT {
            return Err(ConnectionError::LimitExceeded);
        }
        for id in &record.preserved {
            let entry = store.worker_recovery_entry(*id).map_err(store_error)?;
            if matches!(
                entry.state,
                store::RecoveryState::BytesVerified | store::RecoveryState::Attached
            ) && matches!(entry.content, Some(EntryState::File { .. }))
            {
                choices.push(ConflictChoice::Recovery(recovery_info(entry, &connection)));
            }
        }
        choices.extend(
            store
                .tombstone_choices(record.id)
                .map_err(store_error)?
                .into_iter()
                .map(|choice| ConflictChoice::Tombstone(TombstoneVersion(choice))),
        );
        if choices.len() > PAGE_LIMIT {
            return Err(ConnectionError::LimitExceeded);
        }
    }
    Ok(ConflictInfo {
        serial: record.id.serial,
        revision: record.revision,
        kind: record.kind,
        path: record.path,
        source: record.source,
        destination: record.destination,
        preserved_versions: record.preserved.len(),
        resolved: record.resolved,
        choices,
    })
}

impl Worker {
    pub(super) fn recovery_page(
        &self,
        id: ConnectionId,
        cursor: Option<RecoveryCursor>,
        limit: usize,
    ) -> Result<RecoveryPage, ConnectionError> {
        if !(1..=PAGE_LIMIT).contains(&limit) {
            return Err(ConnectionError::LimitExceeded);
        }
        let scope = self.store.worker_projection().map_err(store_error)?;
        let record = self.store.load_connection(id).map_err(store_error)?;
        let page = self
            .store
            .worker_recovery_page(id, cursor.map(|cursor| cursor.0), limit)
            .map_err(store_error)?;
        let result = RecoveryPage {
            entries: page
                .entries
                .into_iter()
                .map(|entry| recovery_info(entry, &record))
                .collect(),
            next: page.next.map(RecoveryCursor),
        };
        scope.finish().map_err(store_error)?;
        Ok(result)
    }
    pub(super) fn conflict_page(
        &self,
        id: ConnectionId,
        cursor: Option<ConflictCursor>,
    ) -> Result<ConflictPage, ConnectionError> {
        let scope = self.store.worker_projection().map_err(store_error)?;
        let page = self
            .store
            .worker_conflicts(id, cursor.map(|cursor| cursor.0), PAGE_LIMIT)
            .map_err(store_error)?;
        let result = ConflictPage {
            entries: page
                .entries
                .into_iter()
                .map(|record| conflict_info(&self.store, record))
                .collect::<Result<_, _>>()?,
            next: page.next.map(ConflictCursor),
        };
        scope.finish().map_err(store_error)?;
        Ok(result)
    }

    pub(super) fn preview_recovery(
        &mut self,
        id: RecoveryId,
        target: Side,
    ) -> Result<SelectionPreview, ConnectionError> {
        supported()?;
        self.preview_selection(
            id.connection,
            Candidate::Recovery(recovery_id(id)),
            target,
            None,
        )
    }

    pub(super) fn preview_conflict(
        &mut self,
        id: ConnectionId,
        serial: u64,
        revision: u64,
        choice: ConflictChoice,
        target: Side,
    ) -> Result<SelectionPreview, ConnectionError> {
        supported()?;
        let conflict = store::ConflictId {
            connection: id,
            serial,
        };
        let record = self.store.conflict_record(conflict).map_err(store_error)?;
        if record.resolved || record.revision != revision {
            return Err(ConnectionError::StaleReview);
        }
        let candidate = match choice {
            ConflictChoice::Recovery(entry) => {
                let selected = recovery_id(entry.id);
                if entry.id.connection != id || !record.preserved.contains(&selected) {
                    return Err(ConnectionError::StaleReview);
                }
                Candidate::Recovery(selected)
            }
            ConflictChoice::Tombstone(choice) => {
                if !self
                    .store
                    .tombstone_choices(conflict)
                    .map_err(store_error)?
                    .contains(&choice.0)
                {
                    return Err(ConnectionError::StaleReview);
                }
                Candidate::Tombstone(choice.0)
            }
        };
        self.preview_selection(id, candidate, target, Some((conflict, revision)))
    }

    /// Establish or revalidate the one active session without beginning an intent.
    /// A preview can explicitly reopen a paused session; accepting an old token cannot.
    fn selection_session(
        &mut self,
        id: ConnectionId,
        allow_open: bool,
    ) -> Result<(), ConnectionError> {
        let active = self
            .active
            .as_mut()
            .filter(|active| active.observer.snapshot().connection.id == id)
            .ok_or(ConnectionError::StaleReview)?;
        if active.running {
            return Err(ConnectionError::Busy);
        }
        let record = self.store.load_connection(id).map_err(store_error)?;
        if record.lifecycle != Lifecycle::Active {
            return Err(ConnectionError::Disconnected);
        }
        let pending = self
            .store
            .executor_position(id)
            .map_err(store_error)?
            .0
            .is_some();
        if active.pins.is_none() {
            if !allow_open {
                return Err(ConnectionError::StaleReview);
            }
            let (source, destination) =
                pin_pair(&record.source, &record.destination).map_err(|(side, error)| {
                    endpoint_refused(active, side, error, pending);
                    error
                })?;
            let session = self.store.transaction_session(id).map_err(store_error)?;
            active.pins = Some(Pins {
                source,
                destination,
                session,
            });
        }
        let pins = active.pins.as_ref().ok_or(ConnectionError::StaleReview)?;
        for (side, endpoint) in [
            (Side::Source, &pins.source),
            (Side::Destination, &pins.destination),
        ] {
            if let Err(error) = endpoint.revalidate_for_write() {
                let error = endpoint_error(error);
                endpoint_refused(active, side, error, pending);
                return Err(error);
            }
        }
        active
            .observer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_operation = pending;
        if pending {
            set_phase(
                active,
                Phase::RecoveryRequired(ConnectionError::RecoveryRequired),
            );
            return Err(ConnectionError::RecoveryRequired);
        }
        self.store
            .executor_session(id, &pins.session)
            .map_err(store_error)?;
        pins.session.require_idle().map_err(store_error)
    }

    fn preview_selection(
        &mut self,
        id: ConnectionId,
        candidate: Candidate,
        target: Side,
        conflict: Option<(store::ConflictId, u64)>,
    ) -> Result<SelectionPreview, ConnectionError> {
        self.selection_session(id, true)?;
        let active = self.active.as_mut().ok_or(ConnectionError::StaleReview)?;
        // A new review supersedes prior consent even when its observation fails.
        active.selection = None;
        active.preview = None;
        active.reviewed = None;
        set_phase(active, Phase::Pending(Pending::NotCompared));
        let pins = active.pins.as_ref().ok_or(ConnectionError::StaleReview)?;
        let (content, original_side) = match &candidate {
            Candidate::Recovery(selected) => {
                let entry = self
                    .store
                    .worker_recovery_entry(*selected)
                    .map_err(store_error)?;
                let record = self.store.load_connection(id).map_err(store_error)?;
                let entry = recovery_info(entry, &record);
                if entry.availability != RecoveryAvailability::RequiresNativeReview {
                    return Err(ConnectionError::Unavailable);
                }
                (entry.content, entry.original_side)
            }
            Candidate::Tombstone(choice) => (None, choice.side()),
        };
        let preview =
            build_preview(&self.store, &candidate, target, conflict, pins).map_err(store_error)?;
        if preview.intent().path.components().len() != 1
            || [
                &preview.intent().source_before,
                &preview.intent().destination_before,
            ]
            .into_iter()
            .flatten()
            .any(|state| !matches!(state, EntryState::File { .. }))
        {
            set_phase(active, Phase::Pending(Pending::Structural));
            return Err(ConnectionError::Unavailable);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ConnectionError::LimitExceeded)?;
        let token = SelectionReviewId(ReviewId {
            generation: self.generation,
            lifetime: Arc::clone(&self.lifetime),
        });
        let result = SelectionPreview {
            review: token.clone(),
            connection: id,
            conflict: conflict.map(|(conflict, revision)| (conflict.serial, revision)),
            path: preview.intent().path.clone(),
            target,
            source: preview.intent().source_before.clone(),
            destination: preview.intent().destination_before.clone(),
            candidate: content,
            original_side,
            expires_in_seconds: REVIEW_SECONDS,
        };
        active.selection = Some(SelectionReview {
            token,
            deadline: Instant::now() + Duration::from_secs(REVIEW_SECONDS),
            preview,
            candidate,
            target,
            conflict,
        });
        Ok(result)
    }

    pub(super) fn restore_selection(
        &mut self,
        token: SelectionReviewId,
        replace: bool,
    ) -> Result<(), ConnectionError> {
        let active = self.active.as_mut().ok_or(ConnectionError::StaleReview)?;
        if active
            .selection
            .as_ref()
            .is_none_or(|review| review.token != token)
        {
            return Err(ConnectionError::StaleReview);
        }
        let review = active
            .selection
            .take()
            .ok_or(ConnectionError::StaleReview)?;
        if Instant::now() >= review.deadline {
            return Err(ConnectionError::StaleReview);
        }
        supported()?;
        let id = review.preview.intent().connection;
        self.selection_session(id, false)?;
        let active = self.active.as_mut().ok_or(ConnectionError::StaleReview)?;
        let pins = active.pins.as_ref().ok_or(ConnectionError::StaleReview)?;
        let fresh = build_preview(
            &self.store,
            &review.candidate,
            review.target,
            review.conflict,
            pins,
        )
        .map_err(store_error)?;
        if Instant::now() >= review.deadline || fresh.intent() != review.preview.intent() {
            set_phase(active, Phase::Pending(Pending::NotCompared));
            return Err(ConnectionError::StaleReview);
        }
        #[cfg(target_os = "linux")]
        {
            set_phase(active, Phase::Syncing);
            // No UI cancellation is forwarded into this begun durable step.
            let selected = if replace {
                review.preview.accept_replacement()
            } else {
                review.preview.accept()
            };
            let outcome = transaction::resolve_regular(
                &mut self.store,
                selected,
                &pins.session,
                &pins.source,
                &pins.destination,
                &AtomicBool::new(false),
            );
            let error = match &outcome {
                transaction::TransactionOutcome::RecoveryRequired { error, .. } => {
                    Some(match error {
                        StoreError::StaleRevision => ConnectionError::StaleReview,
                        StoreError::IdentityChanged => ConnectionError::IdentityChanged,
                        StoreError::Disconnected => ConnectionError::Disconnected,
                        StoreError::RecoverySpace => ConnectionError::RecoverySpace,
                        StoreError::Busy => ConnectionError::Busy,
                        StoreError::UnsupportedDurability => ConnectionError::Unverified,
                        StoreError::Corrupt => ConnectionError::CorruptStore,
                        StoreError::LimitExceeded => ConnectionError::LimitExceeded,
                        StoreError::Persistence(_) => ConnectionError::Persistence,
                        StoreError::RecoveryRequired
                        | StoreError::LegacyNativePending
                        | StoreError::Cancelled => ConnectionError::RecoveryRequired,
                    })
                }
                _ => None,
            };
            self.outcome(outcome);
            error.map_or(Ok(()), Err)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = replace;
            Err(ConnectionError::UnsupportedHost)
        }
    }
}

fn build_preview(
    store: &ConnectionStore,
    candidate: &Candidate,
    target: Side,
    conflict: Option<(store::ConflictId, u64)>,
    pins: &Pins,
) -> Result<store::RestorePreview, StoreError> {
    let cancel = AtomicBool::new(false);
    let source = endpoint::observe(&pins.source, &cancel);
    let destination = endpoint::observe(&pins.destination, &cancel);
    match candidate {
        Candidate::Recovery(selected) => {
            store.restore_preview(*selected, target, conflict, source, destination)
        }
        Candidate::Tombstone(selected) => store.tombstone_preview(
            selected.clone(),
            target,
            conflict.ok_or(StoreError::StaleRevision)?,
            source,
            destination,
        ),
    }
}
