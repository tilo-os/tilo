//! What the palette is waiting for, and what it has shown: its request stays open across a History
//! review, so the person's question is answered rather than dropped.

use super::{ActionPalette, PendingOperation};

impl ActionPalette {
    pub(crate) fn suspend_for_history(&mut self) {
        self.history_suspended = true;
        self.request_generation = uuid::Uuid::new_v4();
    }

    pub(crate) fn history_has_ownership(&self) -> bool {
        self.history_resolution_error.is_some()
            || self
                .pending_dispatch
                .as_ref()
                .is_some_and(|pending| pending.history_target.is_some())
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
            .or_else(|| self.arg.as_ref().and_then(|arg| arg.bound.clone()))
    }

    pub(crate) fn history_pending_address(&self) -> Option<tilo_actions::ActionAddress> {
        self.pending_dispatch
            .as_ref()
            .map(|pending| pending.address.clone())
            .or_else(|| self.arg.as_ref().map(|state| state.address.clone()))
    }

    pub(crate) fn history_pending_invocation(&self) -> Option<tilo_actions::ControlInvocation> {
        self.pending_dispatch
            .as_ref()
            .and_then(PendingOperation::control_invocation)
    }

    pub(crate) fn history_pending_continuation_arguments(
        &self,
    ) -> Option<crate::bound_dispatch::ControlContinuationArguments> {
        self.pending_dispatch
            .as_ref()
            .and_then(PendingOperation::continuation_arguments)
    }

    pub(crate) fn history_exact_capture_request(
        &self,
    ) -> Option<(
        crate::bound_dispatch::local_target::LocalResourceTarget,
        tilo_actions::ControlInvocation,
        crate::bound_dispatch::ControlContinuationArguments,
    )> {
        let pending = self.pending_dispatch.as_ref()?;
        if pending.history_seen.is_some() {
            return None;
        }
        Some((
            pending.history_target.clone()?,
            pending.control_invocation()?,
            pending.continuation_arguments()?,
        ))
    }

    pub(crate) fn retain_history_exact_observation(
        &mut self,
        observation: Result<crate::bound_dispatch::ControlObservation, String>,
    ) {
        let identity = observation
            .as_ref()
            .map(crate::bound_dispatch::ControlObservation::identity)
            .cloned()
            .map_err(Clone::clone);
        if let Some(pending) = self.pending_dispatch.as_mut() {
            pending.history_identity = Some(identity);
            pending.history_seen = Some(observation);
        }
    }

    pub(crate) fn retain_history_ownership(
        &mut self,
        target: crate::bound_dispatch::local_target::LocalResourceTarget,
        identity: Option<Result<crate::bound_dispatch::ControlIdentity, String>>,
        observation: Option<Result<crate::bound_dispatch::ControlObservation, String>>,
    ) {
        if self.history_has_ownership() {
            return;
        }
        if let Some(pending) = self.pending_dispatch.as_mut() {
            pending.history_target = Some(target);
            pending.history_identity = identity;
            pending.history_seen = observation;
        } else if let Some(arg) = self.arg.as_mut() {
            arg.history_target = Some(target);
            arg.history_draft_identity = identity;
        }
    }

    pub(crate) fn retain_history_resolution_error(&mut self, reason: impl Into<String>) {
        if !self.history_has_ownership() {
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
        if self.pending_dispatch.is_some() {
            self.history_review_required = true;
            self.open = true;
        }
    }
}
