#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test says what it needs in a sentence when what it needs is not there"
)]

//! The calendar origin's harness: a package's records over a real store, its origin over the
//! calendar engine, and the calendar servers the tests keep on loopback.

#[allow(dead_code, reason = "the harness holds more than one test asks it yet")]
pub mod fake_caldav;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tilolang_calendar::engine::{AccountSource, CalendarAccount, DirectConnector};
use tilolang_calendar::origin::CalDavOrigin;
use tilolang_runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::origin::{Origin, OriginCx};
use tilolang_tilo::publish::PlanRequest;

/// The app the tests run.
pub const APP: &str = "so.tilo.calendar.test";

/// Its source.
pub const SOURCE: &str = include_str!("../app.tl");

/// The moment every poll happens at.
pub const NOW: &str = "2026-11-23T09:00:00Z";

/// One test's workspace: the package's records, the origin over a calendar server, and the
/// person at it.
pub struct Rig {
    pub host: Host,
    pub origin: Box<CalDavOrigin>,
    pub accounts: Arc<Mutex<Vec<CalendarAccount>>>,
}

impl Rig {
    /// A workspace whose one account signs in to `server`.
    pub fn new(server: &fake_caldav::Server, address: &str, password: &str) -> Self {
        Self::with_accounts(vec![CalendarAccount {
            id: "caldav-account".into(),
            name: "Work".into(),
            server: server.url(),
            username: address.into(),
            password: password.into(),
            refused: None,
            read_only: false,
        }])
    }

    /// A workspace with the accounts given.
    pub fn with_accounts(accounts: Vec<CalendarAccount>) -> Self {
        let host = Host::new(actor(), || None).expect("the host opens");
        host.load_builtin(SOURCE, "").expect("the package compiles");
        let shared = Arc::new(Mutex::new(accounts));
        let read = Arc::clone(&shared);
        let source: AccountSource = Arc::new(move || {
            read.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        });
        let origin = CalDavOrigin::new(
            Box::new(DirectConnector::new(source)),
            Arc::new(AtomicBool::new(true)),
        );
        Self {
            host,
            origin: Box::new(origin),
            accounts: shared,
        }
    }

    /// One poll of the origin, as a frame does.
    pub fn poll(&mut self) {
        let waker: tilolang_tilo::background::Waker = Arc::new(|| {});
        let cx = OriginCx {
            host: &self.host,
            app: APP,
            waker: &waker,
            now: NOW.to_owned(),
        };
        self.origin.poll(&cx);
    }

    /// Polls until nothing is left to do: a sync answered, a write taken.
    /// Polls as frames do until everything asked has been taken in: a sync answered, a write
    /// taken.
    pub fn settle(&mut self) {
        for _ in 0..8 {
            self.poll();
        }
    }

    /// The root of the app's records.
    pub fn root(&self) -> ResourceRef {
        self.host.root_of(APP).expect("the app's root")
    }

    /// The id of the type `name`'s field `field`.
    pub fn field(&self, ty: &str, field: &str) -> String {
        let package = self.host.package(APP).expect("the package");
        package
            .meta
            .types
            .iter()
            .find(|declared| declared.name == ty)
            .and_then(|declared| declared.fields.iter().find(|known| known.name == field))
            .map_or_else(|| panic!("{ty} declares {field}"), |known| known.id.clone())
    }

    /// The items of the root's collection `field`.
    pub fn items(&self, field: &str) -> Vec<ResourceRef> {
        let field = self.field("Calendars", field);
        match self.host.runtime().read(&self.root(), &field) {
            Ok(Value::List(items)) => items
                .iter()
                .filter_map(|value| match value {
                    Value::Ref(r) => Some((*r.target).clone()),
                    _ => None,
                })
                .collect(),
            other => panic!("the collection reads {other:?}"),
        }
    }

    /// The calendars the workspace holds.
    pub fn calendars(&self) -> Vec<ResourceRef> {
        self.items("calendars")
    }

    /// The entries the workspace holds.
    pub fn events(&self) -> Vec<ResourceRef> {
        self.items("events")
    }

    /// What the field `field` of `at` says, as text.
    pub fn text(&self, ty: &str, at: &ResourceRef, field: &str) -> String {
        let field = self.field(ty, field);
        match self.host.runtime().read(at, &field).unwrap() {
            Value::Text(text) => text,
            Value::Opt(Some(held)) => match *held {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            },
            other => panic!("{field} reads {other:?}"),
        }
    }

    /// What the field `field` of `at` says, as a flag.
    pub fn flag(&self, ty: &str, at: &ResourceRef, field: &str) -> bool {
        let field = self.field(ty, field);
        matches!(
            self.host.runtime().read(at, &field).unwrap(),
            Value::Bool(true)
        )
    }

    /// The resource the field `field` of `at` names, where it names one.
    pub fn reference(&self, ty: &str, at: &ResourceRef, field: &str) -> Option<ResourceRef> {
        let field = self.field(ty, field);
        let value = self.host.runtime().read(at, &field).unwrap();
        let target = match value {
            Value::Ref(r) => r.target,
            Value::Opt(Some(held)) => match *held {
                Value::Ref(r) => r.target,
                _ => return None,
            },
            _ => return None,
        };
        Some((*target).clone())
    }

    /// The people the entry `at` names.
    pub fn attendees(&self, at: &ResourceRef) -> Vec<ResourceRef> {
        let field = self.field("Event", "attendees");
        match self.host.runtime().read(at, &field).unwrap() {
            Value::List(items) => items
                .into_iter()
                .filter_map(|value| match value {
                    Value::Ref(r) => Some((*r.target).clone()),
                    _ => None,
                })
                .collect(),
            other => panic!("the people read {other:?}"),
        }
    }

    /// Runs the action `name` on the resource `of` (or on the app's root, where it takes the
    /// whole workspace), with `args`.
    ///
    /// # Panics
    /// The action is refused, or the runtime traps.
    pub fn run(
        &self,
        ty: &str,
        name: &str,
        of: Option<&ResourceRef>,
        args: &[(&str, serde_json::Value)],
    ) {
        let package = self.host.package(APP).expect("the package");
        let kind = package.meta.kind_of(ty);
        let action = package
            .meta
            .actions
            .iter()
            .find(|declared| declared.name == name)
            .unwrap_or_else(|| panic!("the action {name}"))
            .id
            .clone();
        let args: serde_json::Map<String, serde_json::Value> = args
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        let resource = of.map_or_else(|| self.root().id_string(), ResourceRef::id_string);
        let request = PlanRequest {
            app: APP,
            kind: &kind,
            action_id: &action,
            resource: Some(&resource),
            args: &args,
            caller: None,
            from: None,
        };
        let actor = actor();
        let host = &self.host;
        if let Err(error) = host.carrying_out(&actor, || host.plan(&request)) {
            panic!("{name} is refused: {error}");
        }
    }
}

/// The person at the workspace.
fn actor() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// A `Time` argument, as tilolang reads one.
pub fn at(text: &str) -> serde_json::Value {
    serde_json::Value::String(text.to_owned())
}
