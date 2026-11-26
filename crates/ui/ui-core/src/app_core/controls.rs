//! Control dispatch: what the app does with the control intents its tree queued, written once.

use crate::bound_dispatch::local_target::LocalResourceTarget;
use crate::bound_dispatch::{BoundActionTarget, ControlContinuationArguments};
use crate::widget::ResourceOperationStart;
use crate::widget::controls::Origin;
use tilo_actions::{ActionAddress, ApiError, ControlInvocation, InvocationId};

use super::ResourceInvocation;

/// What entering an API started, or why it refused.
pub(crate) type Started = Result<ResourceOperationStart, ApiError>;

/// How long a press of the person's waits for what it changes to arrive from the workspace
/// before the window says it has not: what a just-opened window shows arrives in a round trip.
const ARRIVAL_WAIT: crate::time::Duration = crate::time::Duration::from_secs(30);

/// A press of the person's whose API answered that what it changes has not arrived from the
/// workspace yet ([`tilo_actions::api::NOT_READY`]: nothing was made, ask again). The window
/// asks again each time it dispatches its controls, as any caller of that answer does, until the
/// press goes ahead or [`ARRIVAL_WAIT`] passed; only then is it refused over the window. Its
/// control hears how it ended once it did.
pub(crate) struct WaitingControl {
    /// The component whose control it was, or `None` for the outer document's.
    target: Option<BoundActionTarget>,
    intent: ControlInvocation,
    origin: Origin,
    until: crate::time::Instant,
}

/// Whether `result` says what the press changes has not arrived yet, and to ask again.
fn waits_for_arrival(result: &Started) -> bool {
    matches!(result, Err(ApiError::Unavailable { code, .. }) if code == tilo_actions::api::NOT_READY)
}

/// When a press first refused at `first` (now, when `None`) stops waiting, or `None` once it has.
fn still_waiting(first: Option<crate::time::Instant>) -> Option<crate::time::Instant> {
    let now = crate::time::now();
    let until = first.unwrap_or_else(|| now.checked_add(ARRIVAL_WAIT).unwrap_or(now));
    (now < until).then_some(until)
}

/// What an edit an effect submitted for its invoker came from, told how it ended once the broker
/// settled it: the effect's own answer may wait for what the edit made.
#[derive(Clone)]
pub(crate) enum FollowUpControl {
    /// A control of the window's own document.
    Outer(ActionAddress),
    /// A control of a hosted component.
    Component(BoundActionTarget),
}

impl crate::app::App {
    /// Tell the effect `running` and the control its edit came from how the broker settled it.
    pub(crate) fn follow_up_settled(
        &mut self,
        control: &FollowUpControl,
        running: InvocationId,
        result: &Started,
    ) {
        match control {
            FollowUpControl::Outer(address) => {
                self.outer_control_settled(address, Some(running), result);
            }
            FollowUpControl::Component(target) => {
                self.bound_control_settled(target, Some(running), result);
            }
        }
    }

    /// A control bound to the window's History API (or its unsaved-changes Retry) runs there, never as
    /// an app action.
    pub(crate) fn take_history_invocation(
        &mut self,
        intent: &ControlInvocation,
        owner: Option<crate::history_mode::HistoryOwner>,
    ) -> bool {
        let host_action = self.is_host_action_resource(&intent.address.resource);
        if intent.address.resource != crate::history_mode::api::HISTORY_RESOURCE && !host_action {
            return false;
        }
        let embedded = owner.filter(|_| !host_action).and_then(|owner| {
            crate::history_mode::embedded::invoke_api(self.host_root_mut(), &owner, intent)
        });
        let result = if let Some(result) = embedded {
            self.embedded_history_acted();
            result
                .map(|()| ResourceOperationStart::Completed(tilo_actions::ActionOutput::default()))
        } else {
            self.run_local_history_address(&intent.address, &json_argument(&intent.arguments))
        };
        crate::widget::controls::on_control_invocation_result(
            self.host_root_mut(),
            &intent.address,
            &result,
        );
        true
    }

    /// Run a History API action, or one of the window's own, locally.
    pub(crate) fn run_local_history_address(
        &mut self,
        address: &ActionAddress,
        argument: &str,
    ) -> Started {
        self.history_address(address, Some(argument))
            .unwrap_or_else(|| Err(ApiError::Unknown(address.clone())))
    }

    /// Run what the person chose in the window's palette, once it collected what the action
    /// asks for: a row History kept back only as it was when kept, and any other where the window
    /// publishes its address. One whose action waits for the person's yes goes through the
    /// device's dispatch, which asks them ([`Self::ask_through_device`]).
    pub(crate) fn run_palette_operation(
        &mut self,
        pending: &crate::action_palette::PendingOperation,
    ) -> Started {
        let argument = pending.wire_argument();
        let actor = tilo_actions::Actor::Member(self.local_author().clone());
        let request = ResourceInvocation::new(&pending.address, &actor)
            .argument(argument.as_deref())
            .ambient_context(Some(&pending.context));
        let (target, started) = match &pending.history_target {
            // History is a window's own, and the web build draws no window.
            #[cfg(not(target_arch = "wasm32"))]
            Some(target) => {
                self.held_palette_row_unchanged(pending, target)?;
                let started = match target {
                    LocalResourceTarget::Component(target) => {
                        self.invoke_bound_control(target, &request)
                    }
                    LocalResourceTarget::Outer => self.invoke_outer_control(&request),
                };
                (target.clone(), started)
            }
            #[cfg(target_arch = "wasm32")]
            Some(_) => {
                return Err(ApiError::Unavailable {
                    code: "history_target_on_page".into(),
                    reason: "the web build holds no palette row back to History".into(),
                });
            }
            None => {
                if let Some(ran) = self.history_address(&pending.address, argument.as_deref()) {
                    return ran;
                }
                match &pending.bound {
                    Some(target) => (
                        LocalResourceTarget::Component(target.clone()),
                        self.invoke_bound_control(target, &request),
                    ),
                    None => (
                        LocalResourceTarget::Outer,
                        self.invoke_chosen_address(&request),
                    ),
                }
            }
        };
        match started {
            Err(ApiError::ConfirmationRequired) => {
                let arguments = pending.continuation_arguments().ok_or_else(|| {
                    ApiError::InvalidArguments("The palette's arguments are incomplete".into())
                })?;
                let address = match &target {
                    LocalResourceTarget::Component(bound) => bound.address.address.clone(),
                    LocalResourceTarget::Outer => pending.address.clone(),
                };
                self.ask_through_device(&target, &address, &arguments)
            }
            started => started,
        }
    }

    /// Whether the row History held back for `target` still means, resolves and encodes what it
    /// did when held: refused as stale otherwise.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn held_palette_row_unchanged(
        &self,
        pending: &crate::action_palette::PendingOperation,
        target: &LocalResourceTarget,
    ) -> Result<(), ApiError> {
        use crate::bound_dispatch::{control_draft_identity, control_observation_with_arguments};
        let stale = |reason: &str| ApiError::Unavailable {
            code: "stale_pending_palette_action".into(),
            reason: reason.into(),
        };
        let tree = self.tree();
        let identity =
            || control_draft_identity(tree.root, &tree.state.doc, target, &pending.address);
        match &pending.history_draft_identity {
            Some(Ok(captured)) if identity()? != *captured => {
                return Err(stale(
                    "The palette action resolution domain changed while History was open",
                ));
            }
            Some(Err(reason)) => return Err(stale(reason)),
            Some(Ok(_)) | None => {}
        }
        let captured = match &pending.history_identity {
            Some(Ok(captured)) => captured.clone(),
            Some(Err(reason)) => return Err(stale(reason)),
            None => {
                return Err(stale(
                    "The original palette action identity was not captured",
                ));
            }
        };
        let current = match pending.history_seen {
            Some(_) => captured.clone(),
            None => identity()?,
        };
        if let Some(Err(reason)) = &pending.history_seen {
            return Err(stale(reason));
        }
        let invocation = pending.control_invocation().ok_or_else(|| {
            ApiError::InvalidArguments("The retained palette arguments are incomplete".into())
        })?;
        let arguments = pending.continuation_arguments().ok_or_else(|| {
            ApiError::InvalidArguments(
                "The retained palette argument representation is invalid".into(),
            )
        })?;
        let observed = control_observation_with_arguments(
            tree.root,
            &tree.state.doc,
            target,
            &invocation,
            &arguments,
        )?;
        if let Some(Ok(seen)) = &pending.history_seen
            && observed != *seen
        {
            return Err(stale(
                "The exact action encoding changed while History was open",
            ));
        }
        if captured != current {
            return Err(stale(
                "The action meaning or confirmation policy changed while History was open",
            ));
        }
        Ok(())
    }

    /// The person's own press of `address` on `target` waits for their yes: send it through the
    /// device's dispatch, which asks them over this window, and say why where it could not go.
    pub(crate) fn press_through_device(
        &mut self,
        target: &LocalResourceTarget,
        address: &ActionAddress,
        arguments: ControlContinuationArguments,
    ) -> bool {
        match self.ask_through_device(target, address, &arguments) {
            Ok(_) => true,
            Err(error) => {
                self.refuse_visibly(&error);
                false
            }
        }
    }

    /// Deliver the control intents the tree queued: `registered` ones the control domain drained, then
    /// those the components and the outer document hold.
    pub(crate) fn dispatch_drained_controls(
        &mut self,
        registered: crate::control_domain::RegisteredControls,
    ) -> usize {
        let mut dispatched = 0usize;
        // What waited for what it changes is asked again first, in the order it was pressed.
        for waiting in std::mem::take(self.waiting_controls()) {
            let went = match &waiting.target {
                Some(target) => self.dispatch_bound_control_invocation(
                    target,
                    &waiting.intent,
                    None,
                    waiting.origin,
                    Some(waiting.until),
                ),
                None => self.dispatch_outer_control_invocation(
                    waiting.intent,
                    None,
                    waiting.origin,
                    Some(waiting.until),
                ),
            };
            dispatched = dispatched.saturating_add(usize::from(went));
        }
        let mut registered = registered;
        loop {
            let (went, delivered) = self.dispatch_control_round(std::mem::take(&mut registered));
            dispatched = dispatched.saturating_add(went);
            if delivered == 0 {
                return dispatched;
            }
        }
    }

    /// One round of [`Self::dispatch_drained_controls`]: how many went ahead, and how many were
    /// delivered at all.
    pub(crate) fn dispatch_control_round(
        &mut self,
        registered: crate::control_domain::RegisteredControls,
    ) -> (usize, usize) {
        let mut dispatched = 0usize;
        let mut delivered = 0usize;
        let crate::control_domain::RegisteredControls {
            mut outer,
            components,
        } = registered;
        let bound = crate::bound_dispatch::drain_control_invocations(self.host_root_mut());
        if let Some(error) = bound.error {
            tracing::warn!(
                ?error,
                "control traversal incomplete; delivering already drained intents"
            );
        }
        delivered = delivered
            .saturating_add(components.len())
            .saturating_add(bound.pending.len());
        for (target, intent, on_behalf_of, origin) in components
            .into_iter()
            .map(|(target, intent)| (target, intent, None, Origin::Person))
            .chain(bound.pending)
        {
            if on_behalf_of.is_none()
                && target.binding.enter().is_ok()
                && self.take_search_invocation(&intent, Some(&target.binding))
            {
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            if on_behalf_of.is_none()
                && crate::bound_dispatch::callback_binding(self.tree().root)
                    .is_some_and(|binding| binding.instance_id == target.binding.instance_id)
                && self.take_history_invocation(&intent, None)
            {
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            if self.dispatch_bound_control_invocation(&target, &intent, on_behalf_of, origin, None)
            {
                dispatched = dispatched.saturating_add(1);
            }
        }
        while let Some((intent, on_behalf_of, origin)) = outer
            .pop_front()
            .map(|intent| (intent, None, Origin::Person))
            .or_else(|| {
                crate::widget::input_provenance::take_attributed_with_origin(self.host_root_mut())
            })
        {
            delivered = delivered.saturating_add(1);
            if self.dispatch_outer_control_invocation(intent, on_behalf_of, origin, None) {
                dispatched = dispatched.saturating_add(1);
            }
        }
        (dispatched, delivered)
    }

    /// Deliver one control intent the outer document produced; `waited`, when the person's press
    /// already waits for what it changes to arrive, until when ([`WaitingControl`]).
    pub(crate) fn dispatch_outer_control_invocation(
        &mut self,
        intent: ControlInvocation,
        on_behalf_of: Option<InvocationId>,
        origin: Origin,
        waited: Option<crate::time::Instant>,
    ) -> bool {
        // Only what the person asked for is refused over the window; the
        // app hears of its own invocation's refusal through the feedback.
        let says_refusal = on_behalf_of.is_none() && origin.is_persons();
        if on_behalf_of.is_none()
            && (self.take_history_invocation(&intent, None)
                || self.take_search_invocation(&intent, None))
        {
            return true;
        }
        let acting = on_behalf_of.and_then(|id| self.acting_for_mut().get(&id).cloned());
        if !self.locally_offered(&intent) {
            // The person pressed what the window shows them but their grants keep from them: the
            // workspace decides it, and its refusal is said over the window when it answers. The
            // control is told what became of the press, as one the window offers is.
            if says_refusal && acting.is_none() && self.declares(&intent) {
                let arguments = ControlContinuationArguments::Json(intent.arguments);
                let started = self.ask_through_device(
                    &LocalResourceTarget::Outer,
                    &intent.address,
                    &arguments,
                );
                if let Err(error) = &started {
                    self.refuse_visibly(error);
                }
                self.outer_control_settled(&intent.address, on_behalf_of, &started);
                return started.is_ok();
            }
            let error = ApiError::Unknown(intent.address.clone());
            tracing::warn!(
                resource = %intent.address.resource,
                action = %intent.address.action,
                code = error.code(),
                "action-bound control is unavailable to the local actor"
            );
            if says_refusal {
                self.refuse_visibly(&error);
            } else {
                self.refused_unsaid(&error);
            }
            self.outer_control_settled(&intent.address, on_behalf_of, &Err(error));
            return false;
        }
        if let (Some(acting), Some(running)) = (&acting, on_behalf_of)
            && self.broker_connected()
        {
            let result = self.send_follow_up(
                running,
                &intent.address,
                &intent.arguments,
                acting.context.clone(),
                FollowUpControl::Outer(intent.address.clone()),
            );
            // Sent, it is settled when the broker answers, with what the edit made.
            if let Err(error) = &result {
                self.refused_unsaid(error);
                self.outer_control_settled(&intent.address, on_behalf_of, &result);
            }
            return result.is_ok();
        }
        let json = json_argument(&intent.arguments);
        let actor = acting.as_ref().map_or_else(
            || tilo_actions::Actor::Member(self.local_author().clone()),
            |acting| acting.invoker.acting.by.clone(),
        );
        let scopes = acting
            .as_ref()
            .map(crate::invocation_scope::Continuation::enter);
        let result = self.invoke_outer_control(
            &ResourceInvocation::new(&intent.address, &actor)
                .argument(Some(&json))
                .ambient_context(acting.as_ref().and_then(|acting| acting.context.as_ref())),
        );
        drop(scopes);
        if says_refusal
            && acting.is_none()
            && waits_for_arrival(&result)
            && let Some(until) = still_waiting(waited)
        {
            self.waiting_controls().push(WaitingControl {
                target: None,
                intent,
                origin,
                until,
            });
            return false;
        }
        self.outer_control_settled(&intent.address, on_behalf_of, &result);
        match result {
            Ok(_) => true,
            // The person's own press waits for their yes: the device asks them. Nobody here asked
            // for an effect's edit, and the person at the device is never asked about it.
            Err(ApiError::ConfirmationRequired) if acting.is_none() && says_refusal => self
                .press_through_device(
                    &LocalResourceTarget::Outer,
                    &intent.address,
                    ControlContinuationArguments::Json(intent.arguments),
                ),
            Err(error) => {
                tracing::warn!(resource = %intent.address.resource, action = %intent.address.action, %error, ?origin, "action-bound control invocation refused");
                if acting.is_none() && says_refusal {
                    self.refuse_visibly(&error);
                } else {
                    self.refused_unsaid(&error);
                }
                false
            }
        }
    }

    /// Tell the running effect and the app how an outer control ended.
    pub(crate) fn outer_control_settled(
        &mut self,
        address: &ActionAddress,
        on_behalf_of: Option<InvocationId>,
        result: &Started,
    ) {
        self.note_continuation(on_behalf_of, result);
        crate::widget::controls::on_control_invocation_result(
            self.host_root_mut(),
            address,
            result,
        );
    }

    /// Deliver one control intent a component produced; `waited`, when the person's press already
    /// waits for what it changes to arrive, until when ([`WaitingControl`]).
    pub(crate) fn dispatch_bound_control_invocation(
        &mut self,
        target: &BoundActionTarget,
        intent: &ControlInvocation,
        on_behalf_of: Option<InvocationId>,
        origin: Origin,
        waited: Option<crate::time::Instant>,
    ) -> bool {
        let argument = json_argument(&intent.arguments);
        let acting = on_behalf_of.and_then(|id| self.acting_for_mut().get(&id).cloned());
        if let (Some(acting), Some(running)) = (&acting, on_behalf_of)
            && self.broker_connected()
        {
            let result = self
                .bound_publication()
                .alias(&target.binding, &target.address.address.resource)
                .map_err(crate::bound_dispatch::BoundActionError::into_api)
                .and_then(|alias| {
                    self.send_follow_up(
                        running,
                        &ActionAddress::new(alias, target.address.address.action.clone()),
                        &intent.arguments,
                        acting.context.clone(),
                        FollowUpControl::Component(target.clone()),
                    )
                });
            // Sent, it is settled when the broker answers, with what the edit made.
            if let Err(error) = &result {
                self.refused_unsaid(error);
                self.bound_control_settled(target, on_behalf_of, &result);
            }
            return result.is_ok();
        }
        let actor = acting.as_ref().map_or_else(
            || tilo_actions::Actor::Member(self.local_author().clone()),
            |acting| acting.invoker.acting.by.clone(),
        );
        let scopes = acting
            .as_ref()
            .map(crate::invocation_scope::Continuation::enter);
        let answering = crate::bound_dispatch::Answering::enter(on_behalf_of.is_some());
        let result = self.invoke_bound_control(
            target,
            &ResourceInvocation::new(&target.address.address, &actor)
                .argument(Some(&argument))
                .ambient_context(acting.as_ref().and_then(|acting| acting.context.as_ref())),
        );
        drop(answering);
        drop(scopes);
        if on_behalf_of.is_none()
            && origin.is_persons()
            && (waits_for_arrival(&result) || self.waits_for_publication(target, &result))
            && let Some(until) = still_waiting(waited)
        {
            self.waiting_controls().push(WaitingControl {
                target: Some(target.clone()),
                intent: intent.clone(),
                origin,
                until,
            });
            return false;
        }
        if origin.is_persons() {
            crate::pick::note_invocation(&target.address, result.is_ok());
        }
        self.bound_control_settled(target, on_behalf_of, &result);
        match result {
            Ok(_) => true,
            // The person's own press waits for their yes: the device asks them. Nobody here asked
            // for an effect's edit, and the person at the device is never asked about it.
            Err(ApiError::ConfirmationRequired) if acting.is_none() && origin.is_persons() => self
                .press_through_device(
                    &LocalResourceTarget::Component(target.clone()),
                    &intent.address,
                    ControlContinuationArguments::Json(intent.arguments.clone()),
                ),
            Err(error) => {
                tracing::warn!(%error, ?origin, "bound control invocation refused");
                // Only what the person asked for is refused over the window;
                // the app hears of its own invocation's refusal through the
                // feedback above.
                if acting.is_none() && origin.is_persons() {
                    self.refuse_visibly(&error);
                } else {
                    self.refused_unsaid(&error);
                }
                false
            }
        }
    }

    /// Tell the running effect and the component how its control ended.
    pub(crate) fn bound_control_settled(
        &mut self,
        target: &BoundActionTarget,
        on_behalf_of: Option<InvocationId>,
        result: &Started,
    ) {
        self.note_continuation(on_behalf_of, result);
        if let Err(error) = crate::bound_dispatch::notify_control_invocation_result(
            self.host_root_mut(),
            target,
            result,
            on_behalf_of,
        ) {
            tracing::warn!(?error, "bound control feedback unavailable");
        }
    }

    /// Whether `result` is an address the component does not publish yet: an invocation its app
    /// queued as it started (a link handed to a window that is still building its view) names a row
    /// the view publishes once it is built. Like a press whose change has not arrived, it waits and is
    /// asked again. An action missing from a resource that is published, and an address of a component
    /// that publishes nothing its member may see, are refused at once.
    pub(crate) fn waits_for_publication(
        &mut self,
        target: &BoundActionTarget,
        result: &Started,
    ) -> bool {
        matches!(result, Err(ApiError::Unknown(_))) && self.component_awaits(target)
    }
}

/// `arguments` as the API's text argument.
pub(crate) fn json_argument(arguments: &serde_json::Map<String, serde_json::Value>) -> String {
    format!(
        "{}{}",
        tilo_actions::ARGUMENTS_JSON_PREFIX,
        serde_json::Value::Object(arguments.clone())
    )
}
