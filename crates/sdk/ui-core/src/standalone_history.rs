//! A private History session pinned to the document that opened it.
//!
//! The host owns presentation and semantic actions. This model owns the captured
//! destination and read authorization; it never redirects a stale destination
//! to the outer app document and never commits a live edit while browsing.

use automerge::{AutoCommit, ChangeHash, transaction::Transactable};

use crate::bound_dispatch::{BoundDispatchError, CallbackBinding};
use crate::history_source::{
    AuthorDirectory, CompareError, HistoryComparison, HistorySource, committed_heads,
    compare_entries,
};
use crate::permissions::{PermissionContext, PermissionScope, Verb};
use crate::record::{RecordId, RecordOpenError};
use crate::widget::{HistoryEntry, HistoryEntryId};

#[derive(Clone)]
pub(crate) enum HistoryTarget {
    Bound(Box<CallbackBinding>),
    Legacy {
        app_id: String,
        record: Option<RecordId>,
        /// Supplied by the host and replaced whenever it replaces the document,
        /// including reopening the same record. A record name alone is not a
        /// document lifetime.
        document_identity: uuid::Uuid,
    },
}

pub(crate) struct HistoryReadContext<'a> {
    pub document: &'a AutoCommit,
    pub active_record: Option<&'a RecordId>,
    pub document_identity: uuid::Uuid,
    pub permissions: &'a PermissionContext,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HistoryError {
    Denied,
    Stale,
    Unavailable,
    InvalidSelection,
    Comparison(CompareError),
    /// The live document moved on after the restore was reviewed.
    StaleReview,
    /// An edit is still open; restoring would fold it into the restore.
    PendingEdits,
}

/// One reviewed restore: the version to return to, and the exact committed
/// frontier the person was looking at when they asked for it. Confirming is
/// refused unless the live document is still at that frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestoreReview {
    pub(crate) entry: HistoryEntryId,
    pub(crate) label: String,
    target_heads: Vec<ChangeHash>,
    reviewed_live_heads: Vec<ChangeHash>,
    document_identity: uuid::Uuid,
}

impl RestoreReview {
    /// The committed heads of the version being restored.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn target_heads(&self) -> &[ChangeHash] {
        &self.target_heads
    }

    /// The live frontier the review was taken against, sorted.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reviewed_live_heads(&self) -> &[ChangeHash] {
        &self.reviewed_live_heads
    }
}

fn sorted(mut heads: Vec<ChangeHash>) -> Vec<ChangeHash> {
    heads.sort_unstable();
    heads
}

/// A review the person confirmed, with the document it was taken on.
pub(crate) struct ConfirmedHistoryRestore {
    pub(crate) review: RestoreReview,
    pub(crate) target: HistoryTarget,
}

/// Write a confirmed review into the live document as one new, undoable
/// change attributed to `author`. The frontier is checked again here, inside
/// the same mutation, so nothing that moved the document since confirmation
/// can be silently reverted. A bound record is written through its own action
/// frame and undo log, attributed to the binding's author.
///
/// # Errors
/// Refuses a replaced document, an open edit, a moved frontier or revoked
/// Write without touching the document.
pub(crate) fn apply_restore(
    state: &mut crate::state::State,
    confirmed: &ConfirmedHistoryRestore,
    author: Option<crate::permissions::UserId>,
) -> Result<bool, HistoryError> {
    let review = &confirmed.review;
    if state.document_identity() != review.document_identity {
        return Err(HistoryError::Stale);
    }
    if let HistoryTarget::Bound(binding) = &confirmed.target {
        return apply_record_restore(&mut state.doc, binding, review);
    }
    if state.doc.pending_ops() > 0 {
        return Err(HistoryError::PendingEdits);
    }
    if sorted(committed_heads(&state.doc)) != review.reviewed_live_heads {
        return Err(HistoryError::StaleReview);
    }
    let context = tilo_actions::ActionContext {
        action_label: crate::i18n::I18nManager::lookup_plain("standalone-history-restore-action"),
        ..tilo_actions::ActionContext::default()
    };
    Ok(state.restore_to_heads_with_context(&review.target_heads, None, None, author, &context))
}

fn apply_record_restore(
    outer: &mut AutoCommit,
    binding: &CallbackBinding,
    review: &RestoreReview,
) -> Result<bool, HistoryError> {
    use crate::state::owned_restore::{OwnedRestoreError, restore_reviewed_record};
    // A word still being typed becomes its own step first, as when the
    // framework host opens a review; if that moved the record, the frontier
    // check below refuses the stale review.
    if let Err(error) = binding.close_open_step(outer) {
        return Err(bound_error(&error));
    }
    let receipt = restore_reviewed_record(
        outer,
        binding,
        &review.reviewed_live_heads,
        &review.target_heads,
    )
    .map_err(|error| match error {
        OwnedRestoreError::Denied => HistoryError::Denied,
        OwnedRestoreError::StaleReview => HistoryError::StaleReview,
        OwnedRestoreError::PendingEdits
        | OwnedRestoreError::UnfinishedActionBoundary
        | OwnedRestoreError::ActiveAction => HistoryError::PendingEdits,
        OwnedRestoreError::Bound(error) => bound_error(&error),
        _ => HistoryError::Unavailable,
    })?;
    if receipt.changed {
        // Record persistence and sync observe the new canonical heads.
        binding
            .records
            .schedule_committed_history(&binding.checkout.id);
    }
    Ok(receipt.changed)
}

fn bound_error(error: &BoundDispatchError) -> HistoryError {
    match error {
        BoundDispatchError::ClosedLifetime
        | BoundDispatchError::MissingNode
        | BoundDispatchError::Record(
            RecordOpenError::MissingRecord | RecordOpenError::IdentityMismatch,
        ) => HistoryError::Stale,
        _ => HistoryError::Unavailable,
    }
}

impl HistoryTarget {
    /// Restoring writes, so it needs Write, not Read: the app's grant for an
    /// app-owned document, the binding's current authority for a bound record,
    /// whose write then goes through that record's action frame.
    fn admit_restore(&self, context: &HistoryReadContext<'_>) -> Result<(), HistoryError> {
        match self {
            Self::Bound(binding) => {
                self.read(context, |_| ())?;
                let allowed = binding
                    .permissions
                    .try_borrow()
                    .map_err(|_| HistoryError::Unavailable)?
                    .check(&PermissionScope::app(&binding.app_id, Verb::Write));
                if !allowed {
                    return Err(HistoryError::Denied);
                }
                Ok(())
            }
            Self::Legacy {
                app_id,
                record,
                document_identity,
            } => {
                if *document_identity != context.document_identity
                    || record.as_ref() != context.active_record
                {
                    return Err(HistoryError::Stale);
                }
                if !context
                    .permissions
                    .check(&PermissionScope::app(app_id, Verb::Write))
                {
                    return Err(HistoryError::Denied);
                }
                Ok(())
            }
        }
    }

    fn read<R>(
        &self,
        context: &HistoryReadContext<'_>,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Result<R, HistoryError> {
        match self {
            Self::Bound(binding) => {
                if binding.registry_owner.upgrade().is_none() {
                    return Err(HistoryError::Stale);
                }
                let allowed = binding
                    .permissions
                    .try_borrow()
                    .map_err(|_| HistoryError::Unavailable)?
                    .check(&PermissionScope::app(&binding.app_id, Verb::Read));
                if !allowed {
                    return Err(HistoryError::Denied);
                }
                binding
                    .read_document(context.document, |doc, _| read(doc))
                    .map_err(|error| bound_error(&error))
            }
            Self::Legacy {
                app_id,
                record,
                document_identity,
            } => {
                if *document_identity != context.document_identity
                    || record.as_ref() != context.active_record
                {
                    return Err(HistoryError::Stale);
                }
                if !context
                    .permissions
                    .check(&PermissionScope::app(app_id, Verb::Read))
                {
                    return Err(HistoryError::Denied);
                }
                Ok(read(context.document))
            }
        }
    }
}

pub(crate) struct StandaloneHistory {
    target: HistoryTarget,
    source: HistorySource,
    heads: Option<Vec<ChangeHash>>,
    rows: Vec<HistoryEntry>,
    selection: Vec<HistoryEntryId>,
    comparison: Option<HistoryComparison>,
    invalidated: Option<HistoryError>,
}

impl StandaloneHistory {
    pub(crate) fn open(
        target: HistoryTarget,
        context: &HistoryReadContext<'_>,
        authors: AuthorDirectory,
    ) -> Result<Self, HistoryError> {
        let mut session = Self {
            target,
            source: HistorySource::new().with_authors(authors),
            heads: None,
            rows: Vec::new(),
            selection: Vec::new(),
            comparison: None,
            invalidated: None,
        };
        session.refresh(context)?;
        Ok(session)
    }

    /// Validate authority for an immutable observation of the last rendered
    /// frame. This does not refresh rows or close a pending edit transaction.
    pub(crate) fn validate(&self, context: &HistoryReadContext<'_>) -> Result<(), HistoryError> {
        if let Some(error) = &self.invalidated {
            return Err(error.clone());
        }
        self.target.read(context, |_| ())
    }

    /// Revalidate even on a cache hit. The host must refresh before exposing
    /// cached rows or comparison snapshots to a frame or semantic reader.
    pub(crate) fn refresh(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<bool, HistoryError> {
        if let Some(error) = &self.invalidated {
            return Err(error.clone());
        }
        let projected = self.target.read(context, |doc| {
            let heads = committed_heads(doc);
            if self.heads.as_ref() == Some(&heads) {
                return None;
            }
            let rows = self.source.entries(&mut committed_snapshot(doc));
            Some((heads, rows))
        });
        let Some((heads, rows)) = self.checked(projected)? else {
            return Ok(false);
        };
        self.heads = Some(heads);
        self.rows = rows;
        self.comparison = None;
        if self
            .selection
            .iter()
            .any(|id| !self.rows.iter().any(|row| row.id == *id))
        {
            self.selection.clear();
        }
        Ok(true)
    }

    pub(crate) fn rows(&self) -> &[HistoryEntry] {
        &self.rows
    }

    pub(crate) fn selection(&self) -> &[HistoryEntryId] {
        &self.selection
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn comparison(&self) -> Option<&HistoryComparison> {
        self.comparison.as_ref()
    }

    /// Only currently exposed entries in this captured document are selectable.
    /// Empty selection explicitly clears the comparison; duplicate ids do not
    /// widen a comparison or its allocation bound.
    pub(crate) fn select(
        &mut self,
        context: &HistoryReadContext<'_>,
        entries: &[HistoryEntryId],
    ) -> Result<(), HistoryError> {
        self.refresh(context)?;
        self.selection.clear();
        self.comparison = None;
        if entries.len() > self.rows.len()
            || entries
                .iter()
                .any(|id| !self.rows.iter().any(|row| row.id == *id))
        {
            return Err(HistoryError::InvalidSelection);
        }
        for id in entries {
            if !self.selection.contains(id) {
                self.selection.push(*id);
            }
        }
        Ok(())
    }

    pub(crate) fn compare_selection(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<&HistoryComparison, HistoryError> {
        self.refresh(context)?;
        self.comparison = None;
        if self.selection.is_empty() {
            return Err(HistoryError::InvalidSelection);
        }
        let comparison = self
            .target
            .read(context, |doc| {
                compare_entries(&mut committed_snapshot(doc), &self.selection)
                    .map_err(HistoryError::Comparison)
            })
            .and_then(std::convert::identity);
        self.comparison = Some(self.checked(comparison)?);
        self.comparison.as_ref().ok_or(HistoryError::Unavailable)
    }

    /// Whether this session's document accepts a restore from this person.
    pub(crate) fn can_restore(&self, context: &HistoryReadContext<'_>) -> bool {
        self.invalidated.is_none() && self.target.admit_restore(context).is_ok()
    }

    /// Capture the committed state after `entry` together with the live
    /// frontier being reviewed. Nothing is written.
    pub(crate) fn review_restore(
        &mut self,
        context: &HistoryReadContext<'_>,
        entry: HistoryEntryId,
    ) -> Result<RestoreReview, HistoryError> {
        self.refresh(context)?;
        let label = self
            .rows
            .iter()
            .find(|row| row.id == entry)
            .map(|row| row.summary.clone())
            .ok_or(HistoryError::InvalidSelection)?;
        self.target.admit_restore(context)?;
        let (target_heads, reviewed_live_heads) = self.target.read(context, |doc| {
            (
                crate::history_source::heads_for_entry(&mut committed_snapshot(doc), &entry),
                sorted(committed_heads(doc)),
            )
        })?;
        Ok(RestoreReview {
            entry,
            label,
            target_heads: target_heads.ok_or(HistoryError::InvalidSelection)?,
            reviewed_live_heads,
            document_identity: context.document_identity,
        })
    }

    /// The document this session was opened on; a confirmed restore is
    /// written there and nowhere else.
    pub(crate) fn target(&self) -> &HistoryTarget {
        &self.target
    }

    /// A review a caller took earlier, possibly in another process: the same
    /// admission as [`Self::review_restore`], but against the live frontier
    /// the caller says it reviewed. [`Self::validate_restore`] refuses it
    /// unless the document is still at that frontier.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn review_restore_against(
        &mut self,
        context: &HistoryReadContext<'_>,
        entry: HistoryEntryId,
        reviewed_live_heads: Vec<ChangeHash>,
    ) -> Result<RestoreReview, HistoryError> {
        let review = self.review_restore(context, entry)?;
        Ok(RestoreReview {
            reviewed_live_heads: sorted(reviewed_live_heads),
            ..review
        })
    }

    /// Recheck authority and the reviewed frontier at confirmation.
    pub(crate) fn validate_restore(
        &self,
        context: &HistoryReadContext<'_>,
        review: &RestoreReview,
    ) -> Result<(), HistoryError> {
        if let Some(error) = &self.invalidated {
            return Err(error.clone());
        }
        self.target.admit_restore(context)?;
        if context.document_identity != review.document_identity {
            return Err(HistoryError::Stale);
        }
        let (pending, heads) = self.target.read(context, |doc| {
            (doc.pending_ops() > 0, sorted(committed_heads(doc)))
        })?;
        if pending {
            return Err(HistoryError::PendingEdits);
        }
        if heads != review.reviewed_live_heads {
            return Err(HistoryError::StaleReview);
        }
        Ok(())
    }

    fn checked<R>(&mut self, result: Result<R, HistoryError>) -> Result<R, HistoryError> {
        if let Err(error) = &result {
            self.heads = None;
            self.rows.clear();
            self.selection.clear();
            self.comparison = None;
            // Regranting access or reopening a record requires a fresh user
            // session, so an old queued action cannot revive a closed panel.
            if matches!(error, HistoryError::Denied | HistoryError::Stale) {
                self.invalidated = Some(error.clone());
            }
        }
        result
    }
}

fn committed_snapshot(doc: &AutoCommit) -> AutoCommit {
    let mut snapshot = doc.clone();
    if snapshot.pending_ops() > 0 {
        snapshot.rollback();
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use automerge::{ROOT, ReadDoc, transaction::CommitOptions};

    use super::*;
    use crate::permissions::{Actor, CapOverride, Scope, Subject};

    fn document(value: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "body", value).unwrap();
        doc.commit_with(CommitOptions::default().with_time(1));
        doc
    }

    fn read_only(app_id: &str) -> PermissionContext {
        PermissionContext::from_capabilities(
            Actor::agent("reader"),
            vec![CapOverride::grant(
                Subject::agent("reader"),
                Scope::App(app_id.into()),
                Verb::Read,
            )],
        )
    }

    fn target(record: Option<RecordId>, identity: uuid::Uuid) -> HistoryTarget {
        HistoryTarget::Legacy {
            app_id: "notes".into(),
            record,
            document_identity: identity,
        }
    }

    #[test]
    fn standalone_history_browsing_preserves_edits_and_stays_on_its_document() {
        // Case `browsing_and_comparison_preserve_pending_edit_and_attribution`.
        {
            let mut doc = document("saved");
            let record = RecordId::for_file("a.md");
            let identity = uuid::Uuid::new_v4();
            let permissions = read_only("notes");
            let before = doc.base_heads();
            doc.put(ROOT, "body", "still typing").unwrap();
            let pending = doc.pending_ops();
            let context = HistoryReadContext {
                document: &doc,
                active_record: Some(&record),
                document_identity: identity,
                permissions: &permissions,
            };
            let mut session = StandaloneHistory::open(
                target(Some(record.clone()), identity),
                &context,
                AuthorDirectory::new(),
            )
            .unwrap();
            assert_eq!(session.rows().len(), 1);
            assert!(!session.refresh(&context).unwrap());
            session.select(&context, &[session.rows()[0].id]).unwrap();
            let comparison = session.compare_selection(&context).unwrap();
            let after = AutoCommit::load(&comparison.after_document).unwrap();
            assert_eq!(
                after.get(ROOT, "body").unwrap().unwrap().0.as_str(),
                Some("saved")
            );
            assert_eq!(doc.pending_ops(), pending);
            assert_eq!(doc.base_heads(), before);
            doc.put(ROOT, "body", "finished").unwrap();
            let action = tilo_actions::ActionContext::for_resource("Finish edit", "a", "note", "A");
            doc.commit_with(
                CommitOptions::default().with_message(action.change_message().unwrap()),
            );
            let changes = doc.get_changes(&before);
            assert_eq!(changes.len(), 1);
            assert_eq!(
                changes[0]
                    .message()
                    .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
                    .unwrap()
                    .action_label,
                "Finish edit",
            );
        }

        // Case `record_switch_clears_old_rows_and_does_not_fall_back`.
        {
            let first = document("first");
            let second = document("second");
            let a = RecordId::for_file("a.md");
            let b = RecordId::for_file("b.md");
            let identity = uuid::Uuid::new_v4();
            let permissions = PermissionContext::permissive();
            let context = HistoryReadContext {
                document: &first,
                active_record: Some(&a),
                document_identity: identity,
                permissions: &permissions,
            };
            let mut session = StandaloneHistory::open(
                target(Some(a.clone()), identity),
                &context,
                AuthorDirectory::new(),
            )
            .unwrap();
            session.select(&context, &[session.rows()[0].id]).unwrap();
            session.compare_selection(&context).unwrap();
            let switched = HistoryReadContext {
                document: &second,
                active_record: Some(&b),
                ..context
            };
            assert_eq!(session.refresh(&switched), Err(HistoryError::Stale));
            assert!(session.rows().is_empty());
            assert!(session.selection().is_empty());
            assert!(session.comparison().is_none());
            assert_eq!(session.refresh(&context), Err(HistoryError::Stale));
        }

        // Case `unknown_selection_cannot_materialize_another_documents_change`.
        {
            let doc = document("first");
            let mut foreign = document("foreign");
            let foreign_id = HistorySource::new().entries(&mut foreign)[0].id;
            let identity = uuid::Uuid::new_v4();
            let permissions = PermissionContext::permissive();
            let context = HistoryReadContext {
                document: &doc,
                active_record: None,
                document_identity: identity,
                permissions: &permissions,
            };
            let mut session =
                StandaloneHistory::open(target(None, identity), &context, AuthorDirectory::new())
                    .unwrap();
            assert_eq!(
                session.select(&context, &[foreign_id]),
                Err(HistoryError::InvalidSelection)
            );
            assert_eq!(
                session.compare_selection(&context).unwrap_err(),
                HistoryError::InvalidSelection
            );
            assert!(session.comparison().is_none());
        }
    }

    #[test]
    fn standalone_history_sessions_invalidate_on_replacement_and_revocation() {
        // Case `replacing_same_record_or_app_document_invalidates_old_session`.
        {
            let first = document("first");
            let replacement = document("replacement");
            let permissions = PermissionContext::permissive();
            for record in [None, Some(RecordId::for_tab(3))] {
                let identity = uuid::Uuid::new_v4();
                let context = HistoryReadContext {
                    document: &first,
                    active_record: record.as_ref(),
                    document_identity: identity,
                    permissions: &permissions,
                };
                let mut session = StandaloneHistory::open(
                    target(record.clone(), identity),
                    &context,
                    AuthorDirectory::new(),
                )
                .unwrap();
                let replaced = HistoryReadContext {
                    document: &replacement,
                    document_identity: uuid::Uuid::new_v4(),
                    ..context
                };
                assert_eq!(session.refresh(&replaced), Err(HistoryError::Stale));
                assert!(session.rows().is_empty());
            }
        }

        // Case `revocation_is_checked_on_cached_refresh_select_and_compare`.
        {
            let doc = document("private");
            let identity = uuid::Uuid::new_v4();
            let permissions = read_only("notes");
            let denied = read_only("other-app");
            let context = HistoryReadContext {
                document: &doc,
                active_record: None,
                document_identity: identity,
                permissions: &permissions,
            };
            let revoked = HistoryReadContext {
                permissions: &denied,
                ..context
            };
            for operation in 0..3 {
                let mut session = StandaloneHistory::open(
                    target(None, identity),
                    &context,
                    AuthorDirectory::new(),
                )
                .unwrap();
                let id = session.rows()[0].id;
                session.select(&context, &[id]).unwrap();
                session.compare_selection(&context).unwrap();
                let result = match operation {
                    0 => session.refresh(&revoked).map(|_| ()),
                    1 => session.select(&revoked, &[id]),
                    _ => session.compare_selection(&revoked).map(|_| ()),
                };
                assert_eq!(result, Err(HistoryError::Denied));
                assert!(session.rows().is_empty());
                assert!(session.selection().is_empty());
                assert!(session.comparison().is_none());
                assert_eq!(session.refresh(&context), Err(HistoryError::Denied));
            }
        }
    }
}
