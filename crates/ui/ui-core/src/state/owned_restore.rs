//! Exclusive content restore ownership; host admission is supplied separately.
use super::{State, UndoLog};
use crate::history_mode::restore::{
    PreparedContentRestore, RestoreError, RestoreOutcome, apply_prepared_content_restore,
};
use crate::permissions::MemberId;
use crate::permissions::PermissionContext;
use automerge::{AutoCommit, ChangeHash, transaction::Transactable};
use collaboration::history::InstanceId;
pub(crate) use collaboration::history::{
    OwnedActionAttribution, OwnedActionReceipt, same_frontier,
};

/// Private, synchronous admission staging. A prepared content plan alone is never an
/// authority grant.
pub(crate) struct ConfirmedRestore<'a> {
    _authority: &'a PermissionContext,
    author: MemberId,
    context: tilo_actions::ActionContext,
}
impl ConfirmedRestore<'_> {
    #[cfg(test)]
    pub(crate) fn for_test(
        authority: &PermissionContext,
        author: MemberId,
        context: tilo_actions::ActionContext,
    ) -> ConfirmedRestore<'_> {
        ConfirmedRestore {
            _authority: authority,
            author,
            context,
        }
    }
}

/// A restore the person reviewed in History: the source and scope it names, and the
/// controller whose review token it carries.
pub(crate) struct ReviewedRestore<'a> {
    pub(crate) source: &'a crate::history_mode::HistorySourceRegistration,
    pub(crate) scope: &'a crate::history_mode::HistoryScopeDefinition,
    pub(crate) controller: &'a crate::history_mode::HistoryController,
    pub(crate) review: &'a crate::history_mode::RestoreReview,
}

/// The framework History host's admission API.
pub(crate) fn restore_reviewed_history(
    state: &mut State,
    restore: &ReviewedRestore<'_>,
    (authority, author): (&PermissionContext, &MemberId),
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
        | HistorySourceRegistration::FolderFiles { .. } => {
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
    // With a registered content schema only the declared content returns; without one the
    // whole document returns to the selected version, the way `git revert` of every later
    // change would.
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
    // What the restore did, so History says it: an earlier version returned, or the chosen
    // changes taken back. Naming a range changes nothing anyone reads.
    let did = match (&extent, &changes) {
        (RestoreExtent::Name(_), _) => None,
        (_, Some(changes)) => Some(tilo_actions::Did::history(
            tilo_actions::HistoryOp::Undo,
            u32::try_from(changes.len()).unwrap_or(u32::MAX),
            None,
        )),
        (_, None) => Some(tilo_actions::Did::history(
            tilo_actions::HistoryOp::Restore,
            1,
            None,
        )),
    };
    let mut context = tilo_actions::ActionContext::for_resource(
        review.resource().document_id.as_str(),
        "document",
        title,
    )
    .with_app(&review.resource().app_id, title);
    context.what = did;
    let apply = |authority: &PermissionContext,
                 author: &MemberId,
                 file: bool,
                 state: &mut State| {
        let current = authority.clone().at_unix_time(now);
        if let Err(refusal) = check_restore_authority(
            &current,
            author,
            &review.resource().app_id,
            file.then_some(&review.resource().document_id),
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
            | HistorySourceRegistration::FolderFiles { .. } => {
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

/// Restoring writes: the author must be the actor `current` describes, and `current` must
/// grant Write on the app and, for a file-backed record, on the file. `current` is
/// already stamped at the caller's policy time.
fn check_restore_authority(
    current: &PermissionContext,
    author: &MemberId,
    app_id: &str,
    record: Option<&crate::resource::DocumentId>,
) -> Result<(), RestoreRefusal> {
    use crate::permissions::{Access, ActionKind, MemberId};
    // The installed solo-workspace person is the runner's existing local-author mapping.
    // Other contexts must name the actual author.
    let acting = current.acting();
    let mapped = acting
        .nodes()
        .into_iter()
        .filter_map(|node| node.by.member_id())
        .any(|member| match member {
            MemberId::Person(id) => member == author || id == crate::permissions::LOCAL_USER,
            MemberId::Agent(_) | MemberId::Bot(_) => false,
        });
    if !mapped
        || !current.check(&crate::permissions::PermissionScope::app(
            app_id.to_owned(),
            ActionKind::Undoable,
        ))
    {
        return Err(RestoreRefusal::App);
    }
    if let Some(path) = record.and_then(crate::resource::DocumentId::file_path)
        && !current.check(&crate::permissions::PermissionScope::path(
            path.to_string_lossy().into_owned(),
            Access::Write,
        ))
    {
        return Err(RestoreRefusal::File);
    }
    Ok(())
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
/// What a confirmed restore writes: the schema-declared content of a version, or, for a
/// resource without a content schema, the whole document returned to the version's
/// frontier as one forward change.
#[derive(Clone, Copy)]
pub(crate) enum RestoreWrite<'a> {
    Content(&'a PreparedContentRestore),
    WholeDocument {
        current: &'a [ChangeHash],
        target: &'a [ChangeHash],
    },
    /// Take back exactly these changes (each with the actor whose work it is, newest
    /// first), each on its own, as one forward change: whatever anyone wrote since stays.
    Changes {
        current: &'a [ChangeHash],
        changes: &'a [(ChangeHash, String)],
    },
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
    Whole,
    Changes(&'a [crate::widget::HistoryEntryId]),
    Name(&'a crate::GroupNameRequest),
}

/// Exclusive loan of the original document and its own undo log.
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
        if doc.pending_ops() != 0 {
            return Err(OwnedRestoreError::PendingEdits);
        }
        if !same_frontier(&doc.base_heads(), expected_heads) {
            return Err(OwnedRestoreError::StaleReview);
        }
        if !same_frontier(history.last_heads(), expected_heads) {
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
            self.doc.rollback();
        }
    }
}
/// What the toolkit does with an app's [`State`] beyond keeping it: restore reviewed history into
/// it, merge committed history it was served, and report its undo status in words.
pub(crate) trait StateHistory {
    /// Restore `prepared` as an owned action, for the tests that drive one directly.
    #[cfg(test)]
    fn restore_prepared_owned(
        &mut self,
        admitted: &ConfirmedRestore<'_>,
        prepared: &PreparedContentRestore,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError>;
    /// Write `write` as one owned action the history can take back.
    fn restore_write_owned(
        &mut self,
        admitted: &ConfirmedRestore<'_>,
        write: RestoreWrite<'_>,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError>;
    /// Whether the window's document cannot take a peer's document yet: an unfinished edit, or
    /// undo history over a different frontier.
    fn defers_remote(&self) -> bool;
    /// Merge committed history when nothing local is open over it: `None` when the live
    /// document has uncommitted or unrecorded edits, else whether the merge changed it.
    fn merge_remote_committed(
        &mut self,
        incoming: crate::collaboration::Incoming<'_>,
    ) -> Result<Option<bool>, crate::history_mode::routes::RemoteApplyError>;
    /// What the history can do for `author` right now.
    fn undo_status_snapshot(&self, author: Option<&MemberId>) -> crate::undo::UndoStatus;
}

impl StateHistory for State {
    #[cfg(test)]
    fn restore_prepared_owned(
        &mut self,
        admitted: &ConfirmedRestore<'_>,
        prepared: &PreparedContentRestore,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError> {
        self.restore_write_owned(admitted, RestoreWrite::Content(prepared))
    }

    fn restore_write_owned(
        &mut self,
        admitted: &ConfirmedRestore<'_>,
        write: RestoreWrite<'_>,
    ) -> Result<OwnedActionReceipt, OwnedRestoreError> {
        let (doc, undo, _) = self.parts_mut();
        OwnedContentAction::begin_clean(doc, undo, write.current_heads(), admitted, None)?
            .apply(write)
    }

    fn defers_remote(&self) -> bool {
        self.doc.pending_ops() != 0
            || !same_frontier(self.undo_log().last_heads(), &self.doc.base_heads())
    }

    fn merge_remote_committed(
        &mut self,
        incoming: crate::collaboration::Incoming<'_>,
    ) -> Result<Option<bool>, crate::history_mode::routes::RemoteApplyError> {
        if self.defers_remote() {
            return Ok(None);
        }
        let (doc, undo, outbound) = self.parts_mut();
        let before = doc.get_heads();
        let changed = crate::history_mode::routes::merge_committed(doc, incoming)?;
        if changed {
            undo.rebase(doc);
            // The owner that served this history holds it: the next primary delta must
            // not echo it back as this replica's change.
            outbound.observe_changes_after(doc, &before);
        }
        Ok(Some(changed))
    }

    fn undo_status_snapshot(&self, author: Option<&MemberId>) -> crate::undo::UndoStatus {
        crate::undo::status_snapshot(&self.doc, self.undo_log(), author)
    }
}
