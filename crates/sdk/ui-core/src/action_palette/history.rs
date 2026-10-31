//! What the palette is waiting for, and what it has shown: its request stays
//! open across a History review, so the person's question is answered rather
//! than dropped.

use super::{ActionPalette, PendingOperation};
use tilo_actions::ConfirmationPolicy;

impl ActionPalette {
    pub(crate) fn suspend_for_history(&mut self) {
        self.history_suspended = true;
        self.request_generation = uuid::Uuid::new_v4();
    }

    pub(crate) fn history_has_custody(&self) -> bool {
        self.history_resolution_error.is_some()
            || self
                .pending_dispatch
                .as_ref()
                .is_some_and(|pending| pending.history_target.is_some())
            || self
                .confirm
                .as_ref()
                .is_some_and(|confirm| confirm.pending.history_target.is_some())
            || self
                .arg
                .as_ref()
                .is_some_and(|arg| arg.history_target.is_some())
    }

    pub(crate) fn history_pending_bound_target(
        &self,
    ) -> Option<crate::bound_dispatch::BoundActionTarget> {
        self.pending_dispatch
            .as_ref()
            .and_then(|pending| pending.bound.clone())
            .or_else(|| {
                self.confirm
                    .as_ref()
                    .and_then(|confirm| confirm.pending.bound.clone())
            })
            .or_else(|| self.arg.as_ref().and_then(|arg| arg.bound.clone()))
    }

    pub(crate) fn history_pending_address(&self) -> Option<tilo_actions::ActionAddress> {
        self.pending_dispatch
            .as_ref()
            .map(|pending| pending.address.clone())
            .or_else(|| {
                self.confirm
                    .as_ref()
                    .map(|state| state.pending.address.clone())
            })
            .or_else(|| self.arg.as_ref().map(|state| state.address.clone()))
    }

    pub(crate) fn history_pending_invocation(&self) -> Option<tilo_actions::ControlInvocation> {
        self.pending_dispatch
            .as_ref()
            .and_then(PendingOperation::control_invocation)
            .or_else(|| {
                self.confirm
                    .as_ref()
                    .and_then(|state| state.pending.control_invocation())
            })
    }

    pub(crate) fn history_pending_continuation_arguments(
        &self,
    ) -> Option<crate::bound_dispatch::ControlContinuationArguments> {
        self.pending_dispatch
            .as_ref()
            .and_then(PendingOperation::continuation_arguments)
            .or_else(|| {
                self.confirm
                    .as_ref()
                    .and_then(|state| state.pending.continuation_arguments())
            })
    }

    pub(crate) fn history_exact_capture_request(
        &self,
    ) -> Option<(
        crate::bound_dispatch::local_target::LocalResourceTarget,
        tilo_actions::ControlInvocation,
        crate::bound_dispatch::ControlContinuationArguments,
    )> {
        let pending = self
            .pending_dispatch
            .as_ref()
            .or_else(|| self.confirm.as_ref().map(|state| &state.pending))?;
        if pending.history_witness.is_some() {
            return None;
        }
        Some((
            pending.history_target.clone()?,
            pending.control_invocation()?,
            pending.continuation_arguments()?,
        ))
    }

    pub(crate) fn retain_history_exact_witness(
        &mut self,
        witness: Result<crate::bound_dispatch::ControlWitness, String>,
    ) {
        let identity = witness
            .as_ref()
            .map(crate::bound_dispatch::ControlWitness::identity)
            .cloned()
            .map_err(Clone::clone);
        if let Some(pending) = self.pending_dispatch.as_mut() {
            pending.history_identity = Some(identity);
            pending.history_witness = Some(witness);
        } else if let Some(confirm) = self.confirm.as_mut() {
            confirm.pending.history_identity = Some(identity);
            confirm.pending.history_witness = Some(witness);
        }
    }

    pub(crate) fn retain_history_custody(
        &mut self,
        target: crate::bound_dispatch::local_target::LocalResourceTarget,
        identity: Option<Result<crate::bound_dispatch::ControlIdentity, String>>,
        witness: Option<Result<crate::bound_dispatch::ControlWitness, String>>,
    ) {
        // An unfinished request gets one baseline, including failed evidence.
        if self.history_has_custody() {
            return;
        }
        if let Some(pending) = self.pending_dispatch.as_mut() {
            pending.history_target = Some(target);
            pending.history_identity = identity;
            pending.history_witness = witness;
        } else if let Some(confirm) = self.confirm.as_mut() {
            confirm.pending.history_target = Some(target);
            confirm.pending.history_identity = identity;
            confirm.pending.history_witness = witness;
        } else if let Some(arg) = self.arg.as_mut() {
            arg.history_target = Some(target);
            arg.history_draft_identity = identity;
        }
    }

    pub(crate) fn retain_history_resolution_error(&mut self, reason: impl Into<String>) {
        if !self.history_has_custody() {
            self.history_resolution_error = Some(reason.into());
        }
    }

    pub(crate) fn retain_history_refusal(
        &mut self,
        pending: PendingOperation,
        reason: impl Into<String>,
    ) {
        let reason = reason.into();
        self.pending_dispatch = Some(pending);
        self.history_review_required = true;
        self.open = true;
        self.search_announcement = Some(reason);
    }

    pub(crate) fn resume_after_history(&mut self) {
        self.history_suspended = false;
        let Some(mut pending) = self
            .pending_dispatch
            .take()
            .or_else(|| self.confirm.take().map(|state| state.pending))
        else {
            return;
        };
        pending.confirmation_evidence = serde_json::json!({});
        if matches!(pending.confirmation, ConfirmationPolicy::None) {
            self.pending_dispatch = Some(pending);
            self.history_review_required = true;
            self.open = true;
            return;
        }
        self.ask_confirmation(pending);
        self.open = true;
    }
}
