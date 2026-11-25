//! The person's settings, answered by this session's broker for Settings' own controls.
//!
//! Settings' sections are records another provider keeps: its window shows them and binds its
//! controls to their setters, and each control's action says `run_by("tilo.settings")`, so
//! the click leaves the window as an invocation at that provider through the workspace's broker
//! ([`crate::mcp_server`]'s caller). The session keeps the settings itself, in its settings
//! service, which publishes every section where the settings keep it (`settings/appearance`,
//! `devices/<device>/settings/power`); this is the provider the broker
//! answers those actions under, so a control of Settings' window changes the settings the
//! session publishes whether or not Settings is open, as a script's change does.
//!
//! The rows are the settings the service published on the loop's last pass, read there and
//! handed here ([`Self::offer`]), where a catalogue built on the broker's own thread finds them
//! without waiting on the session. An invocation is handed to the session's API
//! ([`SessionSteps`](crate::platform::steps::SessionSteps)) and answered on the pass, where the
//! service runs it: the same path a service's step takes from the session's own provider, with
//! the broker admitting it first, its confirmation included, under Settings' app id.

use std::sync::Arc;

use tilo_actions::remote::settings_resources as wire;
use tilo_actions::remote::{
    ActionCallError, Admitted, Caller, InvokeRequest, Outcome, Pending, pending,
};
use tilo_actions::{ActionDescriptor, ActionFailure, ActionSpec, ResourceSpec};
use tilo_broker_core::BrokerActor;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

use crate::platform::steps::SessionSteps;

/// The person's settings, as the broker's catalogue names them for Settings' controls.
pub(crate) struct SettingsSteps {
    /// What answers an invocation on the loop's pass: the session's own steps, where the
    /// settings service runs it.
    steps: Arc<dyn SessionSteps>,
    /// The settings the service published on its last pass: what the catalogue lists, and the
    /// sections' setters an invocation is decided against.
    resources: std::sync::RwLock<Vec<ResourceSpec>>,
    /// Where what the services publish stood when the settings were last read
    /// ([`tilo_service::Services::revision`]), so a pass reads them again only once a service
    /// changed what it publishes.
    read_at: std::sync::Mutex<Option<u64>>,
}

impl SettingsSteps {
    /// The person's settings, whose invocations the session answers on its pass through `steps`.
    pub(crate) fn new(steps: Arc<dyn SessionSteps>) -> Self {
        Self {
            steps,
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
            .filter(|resource| settings_sections::service::keeps(resource.id.as_str()))
            .collect();
        let mut held = self.resources.write_recover();
        let changed = !tilo_actions::api::publishes_same(&held, &resources);
        *held = resources;
        changed
    }

    /// Take what this provider offers out of `resources`: the session's own provider lists
    /// what its services publish, and Settings' sections are offered here instead, under
    /// this provider. One address has one entry.
    pub(crate) fn leave_out(resources: &mut Vec<ResourceSpec>) {
        resources.retain(|resource| !settings_sections::service::keeps(resource.id.as_str()));
    }

    /// The rows the settings stand in now.
    fn offered(&self) -> Vec<tilo_actions::api::PlacedAction> {
        tilo_actions::api::survey_placed(&self.resources.read_recover())
    }

    /// Answer the settings' changes from now on, admitted by the grants of Settings' app, for
    /// the person at this device alone.
    pub(crate) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(
            wire::PROVIDER,
            settings_sections::APP_ID,
            Reach::ThisDevice,
            self,
        );
    }

    /// The published action at `address`, as one invocation with `argument` (its slot form) is:
    /// the kind and the risk the value it carries make it, so a change that weakens the person's
    /// security is admitted as that, and asks them first.
    fn invocation_as(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        let published = self.resources.read_recover();
        // Another of the person's devices' section, or an app's first preference: what stands
        // there is admitted as a published action is.
        let standing;
        let resources: &[ResourceSpec] =
            if tilo_actions::api::resolve(&published, address).is_some() {
                &published
            } else {
                standing = [settings_sections::service::unpublished(
                    &published, address,
                )?];
                &standing
            };
        let spec: &ActionSpec = tilo_actions::api::resolve(resources, address)?;
        let need = spec.need_for(None, argument);
        let mut descriptor = tilo_actions::api::survey_placed(resources)
            .into_iter()
            .map(|placed| placed.action)
            .find(|descriptor| &descriptor.address == address)?;
        drop(published);
        descriptor.contract.kind = need.kind;
        descriptor.contract.field = need.field;
        if need.privileged {
            descriptor.contract.risk = tilo_actions::RiskLevel::Privileged;
        }
        Some(descriptor)
    }
}

impl tilo_actions::remote::Admits for SettingsSteps {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = settings_sections::APP_ID;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        self.offered()
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let actor = BrokerActor::for_caller(caller);
        let argument = request.argument.to_slot();
        let (done, asked) = pending();
        // The place the ask is in, as for the session's own steps: an agent's turn stands what a
        // setting opens beside its conversation, the person's own step beside the place in front.
        let beside = crate::mcp_server::service_steps::turn_beside(actor.turn());
        self.steps.invoke_step(
            actor.actor().clone(),
            request.target.address.clone(),
            argument,
            beside,
            request
                .context
                .as_ref()
                .and_then(|context| context.group.clone()),
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

    fn described(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        self.invocation_as(address, argument)
    }
}

/// Why a step did not run, in the words the person at the window reads: what failed and what to
/// do about it, never the name of a component nor a machine code.
fn reason(error: &ActionCallError) -> String {
    match error {
        ActionCallError::UnknownAction => {
            "Settings does not offer that change here any more".to_owned()
        }
        ActionCallError::NotPermitted => "you may not change this setting from here".to_owned(),
        ActionCallError::ConfirmationRequired { .. } => {
            "this change asks you first: answer its question, then try again".to_owned()
        }
        ActionCallError::Unavailable { reason }
        | ActionCallError::InvalidRequest { reason }
        | ActionCallError::Refused { reason }
        | ActionCallError::NotCancellable { reason } => reason.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests;
