//! Registered workspace services share original-custody host admission.
use super::host_action_admission::{AdmittedHostAction, HostActionAdmission};
use super::{BrokerActor, CapnpBrokerState, InvocationCaller, Workspace};
use std::sync::Arc;
use tilo_actions::remote::{ActionHostError, Caller, Confirmation, InvokeRequest};
use tilo_actions::{ActionDescriptor, ActionFailure, InvocationUpdate};

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
    ) -> Result<AdmittedHostAction, ActionHostError> {
        self.authorize_framework_descriptor(authority, action, input)?;
        self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                room: &authority.0.room.to_string(),
                app_id: action.0,
                descriptor: action.1,
                request: input.0,
            },
        )
    }

    fn authorize_framework_descriptor(
        &self,
        authority: (&Workspace, &BrokerActor, u64),
        action: (&str, &ActionDescriptor),
        input: (&InvokeRequest, Option<&str>),
    ) -> Result<(), ActionHostError> {
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
        self.admit_answer(
            authority,
            action.0,
            action.1,
            (argument, request.confirmation.as_ref()),
            witnessed,
        )
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
        self.authorize_framework_descriptor(
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
        let plan = crate::automation::plan_library(
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
        let result = match plan {
            crate::automation::LibraryPlan::Answer(output) => Ok(output),
            crate::automation::LibraryPlan::Change { change, output } => library
                .store(&crate::services::ShortcutLibraryRequest {
                    room: &authority.0.room.to_string(),
                    actor: authority.1,
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
        let admitted = self.admit_framework_descriptor(
            caller,
            origin,
            authority,
            (service.app_id(), &descriptor),
            (request, argument),
        )?;
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
        self.admit_answer(
            authority,
            service.app_id(),
            &descriptor,
            (argument, checked.confirmation.as_ref()),
            witnessed,
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
        let confirmation = match checked.confirmation.as_ref() {
            Some(Confirmation::Confirmed) => serde_json::json!({"confirm": true}),
            Some(Confirmation::Phrase(phrase)) => {
                serde_json::json!({"confirmation_phrase": phrase})
            }
            _ => serde_json::json!({}),
        };
        let started = service.start(&crate::services::WorkspaceGovernanceRequest {
            local_device: caller.device_public_key().is_none()
                && caller.shortcut().is_none()
                && caller.acting_for_person().is_none()
                && request.on_behalf_of.is_none(),
            execution_lease: caller.execution_lease(),
            room: &authority.0.room.to_string(),
            actor: authority.1,
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
                    let result = outcome.unwrap_or_else(|| Err(Box::new(ActionFailure {
                        code: "host_step_stopped".into(), message: "the host step stopped without an answer".into(),
                        details: None, retryable: false,
                    })));
                    if let Err(error) = state.finish_framework_action(admitted, result) {
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
