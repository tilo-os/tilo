//! Review presentation and synchronous delivery to the sole restore admission API.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::State;
use tilo_ui::widget::ModalDialog;
use tilo_ui_base::{i18n::I18nManager, permissions::MemberId, permissions::Verdicts};

#[derive(Clone, Copy)]
pub(super) enum ReviewCommand {
    Open,
    /// Take back just the selected change, leaving everything since: a forward change of its own,
    /// undoable, so it asks no confirmation.
    RevertChange,
    /// Name the selected run of changes with the panel's name field, or remove its name when the field
    /// is empty.
    Name,
    Confirm,
    Cancel,
    Refresh,
}
pub(super) struct ReviewPanel {
    pub(super) dialog: ModalDialog<Text>,
    token: Option<crate::history_mode::RestoreReview>,
}
impl HistoryHost {
    pub(super) fn show_review(
        &mut self,
        token: Option<crate::history_mode::RestoreReview>,
        message: impl AsRef<str>,
    ) {
        let Some(scope) = self.registration.definition.scopes.get(self.active_scope) else {
            self.invalidate_owner();
            return;
        };
        let title = scope.title;
        let confirm = self.commands.clone();
        let cancel = self.commands.clone();
        let dismiss = self.commands.clone();
        let ready = token.is_some();
        let command = if ready {
            ReviewCommand::Confirm
        } else {
            ReviewCommand::Refresh
        };
        self.review = Some(ReviewPanel {
            token,
            dialog: ModalDialog::new(
                I18nManager::format("history-review-title", &[("resource", title.into())]),
                Text::new(message.as_ref()),
            )
            .with_test_id("history-restore-review")
            .open(true)
            .primary_action(
                if ready {
                    "history-review-confirm"
                } else {
                    "history-review-refresh"
                },
                move || {
                    confirm.borrow_mut().push_back(Command::Review(command));
                    super::super::note_news();
                },
            )
            .secondary_action("history-review-cancel", move || {
                cancel
                    .borrow_mut()
                    .push_back(Command::Review(ReviewCommand::Cancel));
                super::super::note_news();
            })
            .on_dismiss(move || {
                dismiss
                    .borrow_mut()
                    .push_back(Command::Review(ReviewCommand::Cancel));
                super::super::note_news();
            }),
        });
    }
    pub fn take_restored_source(&mut self) -> Option<HistorySourceRegistration> {
        self.restored_source.take()
    }
    pub fn has_review_request(&self) -> bool {
        self.review_request.is_some()
    }
    pub fn service_restore(
        &mut self,
        state: &mut State,
        root: &mut dyn Widget,
        authority: Option<&Verdicts>,
        author: &MemberId,
    ) -> bool {
        let Some(command) = self.review_request.take() else {
            return false;
        };
        if matches!(command, ReviewCommand::Cancel) {
            self.review = None;
            return false;
        }
        if self.validate_root(&*root).is_err() {
            self.invalidate_owner();
            return false;
        }
        let Some(scope) = self.registration.definition.scopes.get(self.active_scope) else {
            return false;
        };
        if let Err(reason) =
            crate::history_mode::capability::history_restore_readiness(&*root, scope.key)
        {
            self.show_review(None, reason);
            return false;
        }
        if matches!(
            command,
            ReviewCommand::Open
                | ReviewCommand::Refresh
                | ReviewCommand::RevertChange
                | ReviewCommand::Name
        ) {
            if let HistorySourceRegistration::OwnedDocument { owner, .. } = self.source() {
                owner.close_open_step(&mut state.doc);
            }
            let document = self.source().document(&state.doc).ok();
            if let (Some(controller), Some(document)) = (self.controller.as_mut(), document) {
                controller.observe(&document);
            }
        }
        if self.controller.is_none() {
            return false;
        }
        if matches!(command, ReviewCommand::RevertChange) {
            return self.revert_selected_change(state, root, (authority, author));
        }
        if matches!(command, ReviewCommand::Name) {
            return self.name_selected_changes(state, root, (authority, author));
        }
        let Some(controller) = &self.controller else {
            return false;
        };
        if matches!(command, ReviewCommand::Open | ReviewCommand::Refresh) {
            let review = controller.review_restore();
            let version = self
                .selected
                .first()
                .and_then(|id| self.timeline.entries().iter().find(|entry| &entry.id == id))
                .map_or_else(
                    || I18nManager::lookup_plain("history-review-selected-version"),
                    |entry| entry.summary.clone(),
                );
            match review {
                Ok(review) => self.show_review(
                    Some(review),
                    I18nManager::format("history-review-body", &[("version", version.into())]),
                ),
                Err(_) => {
                    self.show_review(
                        None,
                        I18nManager::lookup_plain("history-review-not-content"),
                    );
                }
            }
            return false;
        }
        let Some(review) = self.review.as_ref().and_then(|panel| panel.token.clone()) else {
            return false;
        };
        if controller.selected_part().is_some() {
            return match controller.restore_part(&review, author) {
                Ok(()) => {
                    self.review = None;
                    true
                }
                Err(error) => {
                    self.show_review(None, error.to_string());
                    false
                }
            };
        }
        self.write_reviewed(
            state,
            root,
            (authority, author),
            &review,
            ReviewedWrite::Whole,
        )
    }

    /// Restore just this change: take back what the selected change did, on its own, against the live
    /// frontier as it is now.
    fn revert_selected_change(
        &mut self,
        state: &mut State,
        root: &mut dyn Widget,
        actor: (Option<&Verdicts>, &MemberId),
    ) -> bool {
        let selected = self.selected.clone();
        self.write_selected(state, root, actor, ReviewedWrite::Changes(&selected))
    }

    /// Name the selected run of changes, from its oldest to its newest, with what the name field holds;
    /// an empty field removes the run's name.
    fn name_selected_changes(
        &mut self,
        state: &mut State,
        root: &mut dyn Widget,
        actor: (Option<&Verdicts>, &MemberId),
    ) -> bool {
        let Some((start, end)) = self.selected_run() else {
            return false;
        };
        self.name_draft_for = None;
        self.toolbar_dirty = true;
        let request = crate::GroupNameRequest {
            document_id: self.selected_name().map(|named| named.id.clone()),
            start,
            end,
            label: self.name_draft.get().trim().to_owned(),
        };
        if request.label.is_empty() && request.document_id.is_none() {
            return false;
        }
        self.write_selected(state, root, actor, ReviewedWrite::Name(&request))
    }

    /// One write about the selected changes, reviewed against the live frontier as it is now.
    fn write_selected(
        &mut self,
        state: &mut State,
        root: &mut dyn Widget,
        actor: (Option<&Verdicts>, &MemberId),
        write: ReviewedWrite<'_>,
    ) -> bool {
        let Some(controller) = &self.controller else {
            return false;
        };
        let Ok(review) = controller.review_restore() else {
            self.show_review(
                None,
                I18nManager::lookup_plain("history-review-not-content"),
            );
            return false;
        };
        self.write_reviewed(state, root, actor, &review, write)
    }

    /// Run the history verb `write` asks for on the reviewed record, and say why when it does not run.
    fn write_reviewed(
        &mut self,
        state: &mut State,
        root: &mut dyn Widget,
        actor: (Option<&Verdicts>, &MemberId),
        review: &crate::history_mode::RestoreReview,
        write: ReviewedWrite<'_>,
    ) -> bool {
        match self.run_reviewed(state, root, actor, review, write) {
            Ok(changed) => {
                self.review = None;
                if changed {
                    self.restored_source = Some(self.source().clone());
                    // Existing writers observe the new canonical heads. Never
                    // finish another action or manufacture another undo span.
                    if let HistorySourceRegistration::OwnedDocument { owner, .. } = self.source()
                        && let Err(error) = owner.schedule_committed_history()
                    {
                        tracing::error!(%error, "a restored record could not be saved");
                    }
                }
                changed
            }
            Err(message) => {
                self.show_review(None, message);
                false
            }
        }
    }

    /// The store's history verb `write` names, run on the reviewed record by its owner: checked
    /// against the review, the author's authority and the live frontier.
    fn run_reviewed(
        &self,
        state: &mut State,
        root: &mut dyn Widget,
        (authority, author): (Option<&Verdicts>, &MemberId),
        review: &crate::history_mode::RestoreReview,
        write: ReviewedWrite<'_>,
    ) -> Result<bool, String> {
        use crate::history_mode::owner::{FINISH_EDIT, HistoryRun, NOT_RESTORED, STALE_REVIEW};
        use crate::history_source::verbs::HistoryEdit;
        let (Some(scope), Some(controller)) = (
            self.registration.definition.scopes.get(self.active_scope),
            self.controller.as_ref(),
        ) else {
            return Err(NOT_RESTORED.to_owned());
        };
        let source = self.source();
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
                crate::history_mode::HistoryError::StaleLiveHeads => STALE_REVIEW.to_owned(),
                crate::history_mode::HistoryError::UncommittedLiveChanges => FINISH_EDIT.to_owned(),
                _ => "This review no longer matches the selected document version.".to_owned(),
            })?;
        let edit = match write {
            ReviewedWrite::Whole => HistoryEdit::Restore {
                version: crate::history_source::verbs::version_name(review.selected_heads()),
                ids: None,
                step: None,
            },
            ReviewedWrite::Changes(entries) => {
                let changes =
                    crate::history_source::revert::changes_of_entries(&mut document, entries);
                if changes.is_empty() {
                    return Err("This change is no longer in the document.".into());
                }
                HistoryEdit::Undo {
                    steps: None,
                    invocation: None,
                    by: None,
                    to: None,
                    step: None,
                    chosen: Some(changes.iter().map(|(hash, _)| hash.to_string()).collect()),
                }
            }
            ReviewedWrite::Name(request) => HistoryEdit::Name {
                from: tilo_ui_base::HistoryEntryId::hex(&request.start),
                to: tilo_ui_base::HistoryEntryId::hex(&request.end),
                name: request.label.clone(),
                replaces: request.document_id.clone(),
            },
        };
        let title = scope.title;
        let app_id = &review.resource().app_id;
        let owner = match source {
            HistorySourceRegistration::OwnedDocument { owner, .. } => Some(owner),
            _ => None,
        };
        let mut apply = |authority: Option<&Verdicts>, author: &MemberId| {
            // Restoring changes the app's records: as the device says of that now. The window's
            // own records are decided where the restore lands.
            if authority.is_some_and(|authority| {
                !authority.permits(&tilo_ui::shown_actions::ShownAction::app(
                    app_id,
                    tilo_ui_base::permissions::ActionKind::Undoable,
                ))
            }) {
                return Err("You no longer have permission to restore this document.".to_owned());
            }
            let run = HistoryRun {
                edit: edit.clone(),
                context: tilo_actions::ActionContext::for_resource(
                    review.resource().document_id.as_str(),
                    "document",
                    title,
                )
                .with_app(app_id, title)
                .with_invoker(tilo_actions::ActionInvoker::admitted(
                    &tilo_actions::Acting::member(author.clone()),
                )),
                current: review.reviewed_live_heads(),
                author,
            };
            if let Some(owner) = owner {
                owner.run_history_edit(&mut *root, &mut state.doc, &run)
            } else {
                let (doc, history, _) = state.parts_mut();
                let action = crate::history_source::DocumentHistoryEdit::new(run.edit.clone());
                run.step(doc, history, &action, None)
            }
        };
        match owner {
            Some(owner) => owner.with_restore_authority(authority, author, apply),
            None => apply(authority, author),
        }
    }
}

/// What a reviewed write in History does: bring a selected version back whole, take back the chosen
/// changes, or name a run of them.
#[derive(Clone, Copy)]
enum ReviewedWrite<'a> {
    Whole,
    Changes(&'a [tilo_ui_base::widget::HistoryEntryId]),
    Name(&'a crate::GroupNameRequest),
}
