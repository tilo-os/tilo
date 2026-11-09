//! Stopping a page action: taking back the tab's one page slot.
//!
//! A page action runs until the engine answers it, so two things end one
//! early: the caller giving up on it (a cancelled invocation) and the tab
//! giving up on the engine (a deadline). Both answer with what is known, which
//! is that the action may already have acted on the page, and neither leaves
//! the tab waiting on an answer nobody reads.
//!
//! Several actors' actions may be in one tab, one running and others waiting
//! their turn ([`super::page_turns`]). A caller that gives up stops its own
//! action and no other: one still waiting is taken out before it ran, and is
//! known to have changed nothing; the one running is stopped by the request
//! the engine knows it under.

use super::{BrowserWidget, PAGE_OPERATION_TIMEOUT, PageOperation, PagePhase, action_failed};

impl BrowserWidget {
    /// What a page operation's phase is called in a log, so a stopped one says
    /// where it had got to.
    pub(super) fn phase_name(phase: &PagePhase) -> &'static str {
        match phase {
            PagePhase::Queued { .. } => "queued",
            PagePhase::Sent => "sent",
            PagePhase::Navigating { .. } => "navigating",
        }
    }

    /// Answer a caller that gave up on `invocation`, stopping the page action
    /// it started. `None` when nothing here is still running for it.
    ///
    /// A stopped action promises nothing about the page: the request may have
    /// reached it before the caller stopped waiting, so the answer says what
    /// became of it is unknown rather than claiming it happened or that it did
    /// not.
    pub(crate) fn answer_a_cancelled_page_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        cancellation_requested: bool,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        // The caller's own reply is stopped when it gives up waiting; the
        // page action it started is stopped with it instead of running on
        // to its deadline in a tab nobody is reading.
        if !cancellation_requested && !self.page_operation_given_up(invocation) {
            return None;
        }
        // An action still waiting its turn reached nothing: it is taken out
        // before it ran, and the engine is never told of it.
        if self.withdraw_waiting_page_action(invocation) {
            tracing::info!(
                "page action {:?} was cancelled by its caller while it waited its turn",
                invocation
            );
            return Some(action_failed(
                invocation,
                "browser.page.cancelled",
                "The page action was cancelled while it waited its turn in this tab, so it \
                 never ran and changed nothing.",
                Some(serde_json::json!({"outcome": "not_run"})),
            ));
        }
        // A read of the tab's record holds no turn and changes nothing.
        if self.withdraw_page_record_read(invocation) {
            return Some(action_failed(
                invocation,
                "browser.page.cancelled",
                "The read of the tab's record was cancelled before it was answered; reading \
                 it changes nothing.",
                Some(serde_json::json!({"outcome": "not_run"})),
            ));
        }
        if !self.cancel_page_operation(invocation) {
            return None;
        }
        Some(ui::tilo_actions::InvocationUpdate::Failed {
            invocation,
            error: ui::tilo_actions::ActionFailure {
                code: "browser.page.cancelled".into(),
                message: "The page action was cancelled before it finished, so whether it \
                          took effect is unknown; read the page again to see what it did."
                    .into(),
                details: None,
                retryable: false,
            },
        })
    }

    /// Answer the caller of a page action that ran out of time, saying that
    /// the page may have acted.
    pub(super) fn answer_a_timed_out_page_operation(
        operation: &PageOperation,
    ) -> ui::tilo_actions::InvocationUpdate {
        let phase = Self::phase_name(&operation.phase);
        // The request may have reached the page before this deadline: a failed
        // outcome promises nothing about what it did.
        tracing::warn!(
            "page action {:?} gave up after {}s with no answer from the engine (phase {})",
            operation.invocation,
            PAGE_OPERATION_TIMEOUT.as_secs(),
            phase
        );
        // Never retryable: page actions are published at most once, and the
        // broker refuses (and the bridge then drops) a retryable failure.
        action_failed(
            operation.invocation,
            "browser.page.timeout",
            "The page did not answer in time, so whether the action took effect is unknown; \
             read the page again to see what it did.",
            Some(serde_json::json!({
                "timeout_seconds": PAGE_OPERATION_TIMEOUT.as_secs(),
                "outcome": "unknown",
            })),
        )
    }

    /// Stop the page action `invocation` started, if one is still running: the
    /// engine is told which request to stop, so it stops waiting and runs the
    /// next action, and the tab's slot is freed for the action that waited
    /// longest. `false` when the invocation runs nothing here.
    fn cancel_page_operation(&mut self, invocation: ui::tilo_actions::InvocationId) -> bool {
        let Some(operation) = self
            .page_operation
            .take_if(|operation| operation.invocation == invocation)
        else {
            return false;
        };
        // The cancel names the request it stops: another actor's action in
        // this tab is never stopped by it.
        self.engine.send(ui::webview::WebViewCommand::CancelPage {
            request_id: operation.request_id,
        });
        tracing::info!(
            "page action {:?} was cancelled by its caller; the engine was told to stop it",
            invocation
        );
        true
    }

    /// Whether the effect reply waiting on `invocation` has been stopped
    /// because its caller gave up waiting for it.
    fn page_operation_given_up(&self, invocation: ui::tilo_actions::InvocationId) -> bool {
        self.pending_replies
            .iter()
            .any(|(waiting, reply)| *waiting == invocation && reply.is_cancelled())
    }
}
