//! The person's settings, answered by this session's broker for Settings' own controls.
//!
//! Settings' sections are records another provider keeps: its window shows them and binds its
//! controls to their field edits, and each control's action says `run_by("tilo.settings")`, so
//! the click leaves the window as an invocation at that provider through the workspace's broker
//! ([`crate::agent_door`]'s caller). The session keeps the settings itself, in its settings
//! service, which publishes every section as `settings:<kind>`; this is the provider the broker
//! answers those actions under, so a control of Settings' window changes the settings the
//! session publishes whether or not Settings is open, as a script's change does.
//!
//! The rows are the settings the service published on the loop's last pass, read there and
//! handed here ([`Self::offer`]), where a catalogue built on the broker's own thread finds them
//! without waiting on the session. An invocation is handed to the session's door
//! ([`SessionSteps`](crate::platform::steps::SessionSteps)) and answered on the pass, where the
//! service runs it: the same path a service's step takes from the session's own provider, with
//! the broker admitting it first, its confirmation included, under Settings' app id.

use std::sync::Arc;

use tilo_actions::remote::settings_records as wire;
use tilo_actions::remote::{ActionHostError, Pending, pending};
use tilo_actions::{ActionDescriptor, ActionFailure, ActionSpec, ResourceSpec};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

use crate::platform::steps::SessionSteps;

/// The person's settings, as the broker's catalogue names them for Settings' controls.
pub(crate) struct SettingsSteps {
    /// What answers an invocation on the loop's pass: the session's own doors, where the
    /// settings service runs it.
    doors: Arc<dyn SessionSteps>,
    /// The settings the service published on its last pass: what the catalogue lists, and the
    /// sections' field edits an invocation is decided against.
    resources: std::sync::RwLock<Vec<ResourceSpec>>,
    /// Where what the services publish stood when the settings were last read
    /// ([`tilo_service::Services::revision`]), so a pass reads them again only once a service
    /// changed what it publishes.
    read_at: std::sync::Mutex<Option<u64>>,
}

impl SettingsSteps {
    /// The person's settings, whose invocations the session answers on its pass through `doors`.
    pub(crate) fn new(doors: Arc<dyn SessionSteps>) -> Self {
        Self {
            doors,
            resources: std::sync::RwLock::default(),
            read_at: std::sync::Mutex::default(),
        }
    }

    /// Publish what the settings service offers among `services` now: what the broker's
    /// catalogue lists from here on. Whether they are new: a pass that read the same settings
    /// again changes nothing, and no catalogue is rebuilt for it. Nothing is read at all until a
    /// service changed what it publishes, since equal revisions publish the same.
    pub(crate) fn offer(&self, services: &tilo_service::Services) -> bool {
        {
            let mut read_at = self.read_at.lock_recover();
            if *read_at == Some(services.revision()) {
                return false;
            }
            *read_at = Some(services.revision());
        }
        let resources: Vec<ResourceSpec> = services
            .resources()
            .into_iter()
            .filter(|resource| tilo_settings_service::keeps(resource.id.as_str()))
            .collect();
        let mut held = self.resources.write_recover();
        let changed = !tilo_actions::door::publishes_same(&held, &resources);
        *held = resources;
        changed
    }

    /// The rows the settings stand in now.
    fn offered(&self) -> Vec<tilo_actions::door::PlacedAction> {
        tilo_actions::door::survey_placed(&self.resources.read_recover())
    }

    /// The published action at `address`, as one invocation with `argument` (its slot form) is:
    /// the kind and the risk the value it carries make it, so a change that weakens the person's
    /// security is admitted as that, and asks them first.
    fn invocation_as(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        let resources = self.resources.read_recover();
        let spec: &ActionSpec = tilo_actions::door::resolve(&resources, address)?;
        let need = spec.need_for(None, argument);
        let mut descriptor = tilo_actions::door::survey_placed(&resources)
            .into_iter()
            .map(|placed| placed.action)
            .find(|descriptor| &descriptor.address == address)?;
        drop(resources);
        descriptor.contract.kind = need.kind;
        descriptor.contract.field = need.field;
        if need.privileged {
            descriptor.contract.risk = tilo_actions::RiskLevel::Privileged;
        }
        Some(descriptor)
    }
}

impl HostStepServices for SettingsSteps {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &str {
        settings_sections::APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        self.offered()
            .into_iter()
            .map(|placed| placed.action)
            .collect()
    }

    fn rows(&self) -> Vec<tilo_actions::door::PlacedAction> {
        self.offered()
    }

    fn invocation(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        self.invocation_as(address, argument)
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let argument = request.argument.map(str::to_owned);
        let (done, asked) = pending();
        // The place the ask is in, as for the session's own steps: an agent's turn stands what a
        // setting opens beside its conversation, the person's own step beside the place in front.
        let beside = request
            .actor
            .turn()
            .map(|turn| turn.conversation.clone())
            .unwrap_or_default();
        self.doors.invoke_step(
            request.actor.id().to_owned(),
            request.address.clone(),
            argument,
            beside,
            request.run.cloned(),
            done,
        );
        Ok(asked.map(|ran| match ran {
            // The session carries an accepted step on with, answering it as it runs.
            Ok(None) => Ok(tilo_actions::ActionOutput::default()),
            Ok(Some(output)) => Ok(output),
            Err(error) => Err(Box::new(ActionFailure {
                code: error.code().to_owned(),
                message: reason(&error),
                details: None,
                retryable: false,
            })),
        }))
    }
}

/// Why a step did not run, in the words the person at the window reads: what failed and what to
/// do about it, never the name of a component nor a machine code.
fn reason(error: &ActionHostError) -> String {
    match error {
        ActionHostError::UnknownAction => {
            "Settings does not offer that change here any more".to_owned()
        }
        ActionHostError::NotPermitted => "you may not change this setting from here".to_owned(),
        ActionHostError::ConfirmationRequired { .. } => {
            "this change asks you first: answer its question, then try again".to_owned()
        }
        ActionHostError::Unavailable { reason }
        | ActionHostError::InvalidRequest { reason }
        | ActionHostError::Refused { reason }
        | ActionHostError::NotCancellable { reason } => reason.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests;
