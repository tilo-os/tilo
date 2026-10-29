//! A session service as the session hosts it.
//!
//! A service (connectivity, audio, power, ...) has no view. It publishes
//! resources: each one a reading of the device (Wi-Fi's networks, the sound
//! outputs, the power mode) and the actions that change it. Quick settings,
//! Settings, the desktop, a script and an agent read the same readings and
//! invoke the same actions, by address, through the session's action door.
//!
//! A service's actions are an [`Effect`] vocabulary: its specs are derived
//! ([`offer`]) and an invocation is decoded back into the vocabulary
//! ([`decode`]) after the published spec checked its arguments and its
//! availability. The device calls behind them (`NetworkManager`, `BlueZ`,
//! `PipeWire`) block, so they run on the service's [`Worker`]: an action
//! is [`Started::Accepted`] at once and its result lands in the reading.
//!
//! What needs a person's answer is asked through [`Questions`], itself a
//! service: each pending question a resource the session's sheet reads and
//! answers through its actions.
//!
//! [`Services`] is the session's side: every service it started, with one
//! catalogue, one reader and one door over them.

pub mod lines;
pub mod questions;
mod worker;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_resource::Effect;

pub use questions::{
    Answer, Asked, QUESTION_KIND, QUESTIONS_KIND, QUESTIONS_RESOURCE, Question, QuestionEffect,
    Questions, question_resource,
};
pub use worker::{Jobs, Wake, Worker};

/// What an invocation started.
#[derive(Clone, Debug, PartialEq)]
pub enum Started {
    /// It finished on return, with this output.
    Done(ActionOutput),
    /// It was taken and runs on the device; its result lands in the
    /// service's reading.
    Accepted,
}

/// One service the session runs.
pub trait Service {
    /// What it publishes now: its resources, each with its actions.
    fn resources(&self) -> Vec<ResourceSpec>;

    /// The reading of `resource` now, as its state's JSON; `None` for a
    /// resource it does not publish.
    fn read(&self, resource: &str) -> Option<Value>;

    /// Carry out the action `action` of its resource `resource`.
    ///
    /// # Errors
    /// The door's refusal (no such action, bad arguments, unavailable) or
    /// why the device did not do it.
    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError>;

    /// Take in what landed from the device since the last call. Whether a
    /// reading changed.
    fn poll(&mut self) -> bool {
        false
    }

    /// Wait until every device call it started has landed, and take them
    /// in: what a headless run does before it reads.
    fn settle(&mut self) {}
}

/// `resource` offering every action of the vocabulary `E`.
#[must_use]
pub fn offer<E: Effect>(resource: ResourceSpec) -> ResourceSpec {
    let mut resource = resource;
    resource.actions.extend(E::specs());
    resource
}

/// Mark the action `action` of `resource` unavailable, with why: the
/// device has no such radio, the daemon is not running.
pub fn unavailable(resource: &mut ResourceSpec, action: &str, code: &str, reason: &str) {
    for spec in &mut resource.actions {
        if spec.id == action {
            *spec = spec.clone().unavailable(code, reason);
        }
    }
}

/// The effect of `E` that invoking `action` of `resource` with `arguments`
/// names, once the spec `published` offers for it checked the arguments
/// and that it is available now. A spec offered bound to an item (a
/// network, an accessory) adds the item's arguments.
///
/// # Errors
/// [`DoorError::Unknown`] when nothing published names it,
/// [`DoorError::Unavailable`], or the arguments' refusal.
pub fn decode<E: Effect>(
    published: &[ResourceSpec],
    resource: &str,
    action: &str,
    arguments: &Map<String, Value>,
) -> Result<E, DoorError> {
    let address = ActionAddress::new(resource, action);
    let (_, spec) = tilo_actions::door::locate(published, &address)
        .ok_or_else(|| DoorError::Unknown(address.clone()))?;
    tilo_actions::door::encode_json(published, &address, arguments)?;
    let mut all = arguments.clone();
    for (name, value) in spec.bound() {
        all.insert(name.clone(), value.clone());
    }
    E::decode(&spec.id, &all).map_err(DoorError::InvalidArguments)
}

/// A reading as its JSON, for [`Service::read`].
#[must_use]
pub fn reading<T: serde::Serialize>(state: &T) -> Value {
    serde_json::to_value(state).unwrap_or_else(|error| {
        tracing::warn!(%error, "a service reading did not serialise");
        Value::Null
    })
}

/// Every service the session started: one catalogue, reader and door.
#[derive(Default)]
pub struct Services {
    services: Vec<Box<dyn Service>>,
    /// Moves whenever what they publish may have changed: a service was
    /// added, an action ran, a reading landed. A reader of the catalogue
    /// reads it again only when this moved.
    revision: u64,
}

impl Services {
    /// Host `service` too.
    pub fn add(&mut self, service: Box<dyn Service>) {
        self.services.push(service);
        self.changed();
    }

    /// Where what they publish stands: equal revisions publish the same.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    const fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Whether it hosts none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    /// Everything the services publish now.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.services
            .iter()
            .flat_map(|service| service.resources())
            .collect()
    }

    /// The reading of `resource`, from the service publishing it.
    #[must_use]
    pub fn read(&self, resource: &str) -> Option<Value> {
        self.services
            .iter()
            .find_map(|service| service.read(resource))
    }

    /// Invoke `action` of `resource` on the service publishing it; `None`
    /// when no service does. An action whose contract asks for
    /// confirmation (restart, power off) is refused with
    /// [`DoorError::ConfirmationRequired`]; once the person confirmed it,
    /// the caller invokes it with [`Self::invoke_confirmed`].
    pub fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        self.route(resource, action, arguments, false)
    }

    /// [`Self::invoke`] after the person confirmed it.
    pub fn invoke_confirmed(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        self.route(resource, action, arguments, true)
    }

    fn route(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
        confirmed: bool,
    ) -> Option<Result<Started, DoorError>> {
        let address = ActionAddress::new(resource, action);
        let (service, asks) = self.services.iter_mut().find_map(|service| {
            let published = service.resources();
            let spec = tilo_actions::door::resolve(&published, &address)?;
            let asks = !matches!(
                spec.confirmation_for_json(arguments),
                tilo_actions::ConfirmationPolicy::None
            );
            Some((service, asks))
        })?;
        if asks && !confirmed {
            return Some(Err(DoorError::ConfirmationRequired));
        }
        let started = service.invoke(resource, action, arguments);
        self.changed();
        Some(started)
    }

    /// Take in what landed on every service. Whether a reading changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for service in &mut self.services {
            changed |= service.poll();
        }
        if changed {
            self.changed();
        }
        changed
    }

    /// Wait for every service's device calls to land.
    pub fn settle(&mut self) {
        for service in &mut self.services {
            service.settle();
        }
        self.changed();
    }
}

#[cfg(test)]
mod tests;
