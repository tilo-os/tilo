//! The session's document host for Rust apps' records, as a session service.
//!
//! One [`ModelService`] holds the records the host keeps for the apps of one
//! workspace, each of one resource kind its model crate declares, and
//! publishes them to the session's door as any service publishes what it
//! keeps: a reading a view is offered, and the actions it admits
//! (`docs/architecture/apps.md` §2, §3).
//!
//! An app's process reaches these over the one connection the launcher made
//! it, by the same addresses it would have used had it kept the record: the
//! record's resource address, and each item's. What it is offered is a
//! reading, never a document; what it submits is an action the model runs
//! here and this host commits as its invoker's step — the app holds no
//! document, key or transport, and the change is the session's.
//!
//! This is the Rust apps' twin of `tilolang_tilo::records_service::RecordService`:
//! that one runs a tilolang package's compiled module, this one a Rust model,
//! and both are session services the same door reaches.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Map, Value as Json};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_resource::Doc;
use tilo_service::{Service, Started};
use tilolang_runtime::engine::{Engine, EngineLimits};

use crate::module::Module;
use crate::record::{Invoker, Record, RecordError};

/// One record the host keeps, and where it is published.
struct Held {
    address: String,
    record: Record,
}

/// The app modules the host runs, and the engine they run on.
struct Modules {
    /// The engine every module runs on: the one the session's packages run on too.
    engine: Rc<Engine>,
    /// Each app's module, by the app's id.
    loaded: BTreeMap<String, Rc<Module>>,
}

impl Modules {
    fn new() -> Result<Self, String> {
        Ok(Self {
            engine: Rc::new(Engine::new(EngineLimits::default())?),
            loaded: BTreeMap::new(),
        })
    }

    /// `app`'s module, loaded from `wasm` the first time it is asked for.
    fn get(&mut self, app: &str, wasm: &[u8]) -> Result<Rc<Module>, RecordError> {
        if let Some(module) = self.loaded.get(app) {
            return Ok(Rc::clone(module));
        }
        let module = Rc::new(
            Module::load(app, wasm, Rc::clone(&self.engine))
                .map_err(|error| RecordError::Module(error.to_string()))?,
        );
        self.loaded.insert(app.to_owned(), Rc::clone(&module));
        Ok(module)
    }
}

/// The records the document host keeps for one workspace's apps, published
/// to the session's door.
///
/// An app's model runs in **its own WebAssembly module** (`docs/architecture/apps.md` §1), not
/// linked into the session: the app's code is about as trustworthy as a random web page's, and
/// this host is privileged. `ModelService` loads a module, asks it to open, read and change the
/// record, and keeps only what it checked.
pub struct ModelService {
    /// The modules the host runs.
    modules: Modules,
    /// Every record, by the app it belongs to and the resource address it is
    /// published at.
    records: Vec<Held>,
    /// The app each record belongs to, by its address: an action is admitted
    /// as that app's, and a reading is offered for it.
    owners: BTreeMap<String, String>,
    /// Who a change the door takes is recorded as, when the person acts here.
    person: String,
    /// Whether anything moved since [`Service::poll`] last asked.
    moved: bool,
}

impl ModelService {
    /// A host that runs the apps' modules, over no record yet, for `person`'s changes.
    pub fn new(person: impl Into<String>) -> Result<Self, String> {
        Ok(Self {
            modules: Modules::new()?,
            records: Vec::new(),
            owners: BTreeMap::new(),
            person: person.into(),
            moved: true,
        })
    }

    /// Keep the record of `kind` that `app` keeps at `address`, over `doc`, whose model runs in
    /// `wasm` — the app's own module. What the record held keeps its ids and history; a
    /// document that holds nothing gets the model's genesis.
    pub fn open(
        &mut self,
        app: &str,
        kind: &str,
        address: &str,
        doc: Doc,
        wasm: &[u8],
    ) -> Result<bool, RecordError> {
        let module = self.modules.get(app, wasm)?;
        let opened = Record::open(app, kind, address, doc, module)?;
        self.owners.insert(address.to_owned(), app.to_owned());
        self.records.push(Held {
            address: address.to_owned(),
            record: opened.record,
        });
        self.moved = true;
        Ok(opened.created)
    }

    /// Keep a brand-new record of `kind` that `app` keeps at `address`, its model running in
    /// `wasm`: the module writes its genesis.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn genesis(
        &mut self,
        app: &str,
        kind: &str,
        address: &str,
        wasm: &[u8],
    ) -> Result<(), RecordError> {
        self.open(app, kind, address, Doc::new(), wasm).map(|_| ())
    }

    /// The record published at `address`, for a host that checks what it holds.
    #[must_use]
    pub fn record(&self, address: &str) -> Option<&Record> {
        self.records
            .iter()
            .find(|held| held.address == address)
            .map(|held| &held.record)
    }

    /// The record published at `address`, mutably.
    pub fn record_mut(&mut self, address: &str) -> Option<&mut Record> {
        self.records
            .iter_mut()
            .find(|held| held.address == address)
            .map(|held| &mut held.record)
    }

    /// The record `address` names, or the record one of whose items it names:
    /// a reading or an action about an item goes to the record that holds it.
    fn holding(&mut self, resource: &str) -> Option<&mut Held> {
        self.records.iter_mut().find(|held| {
            held.address == resource || resource.starts_with(&format!("{}/", held.address))
        })
    }

    /// The app that keeps the record at `address`.
    #[must_use]
    pub fn owner(&self, address: &str) -> Option<&str> {
        self.owners.get(address).map(String::as_str)
    }
}

impl Service for ModelService {
    fn seated(&mut self, person: &str, _earlier: &[String]) {
        person.clone_into(&mut self.person);
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        // A record's tree is published as the record publishes it; this reads
        // that tree without mutating, so the offer is stable between moves.
        self.records
            .iter()
            .filter_map(|held| held.record.offers().ok())
            .collect()
    }

    /// The address this host publishes `app`'s record at: the session serves
    /// the app, so a launch it makes names the address and the app's process
    /// opens no record of its own (`docs/architecture/apps.md` §8 step 2).
    fn hosted(&self, app: &str) -> Option<String> {
        self.records
            .iter()
            .find(|held| {
                self.owners
                    .get(&held.address)
                    .is_some_and(|owner| owner == app)
            })
            .map(|held| held.address.clone())
    }

    /// Every app this host keeps a record of, with the address it is published at: what
    /// [`Self::hosted`] answers, for a host that asks about them all at once.
    fn served(&self) -> Vec<(String, String)> {
        self.records
            .iter()
            .filter_map(|held| {
                let app = self.owners.get(&held.address)?;
                Some((app.clone(), held.address.clone()))
            })
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Json> {
        let held = self.records.iter().find(|held| {
            held.address == resource || resource.starts_with(&format!("{}/", held.address))
        })?;
        held.record.read(resource)?.ok()
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
    ) -> Result<Started, DoorError> {
        let invoker = tilo_actions::ActionInvoker::actor(self.person.clone());
        self.invoke_as(&invoker, resource, action, arguments)
    }

    fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
    ) -> Result<Started, DoorError> {
        let writer = self.person.clone().into_bytes();
        let held = self
            .holding(resource)
            .ok_or_else(|| DoorError::Unknown(ActionAddress::new(resource, action)))?;
        let invoker = Invoker::of(invoker.clone(), Some(writer));
        let output = held
            .record
            .invoke(resource, action, arguments, &invoker)
            .map_err(|error| match error {
                RecordError::Door(refusal) => refusal,
                other => DoorError::Failed(other.to_string()),
            })?;
        self.moved = true;
        Ok(Started::Done(output))
    }

    fn poll(&mut self) -> bool {
        let mut moved = std::mem::take(&mut self.moved);
        for held in &mut self.records {
            moved |= held.record.take_moved();
        }
        moved
    }

    fn moved(&mut self) -> Option<Vec<String>> {
        let mut moved = Vec::new();
        for held in &mut self.records {
            if held.record.take_moved() {
                moved.push(held.address.clone());
            }
        }
        self.moved = false;
        Some(moved)
    }
}
