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
    Confirm,
    FileConfirm(uuid::Uuid),
    Cancel,
    Refresh,
}
pub(super) struct ReviewPanel {
    pub(super) dialog: ModalDialog<Text>,
    token: Option<crate::history_mode::RestoreReview>,
    native: Option<(
        crate::history_mode::capability::NativeHistoryActions,
        Option<serde_json::Value>,
    )>,
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
        // A refusal clears the confirmation token, not the native engine
        // identity. Losing a capability must not downgrade a retry to the
        // schema restore door. Only cancel/owner replacement ends the review.
        let native = self.review.as_ref().and_then(|panel| {
            panel
                .native
                .as_ref()
                .map(|(actions, review)| (actions.clone(), token.as_ref().and(review.clone())))
        });
        self.review = Some(ReviewPanel {
            token,
            native,
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
    fn service_native_review(
        &mut self,
        state: &mut State,
        root: &mut dyn Widget,
        transport: &mut dyn tilo_actions::ActionTransport,
        command: ReviewCommand,
        actions: Option<crate::history_mode::capability::NativeHistoryActions>,
    ) -> bool {
        let result: Result<bool, String> = (|| {
            let controller = self.controller.as_ref().ok_or("History is unavailable.")?;
            if matches!(command, ReviewCommand::Open | ReviewCommand::Refresh) {
                let actions = actions.ok_or("Native recovery is no longer available.")?;
                let (pinned, _) = self
                    .review
                    .as_ref()
                    .and_then(|panel| panel.native.as_ref())
                    .ok_or("Review restoration again.")?;
                if actions.restore != pinned.restore || actions.review != pinned.review {
                    return Err("Native recovery changed. Review restoration again.".into());
                }
                let token = controller
                    .review_restore()
                    .map_err(|error| error.to_string())?;
                let document = self
                    .source()
                    .document(&state.doc)
                    .map_err(|error| error.to_string())?;
                controller
                    .validate_restore(&token, &document)
                    .map_err(|error| error.to_string())?;
                // Whole-version UI restoration is still an explicit exact-path
                // selection. An empty diff is never an instruction to restore all.
                let paths: Vec<_> = crate::history_source::steps::changed_items(
                    &mut document.clone(),
                    token.selected_heads(),
                    token.reviewed_live_heads(),
                )
                .into_iter()
                .map(|item| item.path)
                .collect();
                if paths.is_empty() {
                    return Err("The selected version has no differing content.".into());
                }
                let version = token
                    .selected_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("+");
                let output = self.invoke_native_history(
                    state,
                    root,
                    transport,
                    (
                        &actions.review,
                        serde_json::json!({"version": version, "paths": paths}),
                        false,
                    ),
                )?;
                let review = output
                    .and_then(|value| value.get("review").cloned())
                    .ok_or("Native recovery did not return a review.")?;
                let title = token
                    .selection()
                    .first()
                    .and_then(|id| controller.entries().iter().find(|entry| &entry.id == id))
                    .map_or_else(
                        || I18nManager::lookup_plain("history-review-selected-version"),
                        |entry| entry.summary.clone(),
                    );
                self.show_review(
                    Some(token),
                    I18nManager::format("history-review-body", &[("version", title.into())]),
                );
                if let Some(panel) = &mut self.review {
                    panel.native = Some((actions, Some(review)));
                }
                return Ok(false);
            }
            let panel = self.review.as_ref().ok_or("Review restoration again.")?;
            let token = panel.token.as_ref().ok_or("Review restoration again.")?;
            let (pinned, review) = panel.native.as_ref().ok_or("Review restoration again.")?;
            let review = review.as_ref().ok_or("Review restoration again.")?;
            let current = actions.ok_or("Native recovery is no longer available.")?;
            if current.restore != pinned.restore || current.review != pinned.review {
                return Err("Native recovery changed. Review restoration again.".into());
            }
            let document = self
                .source()
                .document(&state.doc)
                .map_err(|error| error.to_string())?;
            controller
                .validate_restore(token, &document)
                .map_err(|error| error.to_string())?;
            let address = pinned.restore.clone();
            let arguments = serde_json::json!({"review": review});
            self.invoke_native_history(state, root, transport, (&address, arguments, true))?;
            self.review = None;
            self.restored_source = Some(self.source().clone());
            Ok(true)
        })();
        match result {
            Ok(changed) => changed,
            Err(message) => {
                self.show_review(None, message);
                false
            }
        }
    }

    fn invoke_native_history(
        &self,
        state: &mut State,
        root: &mut dyn Widget,
        transport: &mut dyn tilo_actions::ActionTransport,
        request: (&tilo_actions::ActionAddress, serde_json::Value, bool),
    ) -> Result<Option<serde_json::Value>, String> {
        let (address, arguments, confirmed) = request;
        let HistorySourceRegistration::OwnedDocument { owner, .. } = self.source() else {
            return Err("Native recovery requires a captured document owner.".into());
        };
        let crate::history_mode::owner::OwnerKind::Component(binding) = &owner.0 else {
            return Err("Native recovery requires a native document owner.".into());
        };
        let snapshots =
            crate::bound_dispatch::discover_bound_resources(root, &state.doc, Some(address))
                .map_err(|error| format!("{error:?}"))?;
        let snapshots: Vec<_> = snapshots
            .into_iter()
            .filter(|snapshot| snapshot.binding.same_component(binding))
            .collect();
        let resources = snapshots
            .iter()
            .flat_map(|snapshot| snapshot.resources().iter().cloned())
            .collect::<Vec<_>>();
        let (_, _, spec) = crate::bound_dispatch::resolve_resource_invocation(
            &resources,
            address,
            None,
            arguments.as_object(),
            confirmed,
        )
        .map_err(|error| format!("{error:?}"))?;
        if spec.contract.execution != tilo_actions::ExecutionMode::Immediate {
            return Err("Native recovery must complete synchronously.".into());
        }
        let target = crate::bound_dispatch::select_bound_target(&snapshots, address, None)
            .map_err(|error| format!("{error:?}"))?
            .ok_or("The captured owner no longer publishes native recovery.")?;
        let started = crate::bound_dispatch::invoke_resource_action(
            root,
            &mut state.doc,
            &target,
            transport,
            crate::bound_dispatch::ResourceActionRequest {
                argument: None,
                arguments: arguments.as_object(),
                actor: &binding.author.0,
                confirmed,
                broker_admitted: false,
                invocation: tilo_actions::InvocationId::new(),
                source_invocation: None,
                ambient: None,
                scope: None,
                route: None,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        match started {
            crate::widget::ResourceOperationStart::Completed(output) => Ok(output.value),
            _ => Err("Native recovery must complete synchronously.".into()),
        }
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
        root: &mut dyn Widget,
        authority: &PermissionContext,
        author: &UserId,
        transport: &mut dyn tilo_actions::ActionTransport,
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
        let scope_key = scope.key;
        let native = if matches!(
            self.source(),
            HistorySourceRegistration::CheckedFiles { .. }
        ) {
            None
        } else {
            crate::widget::capability::capability::<
                dyn crate::history_mode::capability::DocumentHistory,
            >(root)
            .and_then(|history| history.native_history_actions(scope_key))
        };
        // Pin the engine and exact action routes before any review can refuse,
        // including readiness, frontier validation and missing action output.
        // A native confirmation witness is acquired separately on success.
        if let Some(actions) = &native
            && self
                .review
                .as_ref()
                .is_none_or(|panel| panel.native.is_none())
        {
            self.show_review(
                None,
                I18nManager::lookup_plain("history-review-not-content"),
            );
            if let Some(panel) = &mut self.review {
                panel.native = Some((actions.clone(), None));
            }
        }
        if let Err(reason) =
            crate::history_mode::capability::history_restore_readiness(root, scope_key)
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
        if matches!(command, ReviewCommand::Open | ReviewCommand::Refresh) {
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
        let Some(controller) = &self.controller else {
            return false;
        };
        if native.is_some()
            || self
                .review
                .as_ref()
                .is_some_and(|panel| panel.native.is_some())
        {
            return self.service_native_review(state, root, transport, command, native);
        }
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
            authority,
            author,
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
