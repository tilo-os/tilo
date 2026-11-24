//! Registered workspace services share original-ownership host admission.
use super::host_action_admission::{AdmittedHostAction, HostActionAdmission};
use super::{AppBrokerState, BrokerActor, InvocationCaller};
use std::sync::Arc;
use tilo_actions::remote::{ActionCallError, Caller, Confirmation, InvokeRequest};
use tilo_actions::{ActionDescriptor, ActionFailure, InvocationUpdate};
use tilo_desktop_model::Workspace;

/// The actor a step from an agent's own connection acts as, when the step is a step of a turn
/// the agent answers: the connection's admitted agent, in the turn's agent session, on the turn.
///
/// A turn's steps are requests on the agent's connection served as the turn's: the caller carries
/// the turn and the turn's session (`Caller::on_roster_turn(..).in_agent_session(..)`), which
/// `agent_session_of` checks goes, while the actor the admitted connection makes carries neither,
/// since the connection itself answers no turn. Every step of the turn then acted outside it: a
/// session's own step (the API's programs, `task_programs`) refused it as "an agent runs programs
/// as a step of its session", what the turn changed was not caused by its session, and what a
/// step opened or placed stood at the canvas' origin rather than beside the turn that asked,
/// since the session stands it beside the turn its actor is on. Only the same agent's turn is
/// taken, and a session or turn the connection's actor has of its own is kept.
fn in_turn_session(actor: BrokerActor, caller: &Caller) -> BrokerActor {
    let acting = caller.acting();
    let same_agent = actor.is_agent()
        && acting.agent_id().is_some()
        && actor.member().agent_id() == acting.agent_id();
    if !same_agent {
        return actor;
    }
    let actor = match (&acting.agent_session, actor.session()) {
        (Some(session), None) => actor.in_session(session.as_str()),
        _ => actor,
    };
    match caller.turn() {
        Some(turn) if actor.turn().is_none() => actor.on_turn(Some(turn.clone())),
        _ => actor,
    }
}

#[cfg(test)]
mod turn_session_tests {
    use super::{BrokerActor, Caller, in_turn_session};

    /// The person whose message a roster agent's turn answers, and the key its certificate
    /// chains to.
    const PERSON: [u8; 32] = [0x11; 32];

    /// An agent `agent` bound to its roster entry, answering `turn` for [`PERSON`].
    fn on_turn(agent: &str, turn: tilo_action_policy::turn::Turn) -> Caller {
        Caller::delegated_agent(agent, PERSON, [0x12; 32])
            .on_roster(0, PERSON)
            .on_roster_turn(turn)
    }

    /// A step from the agent's connection, for the turn it answers, acts in that turn's
    /// session and on that turn; a step that is no turn's, or another agent's, does not.
    #[test]
    fn a_turns_step_acts_in_the_turns_session() {
        let turn = tilo_action_policy::turn::Turn::new("chat.workspace/channels/c", "m1");
        let caller = on_turn("scribe", turn.clone())
            .in_workspace("w")
            .in_agent_session("so.tilo.session/agent-session/s1");
        let connection = BrokerActor::agent("scribe").for_person("person");
        let acting = in_turn_session(connection.clone(), &caller);
        assert_eq!(acting.session(), Some("so.tilo.session/agent-session/s1"));
        assert_eq!(acting.member().agent_id(), Some("scribe"));
        // On the turn it answers: the session stands what the step opens beside that turn.
        assert_eq!(acting.turn(), Some(&turn));

        let other = BrokerActor::agent("other").for_person("person");
        let other = in_turn_session(other, &caller);
        assert_eq!(other.session(), None);
        assert_eq!(
            other.turn(),
            None,
            "another agent's step is not on this turn"
        );

        let no_session = on_turn("scribe", turn).in_workspace("w");
        assert_eq!(in_turn_session(connection, &no_session).session(), None);
    }
}

#[cfg(test)]
#[path = "remaining_workspace_actions_tests.rs"]
mod tests;

// Legacy governance validates resource names and confirmation at the service.
// Still bind the exact requested address into durable host admission, without
// turning a catalogue resource or its confirmation into a new admission floor.
fn governance_descriptor(
    service: &dyn crate::apps::services::WorkspaceGovernanceServices,
    workspace: &Workspace,
    address: &tilo_actions::ActionAddress,
) -> Option<ActionDescriptor> {
    let owner_action = service.requires_descriptor_admission(&address.action);
    let descriptors = service.descriptors(workspace);
    let mut descriptor = descriptors
        .iter()
        .find(|descriptor| descriptor.address == *address)
        .or_else(|| {
            if owner_action {
                None
            } else {
                descriptors
                    .iter()
                    .find(|descriptor| descriptor.address.action == address.action)
            }
        })?
        .clone();
    descriptor.address = address.clone();
    Some(descriptor)
}

impl AppBrokerState {
    /// Resolve the remaining registered framework services before their legacy
    /// dispatcher. `None` means this is not one of these services, not a retry
    /// after a refusal. The caller's exact request supplies UUID and context.
    pub(super) fn start_remaining_workspace_action(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        authority: (&Workspace, &BrokerActor, u64),
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Option<Result<u64, ActionCallError>> {
        let address = &request.target.address;
        if address.resource == crate::apps::automation::LIBRARY_RESOURCE
            && self.services.shortcut_library.is_some()
        {
            return Some(
                self.admitted_shortcut_library(caller, origin, authority, request, argument),
            );
        }
        if let Some(service) = self.services.keyboard_shortcuts.as_ref()
            && service.is_resource(&address.resource)
        {
            // A signed remote device is not physical intent on this keyboard.
            return Some(if caller.device_public_key().is_some() {
                Err(ActionCallError::NotPermitted)
            } else {
                self.admitted_keyboard_shortcut(caller, origin, authority, request, argument)
            });
        }
        if self
            .services
            .workspace_governance
            .as_ref()
            .is_some_and(|service| service.handles(&address.action))
        {
            return Some(
                self.admitted_workspace_governance(caller, origin, authority, request, argument),
            );
        }
        None
    }

    fn admit_framework_descriptor(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        authority: (&Workspace, &BrokerActor, u64),
        action: (&str, &ActionDescriptor),
        input: (&InvokeRequest, Option<&str>),
    ) -> Result<(AdmittedHostAction, bool), ActionCallError> {
        let approved = self.authorize_framework_descriptor(authority, action, input)?;
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                workspace: &authority.0.id().to_string(),
                app_id: action.0,
                descriptor: action.1,
                request: input.0,
            },
        )?;
        #[cfg(test)]
        tests::at_effect_barrier();
        Ok((admitted, approved))
    }

    fn authorize_framework_descriptor(
        &self,
        authority: (&Workspace, &BrokerActor, u64),
        action: (&str, &ActionDescriptor),
        input: (&InvokeRequest, Option<&str>),
    ) -> Result<bool, ActionCallError> {
        let (_, actor, _) = authority;
        let (request, argument) = input;
        let approved = super::approval::approved_by_person(request.confirmation.as_ref());
        if !Self::confirmed_as_asked(
            actor,
            action.1,
            argument,
            request.confirmation.as_ref(),
            approved,
        ) {
            return Err(ActionCallError::confirmation_required());
        }
        self.framework_policy(
            authority,
            action,
            request.confirmation.as_ref(),
            approved,
            false,
        )?;
        Ok(approved)
    }

    // Admission may synchronously cross disk and owner-service barriers. Never
    // reuse the workspace or certificate ceiling sampled before those calls.
    pub(super) fn current_framework_authority(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        workspace: &str,
    ) -> Result<(Workspace, BrokerActor, u64), ActionCallError> {
        let unavailable = |reason| ActionCallError::Unavailable { reason };
        if !self.is_accepting() || (caller.execution_id().is_some() && !caller.execution_is_live())
        {
            return Err(ActionCallError::NotPermitted);
        }
        let original = match origin {
            Some(InvocationCaller::Certified {
                caller: original,
                foreign_restriction,
            }) => {
                self.checked_policy_time(original.policy_time.as_ref())
                    .map_err(unavailable)?;
                let current = self.live_action_caller(original).map_err(unavailable)?;
                let certificate = current
                    .certificate
                    .as_ref()
                    .ok_or(ActionCallError::NotPermitted)?;
                if original.workspace != workspace {
                    return Err(ActionCallError::NotPermitted);
                }
                if caller.device_public_key().is_none() {
                    self.current_native_person(workspace, certificate)
                        .map_err(unavailable)?;
                }
                let mut actor = current.broker_actor();
                actor.capped |= super::cap_applies_in(
                    self.certificate_authority.get().map(AsRef::as_ref),
                    workspace,
                    *foreign_restriction,
                    Some(&certificate.device_public_key),
                );
                Some(actor)
            }
            Some(InvocationCaller::Local {
                route,
                admitted,
                time,
                ..
            }) => {
                self.checked_policy_time(time.as_ref())
                    .map_err(unavailable)?;
                if route.cluster_id != workspace {
                    return Err(ActionCallError::NotPermitted);
                }
                if let Some(registration) = &admitted.worker_registration {
                    let workers = self
                        .host_workers
                        .lock()
                        .map_err(|_| unavailable("worker registry unavailable".into()))?;
                    if !workers
                        .values()
                        .any(|worker| worker.worker_registration.as_ref() == Some(registration))
                    {
                        return Err(ActionCallError::NotPermitted);
                    }
                }
                let current = self
                    .revalidate_admission(route, admitted)
                    .map_err(|error| unavailable(error.to_string()))?;
                self.checked_policy_time(time.as_ref())
                    .map_err(unavailable)?;
                Some(in_turn_session(current.broker_actor(), caller))
            }
            Some(InvocationCaller::NativePerson {
                workspace: original,
                certificate,
                time,
                ..
            }) => {
                if original != workspace {
                    return Err(ActionCallError::NotPermitted);
                }
                self.checked_policy_time(Some(time)).map_err(unavailable)?;
                self.current_native_person(workspace, certificate)
                    .map_err(unavailable)?;
                Some(self.certified_person(workspace, certificate))
            }
            None => None,
        };
        let (workspace, actor, now) = self
            .decide_for(caller, workspace, |workspace, actor, now| {
                (workspace.clone(), actor.clone(), now)
            })
            .ok_or(ActionCallError::NotPermitted)?;
        Ok((workspace, original.unwrap_or(actor), now))
    }

    pub(super) fn framework_policy(
        &self,
        authority: (&Workspace, &BrokerActor, u64),
        action: (&str, &ActionDescriptor),
        confirmation: Option<&Confirmation>,
        approved: bool,
        spend: bool,
    ) -> Result<(), ActionCallError> {
        let (workspace, actor, now) = authority;
        let (answer, charged) = actor.action_verdict_in(
            workspace,
            action.0,
            (
                &action.1.resource_kind,
                &action.1.address.resource,
                &action.1.address.action,
            ),
            &action.1.contract.need(),
            now,
        );
        match answer {
            tilo_action_policy::Answer::Deny => return Err(ActionCallError::NotPermitted),
            tilo_action_policy::Answer::Ask
                if !(approved
                    || matches!(confirmation, Some(Confirmation::Asked(_)))
                    || (actor.direct() && confirmation == Some(&Confirmation::Confirmed))) =>
            {
                return Err(ActionCallError::confirmation_required());
            }
            _ => {}
        }
        if spend {
            self.spend_once_grants(&workspace.id().to_string(), &charged);
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered admission of a shortcut library operation: descriptor, approval, decode, catalogue and run"
    )]
    fn admitted_shortcut_library(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        authority: (&Workspace, &BrokerActor, u64),
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionCallError> {
        let library = self
            .services
            .shortcut_library
            .as_ref()
            .ok_or(ActionCallError::UnknownAction)?;
        let descriptor =
            crate::apps::automation::library_descriptor(&request.target.address.action)
                .ok_or(ActionCallError::UnknownAction)?;
        let approved = self.authorize_framework_descriptor(
            authority,
            (tilo_automation::SHORTCUTS_APP_ID, &descriptor),
            (request, argument),
        )?;
        let operation =
            crate::apps::automation::decode_library(&request.target.address.action, argument)
                .map_err(|reason| ActionCallError::InvalidRequest { reason })?;
        let catalog = tilo_automation::ActionCatalog::new(
            self.eager_catalogue(caller, super::action_provider::RowScope::All)
                .0,
        );
        let keymap = library.keymap();
        let _plan = crate::apps::automation::plan_library(
            authority.0,
            operation,
            &crate::apps::automation::SaveContext {
                catalog: &catalog,
                saver: authority.1.member(),
                keymap: &keymap,
                fixed_label: &|fixed| library.fixed_shortcut_label(fixed),
            },
        )
        .map_err(|reason| ActionCallError::Refused { reason })?;
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                workspace: &authority.0.id().to_string(),
                app_id: tilo_automation::SHORTCUTS_APP_ID,
                descriptor: &descriptor,
                request,
            },
        )?;
        #[cfg(test)]
        tests::at_effect_barrier();
        let workspace_id = authority.0.id().to_string();
        let (plan, actor) = self.framework_effect_ready(&admitted, || {
            let (workspace, actor, _) =
                self.current_framework_authority(caller, origin, &workspace_id)?;
            // Replan on the current document: a valid old plan can overwrite a
            // concurrent shortcut edit even when the caller still has permission.
            let operation =
                crate::apps::automation::decode_library(&request.target.address.action, argument)
                    .map_err(|reason| ActionCallError::InvalidRequest { reason })?;
            let catalog = tilo_automation::ActionCatalog::new(
                self.eager_catalogue(caller, super::action_provider::RowScope::All)
                    .0,
            );
            let keymap = library.keymap();
            let plan = crate::apps::automation::plan_library(
                &workspace,
                operation,
                &crate::apps::automation::SaveContext {
                    catalog: &catalog,
                    saver: actor.member(),
                    keymap: &keymap,
                    fixed_label: &|fixed| library.fixed_shortcut_label(fixed),
                },
            )
            .map_err(|reason| ActionCallError::Refused { reason })?;
            let (current_workspace, actor, now) =
                self.current_framework_authority(caller, origin, &workspace_id)?;
            if serde_json::to_value(&workspace).ok()
                != serde_json::to_value(&current_workspace).ok()
            {
                return Err(ActionCallError::Refused {
                    reason: "shortcut library changed while planning; retry with a new invocation"
                        .into(),
                });
            }
            self.framework_policy(
                (&current_workspace, &actor, now),
                (tilo_automation::SHORTCUTS_APP_ID, &descriptor),
                request.confirmation.as_ref(),
                approved,
                true,
            )?;
            Ok((plan, actor))
        })?;
        let (change, output) = match plan {
            crate::apps::automation::LibraryPlan::Answer(output) => {
                return self.finish_framework_action(admitted, Ok(output));
            }
            crate::apps::automation::LibraryPlan::Stop(stop) => {
                let stopped = self.ended_invocations.open.running.stop(
                    &workspace_id,
                    &stop,
                    "asked for from the shortcuts library",
                );
                return self.finish_framework_action(
                    admitted,
                    Ok(crate::apps::automation::stopped_output(&stopped)),
                );
            }
            crate::apps::automation::LibraryPlan::Count { id, every } => {
                let counted = self
                    .membership
                    .as_ref()
                    .map(|membership| {
                        membership
                            .done_under(&workspace_id, &crate::apps::automation::firings_of(&id))
                    })
                    .unwrap_or_default()
                    .iter()
                    .filter(|answer| {
                        answer
                            .get("counted")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                    })
                    .count();
                return self.finish_framework_action(
                    admitted,
                    Ok(crate::apps::automation::count_output(counted, every)),
                );
            }
            crate::apps::automation::LibraryPlan::Change { change, output } => (change, output),
        };
        let failed = |message: String| {
            Box::new(ActionFailure {
                code: "shortcut_library_failed".into(),
                message,
                details: None,
                retryable: false,
            })
        };
        // The change commits at the workspace's governor, which may be another
        // device's host: awaited on the library's runtime, and the invocation
        // finishes when it does.
        let runtime = match library.runtime() {
            Ok(runtime) => runtime,
            Err(message) => return self.finish_framework_action(admitted, Err(failed(message))),
        };
        let stored = library.store(crate::apps::services::ShortcutLibraryRequest {
            workspace: workspace_id,
            actor: actor.member().clone(),
            acting_for: caller.acting_for_person().map(str::to_owned),
            change,
            context: request
                .context
                .clone()
                .unwrap_or_else(|| crate::apps::automation::library_context(&descriptor)),
            invocation: admitted.invocation,
        });
        let id = admitted.id;
        let broker = Arc::clone(self);
        runtime.spawn(async move {
            let result = stored.await.map(|()| output).map_err(failed);
            if let Err(error) = broker.finish_framework_action(admitted, result) {
                tracing::warn!(?error, "a shortcut change finished after its invocation");
            }
        });
        Ok(id)
    }

    fn admitted_keyboard_shortcut(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        authority: (&Workspace, &BrokerActor, u64),
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionCallError> {
        let service = self
            .services
            .keyboard_shortcuts
            .as_ref()
            .ok_or(ActionCallError::UnknownAction)?;
        let mut keymap = settings_store::hardware::ShortcutConfig::load();
        let descriptor = service
            .descriptor(&keymap, &request.target.address)
            .ok_or(ActionCallError::UnknownAction)?;
        let (admitted, approved) = self.admit_framework_descriptor(
            caller,
            origin,
            authority,
            (service.app_id(), &descriptor),
            (request, argument),
        )?;
        self.framework_effect_ready(&admitted, || {
            keymap = settings_store::hardware::ShortcutConfig::load();
            let fresh = service
                .descriptor(&keymap, &request.target.address)
                .ok_or(ActionCallError::UnknownAction)?;
            if serde_json::to_value(&fresh).ok() != serde_json::to_value(&descriptor).ok() {
                return Err(ActionCallError::NotPermitted);
            }
            let (workspace, actor, now) =
                self.current_framework_authority(caller, origin, &authority.0.id().to_string())?;
            self.framework_policy(
                (&workspace, &actor, now),
                (service.app_id(), &fresh),
                request.confirmation.as_ref(),
                approved,
                true,
            )?;
            Ok(())
        })?;
        let result = service.apply(&keymap, &request.target.address, argument);
        self.finish_framework_action(admitted, result)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered admission of a governance action: descriptor, owner confirmation floor, approval and run"
    )]
    fn admitted_workspace_governance(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        authority: (&Workspace, &BrokerActor, u64),
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionCallError> {
        let service = self
            .services
            .workspace_governance
            .as_ref()
            .ok_or(ActionCallError::UnknownAction)?;
        let owner_action = service.requires_descriptor_admission(&request.target.address.action);
        let descriptor =
            governance_descriptor(service.as_ref(), authority.0, &request.target.address)
                .ok_or(ActionCallError::UnknownAction)?;
        // Owner actions retain their explicit direct-person confirmation floor.
        if owner_action && !authority.1.direct() {
            return Err(ActionCallError::NotPermitted);
        }
        let approved =
            owner_action && super::approval::approved_by_person(request.confirmation.as_ref());
        if owner_action && !approved && request.confirmation != Some(Confirmation::Confirmed) {
            return Err(ActionCallError::confirmation_required());
        }
        // Convert only the exact consumed owner approval; ordinary governance
        // retains its legacy phrase/confirmation semantics at the service.
        let mut checked = request.clone();
        if approved {
            checked.confirmation = Some(Confirmation::Confirmed);
        }
        // Admission records the original approval-bearing request, not a rewrite.
        self.framework_policy(
            authority,
            (service.app_id(), &descriptor),
            checked.confirmation.as_ref(),
            approved,
            false,
        )?;
        if owner_action
            && !Self::confirmed_as_asked(
                authority.1,
                &descriptor,
                argument,
                checked.confirmation.as_ref(),
                approved,
            )
        {
            return Err(ActionCallError::confirmation_required());
        }
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                workspace: &authority.0.id().to_string(),
                app_id: service.app_id(),
                descriptor: &descriptor,
                request,
            },
        )?;
        #[cfg(test)]
        tests::at_effect_barrier();
        let confirmation = match checked.confirmation.as_ref() {
            Some(Confirmation::Confirmed) => serde_json::json!({"confirm": true}),
            Some(Confirmation::Phrase(phrase)) => {
                serde_json::json!({"confirmation_phrase": phrase})
            }
            _ => serde_json::json!({}),
        };
        let workspace_id = authority.0.id().to_string();
        let actor = self.framework_effect_ready(&admitted, || {
            let (workspace, _, _) =
                self.current_framework_authority(caller, origin, &workspace_id)?;
            let fresh = governance_descriptor(service.as_ref(), &workspace, &descriptor.address)
                .ok_or(ActionCallError::UnknownAction)?;
            let (current_workspace, actor, now) =
                self.current_framework_authority(caller, origin, &workspace_id)?;
            if serde_json::to_value(&fresh).ok() != serde_json::to_value(&descriptor).ok()
                || serde_json::to_value(&workspace).ok()
                    != serde_json::to_value(&current_workspace).ok()
                || (owner_action && !actor.direct())
            {
                return Err(ActionCallError::NotPermitted);
            }
            self.framework_policy(
                (&current_workspace, &actor, now),
                (service.app_id(), &fresh),
                checked.confirmation.as_ref(),
                approved,
                true,
            )?;
            Ok(actor)
        })?;
        let started = service.start(&crate::apps::services::WorkspaceGovernanceRequest {
            local_device: (caller.device_public_key().is_none()
                || matches!(&admitted.owner,
                    super::InvocationOwner::Certified { device, .. }
                    | super::InvocationOwner::AuthenticatedLocal { device, agent: None, .. }
                    if self.services.native_certificates.as_ref().is_some_and(|authority|
                        uuid::Uuid::parse_str(&workspace_id).is_ok_and(|workspace|
                            authority.governs(workspace, Some(device))))))
                && !caller.acting().is_automation()
                && caller.acting_for_person().is_none()
                && request.on_behalf_of.is_none(),
            execution_lease: caller.execution_lease(),
            workspace: &workspace_id,
            actor: &actor,
            resource: &request.target.address.resource,
            action: &request.target.address.action,
            argument,
            confirmation: &confirmation,
            invocation: admitted.invocation,
        });
        let id = admitted.id;
        match started {
            Ok(started) => {
                let state = Arc::clone(self);
                started.then(move |outcome| {
                    let completed = if let Some(result) = outcome {
                        state.finish_framework_action(admitted, result)
                    } else {
                        let update = InvocationUpdate::Failed { invocation: admitted.invocation,
                            error: ActionFailure {
                                code: "host_step_stopped".into(), message: "the host step stopped without an answer".into(),
                                details: None, retryable: false,
                            } };
                        state.complete_uncertain_host_action(&admitted, update).map(|()| admitted.id)
                    };
                    if let Err(error) = completed {
                        tracing::warn!(%error, "workspace service completion could not be retained");
                    }
                });
            }
            Err(message) => {
                self.finish_framework_action(
                    admitted,
                    Err(Box::new(ActionFailure {
                        code: "workspace_governance_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    })),
                )?;
            }
        }
        Ok(id)
    }

    pub(super) fn framework_effect_ready<T>(
        &self,
        admitted: &AdmittedHostAction,
        check: impl FnOnce() -> Result<T, ActionCallError>,
    ) -> Result<T, ActionCallError> {
        check().inspect_err(|error| {
            let update = InvocationUpdate::Failed {
                invocation: admitted.invocation,
                error: ActionFailure {
                    code: "host_action_refused".into(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                },
            };
            if let Err(error) = self.complete_host_action(admitted, update) {
                tracing::warn!(%error, "host action refusal could not be retained");
            }
        })
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the dispatch permit is spent here: taken by value, a caller cannot complete it twice"
    )]
    fn finish_framework_action(
        &self,
        admitted: AdmittedHostAction,
        result: crate::apps::services::WorkspaceGovernanceOutcome,
    ) -> Result<u64, ActionCallError> {
        let invocation = admitted.invocation;
        let update = match result {
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => InvocationUpdate::Failed {
                invocation,
                error: *error,
            },
        };
        self.complete_host_action(&admitted, update)?;
        Ok(admitted.id)
    }
}
