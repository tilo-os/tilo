//! Outside origins on the platform: what keeps the resources of a type with an origin
//! (`type Message (origin: imap.message)`) in step with its service (convergence.md, "Outside
//! services"; `tilolang::origin`).
//!
//! An [`Origin`] is native code the instance runs beside the package: the connection to the
//! service, with the service's credentials, which package code never sees. Each frame the
//! instance polls it: it writes what the service said into the resources that hold its kinds
//! (one change by the origin, through [`tilolang_runtime::Runtime::apply_origin`]) and tells
//! the service what people changed in the fields people change. The outside effects package
//! code hands its capabilities (`outside("smtp.send", ...)`) are delivered to it from the
//! outbox, as every outside effect is.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value as Json;
use tilolang_runtime::meta::Meta;
use tilolang_runtime::{Delivery, OutsideSink};

use crate::background::Waker;
use crate::host::Host;

/// What a poll did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Polled {
    /// It wrote into the room: the instance hands the change to the door.
    pub wrote: bool,
    /// An effect it was handed has an outcome now: the outbox is settled again.
    pub settle: bool,
}

/// What an origin is polled with.
pub struct OriginCx<'a> {
    pub host: &'a Host,
    /// The package the instance runs.
    pub app: &'a str,
    /// Wakes the instance's frame loop when the service answers.
    pub waker: &'a Waker,
    /// The time now, as the runtime records it.
    pub now: String,
}

/// The platform's side of an outside service.
pub trait Origin {
    /// The origin's name: the first part of its kinds (`imap`).
    fn name(&self) -> &str;

    /// Whether the outside effects of `capability` (`smtp.send`) are the origin's to carry out.
    fn serves(&self, capability: &str) -> bool;

    /// Whether the origin has something to do: the service answered (and woke the instance),
    /// or people changed what the service is told. The instance polls it only then, so nothing
    /// runs while nothing changed.
    fn due(&self) -> bool;

    /// Take in what the service said and tell it what people changed.
    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled;

    /// An outside effect for one of the origin's capabilities: hand it to the service, or say
    /// what became of it since it was handed over.
    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery;
}

/// An origin as the instance holds it.
pub type SharedOrigin = Rc<RefCell<dyn Origin>>;

/// The origins of an instance, which it may gain after its runtime was set up.
pub type Origins = Rc<RefCell<Vec<SharedOrigin>>>;

/// The outside sink of an instance: each origin's capabilities go to it, the rest to the sink
/// it had.
pub struct RoutingSink {
    pub origins: Origins,
    pub rest: Box<dyn OutsideSink>,
}

impl OutsideSink for RoutingSink {
    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery {
        for origin in self.origins.borrow().iter() {
            // Busy answering a poll: asked again on the next settle. The instance polls its
            // origins and settles one after the other, so only a settle from within a poll
            // gets here, and nothing then obliges another settle: it is said.
            let Ok(mut origin) = origin.try_borrow_mut() else {
                tracing::warn!(capability, "an effect was handed to an origin busy polling");
                return Delivery::Pending;
            };
            if origin.serves(capability) {
                return origin.deliver(capability, payload);
            }
        }
        self.rest.deliver(capability, payload)
    }
}

/// A type of a loaded package that holds one of an origin's kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginType {
    pub app: String,
    /// The type's id.
    pub ty: String,
    /// The kind it holds: `imap.message`.
    pub kind: String,
    /// The id of each of its fields, by name.
    pub fields: BTreeMap<String, String>,
}

impl OriginType {
    /// The id of the field named `name`, when the type declares it.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

/// The types of `meta` that hold kinds of the origin `origin`.
#[must_use]
pub fn origin_types(meta: &Meta, origin: &str) -> Vec<OriginType> {
    meta.types
        .iter()
        .filter_map(|t| {
            let kind = t.origin.as_ref()?;
            (kind.split('.').next() == Some(origin)).then(|| OriginType {
                app: meta.app.clone(),
                ty: t.id.clone(),
                kind: kind.clone(),
                fields: t
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.id.clone()))
                    .collect(),
            })
        })
        .collect()
}

/// Whether any type of `meta` holds a kind of `origin`.
#[must_use]
pub fn uses_origin(meta: &Meta, origin: &str) -> bool {
    !origin_types(meta, origin).is_empty()
}
