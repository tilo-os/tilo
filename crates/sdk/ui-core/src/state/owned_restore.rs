//! Exclusive content restore ownership; host admission is supplied separately.
use super::{InstanceId, State, UndoLog, UserId};
use crate::history_mode::restore::{
    PreparedContentRestore, RestoreError, RestoreOutcome, apply_prepared_content_restore,
};
use crate::permissions::PermissionContext;
use automerge::{AutoCommit, ChangeHash, transaction::Transactable};

/// Private, synchronous admission staging. Only the History hosts' doors in
/// this module construct one, after binding current authority and the exact
/// owner/lifetime. A prepared content plan alone is never an authority grant.
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

/// A restore the person reviewed in History: the source and scope it names,
/// and the controller whose review token it carries.
pub(crate) struct ReviewedRestore<'a> {
    pub(crate) source: &'a crate::history_mode::HistorySourceRegistration,
    pub(crate) scope: &'a crate::history_mode::HistoryScopeDefinition,
    pub(crate) controller: &'a crate::history_mode::HistoryController,
    pub(crate) review: &'a crate::history_mode::RestoreReview,
}

/// The framework History host's admission door. The current owner/context is
/// borrowed through preparation, final authorization and the synchronous
/// canonical write.
pub(crate) fn restore_reviewed_history(
    state: &mut State,
    restore: &ReviewedRestore<'_>,
    (authority, author): (&PermissionContext, &UserId),
    now: u64,
    extent: RestoreExtent<'_>,
) -> Result<OwnedActionReceipt, String> {
    use crate::history_mode::HistorySourceRegistration;
    let ReviewedRestore {
        source,
        scope,
        controller,
        review,
    } = *restore;
    let schema = scope.content_schema;
    let title = scope.title;
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
    let mut document = source
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
    // Restoring just the selected changes takes back what they did, each on
    // its own, and leaves everything written since: no schema is consulted,
    // since nothing but those changes' own effects is touched.
    let changes = match extent {
        RestoreExtent::Whole | RestoreExtent::Name(_) => None,
        RestoreExtent::Changes(entries) => {
            let changes = crate::history_source::revert::changes_of_entries(&mut document, entries);
            if changes.is_empty() {
                return Err("This change is no longer in the document.".into());
            }
            Some(changes)
        }
    };
    // With a registered content schema only the declared content returns;
    // without one the whole document returns to the selected version, the
    // way `git revert` of every later change would.
    let prepared = schema
        .filter(|_| changes.is_none() && !matches!(extent, RestoreExtent::Name(_)))
        .map(|schema| {
            crate::history_mode::restore::prepare_content_restore(
                &document,
                schema,
                review.reviewed_live_heads(),
                review.selected_heads(),
            )
            .map_err(|_| crate::i18n::I18nManager::lookup_plain("history-restore-unsupported"))
        })
        .transpose()?;
    let write = if let RestoreExtent::Name(request) = extent {
        RestoreWrite::Name {
            current: review.reviewed_live_heads(),
            request,
        }
    } else {
        match (&changes, prepared.as_ref()) {
            (Some(changes), _) => RestoreWrite::Changes {
                current: review.reviewed_live_heads(),
                changes,
            },
            (None, Some(prepared)) => RestoreWrite::Content(prepared),
            (None, None) => RestoreWrite::WholeDocument {
                current: review.reviewed_live_heads(),
                target: review.selected_heads(),
            },
        }
    };
    let context = tilo_actions::ActionContext::for_resource(
        review.resource().record_id.as_str(),
        "document",
        title,
    )
    .with_app(&review.resource().app_id, title);
    let apply = |authority: &PermissionContext, author: &UserId, file: bool, state: &mut State| {
        let current = authority.clone().at_unix_time(now);
        if let Err(refusal) = check_restore_authority(
            &current,
            author,
            &review.resource().app_id,
            file.then_some(&review.resource().record_id),
        ) {
            return Err(match refusal {
                RestoreRefusal::App => {
                    "You no longer have permission to restore this document.".to_owned()
                }
                RestoreRefusal::File => "You do not have permission to write this file.".to_owned(),
            });
        }
        let admitted = ConfirmedRestore {
            _authority: authority,
            author: author.clone(),
            context,
        };
        let receipt = match source {
            HistorySourceRegistration::HostDocument { .. } => {
                state.restore_write_owned(&admitted, write)
            }
            HistorySourceRegistration::OwnedDocument { owner, .. } => {
                owner.restore_write(&mut state.doc, &admitted, write)
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

enum RestoreRefusal {
    App,
    File,
}

/// Restoring writes: the author must be the actor `current` describes, and
/// `current` must grant Write on the app and, for a file-backed record, on the
/// file. `current` is already stamped at the caller's policy time.
fn check_restore_authority(
    current: &PermissionContext,
    author: &UserId,
    app_id: &str,
    record: Option<&crate::record::RecordId>,
) -> Result<(), RestoreRefusal> {
    use crate::permissions::{Access, ActionKind, Principal};
    // The installed solo-workspace principal is the runner's existing
    // local-author mapping. Other contexts must name the actual author.
    let mapped = current.actor().principals.contains(&author.principal())
        || current
            .actor()
            .principals
            .contains(&Principal::Person(crate::permissions::LOCAL_USER.into()));
    if !mapped
        || !current.check(&crate::permissions::PermissionScope::app(
            app_id.to_owned(),
            ActionKind::Undoable,
        ))
    {
        return Err(RestoreRefusal::App);
    }
    if let Some(path) = record.and_then(crate::record::RecordId::file_path)
        && !current.check(&crate::permissions::PermissionScope::path(
            path.to_string_lossy().into_owned(),
            Access::Write,
        ))
    {
        return Err(RestoreRefusal::File);
    }
    Ok(())
}

/// The standalone History host's door for a bound record: return the record
/// to `target` as one forward change written through the record's own action
/// frame and undo log, attributed to the binding's author. Refused unless the
/// record is still exactly at `current`, the frontier the person reviewed.
pub(crate) fn restore_reviewed_record(
    outer: &mut AutoCommit,
    binding: &crate::bound_dispatch::CallbackBinding,
    current: &[ChangeHash],
    target: &[ChangeHash],
) -> Result<OwnedActionReceipt, OwnedRestoreError> {
    let authority = binding.permissions.borrow();
    check_restore_authority(
        &authority,
        &binding.author,
        &binding.app_id,
        Some(&binding.checkout.id),
    )
    .map_err(|_| OwnedRestoreError::Denied)?;
    let mut context = tilo_actions::ActionContext {
        what: Some(tilo_actions::Did::history(
            tilo_actions::HistoryOp::Restore,
            0,
            None,
        )),
        ..tilo_actions::ActionContext::default()
    }
    .enriched_by(&binding.operation_context);
    if context.app.is_none() {
        context = context.with_app(&binding.app_id, &binding.app_id);
    }
    let admitted = ConfirmedRestore {
        _authority: &authority,
        author: binding.author.clone(),
        context,
    };
    binding.restore_write(
        outer,
        &admitted,
        RestoreWrite::WholeDocument { current, target },
    )
}
/// The History action door's naming write on the app's own document: name
/// a run of changes, or remove its name, as one forward change through the
/// document's undo log, attributed to `author`. Undo and the history list
/// both pass over it: a name is no step of its own. The door has already
/// checked that its caller may write.
pub(crate) fn name_history_run(
    state: &mut State,
    authority: &PermissionContext,
    author: UserId,
    context: tilo_actions::ActionContext,
    request: &crate::GroupNameRequest,
) -> Result<OwnedActionReceipt, OwnedRestoreError> {
    let current = state.doc.base_heads();
    let admitted = ConfirmedRestore {
        _authority: authority,
        author,
        context,
    };
    state.restore_write_owned(
        &admitted,
        RestoreWrite::Name {
            current: &current,
            request,
        },
    )
}
#[derive(Debug)]
pub(crate) struct OwnedActionReceipt {
    pub(crate) after: Vec<ChangeHash>,
    pub(crate) changed: bool,
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
    /// The record's current authority does not grant this author Write.
    Denied,
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

/// What a confirmed restore writes: the schema-declared content of a version,
/// or, for a resource without a content schema, the whole document returned to
/// the version's frontier as one forward change.
#[derive(Clone, Copy)]
pub(crate) enum RestoreWrite<'a> {
    Content(&'a PreparedContentRestore),
    WholeDocument {
        current: &'a [ChangeHash],
        target: &'a [ChangeHash],
    },
    /// Take back exactly these changes (each with the actor whose work it
    /// is, newest first), each on its own, as one forward change: whatever
    /// anyone wrote since stays.
    Changes {
        current: &'a [ChangeHash],
        changes: &'a [(ChangeHash, String)],
    },
    /// Name a run of changes, or remove its name: no content moves.
    Name {
        current: &'a [ChangeHash],
        request: &'a crate::GroupNameRequest,
    },
}
impl RestoreWrite<'_> {
    pub(crate) fn current_heads(&self) -> &[ChangeHash] {
        match self {
            Self::Content(prepared) => prepared.current_heads(),
            Self::WholeDocument { current, .. }
            | Self::Changes { current, .. }
            | Self::Name { current, .. } => current,
        }
    }
}

/// How much of a selected version a reviewed restore brings back.
#[derive(Clone, Copy)]
pub(crate) enum RestoreExtent<'a> {
    /// The whole version.
    Whole,
    /// Only what these selected changes did, taken back on their own.
    Changes(&'a [crate::widget::HistoryEntryId]),
    /// Nothing comes back: the selected run of changes is named.
    Name(&'a crate::GroupNameRequest),
}

/// Exclusive loan of the original document and its own undo log. Its only
/// writers are the prepared content engine and the whole-document revert; no
/// committing callback is accepted.
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
    pub(crate) fn apply(
        mut self,
        write: RestoreWrite<'_>,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError> {
        let outcome = match write {
            RestoreWrite::Content(prepared) => apply_prepared_content_restore(self.doc, prepared)
                .map_err(OwnedRestoreError::Engine)?,
            RestoreWrite::WholeDocument { current, target } => {
                super::write_revert_patches(self.doc, current, target);
                if self.doc.pending_ops() > 0 {
                    RestoreOutcome::Changed
                } else {
                    RestoreOutcome::Unchanged
                }
            }
            RestoreWrite::Name { request, .. } => {
                if crate::history_source::groups::apply_name_request(self.doc, request) {
                    RestoreOutcome::Changed
                } else {
                    RestoreOutcome::Unchanged
                }
            }
            RestoreWrite::Changes { changes, .. } => {
                crate::history_source::revert::revert_changes(
                    self.doc,
                    changes,
                    &crate::history_source::AuthorDirectory::default(),
                )
                .map_err(|_| OwnedRestoreError::EngineContract)?;
                if self.doc.pending_ops() > 0 {
                    RestoreOutcome::Changed
                } else {
                    RestoreOutcome::Unchanged
                }
            }
        };
        match (outcome, self.doc.pending_ops() > 0) {
            (RestoreOutcome::Unchanged, false) => Ok(OwnedActionReceipt {
                after: self.before.clone(),
                changed: false,
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
        let message = self.stamp(attribution.message, attribution.author.as_ref());
        let committed = tilo_resource::history::commit_stamped_with_message(doc, message).is_some();
        let after = doc.get_heads();
        if committed {
            self.record_committed_span(
                doc,
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
            after,
            changed: committed,
        }
    }
}
impl State {
    #[cfg(test)]
    pub(crate) fn restore_prepared_owned(
        &mut self,
        admitted: &ConfirmedRestore<'_>,
        prepared: &PreparedContentRestore,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError> {
        self.restore_write_owned(admitted, RestoreWrite::Content(prepared))
    }
    pub(crate) fn restore_write_owned(
        &mut self,
        admitted: &ConfirmedRestore<'_>,
        write: RestoreWrite<'_>,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError> {
        OwnedContentAction::begin_clean(
            &mut self.doc,
            &mut self.undo,
            write.current_heads(),
            admitted,
            None,
        )?
        .apply(write)
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
        let before = self.doc.get_heads();
        let changed = crate::history_mode::routes::merge_committed(&mut self.doc, mutation)?;
        if changed {
            self.undo.rebase(&mut self.doc);
            // The owner that served this history holds it: the next primary
            // delta must not echo it back as this replica's change.
            self.outbound.observe_changes_after(&mut self.doc, &before);
        }
        Ok(Some(changed))
    }
}

impl State {
    pub(crate) fn undo_status_snapshot(&self, author: Option<&UserId>) -> crate::undo::UndoStatus {
        crate::undo::status_snapshot(&self.doc, &self.undo, author)
    }
}
