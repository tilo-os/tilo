//! Stopping a page action: taking back the tab's one page slot.
//!
//! Two things end an action early: the caller giving up on it, and the tab giving up on
//! the engine at a deadline. Both answer with what is known, that the action may already
//! have acted on the page.

use super::{BrowserWidget, PAGE_OPERATION_TIMEOUT, PageOperation, PagePhase, action_failed};

impl BrowserWidget {
    pub(super) fn phase_name(phase: &PagePhase) -> &'static str {
        match phase {
            PagePhase::Queued { .. } => "queued",
            PagePhase::Sent => "sent",
            PagePhase::Navigating { .. } => "navigating",
        }
    }

    /// Answer a caller that gave up on `invocation`, stopping the page action it started.
    /// `None` when nothing here is still running for it.
    ///
    /// The request may have reached the page before the caller stopped waiting, so the
    /// answer says that what became of it is unknown.
    pub(crate) fn answer_a_cancelled_page_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        cancellation_requested: bool,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        let stopped = if cancellation_requested {
            self.cancel_page_operation(invocation)
        } else {
            // The caller's own reply is stopped when it gives up waiting; the page action
            // it started is stopped with it instead of running on to its deadline.
            self.page_operation_given_up(invocation) && self.cancel_page_operation(invocation)
        };
        if !stopped {
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

    /// Answer the caller of a page action that ran out of time, saying that the page may
    /// have acted.
    pub(super) fn answer_a_timed_out_page_operation(
        operation: &PageOperation,
    ) -> ui::tilo_actions::InvocationUpdate {
        let phase = Self::phase_name(&operation.phase);
        // The request may have reached the page before this deadline: a failed outcome
        // promises nothing about what it did.
        tracing::warn!(
            "page action {:?} gave up after {}s with no answer from the engine (phase {})",
            operation.invocation,
            PAGE_OPERATION_TIMEOUT.as_secs(),
            phase
        );
        // Never retryable: page actions are published at most once, and the broker refuses
        // and then drops a retryable failure.
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

    /// Stop the page action `invocation` started, if one still runs: the engine is told so
    /// it stops waiting and runs the next action, and the slot is freed.
    fn cancel_page_operation(&mut self, invocation: ui::tilo_actions::InvocationId) -> bool {
        if self
            .page_operation
            .as_ref()
            .is_none_or(|operation| operation.invocation != invocation)
        {
            return false;
        }
        self.page_operation = None;
        self.reader_request = None;
        self.engine.send(ui::webview::WebViewCommand::CancelPage);
        tracing::info!(
            "page action {:?} was cancelled by its caller; the engine was told to stop it",
            invocation
        );
        true
    }

    /// Whether the effect reply waiting on `invocation` was stopped because its caller gave
    /// up waiting.
    fn page_operation_given_up(&self, invocation: ui::tilo_actions::InvocationId) -> bool {
        self.pending_replies
            .iter()
            .any(|(waiting, reply)| *waiting == invocation && reply.is_cancelled())
    }
}
