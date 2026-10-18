//! Registered workspace services share original-custody host admission.
use super::host_action_admission::{AdmittedHostAction, HostActionAdmission};
use super::{BrokerActor, CapnpBrokerState, InvocationCaller};
use std::sync::Arc;
use tilo_actions::remote::{ActionHostError, Caller, Confirmation, InvokeRequest};
use tilo_actions::{ActionDescriptor, ActionFailure, InvocationUpdate};
use tilo_desktop_model::Workspace;

#[cfg(test)]
#[path = "remaining_workspace_actions_tests.rs"]
mod tests;

impl CapnpBrokerState {
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
    ) -> Option<Result<u64, ActionHostError>> {
        let address = &request.target.address;
        if address.resource == crate::automation::LIBRARY_RESOURCE
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
                Err(ActionHostError::NotPermitted)
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
    ) -> Result<(AdmittedHostAction, bool), ActionHostError> {
        let witnessed = self.authorize_framework_descriptor(authority, action, input)?;
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                room: &authority.0.room.to_string(),
                app_id: action.0,
                descriptor: action.1,
                request: input.0,
            },
        )?;
        #[cfg(test)]
        tests::at_effect_barrier();
        Ok((admitted, witnessed))
    }

    fn authorize_framework_descriptor(
        &self,
        authority: (&Workspace, &BrokerActor, u64),
        action: (&str, &ActionDescriptor),
        input: (&InvokeRequest, Option<&str>),
    ) -> Result<bool, ActionHostError> {
        let (workspace, actor, _) = authority;
        let (request, argument) = input;
        let room = workspace.room.to_string();
        let witnessed = self.consume_answer(
            request.confirmation.as_ref(),
            &self.witness_binding(actor.id(), &room, request),
        );
        if !Self::confirmed_as_asked(
            actor,
            action.1,
            argument,
            request.confirmation.as_ref(),
            witnessed,
        ) {
            return Err(ActionHostError::ConfirmationRequired);
        }
        self.framework_policy(
            authority,
            action,
            request.confirmation.as_ref(),
            witnessed,
            false,
        )?;
        Ok(witnessed)
    }

    // Admission may synchronously cross disk and owner-service barriers. Never
    // reuse the workspace or certificate ceiling sampled before those calls.
    pub(super) fn current_framework_authority(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        room: &str,
    ) -> Result<(Workspace, BrokerActor, u64), ActionHostError> {
        let unavailable = |reason| ActionHostError::Unavailable { reason };
        if !self.is_accepting() || (caller.execution_id().is_some() && !caller.execution_is_live())
        {
            return Err(ActionHostError::NotPermitted);
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
                    .ok_or(ActionHostError::NotPermitted)?;
                if original.room != room {
                    return Err(ActionHostError::NotPermitted);
                }
                if caller.device_public_key().is_none() {
                    self.current_native_person(room, certificate)
                        .map_err(unavailable)?;
                }
                let mut actor = current.broker_actor();
                actor.capped |= super::cap_applies_in(
                    self.certificate_authority.get().map(AsRef::as_ref),
                    room,
                    *foreign_restriction,
                    Some(&certificate.device_public_key),
                );
                Some(actor)
            }
            Some(InvocationCaller::Local {
                route,
                admitted,
                time,
            }) => {
                self.checked_policy_time(time.as_ref())
                    .map_err(unavailable)?;
                if route.cluster_id != room {
                    return Err(ActionHostError::NotPermitted);
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
                        return Err(ActionHostError::NotPermitted);
                    }
                }
                let current = self
                    .revalidate_admission(route, admitted)
                    .map_err(|error| unavailable(error.to_string()))?;
                self.checked_policy_time(time.as_ref())
                    .map_err(unavailable)?;
                Some(current.broker_actor())
            }
            Some(InvocationCaller::NativePerson {
                room: original,
                certificate,
                time,
                ..
            }) => {
                if original != room {
                    return Err(ActionHostError::NotPermitted);
                }
                self.checked_policy_time(Some(time)).map_err(unavailable)?;
                self.current_native_person(room, certificate)
                    .map_err(unavailable)?;
                Some(self.certified_person(room, certificate))
            }
            None => None,
        };
        let (workspace, actor, now) = self
            .decide_for(caller, room, |workspace, actor, now| {
                (workspace.clone(), actor.clone(), now)
            })
            .ok_or(ActionHostError::NotPermitted)?;
        Ok((workspace, original.unwrap_or(actor), now))
    }

    fn framework_policy(
        &self,
        authority: (&Workspace, &BrokerActor, u64),
        action: (&str, &ActionDescriptor),
        confirmation: Option<&Confirmation>,
        witnessed: bool,
        spend: bool,
    ) -> Result<(), ActionHostError> {
        let (workspace, actor, now) = authority;
        let (answer, spent) = actor.action_verdict_in(
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
            tilo_action_policy::Answer::Deny => return Err(ActionHostError::NotPermitted),
            tilo_action_policy::Answer::Ask
                if !(witnessed
                    || (actor.direct() && confirmation == Some(&Confirmation::Confirmed))) =>
            {
                return Err(ActionHostError::ConfirmationRequired);
            }
            _ => {}
        }
        if spend {
            self.spend_once_grants(&workspace.room.to_string(), &spent);
        }
        Ok(())
    }

    fn admitted_shortcut_library(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        authority: (&Workspace, &BrokerActor, u64),
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let library = self
            .services
            .shortcut_library
            .as_ref()
            .ok_or(ActionHostError::UnknownAction)?;
        let descriptor = crate::automation::library_descriptor(&request.target.address.action)
            .ok_or(ActionHostError::UnknownAction)?;
        let witnessed = self.authorize_framework_descriptor(
            authority,
            (tilo_automation::SHORTCUTS_APP_ID, &descriptor),
            (request, argument),
        )?;
        let operation = crate::automation::decode_library(&request.target.address.action, argument)
            .map_err(|reason| ActionHostError::InvalidRequest { reason })?;
        let catalog = tilo_automation::ActionCatalog::new(
            self.eager_catalogue(caller, super::action_host::RowScope::All)
                .0,
        );
        let keymap = library.keymap();
        let _plan = crate::automation::plan_library(
            authority.0,
            operation,
            &crate::automation::SaveContext {
                catalog: &catalog,
                saver: authority.1.id(),
                keymap: &keymap,
                fixed_label: &|fixed| library.fixed_shortcut_label(fixed),
            },
        )
        .map_err(|reason| ActionHostError::Refused { reason })?;
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                room: &authority.0.room.to_string(),
                app_id: tilo_automation::SHORTCUTS_APP_ID,
                descriptor: &descriptor,
                request,
            },
        )?;
        #[cfg(test)]
        tests::at_effect_barrier();
        let room = authority.0.room.to_string();
        let (plan, actor) = self.framework_effect_ready(&admitted, || {
            let (workspace, actor, _) = self.current_framework_authority(caller, origin, &room)?;
            // Replan on the current document: a valid old plan can overwrite a
            // concurrent shortcut edit even when the caller still has permission.
            let operation =
                crate::automation::decode_library(&request.target.address.action, argument)
                    .map_err(|reason| ActionHostError::InvalidRequest { reason })?;
            let catalog = tilo_automation::ActionCatalog::new(
                self.eager_catalogue(caller, super::action_host::RowScope::All)
                    .0,
            );
            let keymap = library.keymap();
            let plan = crate::automation::plan_library(
                &workspace,
                operation,
                &crate::automation::SaveContext {
                    catalog: &catalog,
                    saver: actor.id(),
                    keymap: &keymap,
                    fixed_label: &|fixed| library.fixed_shortcut_label(fixed),
                },
            )
            .map_err(|reason| ActionHostError::Refused { reason })?;
            let (current_workspace, actor, now) =
                self.current_framework_authority(caller, origin, &room)?;
            if serde_json::to_value(&workspace).ok()
                != serde_json::to_value(&current_workspace).ok()
            {
                return Err(ActionHostError::Refused {
                    reason: "shortcut library changed while planning; retry with a new invocation"
                        .into(),
                });
            }
            self.framework_policy(
                (&current_workspace, &actor, now),
                (tilo_automation::SHORTCUTS_APP_ID, &descriptor),
                request.confirmation.as_ref(),
                witnessed,
                true,
            )?;
            Ok((plan, actor))
        })?;
        let result = match plan {
            crate::automation::LibraryPlan::Answer(output) => Ok(output),
            crate::automation::LibraryPlan::Change { change, output } => library
                .store(&crate::services::ShortcutLibraryRequest {
                    room: &room,
                    actor: &actor,
                    change: &change,
                    context: request
                        .context
                        .clone()
                        .unwrap_or_else(|| crate::automation::library_context(&descriptor)),
                    invocation: admitted.invocation,
                })
                .map(|()| output)
                .map_err(|message| {
                    Box::new(ActionFailure {
                        code: "shortcut_library_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    })
                }),
        };
        self.finish_framework_action(admitted, result)
    }

    fn admitted_keyboard_shortcut(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        authority: (&Workspace, &BrokerActor, u64),
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let service = self
            .services
            .keyboard_shortcuts
            .as_ref()
            .ok_or(ActionHostError::UnknownAction)?;
        let mut keymap = settings_store::hardware::ShortcutConfig::load();
        let descriptor = service
            .descriptor(&keymap, &request.target.address)
            .ok_or(ActionHostError::UnknownAction)?;
        let (admitted, witnessed) = self.admit_framework_descriptor(
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
                .ok_or(ActionHostError::UnknownAction)?;
            if serde_json::to_value(&fresh).ok() != serde_json::to_value(&descriptor).ok() {
                return Err(ActionHostError::NotPermitted);
            }
            let (workspace, actor, now) =
                self.current_framework_authority(caller, origin, &authority.0.room.to_string())?;
            self.framework_policy(
                (&workspace, &actor, now),
                (service.app_id(), &fresh),
                request.confirmation.as_ref(),
                witnessed,
                true,
            )?;
            Ok(())
        })?;
        let result = service
            .apply(&mut keymap, &request.target.address, argument)
            .and_then(|(output, changed)| {
                if changed {
                    keymap.save().map_err(|error| {
                        Box::new(ActionFailure {
                            code: "save_failed".into(),
                            message: error.to_string(),
                            details: None,
                            retryable: true,
                        })
                    })?;
                }
                Ok(output)
            });
        self.finish_framework_action(admitted, result)
    }

    fn admitted_workspace_governance(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        authority: (&Workspace, &BrokerActor, u64),
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let service = self
            .services
            .workspace_governance
            .as_ref()
            .ok_or(ActionHostError::UnknownAction)?;
        let descriptor = service
            .descriptors(authority.0)
            .into_iter()
            .find(|descriptor| descriptor.address == request.target.address)
            .ok_or(ActionHostError::UnknownAction)?;
        // Owner actions retain their explicit direct-person confirmation floor.
        let owner_action = service.requires_descriptor_admission(&descriptor.address.action);
        if owner_action && !authority.1.direct() {
            return Err(ActionHostError::NotPermitted);
        }
        let witnessed = owner_action
            && self.consume_answer(
                request.confirmation.as_ref(),
                &self.witness_binding(authority.1.id(), &authority.0.room.to_string(), request),
            );
        if owner_action && !witnessed && request.confirmation != Some(Confirmation::Confirmed) {
            return Err(ActionHostError::ConfirmationRequired);
        }
        // Convert only the exact consumed owner witness; ordinary governance
        // retains its legacy phrase/confirmation semantics at the service.
        let mut checked = request.clone();
        if witnessed {
            checked.confirmation = Some(Confirmation::Confirmed);
        }
        // Admission records the original witness-bearing request, not a rewrite.
        self.framework_policy(
            authority,
            (service.app_id(), &descriptor),
            checked.confirmation.as_ref(),
            witnessed,
            false,
        )?;
        if !Self::confirmed_as_asked(
            authority.1,
            &descriptor,
            argument,
            checked.confirmation.as_ref(),
            witnessed,
        ) {
            return Err(ActionHostError::ConfirmationRequired);
        }
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                room: &authority.0.room.to_string(),
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
        let room = authority.0.room.to_string();
        let actor = self.framework_effect_ready(&admitted, || {
            let (workspace, _, _) = self.current_framework_authority(caller, origin, &room)?;
            let fresh = service
                .descriptors(&workspace)
                .into_iter()
                .find(|fresh| fresh.address == descriptor.address)
                .ok_or(ActionHostError::UnknownAction)?;
            let (current_workspace, actor, now) =
                self.current_framework_authority(caller, origin, &room)?;
            if serde_json::to_value(&fresh).ok() != serde_json::to_value(&descriptor).ok()
                || serde_json::to_value(&workspace).ok()
                    != serde_json::to_value(&current_workspace).ok()
                || (owner_action && !actor.direct())
            {
                return Err(ActionHostError::NotPermitted);
            }
            self.framework_policy(
                (&current_workspace, &actor, now),
                (service.app_id(), &fresh),
                checked.confirmation.as_ref(),
                witnessed,
                true,
            )?;
            Ok(actor)
        })?;
        let started = service.start(&crate::services::WorkspaceGovernanceRequest {
            local_device: caller.device_public_key().is_none()
                && caller.shortcut().is_none()
                && caller.acting_for_person().is_none()
                && request.on_behalf_of.is_none(),
            execution_lease: caller.execution_lease(),
            room: &room,
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
                    let uncertain = outcome.is_none();
                    let result = outcome.unwrap_or_else(|| Err(Box::new(ActionFailure {
                        code: "host_step_stopped".into(), message: "the host step stopped without an answer".into(),
                        details: None, retryable: false,
                    })));
                    let completed = if uncertain {
                        let update = InvocationUpdate::Failed { invocation: admitted.invocation,
                            error: *result.expect_err("missing service answer") };
                        state.complete_uncertain_host_action(&admitted, update).map(|()| admitted.id)
                    } else {
                        state.finish_framework_action(admitted, result)
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

    fn framework_effect_ready<T>(
        &self,
        admitted: &AdmittedHostAction,
        check: impl FnOnce() -> Result<T, ActionHostError>,
    ) -> Result<T, ActionHostError> {
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

    fn finish_framework_action(
        &self,
        admitted: AdmittedHostAction,
        result: crate::services::WorkspaceGovernanceOutcome,
    ) -> Result<u64, ActionHostError> {
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
