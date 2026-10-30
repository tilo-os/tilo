//! Control dispatch: what the native and headless app hosts do with the
//! control intents their tree queued, written once. Each host lends the
//! doors an intent finally enters through [`ControlHost`].

use crate::bound_dispatch::local_target::LocalResourceTarget;
use crate::bound_dispatch::{BoundActionTarget, ControlContinuationArguments, Origin};
use crate::widget::ResourceOperationStart;
use tilo_actions::{ActionAddress, ControlInvocation, DoorError, InvocationId};

use super::{HostCore, ResourceInvocation};

/// What entering a door started, or why it refused.
pub(crate) type Started = Result<ResourceOperationStart, DoorError>;

pub(crate) trait ControlHost: HostCore {
    /// The person at this device.
    fn local_author(&self) -> &crate::permissions::UserId;

    /// How the host publishes its components' resources to the workspace.
    fn bound_publication(&self) -> &crate::bound_dispatch::BoundPublication;

    /// Whether the workspace's broker is at hand to admit an effect's edits
    /// for its invoker.
    fn broker_connected(&self) -> bool;

    /// Ask the broker to admit an edit the effect invocation `running`
    /// submitted for its invoker.
    fn send_follow_up(
        &mut self,
        running: InvocationId,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        context: Option<tilo_actions::ActionContext>,
    ) -> Started;

    /// The confirmation `intent` needs from the local actor, or `None` when
    /// its action is unavailable to them.
    fn local_confirmation(
        &self,
        intent: &ControlInvocation,
    ) -> Option<tilo_actions::ConfirmationPolicy>;

    /// A control bound to the window's History door (or its unsaved-changes
    /// Retry) runs there, never as an app action. An app `owner` hosted in a
    /// tile opens that tile's History. Whether it was one.
    fn take_history_invocation(
        &mut self,
        intent: &ControlInvocation,
        owner: Option<crate::history_mode::HistoryOwner>,
    ) -> bool {
        let host_action = self.is_host_action_resource(&intent.address.resource);
        if intent.address.resource != crate::history_mode::door::HISTORY_RESOURCE && !host_action {
            return false;
        }
        let embedded = owner.filter(|_| !host_action).and_then(|owner| {
            crate::history_mode::embedded::invoke_door(self.host_root_mut(), &owner, intent)
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

    /// Whether `resource` names one of the window's own actions that runs
    /// like History (the unsaved-changes Retry in its header).
    fn is_host_action_resource(&self, _resource: &str) -> bool {
        false
    }

    /// Run a History door action, or one of the window's own, locally.
    fn run_local_history_address(&mut self, address: &ActionAddress, argument: &str) -> Started;

    /// A History surface embedded in the app took a control.
    fn embedded_history_acted(&mut self) {}

    /// A control addressed to the host search door opens search instead of
    /// reaching the app; `owner` is the component it came from. Whether it
    /// was one.
    fn take_search_invocation(
        &mut self,
        intent: &ControlInvocation,
        owner: Option<&crate::bound_dispatch::CallbackBinding>,
    ) -> bool;

    /// Enter the door of the component `target` names.
    fn invoke_bound_control(
        &mut self,
        target: &BoundActionTarget,
        request: &ResourceInvocation<'_>,
    ) -> Started;

    /// Enter the outer document's door at `request.address`.
    fn invoke_outer_control(&mut self, request: &ResourceInvocation<'_>) -> Started;

    /// The control intents this host's tree registered, by owner.
    fn control_domain(&self) -> &crate::control_custody::ControlDomain;

    /// Put `request` to the person in the window's palette. Whether the
    /// question went up or the action went ahead.
    fn ask_in_palette(&mut self, request: crate::bound_dispatch::OrdinaryConfirmation) -> bool;

    /// Ask the person before `address` goes ahead: as the door asked, or,
    /// `confirming`, as the app asked for a pick of theirs
    /// ([`Origin::PersonConfirming`]). Whether a question went up or the
    /// action went ahead.
    fn ask_person(
        &mut self,
        target: LocalResourceTarget,
        address: ActionAddress,
        arguments: ControlContinuationArguments,
        confirming: bool,
    ) -> bool {
        let tree = self.tree();
        let request = crate::bound_dispatch::OrdinaryConfirmation::prepare(
            tree.root,
            &tree.state.doc,
            target,
            self.control_domain(),
            address,
            arguments,
        );
        match request {
            Ok(request) if confirming => self.ask_in_palette(request.confirming()),
            Ok(request) => self.ask_in_palette(request.asking_first()),
            Err(error) => {
                self.refuse_visibly(&error);
                false
            }
        }
    }

    /// Say that an invocation the person made did not run, and why, at the
    /// control they used.
    fn refuse_visibly(&mut self, error: &DoorError);

    /// An invocation nobody at this device asked for was refused; only its
    /// app hears of it.
    fn refused_unsaid(&mut self, _error: &DoorError) {}

    /// Enter the door `target` names with `request`.
    fn invoke_local_target(
        &mut self,
        target: &LocalResourceTarget,
        request: &ResourceInvocation<'_>,
    ) -> Started {
        match target {
            LocalResourceTarget::Component(target) => self.invoke_bound_control(
                target,
                &ResourceInvocation {
                    address: &target.address.address,
                    ..*request
                },
            ),
            LocalResourceTarget::Outer => self.invoke_outer_control(request),
        }
    }

    /// Run a control a History surface held back, as the person at this
    /// device, confirmed when they already confirmed it.
    fn invoke_held_control(
        &mut self,
        prepared: crate::history_mode::pending_controls::PreparedContinuation,
    ) -> Started {
        let argument = json_argument(&prepared.invocation.arguments);
        let actor = self.local_author().0.clone();
        self.invoke_local_target(
            &prepared.target,
            &ResourceInvocation::new(&prepared.invocation.address, &actor)
                .argument(Some(&argument))
                .confirmed(prepared.confirmed),
        )
    }

    fn open_ordinary_confirmation(
        &mut self,
        target: LocalResourceTarget,
        address: ActionAddress,
        arguments: ControlContinuationArguments,
    ) -> bool {
        self.ask_person(target, address, arguments, false)
    }

    /// Deliver the control intents the tree queued: `registered` ones the
    /// control domain drained, then those the components and the outer
    /// document hold. How many went ahead.
    fn dispatch_drained_controls(
        &mut self,
        registered: crate::control_custody::RegisteredControls,
    ) -> usize {
        let mut dispatched = 0usize;
        let crate::control_custody::RegisteredControls {
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
            // The window's own component's History control (the History
            // button in a framework app's header row) opens this window's
            // History, as the shortcut does; it is no app action.
            if on_behalf_of.is_none()
                && self
                    .tree()
                    .root
                    .callback_binding()
                    .is_some_and(|binding| binding.instance_id == target.binding.instance_id)
                && self.take_history_invocation(&intent, None)
            {
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            if self.dispatch_bound_control_invocation(&target, &intent, on_behalf_of, origin) {
                dispatched = dispatched.saturating_add(1);
            }
        }
        while let Some((intent, on_behalf_of, origin)) = outer
            .pop_front()
            .map(|intent| (intent, None, Origin::Person))
            .or_else(|| crate::bound_dispatch::take_attributed_with_origin(self.host_root_mut()))
        {
            if self.dispatch_outer_control_invocation(intent, on_behalf_of, origin) {
                dispatched = dispatched.saturating_add(1);
            }
        }
        dispatched
    }

    /// Deliver one control intent the outer document produced. Whether it
    /// went ahead.
    fn dispatch_outer_control_invocation(
        &mut self,
        intent: ControlInvocation,
        on_behalf_of: Option<InvocationId>,
        origin: Origin,
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
        // An edit an effect submitted is applied for the effect's invoker,
        // in its context, as long as the effect runs.
        let acting = on_behalf_of.and_then(|id| self.acting_for_mut().get(&id).cloned());
        let Some(confirmation) = self.local_confirmation(&intent) else {
            let error = DoorError::Unknown(intent.address.clone());
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
        };
        let needs_confirmation = !matches!(confirmation, tilo_actions::ConfirmationPolicy::None);
        if acting.is_some() && needs_confirmation {
            // Nobody here asked for it: the person at the device is never
            // prompted to confirm another invoker's edit.
            tracing::warn!(resource = %intent.address.resource, action = %intent.address.action, "an effect's edit that needs confirmation is refused");
            let error = DoorError::ConfirmationRequired;
            self.refused_unsaid(&error);
            self.outer_control_settled(&intent.address, on_behalf_of, &Err(error));
            return false;
        }
        let confirming = acting.is_none() && origin == Origin::PersonConfirming;
        if confirming || needs_confirmation {
            // A bound control never manufactures confirmation. Hand the
            // exact address to the standard palette flow, which owns the
            // explicit confirm/phrase UI and only then enters the door.
            crate::widget::controls::on_control_invocation_result(
                self.host_root_mut(),
                &intent.address,
                &Err(DoorError::ConfirmationRequired),
            );
            self.ask_person(
                LocalResourceTarget::Outer,
                intent.address,
                ControlContinuationArguments::Json(intent.arguments),
                confirming,
            );
            return true;
        }
        // With the workspace's broker at hand it admits the edit for the
        // invoker, with their own grants.
        if let (Some(acting), Some(running)) = (&acting, on_behalf_of)
            && self.broker_connected()
        {
            let result = self.send_follow_up(
                running,
                &intent.address,
                &intent.arguments,
                acting.context.clone(),
            );
            if let Err(error) = &result {
                self.refused_unsaid(error);
            }
            self.outer_control_settled(&intent.address, on_behalf_of, &result);
            return result.is_ok();
        }
        let json = json_argument(&intent.arguments);
        let actor = acting.as_ref().map_or_else(
            || self.local_author().0.clone(),
            |acting| acting.invoker.actor.clone(),
        );
        let scopes = acting
            .as_ref()
            .map(crate::invocation_scope::Continuation::enter);
        // An outer control was produced outside every component: it acts on the outer document it was captured with, even
        // when an owner declares the same address.
        let result = self.invoke_outer_control(
            &ResourceInvocation::new(&intent.address, &actor)
                .argument(Some(&json))
                .ambient_context(acting.as_ref().and_then(|acting| acting.context.as_ref())),
        );
        drop(scopes);
        self.outer_control_settled(&intent.address, on_behalf_of, &result);
        match result {
            Ok(_) => true,
            // The action needs no confirmation of its own, but the room's
            // grants ask the person here before it runs.
            Err(DoorError::ConfirmationRequired) if acting.is_none() => {
                self.open_ordinary_confirmation(
                    LocalResourceTarget::Outer,
                    intent.address,
                    ControlContinuationArguments::Json(intent.arguments),
                );
                true
            }
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
    fn outer_control_settled(
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

    /// Deliver one control intent a component produced. Whether it went
    /// ahead (or a question about it went up).
    fn dispatch_bound_control_invocation(
        &mut self,
        target: &BoundActionTarget,
        intent: &ControlInvocation,
        on_behalf_of: Option<InvocationId>,
        origin: Origin,
    ) -> bool {
        // A pick the app asks the person to confirm is asked before it
        // reaches the door, through the same question the door's own
        // confirmations get.
        if origin == Origin::PersonConfirming && on_behalf_of.is_none() {
            return self.ask_person(
                LocalResourceTarget::Component(target.clone()),
                intent.address.clone(),
                ControlContinuationArguments::Json(intent.arguments.clone()),
                true,
            );
        }
        let argument = json_argument(&intent.arguments);
        // An edit an effect submitted is applied for the effect's invoker,
        // in its context, as far as the invoker was admitted.
        let acting = on_behalf_of.and_then(|id| self.acting_for_mut().get(&id).cloned());
        // With the workspace's broker at hand it admits the edit for the
        // invoker, with their own grants, under the component's published
        // address.
        if let (Some(acting), Some(running)) = (&acting, on_behalf_of)
            && self.broker_connected()
        {
            let result = self
                .bound_publication()
                .alias(&target.binding, &target.address.address.resource)
                .map_err(crate::bound_dispatch::BoundActionError::into_door)
                .and_then(|alias| {
                    self.send_follow_up(
                        running,
                        &ActionAddress::new(alias, target.address.address.action.clone()),
                        &intent.arguments,
                        acting.context.clone(),
                    )
                });
            if let Err(error) = &result {
                self.refused_unsaid(error);
            }
            self.bound_control_settled(target, on_behalf_of, &result);
            return result.is_ok();
        }
        let actor = acting.as_ref().map_or_else(
            || self.local_author().0.clone(),
            |acting| acting.invoker.actor.clone(),
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
        if origin.is_persons() {
            crate::pick::note_invocation(&target.address, result.is_ok());
        }
        self.bound_control_settled(target, on_behalf_of, &result);
        match result {
            Ok(_) => true,
            // Nobody here asked for it: the person at the device is never
            // prompted to confirm another invoker's edit.
            Err(DoorError::ConfirmationRequired) if acting.is_none() => {
                self.open_ordinary_confirmation(
                    LocalResourceTarget::Component(target.clone()),
                    intent.address.clone(),
                    ControlContinuationArguments::Json(intent.arguments.clone()),
                );
                true
            }
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
    fn bound_control_settled(
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
}

/// `arguments` as the door's text argument.
pub(crate) fn json_argument(arguments: &serde_json::Map<String, serde_json::Value>) -> String {
    format!(
        "{}{}",
        tilo_actions::ARGUMENTS_JSON_PREFIX,
        serde_json::Value::Object(arguments.clone())
    )
}
