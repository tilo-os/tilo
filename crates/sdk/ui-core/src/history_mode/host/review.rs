//! Review presentation and synchronous delivery to the sole restore admission door.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::{
    State,
    i18n::I18nManager,
    permissions::{PermissionContext, UserId},
    widget::ModalDialog,
};

#[derive(Clone, Copy)]
pub(super) enum ReviewCommand {
    Open,
    /// Take back just the selected change, leaving everything since: a
    /// forward change of its own, undoable, so it asks no confirmation.
    RevertChange,
    Confirm,
    FileConfirm(uuid::Uuid),
    Cancel,
    Refresh,
}
pub(super) struct ReviewPanel {
    pub(super) dialog: ModalDialog<Text>,
    token: Option<crate::history_mode::RestoreReview>,
}
impl HistoryHost {
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, clock: fn() -> Option<u64>) {
        self.clock = clock;
    }

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
        let ready = token.is_some()
            || self
                .files
                .as_ref()
                .is_some_and(|files| files.review.is_some());
        let command =
            if let Some(files) = self.files.as_ref().filter(|files| files.review.is_some()) {
                ReviewCommand::FileConfirm(files.confirmation_token())
            } else if ready {
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
                move || confirm.borrow_mut().push_back(Command::Review(command)),
            )
            .secondary_action("history-review-cancel", move || {
                cancel
                    .borrow_mut()
                    .push_back(Command::Review(ReviewCommand::Cancel));
            })
            .on_dismiss(move || {
                dismiss
                    .borrow_mut()
                    .push_back(Command::Review(ReviewCommand::Cancel));
            }),
        });
    }
    pub(crate) fn take_restored_source(&mut self) -> Option<HistorySourceRegistration> {
        self.restored_source.take()
    }
    pub(crate) fn has_review_request(&self) -> bool {
        self.review_request.is_some()
    }
    pub(crate) fn service_restore(
        &mut self,
        state: &mut State,
        root: &dyn Widget,
        authority: &PermissionContext,
        author: &UserId,
    ) -> bool {
        let Some(command) = self.review_request.take() else {
            return false;
        };
        if matches!(command, ReviewCommand::Cancel) {
            self.review = None;
            if let Some(files) = &mut self.files {
                files.cancel_review();
            }
            return false;
        }
        if self.validate_root(root).is_err() {
            self.invalidate_owner();
            return false;
        }
        let Some(scope) = self.registration.definition.scopes.get(self.active_scope) else {
            return false;
        };
        if let Err(reason) =
            crate::history_mode::capability::history_restore_readiness(root, scope.key)
        {
            if let Some(files) = &mut self.files {
                files.cancel_review();
            }
            self.show_review(None, reason);
            return false;
        }
        if matches!(
            self.source(),
            HistorySourceRegistration::CheckedFiles { .. }
        ) {
            self.service_file_review(command);
            return false;
        }
        if matches!(command, ReviewCommand::FileConfirm(_)) {
            return false;
        }
        if matches!(
            command,
            ReviewCommand::Open | ReviewCommand::Refresh | ReviewCommand::RevertChange
        ) {
            // Review against a finished boundary: the word still being typed
            // becomes its own step, and the review sees the frontier after it.
            if let HistorySourceRegistration::OwnedDocument { owner, .. } = self.source() {
                owner.close_open_step(&mut state.doc);
            }
            let document = self.source().document(&state.doc).ok();
            if let (Some(controller), Some(document)) = (self.controller.as_mut(), document)
                && let Err(error) = controller.refresh(&document)
            {
                tracing::debug!(%error, "history review could not observe the live frontier");
            }
        }
        if self.controller.is_none() {
            return false;
        }
        if matches!(command, ReviewCommand::RevertChange) {
            return self.revert_selected_change(state, (authority, author));
        }
        let Some(controller) = &self.controller else {
            return false;
        };
        if matches!(command, ReviewCommand::Open | ReviewCommand::Refresh) {
            let review = controller.review_restore();
            let version = self
                .selected
                .first()
                .and_then(|id| controller.entries().iter().find(|entry| &entry.id == id))
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
        let Some(review) = self.review.as_ref().and_then(|panel| panel.token.as_ref()) else {
            return false;
        };
        let Some(scope) = self.registration.definition.scopes.get(self.active_scope) else {
            return false;
        };
        let Some(now) = (self.clock)() else {
            self.show_review(None, I18nManager::lookup_plain("history-review-no-clock"));
            return false;
        };
        let result = crate::state::owned_restore::restore_reviewed_history(
            state,
            &crate::state::owned_restore::ReviewedRestore {
                source: self.source(),
                scope,
                controller,
                review,
            },
            (authority, author),
            now,
            crate::state::owned_restore::RestoreExtent::Whole,
        );
        match result {
            Ok(receipt) => {
                self.review = None;
                if receipt.changed {
                    self.restored_source = Some(self.source().clone());
                    // Existing writers observe the new canonical heads. Never
                    // finish another action or manufacture another undo span.
                    if let HistorySourceRegistration::OwnedDocument { owner, .. } = self.source() {
                        owner.schedule_committed_history();
                    }
                }
                receipt.changed
            }
            Err(message) => {
                self.show_review(None, message);
                false
            }
        }
    }

    /// Restore just this change: take back what the selected change did, on
    /// its own, against the live frontier as it is now. A refusal shows in
    /// the review panel, as a refused restore does.
    fn revert_selected_change(
        &mut self,
        state: &mut State,
        (authority, author): (&PermissionContext, &UserId),
    ) -> bool {
        let Some(scope) = self.registration.definition.scopes.get(self.active_scope) else {
            return false;
        };
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
        let Some(now) = (self.clock)() else {
            self.show_review(None, I18nManager::lookup_plain("history-review-no-clock"));
            return false;
        };
        let selected = self.selected.clone();
        let result = crate::state::owned_restore::restore_reviewed_history(
            state,
            &crate::state::owned_restore::ReviewedRestore {
                source: self.source(),
                scope,
                controller,
                review: &review,
            },
            (authority, author),
            now,
            crate::state::owned_restore::RestoreExtent::Changes(&selected),
        );
        match result {
            Ok(receipt) => {
                self.review = None;
                if receipt.changed {
                    self.restored_source = Some(self.source().clone());
                    if let HistorySourceRegistration::OwnedDocument { owner, .. } = self.source() {
                        owner.schedule_committed_history();
                    }
                }
                receipt.changed
            }
            Err(message) => {
                self.show_review(None, message);
                false
            }
        }
    }
}
