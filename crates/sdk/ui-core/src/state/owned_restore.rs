//! Exclusive content restore ownership; host admission is supplied separately.
use super::{InstanceId, State, UndoLog, UserId};
use crate::history_mode::restore::{
    PreparedContentRestore, RestoreError, RestoreOutcome, apply_prepared_content_restore,
};
use crate::permissions::PermissionContext;
use automerge::{AutoCommit, ChangeHash, transaction::Transactable};

/// Private, synchronous admission staging. No production constructor exists:
/// the History host must bind current authority and exact owner/lifetime first.
/// A prepared content plan alone is never an authority grant.
pub(crate) struct ConfirmedRestore<'a> {
    _authority: &'a PermissionContext,
    author: UserId,
    context: tilo_actions::ActionContext,
}
impl ConfirmedRestore<'_> {
    #[cfg(test)]
    pub(crate) fn for_test(
        authority: &PermissionContext,
        author: UserId,
        context: tilo_actions::ActionContext,
    ) -> ConfirmedRestore<'_> {
        ConfirmedRestore {
            _authority: authority,
            author,
            context,
        }
    }
}

/// The only production admission door. The current owner/context is borrowed
/// through preparation, final authorization and the synchronous canonical write.
#[allow(
    clippy::too_many_arguments,
    reason = "restore admission binds the reviewed source and schema to current state, authority, author, title and time"
)]
pub(crate) fn restore_reviewed_history(
    state: &mut State,
    source: &crate::history_mode::HistorySourceRegistration,
    controller: &crate::history_mode::HistoryController,
    review: &crate::history_mode::RestoreReview,
    schema: &'static crate::history_mode::restore::HistoryRestoreSchema,
    authority: &PermissionContext,
    author: &UserId,
    title: &str,
    now: u64,
) -> Result<OwnedActionReceipt, String> {
    use crate::history_mode::HistorySourceRegistration;
    let resource = match source {
        HistorySourceRegistration::HostDocument { resource, .. }
        | HistorySourceRegistration::OwnedDocument { resource, .. } => resource,
        HistorySourceRegistration::Unversioned { .. }
        | HistorySourceRegistration::CheckedFiles { .. } => {
            return Err("This resource has no restorable versions.".into());
        }
    };
    if resource != review.resource() {
        return Err("This review belongs to a different document.".into());
    }
    let document = source
        .document(&state.doc)
        .map_err(|_| "This document is no longer available.")?;
    controller
        .validate_restore(review, &document)
        .map_err(|error| match error {
            crate::history_mode::HistoryError::StaleLiveHeads => {
                "Live content changed. Refresh review before restoring.".to_owned()
            }
            crate::history_mode::HistoryError::UncommittedLiveChanges => {
                "Finish the current edit before restoring.".to_owned()
            }
            _ => "This review no longer matches the selected document version.".to_owned(),
        })?;
    let prepared = crate::history_mode::restore::prepare_content_restore(
        &document,
        schema,
        review.reviewed_live_heads(),
        review.selected_heads(),
    )
    .map_err(
        |_| "This version cannot be restored safely with this application's content format.",
    )?;
    let context = tilo_actions::ActionContext::for_resource(
        "Restore version",
        review.resource().record_id.as_str(),
        "document",
        title,
    )
    .with_app(&review.resource().app_id, title);
    let apply = |authority: &PermissionContext, author: &UserId, file: bool, state: &mut State| {
        use crate::permissions::{CapDecision, Principal, ResourceRef, Verb};
        let current = authority.clone().at_unix_time(now);
        // The installed solo-workspace principal is the runner's existing
        // local-author mapping. Other contexts must name the actual author.
        let mapped = current.actor().principals.contains(&author.principal())
            || current
                .actor()
                .principals
                .contains(&Principal::Person(crate::permissions::LOCAL_USER.into()));
        if !mapped
            || current.decide(
                &ResourceRef::App(review.resource().app_id.clone()),
                Verb::Write,
            ) != CapDecision::Allow
        {
            return Err("You no longer have permission to restore this document.".to_owned());
        }
        if file
            && let Some(path) = review.resource().record_id.file_path()
            && current.decide(
                &ResourceRef::FsPath(path.to_string_lossy().into_owned()),
                Verb::Write,
            ) != CapDecision::Allow
        {
            return Err("You do not have permission to write this file.".to_owned());
        }
        let admitted = ConfirmedRestore {
            _authority: authority,
            author: author.clone(),
            context,
        };
        let receipt = match source {
            HistorySourceRegistration::HostDocument { .. } => {
                state.restore_prepared_owned(&admitted, &prepared)
            }
            HistorySourceRegistration::OwnedDocument { owner, .. } => {
                owner.restore_prepared(&mut state.doc, &admitted, &prepared)
            }
            HistorySourceRegistration::Unversioned { .. }
            | HistorySourceRegistration::CheckedFiles { .. } => {
                return Err("This resource has no restorable versions.".to_owned());
            }
        };
        receipt.map_err(|error| match error {
            OwnedRestoreError::PendingEdits
            | OwnedRestoreError::UnfinishedActionBoundary
            | OwnedRestoreError::ActiveAction => {
                "Finish the current edit before restoring.".to_owned()
            }
            OwnedRestoreError::StaleReview => {
                "Live content changed. Refresh review before restoring.".to_owned()
            }
            OwnedRestoreError::Engine(error) => {
                let _ = error;
                "The document could not be restored safely.".to_owned()
            }
            OwnedRestoreError::Bound(error) => {
                let _ = error;
                "The document could not be restored safely.".to_owned()
            }
            _ => "The document could not be restored safely.".to_owned(),
        })
    };
    match source {
        HistorySourceRegistration::OwnedDocument { owner, .. } => {
            owner.with_restore_authority(authority, author, |authority, author| {
                apply(authority, author, true, state)
            })
        }
        _ => apply(authority, author, false, state),
    }
}
#[derive(Debug)]
pub(crate) struct OwnedActionReceipt {
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained in receipts for restore audit tests")
    )]
    pub(crate) before: Vec<ChangeHash>,
    pub(crate) after: Vec<ChangeHash>,
    pub(crate) changed: bool,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained in receipts for restore audit tests")
    )]
    pub(crate) history_revision: u64,
}
#[derive(Debug)]
pub(crate) enum OwnedRestoreError {
    PendingEdits,
    StaleReview,
    UnfinishedActionBoundary,
    HistoryExhausted,
    GenerationExhausted,
    ActiveAction,
    Engine(RestoreError),
    EngineContract,
    Bound(crate::bound_dispatch::BoundDispatchError),
}
/// Existing native commits and new owned restores use this same attribution.
pub(super) struct OwnedActionAttribution {
    pub(super) author: Option<UserId>,
    pub(super) instance_id: Option<InstanceId>,
    pub(super) focused_node_idx: Option<usize>,
    pub(super) widget_metadata: Option<String>,
    pub(super) message: Option<String>,
}

pub(crate) fn same_frontier(left: &[ChangeHash], right: &[ChangeHash]) -> bool {
    let normalize = |heads: &[ChangeHash]| {
        let mut heads = heads.to_vec();
        heads.sort_unstable();
        heads.dedup();
        heads
    };
    normalize(left) == normalize(right)
}

/// Exclusive loan of the original document and its own undo log. Its only
/// writer is the prepared content engine; no committing callback is accepted.
pub(crate) struct OwnedContentAction<'a> {
    doc: &'a mut AutoCommit,
    history: &'a mut UndoLog,
    before: Vec<ChangeHash>,
    attribution: Option<OwnedActionAttribution>,
    finished: bool,
}
impl<'a> OwnedContentAction<'a> {
    pub(crate) fn begin_clean(
        doc: &'a mut AutoCommit,
        history: &'a mut UndoLog,
        expected_heads: &[ChangeHash],
        admitted: &ConfirmedRestore<'_>,
        instance_id: Option<InstanceId>,
    ) -> Result<Self, OwnedRestoreError> {
        // Do not use get_heads, save, rebase or any auto-committing observation.
        if doc.pending_ops() != 0 {
            return Err(OwnedRestoreError::PendingEdits);
        }
        if !same_frontier(&doc.base_heads(), expected_heads) {
            return Err(OwnedRestoreError::StaleReview);
        }
        if !same_frontier(&history.last_heads, expected_heads) {
            return Err(OwnedRestoreError::UnfinishedActionBoundary);
        }
        if !history.can_bump() {
            return Err(OwnedRestoreError::HistoryExhausted);
        }
        Ok(Self {
            doc,
            history,
            before: expected_heads.to_vec(),
            attribution: Some(OwnedActionAttribution {
                author: Some(admitted.author.clone()),
                instance_id,
                focused_node_idx: None,
                widget_metadata: None,
                message: admitted.context.change_message(),
            }),
            finished: false,
        })
    }
    pub(crate) fn apply_restore(
        mut self,
        prepared: &PreparedContentRestore,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError> {
        let outcome = apply_prepared_content_restore(self.doc, prepared)
            .map_err(OwnedRestoreError::Engine)?;
        match (outcome, self.doc.pending_ops() > 0) {
            (RestoreOutcome::Unchanged, false) => Ok(OwnedActionReceipt {
                before: self.before.clone(),
                after: self.before.clone(),
                changed: false,
                history_revision: self.history.revision(),
            }),
            (RestoreOutcome::Changed, true) => {
                let Some(attribution) = self.attribution.take() else {
                    return Err(OwnedRestoreError::EngineContract);
                };
                let receipt =
                    self.history
                        .commit_owned_pending(self.doc, self.before.clone(), attribution);
                self.finished = true;
                Ok(receipt)
            }
            _ => Err(OwnedRestoreError::EngineContract),
        }
    }
}
impl Drop for OwnedContentAction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // Entry refused others' pending work. Automerge rollback discards
            // this transaction's patch log too; retain the existing diff cursor.
            self.doc.rollback();
        }
    }
}
impl UndoLog {
    /// The canonical physical stamp + explicit undo span finalizer. Capacity is
    /// reserved by the caller before any writes; no user callbacks run here.
    pub(super) fn commit_owned_pending(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        attribution: OwnedActionAttribution,
    ) -> OwnedActionReceipt {
        let committed =
            qdoc::history::commit_stamped_with_message(doc, attribution.message).is_some();
        let after = doc.get_heads();
        if committed {
            self.record_committed_span_owned(
                before.clone(),
                after.clone(),
                attribution.focused_node_idx,
                attribution.widget_metadata,
                attribution.author,
                attribution.instance_id,
            );
        } else {
            // Native no-op commits historically refresh their baseline. Restore
            // no-ops never enter this finalizer and preserve all bookkeeping.
            self.last_heads.clone_from(&after);
        }
        OwnedActionReceipt {
            before,
            after,
            changed: committed,
            history_revision: self.revision(),
        }
    }
}
impl State {
    pub(crate) fn restore_prepared_owned(
        &mut self,
        admitted: &ConfirmedRestore<'_>,
        prepared: &PreparedContentRestore,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError> {
        OwnedContentAction::begin_clean(
            &mut self.doc,
            &mut self.undo,
            prepared.current_heads(),
            admitted,
            None,
        )?
        .apply_restore(prepared)
    }
}

impl State {
    pub(crate) fn merge_remote_committed(
        &mut self,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<Option<bool>, crate::history_mode::routes::RemoteApplyError> {
        if self.doc.pending_ops() != 0
            || !same_frontier(&self.undo.last_heads, &self.doc.base_heads())
        {
            return Ok(None);
        }
        let changed = crate::history_mode::routes::merge_committed(&mut self.doc, mutation)?;
        if changed {
            self.undo.rebase(&mut self.doc);
        }
        Ok(Some(changed))
    }
}

impl State {
    pub(crate) fn undo_status_snapshot(&self, author: Option<&UserId>) -> crate::undo::UndoStatus {
        crate::undo::status_snapshot(&self.doc, &self.undo, author)
    }
}
