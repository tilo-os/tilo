//! What a framework app's host reads of an app shown by the driver: the record its window shows,
//! calls made within a caller's context (an agent's run), the jobs they start and how they end,
//! and the items a collection leaves past its page.

#[cfg(feature = "model")]
use tilo_actions::{ActionAddress, ApiError, InvocationId, InvocationUpdate};

use super::Driver;
#[cfg(feature = "model")]
use crate::widget::ResourceOperationStart;

impl Driver {
    #[cfg(feature = "model")]
    /// The record the hosted component shows now: the one it was launched on, or one it opened in
    /// that one's place.
    #[must_use]
    pub fn component_checkout(&self) -> Option<&crate::resource::DocumentCheckout> {
        self.app
            .root_widget
            .as_any()
            .and_then(|root| root.downcast_ref::<crate::widget::BoundAppWidget>())
            .map(|bound| bound.component.record())
    }

    #[cfg(feature = "model")]
    /// [`Self::invoke_action`] as the person at the surface within `ambient`, the context a
    /// caller's connection gives every invocation it makes (an agent's run, its purpose): what
    /// the action changes is recorded with it.
    ///
    /// # Errors
    /// As [`Self::invoke_action`].
    pub fn invoke_action_within(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
        ambient: &tilo_actions::ActionContext,
    ) -> Result<ResourceOperationStart, ApiError> {
        let actor = tilo_actions::Actor::Member(self.app.local_author.clone());
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(arguments.clone())
        );
        let started = self.app.invoke_resource_action(
            crate::app_core::ResourceInvocation::new(address, &actor)
                .argument(Some(&argument))
                .approval(approval)
                .ambient_context(Some(ambient)),
        );
        self.run_frame();
        started
    }

    #[cfg(feature = "model")]
    /// [`Self::start_bound_job_for`] within `ambient` for the action the hosted component
    /// `address.instance_id` publishes: the task the invoker carries out (an agent's run, its
    /// purpose), recorded with every edit the job makes. The job runs under the invocation
    /// `ambient` names, the one its host admitted the call under; a fresh one when it names none.
    ///
    /// # Errors
    /// As [`Self::start_bound_job_for`].
    ///
    /// # Panics
    /// When the driver's peer is not connected ([`Self::connect_peer`]).
    pub fn start_bound_job_within(
        &mut self,
        invoker: Option<tilo_actions::ActionInvoker>,
        ambient: Option<&tilo_actions::ActionContext>,
        address: &crate::bound_dispatch::BoundActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
    ) -> Result<(InvocationId, ResourceOperationStart), ApiError> {
        self.start_job_within(
            invoker,
            ambient,
            &address.address,
            arguments,
            approval,
            Some(address.instance_id),
        )
    }

    #[cfg(feature = "model")]
    /// [`Self::start_bound_job_within`] or [`Self::start_bound_job_for`]: `instance` scopes the
    /// resources the call is admitted against to the component that publishes them, when one does.
    #[allow(
        clippy::too_many_lines,
        reason = "the admission, the delivered call and its answer are one host call"
    )]
    pub(super) fn start_job_within(
        &mut self,
        invoker: Option<tilo_actions::ActionInvoker>,
        ambient: Option<&tilo_actions::ActionContext>,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
        instance: Option<crate::application::InstanceId>,
    ) -> Result<(InvocationId, ResourceOperationStart), ApiError> {
        let resources = match instance {
            Some(instance) => {
                let scoped = self
                    .bound_resources_of(Some(instance), Some(&address.resource))
                    .unwrap_or_default();
                if scoped.is_empty() {
                    self.resources()
                } else {
                    scoped
                }
            }
            None => self.resources(),
        };
        let spec = tilo_actions::api::resolve(&resources, address);
        let policy = spec.map(|spec| spec.confirmation_for_json(arguments));
        // A call that asks first and comes without the person's approve is refused for it before
        // anything else is said of it; the approve answers whatever the action asks, as the
        // broker takes the call sent again with it.
        let confirmation = match (policy, approval) {
            (None | Some(tilo_actions::ConfirmationPolicy::None), _) => None,
            (Some(tilo_actions::ConfirmationPolicy::Confirm), Some(_)) => {
                Some(tilo_actions::remote::Confirmation::Confirmed)
            }
            (Some(tilo_actions::ConfirmationPolicy::Phrase { phrase }), Some(_)) => {
                Some(tilo_actions::remote::Confirmation::Phrase(phrase))
            }
            (Some(_), None) => return Err(ApiError::ConfirmationRequired),
        };
        let (actor, context) = match invoker {
            Some(invoker) => (
                invoker.acting.by.clone(),
                Some(ambient.cloned().unwrap_or_default().with_invoker(invoker)),
            ),
            None => (
                tilo_actions::Actor::Member(self.app.local_author.clone()),
                ambient.cloned(),
            ),
        };
        // The handler reads the person's yes from the call it runs, as the host that admitted
        // it sets it.
        let context = match approval {
            Some(approval) => {
                let mut context = context.unwrap_or_default();
                context.approval = Some(approval.clone());
                Some(context)
            }
            None => context,
        };
        // An immediate document action is the app's own, as when its person makes it: it runs
        // where the component publishes it, resolved and judged there, and answers once its change
        // is made. A call of the app's own person never reaches the broker for one, and one the
        // view's member makes runs under the permissions the view was opened with. Only a call the
        // workspace carries — an effect, a job, or an action another workspace decides — is
        // delivered, and answers once the window showed what it caused.
        let runs_here = spec.is_some_and(|spec| {
            spec.kind == tilo_actions::OperationKind::Document
                && spec.contract.execution == tilo_actions::ExecutionMode::Immediate
        }) && actor.member_id() == Some(&self.app.local_author)
            && self.component_instance_id().is_some()
            && instance.is_none_or(|instance| self.component_instance_id() == Some(instance));
        if runs_here {
            // What the action says of itself as it stands still decides it: a call the app's own
            // person makes is refused while it is unavailable, and one their grant withholds is
            // left to the verdict that names the permission.
            if let Some(tilo_actions::Availability::Unavailable { code, reason }) =
                spec.map(|spec| &spec.contract.availability)
                && code != tilo_actions::Availability::NOT_ALLOWED
            {
                return Err(ApiError::Unavailable {
                    code: code.clone(),
                    reason: reason.clone(),
                });
            }
            let invocation = ambient
                .and_then(|ambient| ambient.source_invocation)
                .unwrap_or_default();
            let started = self.start_own_call(
                address,
                arguments,
                approval,
                &actor,
                context.as_ref(),
                invocation,
            )?;
            return Ok((invocation, started));
        }
        let admitted =
            tilo_actions::api::admit(&resources, address, arguments, confirmation.as_ref());
        // An action the window's member may not take is left out of what it publishes, or listed
        // to it as not allowed; either way the caller is told their permissions refuse it, saying
        // why, as the workspace's API says it, and an action nothing publishes stays unknown.
        if let Err(refusal) = &admitted
            && (matches!(refusal, ApiError::Unknown(_))
                || matches!(
                    refusal,
                    ApiError::Unavailable { code, .. }
                        if code == tilo_actions::Availability::NOT_ALLOWED
                ))
            && let Some(instance) = instance.or_else(|| self.component_instance_id())
        {
            return Err(crate::bound_dispatch::unoffered(
                self.app.root_widget.as_ref(),
                &self.app.state.doc,
                address,
                instance,
            ));
        }
        admitted?;
        // The broker answers a call of an action published to run on (a job, a stream) as
        // accepted once it admitted it: whatever the app answers after is the job's end, however
        // soon it comes.
        let job = tilo_actions::api::resolve(&resources, address).is_some_and(|spec| {
            !matches!(
                spec.contract.execution,
                tilo_actions::ExecutionMode::Immediate
            )
        });
        let invocation = self.deliver_call(
            &address.resource,
            &address.action,
            &serde_json::Value::Object(arguments.clone()),
            actor,
            context,
        );
        if job {
            // A call that ended the moment it was delivered started no job: its caller hears
            // what refused it now, as one refused before it started does, rather than a progress
            // that lasts no longer than the call.
            match self.job_end(invocation) {
                Some(InvocationUpdate::Failed { error, .. }) if error.code == "unknown_action" => {
                    // The app could not resolve what its own window listed: what its permissions
                    // keep from the caller answers as that permission, not as no such action.
                    let instance = instance.or_else(|| self.component_instance_id());
                    return Err(instance.map_or_else(
                        || super::jobs::failure_refusal(address, error),
                        |instance| self.unoffered(address, instance),
                    ));
                }
                Some(InvocationUpdate::Failed { error, .. }) => {
                    return Err(super::jobs::failure_refusal(address, error));
                }
                Some(InvocationUpdate::Cancelled { message, .. }) => {
                    return Err(ApiError::Failed(
                        message.unwrap_or_else(|| "cancelled".to_owned()),
                    ));
                }
                _ => return Ok((invocation, ResourceOperationStart::Accepted)),
            }
        }
        let started = match self.job_end(invocation) {
            Some(InvocationUpdate::Succeeded { output, .. }) => {
                ResourceOperationStart::Completed(output)
            }
            Some(InvocationUpdate::Failed { error, .. }) if error.code == "unknown_action" => {
                let instance = instance.or_else(|| self.component_instance_id());
                return Err(instance.map_or_else(
                    || super::jobs::failure_refusal(address, error),
                    |instance| self.unoffered(address, instance),
                ));
            }
            Some(InvocationUpdate::Failed { error, .. }) => {
                return Err(super::jobs::failure_refusal(address, error));
            }
            Some(InvocationUpdate::Cancelled { message, .. }) => {
                return Err(ApiError::Failed(
                    message.unwrap_or_else(|| "cancelled".to_owned()),
                ));
            }
            _ => ResourceOperationStart::Accepted,
        };
        Ok((invocation, started))
    }

    #[cfg(feature = "model")]
    /// Run `address`, an immediate document action the app's own person makes, where the app
    /// resolves and judges it, as its own press runs there: then the frame that shows what it
    /// changed is the one whoever reads the app runs.
    fn start_own_call(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
        actor: &tilo_actions::Actor,
        context: Option<&tilo_actions::ActionContext>,
        invocation: InvocationId,
    ) -> Result<ResourceOperationStart, ApiError> {
        let argument = crate::app_core::json_argument(arguments);
        let invoke = |driver: &mut Self| {
            driver.app.invoke_resource_action(
                crate::app_core::ResourceInvocation::new(address, actor)
                    .argument(Some(&argument))
                    .approval(approval)
                    .invocation(Some(invocation))
                    .ambient_context(context),
            )
        };
        // A view operation that reads the laid-out view asks for that frame first, as the app's
        // own API answers: the call runs again on the view it then stands on.
        let started = crate::widget::resource_operation::laying_out_first(|| invoke(self));
        match started {
            Err(error) if crate::widget::resource_operation::asks_to_lay_out_first(&error) => {
                self.relayout();
                invoke(self)
            }
            other => other,
        }
    }

    #[cfg(feature = "model")]
    /// What a call of `address` at `instance` is told when the app could not resolve it: the
    /// permission its publication keeps it from the caller, or that nothing publishes it.
    fn unoffered(
        &self,
        address: &ActionAddress,
        instance: crate::application::InstanceId,
    ) -> ApiError {
        crate::bound_dispatch::unoffered(
            self.app.root_widget.as_ref(),
            &self.app.state.doc,
            address,
            instance,
        )
    }

    #[cfg(feature = "model")]
    /// Run frames as an idle window does until the job `invocation` ended or `timeout` passed on
    /// the current [`crate::background::Background`]'s clock: what it ended as, `None` while it
    /// still runs.
    pub fn await_job(
        &mut self,
        invocation: InvocationId,
        timeout: crate::time::Duration,
    ) -> Option<InvocationUpdate> {
        self.wait_until(timeout, |driver| driver.job_end(invocation).is_some());
        self.job_end(invocation)
    }

    #[cfg(feature = "model")]
    /// What the job `invocation` ended as, if it has.
    fn job_end(&self, invocation: InvocationId) -> Option<InvocationUpdate> {
        self.call_updates(invocation)
            .into_iter()
            .find(InvocationUpdate::is_terminal)
    }

    /// Take up everything the app's widgets read, also what was written where no pass would take
    /// it up until something else asked for one (local state a test or a host set from outside):
    /// the model is synchronized now, as a hosted worker's pump synchronizes it when told to, and
    /// the frame that shows it follows. The sync does not advance the widgets' update — the frame's
    /// own pump does that, once — so a frame nothing else moved stays quiet. What a capture, an
    /// audit or a settled test reads is then never a stale snapshot.
    pub fn refresh_model(&mut self) {
        let _ = self.app.sync_model(true);
        self.run_frame();
    }

    /// The items the app leaves out past each collection's published page, by id and label, read
    /// without building them ([`crate::widget::past_page_labels`]).
    #[must_use]
    pub fn past_page_labels(&self) -> Vec<(String, String)> {
        crate::widget::past_page_labels(self.app.root_widget.as_ref(), &self.app.state.doc)
    }

    /// The items the app leaves out past each collection's published page, every one built
    /// ([`crate::widget::resources_past_page`]).
    #[must_use]
    pub fn resources_past_page(&self) -> Vec<tilo_actions::ResourceSpec> {
        crate::widget::resources_past_page(self.app.root_widget.as_ref(), &self.app.state.doc)
    }

    #[cfg(feature = "model")]
    /// The hosted component instance that owns the widget named `name`: whose bound actions a
    /// press on it invokes.
    #[must_use]
    pub fn semantic_owner_instance(&self, name: &str) -> Option<crate::application::InstanceId> {
        let node = *self.named_target_nodes().get(name)?;
        self.app
            .bindings
            .binding_for(node)
            .map(|binding| binding.instance_id)
    }
}
