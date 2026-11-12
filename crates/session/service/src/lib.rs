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

/// A place an invocation asked the session to open: the resource, and the view it is shown
/// through when it is not the resource's primary one (`docs/designs/app-language/one-app-model.md`
/// §3, "`open(r)` places r with its default view, beside where it was asked, for every app").
///
/// Opening a place is the system's, never a resource's own act (shell.md §8, "The canvas composes
/// views; it opens nothing"). A service that keeps a resource's record publishes its `open` as the
/// other actions are published — an agent and a view read the catalogue the same way — and answers
/// it here: the invocation changed nothing of the record, and the session places the resource
/// through its arrangement, as [`crate::Service::take_open`] hands it over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    /// The resource to place, by its published id.
    pub resource: String,
    /// The view to show it through; empty for its primary one.
    pub view: String,
    /// The place it stands beside, by the resource that place shows: the conversation whose turn
    /// asked, when an agent's invocation queued it. Empty means the place in front.
    pub beside: String,
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

    /// How long until something it keeps is due on a clock of its own — a
    /// package's trigger at the instant it names — so the session's wait ends
    /// then and its next pass does it. `None` when nothing it keeps is timed.
    fn wakes_in(&self) -> Option<std::time::Duration> {
        None
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

    /// The resource address this service publishes the app `app`'s record at,
    /// when it runs that app's model: the session's document host serves the
    /// app, so a launch it makes names the address and the app's process opens
    /// no record of its own (`docs/architecture/apps.md` §8 step 2,
    /// `one-app-model.md` §4). `None` for a service that runs no such model.
    fn hosted(&self, _app: &str) -> Option<String> {
        None
    }

    /// Every app whose model this service runs, by the app, with the resource address it publishes
    /// its record at: what [`Self::hosted`] answers, for a host that asks about them all at once —
    /// the session tells its launches which apps it serves, so a plain `open` of one places that
    /// resource instead of awaiting a window process that never publishes
    /// (`docs/designs/app-language/one-app-model.md` §3).
    fn served(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The places [`Self::invoke`] and [`Self::invoke_as`] were asked to open since this was last
    /// taken, each with the actor that asked: an invocation of a resource's `open` changes nothing
    /// of the record, and opening is the system's ([`Open`], shell.md §8, "The canvas composes
    /// views; it opens nothing"). The session places each through its arrangement, attributed to
    /// that actor and admitted as the same step `shell:arrangement#open` is. A service that
    /// publishes no `open` has nothing to take.
    fn take_open(&mut self) -> Vec<(tilo_actions::ActionInvoker, Open)> {
        Vec::new()
    }

    /// The place the invocations that follow are asked in, by the resource that place shows: the
    /// conversation of a turn an agent acts in, or empty for the place in front. A place an
    /// invocation opens stands beside it (`docs/designs/app-language/one-app-model.md` §3).
    /// [`crate::Services::invoke_as_beside`] sets it for one invocation; a service that opens no
    /// place has nothing to note.
    fn note_beside(&mut self, _beside: &str) {}

    /// The record of the app `app` this service keeps: what a connection the session admits — a
    /// REPL, an agent's task — reads and changes by the app, as it does Notes'. A service that
    /// keeps the session's one record of every tilolang app answers each app's (the shell's
    /// records service, which runs the apps the session hosts); `None` for a service that keeps
    /// no record, or none of that app.
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

    /// The outside origins this service keeps its records in step with
    /// (`docs/architecture/apps.md` §1, §8): for each, the app whose package declares it and
    /// the origin's name (`imap`, `youtube`). The session starts one connector process per app
    /// and origin, in the sandbox the app's signed manifest declares for it, and hands the
    /// service the connection back with [`Self::connect`].
    fn connectors(&self) -> Vec<Connector> {
        Vec::new()
    }

    /// Reach the connector of `origin`, at the far end of `connection`: what the service asks
    /// the outside service, and what it answers, travel there. The session calls it once per
    /// [`Connector`] the service names, when it has started the connector.
    ///
    /// # Errors
    /// The connection cannot be used.
    #[cfg(unix)]
    fn connect(&mut self, _origin: &str, _connection: std::os::fd::OwnedFd) -> Result<(), String> {
        Ok(())
    }

    /// Have `wake` called whenever an origin this service keeps in step answers, so the
    /// session's loop polls it and the answer lands in its records.
    fn wake_origins(&mut self, _wake: Wake) {}
}

/// An outside origin a service keeps its records in step with: the app whose package declares
/// it, and the origin's name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Connector {
    /// The app, by its package id (`so.tilo.lang.tube`): the launch whose manifest names the
    /// origin, and whose sandbox the connector runs in.
    pub app: String,
    /// The origin's name, as the manifest and the app's capabilities name it (`youtube`).
    pub origin: String,
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
    /// Every outside origin the services keep their records in step with, by app and origin:
    /// what the session starts connectors for.
    #[must_use]
    pub fn connectors(&self) -> Vec<Connector> {
        let mut connectors: Vec<Connector> = Vec::new();
        for service in &self.started {
            for connector in service.connectors() {
                if !connectors.contains(&connector) {
                    connectors.push(connector);
                }
            }
        }
        connectors
    }

    /// Have `wake` called whenever an origin a service keeps in step has an answer, so the
    /// session's loop polls the service and the answer lands in its records.
    pub fn wake_origins(&mut self, wake: &Wake) {
        for service in &mut self.started {
            if !service.connectors().is_empty() {
                service.wake_origins(std::sync::Arc::clone(wake));
            }
        }
    }

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

    /// The resource address a service publishes the app `app`'s record at,
    /// when it runs that app's model; see [`Service::hosted`].
    #[must_use]
    pub fn hosted(&self, app: &str) -> Option<String> {
        self.started.iter().find_map(|service| service.hosted(app))
    }

    /// Every app whose model one of the services runs, by the app, with the resource address it
    /// publishes its record at; see [`Service::served`].
    #[must_use]
    pub fn served(&self) -> Vec<(String, String)> {
        self.started
            .iter()
            .flat_map(|service| service.served())
            .collect()
    }

    /// The places the services were asked to open since this was last taken, each with the actor
    /// that asked; see [`Service::take_open`].
    #[must_use]
    pub fn take_open(&mut self) -> Vec<(tilo_actions::ActionInvoker, Open)> {
        let opened: Vec<(tilo_actions::ActionInvoker, Open)> = self
            .started
            .iter_mut()
            .flat_map(|service| service.take_open())
            .collect();
        if !opened.is_empty() {
            self.changed();
        }
        opened
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
        self.invoke_as_beside(invoker, resource, action, arguments, "")
    }

    /// [`Self::invoke_as`] with the place the ask is in named: the resource the place shows — the
    /// conversation of the turn an agent asks in — or empty for the place in front. A place the
    /// invocation opens stands beside it (`docs/designs/app-language/one-app-model.md` §3).
    ///
    /// # Errors
    /// The service's refusal.
    pub fn invoke_as_beside(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
        beside: &str,
    ) -> Result<Started, DoorError> {
        let address = ActionAddress::new(resource, action);
        let index = self
            .started
            .iter()
            .position(|service| {
                tilo_actions::door::resolve(&service.resources(), &address).is_some()
            })
            .ok_or_else(|| DoorError::Unknown(address.clone()))?;
        let service = self
            .started
            .get_mut(index)
            .ok_or_else(|| DoorError::Unknown(address.clone()))?;
        service.note_beside(beside);
        let started = service.invoke_as(invoker, resource, action, arguments);
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
        self.route(resource, action, arguments, false, None)
    }

    /// [`Self::invoke`] as `invoker` (an agent, a script), behind the same confirmation:
    /// what the change it makes is attributed to, where the door's caller is not the person.
    pub fn invoke_by(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        self.route(resource, action, arguments, false, Some(invoker))
    }

    /// [`Self::invoke`] after the person confirmed it.
    pub fn invoke_confirmed(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        self.route(resource, action, arguments, true, None)
    }

    fn route(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
        confirmed: bool,
        invoker: Option<&tilo_actions::ActionInvoker>,
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
        let service = self.started.get_mut(index)?;
        let started = match invoker {
            Some(invoker) => service.invoke_as(invoker, resource, action, arguments),
            None => service.invoke(resource, action, arguments),
        };
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

    /// How long until the soonest thing a service keeps is due: the session ends
    /// its wait then (see [`Service::wakes_in`]).
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        self.started
            .iter()
            .filter_map(|service| service.wakes_in())
            .min()
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

    fn wakes_in(&self) -> Option<std::time::Duration> {
        Self::wakes_in(self)
    }

    fn settle(&mut self) {
        Self::settle(self);
    }

    fn module(&self, app: &str) -> Option<std::sync::Arc<[u8]>> {
        Self::module(self, app)
    }

    fn hosted(&self, app: &str) -> Option<String> {
        Self::hosted(self, app)
    }

    fn served(&self) -> Vec<(String, String)> {
        Self::served(self)
    }

    #[cfg(unix)]
    fn connect(&mut self, origin: &str, connection: std::os::fd::OwnedFd) -> Result<(), String> {
        // The service that names `origin` takes it; a session with two is asked for the first.
        let mut connection = Some(connection);
        for service in &mut self.started {
            if !service
                .connectors()
                .iter()
                .any(|connector| connector.origin == origin)
            {
                continue;
            }
            let Some(given) = connection.take() else {
                break;
            };
            service.connect(origin, given)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
