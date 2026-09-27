//! Review presentation and synchronous delivery to the sole restore admission door.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::{
    State,
    permissions::{PermissionContext, UserId},
    widget::ModalDialog,
};

#[derive(Clone, Copy)]
pub(super) enum ReviewCommand {
    Open,
    Confirm,
    FileConfirm(uuid::Uuid),
    Cancel,
    Refresh,
}
pub(super) struct ReviewPanel {
    pub(super) dialog: ModalDialog<Text>,
    token: Option<crate::history_mode::RestoreReview>,
}
pub(super) fn unix_now() -> Option<u64> {
    crate::time::wall()
        .duration_since(crate::time::UNIX_EPOCH)
        .ok()
        .map(|time| time.as_secs())
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
            dialog: ModalDialog::new(format!("Restore {title}"), Text::new(message.as_ref()))
                .open(true)
                .primary_action(
                    if ready {
                        "Restore version"
                    } else {
                        "Refresh review"
                    },
                    move || confirm.borrow_mut().push_back(Command::Review(command)),
                )
                .secondary_action("Cancel", move || {
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
        if let Err(reason) = root.history_restore_readiness(scope.key) {
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
        let Some(controller) = &self.controller else {
            return false;
        };
        if matches!(command, ReviewCommand::Open | ReviewCommand::Refresh) {
            let review = controller.review_restore();
            let version = self
                .selected
                .first()
                .and_then(|id| controller.entries().iter().find(|entry| &entry.id == id))
                .map_or("Selected version", |entry| entry.summary.as_str());
            match review {
                Ok(review) => self.show_review(Some(review), format!("Version: {version}\n\nRestoring creates a new change. Existing history remains available.\n\nRestore version or Cancel.")),
                Err(_) => self.show_review(None, "This activity does not contain restorable document content."),
            }
            return false;
        }
        let Some(review) = self.review.as_ref().and_then(|panel| panel.token.as_ref()) else {
            return false;
        };
        let Some(scope) = self.registration.definition.scopes.get(self.active_scope) else {
            return false;
        };
        let Some(schema) = scope.content_schema else {
            return false;
        };
        let Some(now) = (self.clock)() else {
            self.show_review(
                None,
                "The current time is unavailable. Restore cannot check permission expiry.",
            );
            return false;
        };
        let result = crate::state::owned_restore::restore_reviewed_history(
            state,
            self.source(),
            controller,
            review,
            schema,
            authority,
            author,
            scope.title,
            now,
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
}
