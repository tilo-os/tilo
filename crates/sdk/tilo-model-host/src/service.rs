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

use serde_json::{Map, Value as Json};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_resource::{Doc, Kinds};
use tilo_service::{Service, Started};

use crate::record::{Invoker, Record, RecordError};

/// One record the host keeps, and where it is published.
struct Held {
    address: String,
    record: Record,
}

/// The records the document host keeps for one workspace's apps, published
/// to the session's door.
pub struct ModelService {
    /// The resource types the host links, by kind: the model crates the
    /// session runs.
    kinds: Kinds,
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
    /// A host with the models `kinds` links, over no record yet, for
    /// `person`'s changes.
    #[must_use]
    pub fn new(kinds: Kinds, person: impl Into<String>) -> Self {
        Self {
            kinds,
            records: Vec::new(),
            owners: BTreeMap::new(),
            person: person.into(),
            moved: true,
        }
    }

    /// Keep the record of `kind` that `app` keeps at `address`, over `doc`:
    /// the genesis when `doc` holds nothing yet. What the record held keeps
    /// its ids and history ([`Record::open`]).
    ///
    /// # Errors
    /// The kind names no model this host links, or the document does not read
    /// as the model.
    pub fn open(
        &mut self,
        app: &str,
        kind: &str,
        address: &str,
        doc: Doc,
    ) -> Result<bool, RecordError> {
        let opened = Record::open(&self.kinds, kind, address, doc)?;
        self.owners.insert(address.to_owned(), app.to_owned());
        self.records.push(Held {
            address: address.to_owned(),
            record: opened.record,
        });
        self.moved = true;
        Ok(opened.created)
    }

    /// Keep a brand-new record of `kind` that `app` keeps at `address`.
    ///
    /// # Errors
    /// As [`Record::genesis`].
    pub fn genesis(&mut self, app: &str, kind: &str, address: &str) -> Result<(), RecordError> {
        let record = Record::genesis(&self.kinds, kind, address)?;
        self.owners.insert(address.to_owned(), app.to_owned());
        self.records.push(Held {
            address: address.to_owned(),
            record,
        });
        self.moved = true;
        Ok(())
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
