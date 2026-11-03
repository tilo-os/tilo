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
pub use worker::{Jobs, LoopWake, Wake, Worker};

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
    /// Who sits at this device, as the session knows them before it starts
    /// the service: the person's id, which what they do here is theirs
    /// under, and the ids they went by before this device had an identity.
    /// A service that records who changed what authors its changes as this
    /// person; one that records none ignores it.
    fn seated(&mut self, _person: &str, _earlier: &[String]) {}

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

    /// [`Self::invoke`] as `invoker` (an agent, a script), which what the change makes is
    /// attributed to. The default runs it as the person seated here, as [`Self::invoke`] does.
    ///
    /// # Errors
    /// The door's refusal.
    fn invoke_as(
        &mut self,
        _invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        self.invoke(resource, action, arguments)
    }

    /// Take in what landed from the device since the last call. Whether a
    /// reading changed.
    fn poll(&mut self) -> bool {
        false
    }

    /// Wait until every device call it started has landed, and take them
    /// in: what a headless run does before it reads.
    fn settle(&mut self) {}

    /// The resources whose readings moved since the last call, by id: what
    /// a reader following a few of them reads again, and only those. `None`
    /// when it cannot say which, so any of its readings may have moved.
    fn moved(&mut self) -> Option<Vec<String>> {
        None
    }

    /// The compiled module of the app `app` whose records it publishes, its
    /// description embedded: what a shell loads to draw those records
    /// through the app's own views. `None` for an app it keeps nothing of.
    fn module(&self, _app: &str) -> Option<std::sync::Arc<[u8]>> {
        None
    }

    /// The record of the app `app` this service keeps, when the session runs the app's package
    /// itself (the shell) and its records are the person's as any app's: what a connection the
    /// session admits — a REPL, an agent's task — reads and changes by the app, as it does
    /// Notes'. `None` for a service that keeps no record, or none of that app.
    fn record(&self, _app: &str) -> Option<Record> {
        None
    }

    /// Every record [`Self::record`] answers, each with the app it is of: what a connection the
    /// session admits reads by the app.
    fn records(&self) -> Vec<Record> {
        Vec::new()
    }

    /// Publish the package `share` names, which the session runs itself (the shell), with the
    /// records an app's are: its roots are made, and what its parts write into the document
    /// it keeps them in (`share.records`) is published to every connection from then on.
    ///
    /// The default keeps none: a service that runs no package of its own has nothing to share.
    ///
    /// # Errors
    /// The package's roots cannot be made, or its record does not apply.
    fn share(&mut self, _share: &Share<'_>) -> Result<(), String> {
        Ok(())
    }

    /// Take in what the parts of a [`Self::share`]d package wrote into the document they share.
    /// Whether anything was.
    ///
    /// # Errors
    /// The changes do not apply.
    fn follow(&mut self) -> Result<bool, String> {
        Ok(false)
    }
}

/// A package the session runs itself, handed to [`Service::share`]: which it is, its compiled
/// module, and the document its parts keep the records in.
pub struct Share<'a> {
    /// The app's id, as its package names it (`so.tilo.shell`).
    pub app: &'a str,
    /// Its compiled module, its description embedded.
    pub wasm: &'a [u8],
    /// The document its parts commit into: the record, which the service publishes and the
    /// parts draw from. The service keeps it and takes in what they write there.
    pub records: &'a std::rc::Rc<std::cell::RefCell<tilo_resource::Doc>>,
}

/// A record an app keeps, as [`Service::record`] publishes it: the app, its id there, the
/// document, and the packages whose code a reader of it needs.
pub struct Record {
    /// The app the record is of, as its package names it (`so.tilo.shell`).
    pub app: String,
    /// The record's id in its app, as the workspace names it (empty for a package's record of
    /// the workspace).
    pub id: String,
    /// The document: its committed content, as Automerge writes it.
    pub document: Vec<u8>,
    /// The compiled module of the app and of each package it uses, each its description
    /// embedded: what a task's host loads to run a program that reads the record by the app's
    /// own package.
    pub packages: Vec<std::sync::Arc<[u8]>>,
}

/// `resource` offering every action of the vocabulary `E`.
pub use tilo_resource::offer;

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
    E::decode(&spec.id, &all).map_err(|error| DoorError::InvalidArguments(error.to_string()))
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
    started: Vec<Box<dyn Service>>,
    /// Moves whenever what they publish may have changed: a service was
    /// added, an action ran, a reading landed. A reader of the catalogue
    /// reads it again only when this moved.
    revision: u64,
    /// Per service, whether what it publishes may have changed since
    /// [`Self::moved_among`] last asked: what a service that cannot say
    /// which of its readings moved is read again for.
    stirred: Vec<bool>,
}

impl Services {
    /// Host `service` too.
    pub fn add(&mut self, service: Box<dyn Service>) {
        self.started.push(service);
        self.stirred.push(true);
        self.changed();
    }

    /// Of `followed`, the resources whose readings moved since the last
    /// call: each service names those of its readings that moved, and one
    /// that cannot say which names every followed resource it publishes,
    /// once something of its moved.
    pub fn moved_among(
        &mut self,
        followed: &std::collections::BTreeSet<String>,
    ) -> std::collections::BTreeSet<String> {
        let mut moved = std::collections::BTreeSet::new();
        for (service, stirred) in self.started.iter_mut().zip(&mut self.stirred) {
            let was_stirred = std::mem::take(stirred);
            match service.moved() {
                Some(ids) => moved.extend(ids.into_iter().filter(|id| followed.contains(id))),
                None if was_stirred => moved.extend(
                    followed
                        .iter()
                        .filter(|id| service.read(id).is_some())
                        .cloned(),
                ),
                None => {}
            }
        }
        moved
    }

    /// The compiled module of the app `app`, from the service keeping its
    /// records; see [`Service::module`].
    #[must_use]
    pub fn module(&self, app: &str) -> Option<std::sync::Arc<[u8]>> {
        self.started.iter().find_map(|service| service.module(app))
    }

    /// The record of the app `app`, from the service keeping it; see [`Service::record`].
    #[must_use]
    pub fn record(&self, app: &str) -> Option<Record> {
        self.started.iter().find_map(|service| service.record(app))
    }

    /// Every record the services keep for apps the session runs itself, by the app: what a
    /// connection the session admits reads by the app, as it does any app's records.
    #[must_use]
    pub fn records(&self) -> Vec<Record> {
        self.started
            .iter()
            .flat_map(|service| service.records())
            .collect()
    }

    /// Publish the package `share` names, which the session runs itself (the shell), with the
    /// apps' records: offered to every service, the one keeping those records takes it.
    ///
    /// # Errors
    /// A service's record does not apply.
    pub fn share(&mut self, share: &Share<'_>) -> Result<(), String> {
        for service in &mut self.started {
            service.share(share)?;
        }
        self.changed();
        Ok(())
    }

    /// Take in what the parts of the packages [`Self::share`]d wrote into the documents they
    /// share since the last pass, and publish it. Whether anything was.
    ///
    /// # Errors
    /// A service's shared record does not apply.
    pub fn follow_shared(&mut self) -> Result<bool, String> {
        let mut followed = false;
        for service in &mut self.started {
            followed |= service.follow()?;
        }
        if followed {
            self.changed();
        }
        Ok(followed)
    }

    /// [`Self::invoke`] as `invoker` (an agent, a script): what the change it makes is
    /// attributed to. The service's own door decides whether the caller may.
    ///
    /// # Errors
    /// The service's refusal.
    pub fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let address = ActionAddress::new(resource, action);
        let index = self
            .started
            .iter()
            .position(|service| {
                tilo_actions::door::resolve(&service.resources(), &address).is_some()
            })
            .ok_or_else(|| DoorError::Unknown(address.clone()))?;
        let started = self
            .started
            .get_mut(index)
            .ok_or_else(|| DoorError::Unknown(address.clone()))?
            .invoke_as(invoker, resource, action, arguments);
        if let Some(stirred) = self.stirred.get_mut(index) {
            *stirred = true;
        }
        if started.is_ok() {
            self.changed();
        }
        started
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
        self.started.is_empty()
    }

    /// Everything the services publish now.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.started
            .iter()
            .flat_map(|service| service.resources())
            .collect()
    }

    /// The reading of `resource`, from the service publishing it.
    #[must_use]
    pub fn read(&self, resource: &str) -> Option<Value> {
        self.started
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
        let (index, asks) = self
            .started
            .iter()
            .enumerate()
            .find_map(|(index, service)| {
                let published = service.resources();
                let spec = tilo_actions::door::resolve(&published, &address)?;
                let asks = !matches!(
                    spec.confirmation_for_json(arguments),
                    tilo_actions::ConfirmationPolicy::None
                );
                Some((index, asks))
            })?;
        if asks && !confirmed {
            return Some(Err(DoorError::ConfirmationRequired));
        }
        let started = self
            .started
            .get_mut(index)?
            .invoke(resource, action, arguments);
        if let Some(stirred) = self.stirred.get_mut(index) {
            *stirred = true;
        }
        self.changed();
        Some(started)
    }

    /// Take in what landed on every service. Whether a reading changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for (service, stirred) in self.started.iter_mut().zip(&mut self.stirred) {
            let moved = service.poll();
            *stirred |= moved;
            changed |= moved;
        }
        if changed {
            self.changed();
        }
        changed
    }

    /// Wait for every service's device calls to land.
    pub fn settle(&mut self) {
        for service in &mut self.started {
            service.settle();
        }
        self.stirred.fill(true);
        self.changed();
    }
}

/// Several services served as one: what a helper process serving more
/// than one over its pipes ([`lines::serve`]) hands it. The session asked
/// the person first where an action asks it, so what arrives is confirmed.
impl Service for Services {
    fn resources(&self) -> Vec<ResourceSpec> {
        Self::resources(self)
    }

    fn read(&self, resource: &str) -> Option<Value> {
        Self::read(self, resource)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        self.invoke_confirmed(resource, action, arguments)
            .unwrap_or_else(|| Err(DoorError::Unknown(ActionAddress::new(resource, action))))
    }

    fn poll(&mut self) -> bool {
        Self::poll(self)
    }

    fn settle(&mut self) {
        Self::settle(self);
    }

    fn module(&self, app: &str) -> Option<std::sync::Arc<[u8]>> {
        Self::module(self, app)
    }
}

#[cfg(test)]
mod tests;
