#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a test says what it needs in a sentence when what it needs is not there, and its bounds are constants counting down a settle"
)]

//! The mail origin's harness: the Mail app's record shape over a real store, its origin over
//! Mail's engine, and a stand-in mail server on loopback. The package run here is the app's
//! types and its send, kept small: what a window of the app sees of its account and its mail is
//! what these records say.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use tilo_account_probe::stand_in::Started;
use tilolang_runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::origin::{Origin, OriginCx};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::runtime::{Delivery, OutsideSink};

use crate::engine::{AccountSource, EngineConnector, MailAccount};
use crate::origin::ImapOrigin;

/// The app the tests run.
pub const APP: &str = "so.tilo.mail.test";

/// Its source.
pub const SOURCE: &str = include_str!("../tests/app.tl");

/// The moment every poll happens at.
pub const NOW: &str = "2026-11-23T09:00:00Z";

/// The bound a settle fails at: the workspace showing what the test waits for ends it, so this
/// only has to catch a settle that never gets there.
const SETTLE_BOUND: Duration = Duration::from_secs(300);
/// How long between two polls while no answer the engine sends is in flight, so a poll that
/// only rereads the workspace does not spin.
const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(20);

/// The engine's wake, as the connector's answer over its socket is: a ring says there is
/// something to take in, and a settle waiting on an answer blocks until it comes instead of
/// polling on a timer.
#[derive(Default)]
struct Wakes {
    generation: Mutex<u64>,
    rang: Condvar,
}

impl Wakes {
    /// The engine has an answer: the next settle takes it in.
    fn ring(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation = generation.wrapping_add(1);
        drop(generation);
        self.rang.notify_all();
    }

    fn generation(&self) -> u64 {
        *self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Block until the engine rings after `seen`, as the connector's socket would wake a frame:
    /// a settle on a busy machine waits as long as the engine needs, and only a wake that never
    /// comes is the failure.
    fn wait_past(&self, seen: u64, deadline: Instant, what: &str) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *generation == seen {
            let now = Instant::now();
            assert!(
                now < deadline,
                "{what} did not show in {}s",
                SETTLE_BOUND.as_secs()
            );
            let (next, timeout) = self
                .rang
                .wait_timeout(generation, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            generation = next;
            assert!(
                !timeout.timed_out() || *generation != seen,
                "{what} did not show in {}s",
                SETTLE_BOUND.as_secs()
            );
        }
        drop(generation);
    }
}

/// Where the host's settled outside effects go: the origin this test drives, as the instance's
/// routing sink hands them to the app's origins.
struct OriginSink {
    origin: Rc<RefCell<Box<ImapOrigin>>>,
}

impl OutsideSink for OriginSink {
    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery {
        let mut origin = self.origin.borrow_mut();
        if origin.serves(capability) {
            origin.deliver(capability, payload)
        } else {
            Delivery::Rejected(format!(
                "the mail connector does not carry out {capability}"
            ))
        }
    }
}

/// One test's workspace: the package's records, the origin over Mail's engine, and the person
/// at it.
pub struct Rig {
    pub host: Host,
    origin: Rc<RefCell<Box<ImapOrigin>>>,
    pub accounts: Arc<Mutex<Vec<MailAccount>>>,
    wakes: Arc<Wakes>,
}

impl Rig {
    /// A workspace whose one account signs in to `server` as `address` with `password`.
    pub fn new(server: &Started, address: &str, password: &str) -> Self {
        Self::with_accounts(
            vec![account(server, address, password)],
            vec![server.certificate.clone()],
        )
    }

    /// A workspace with the accounts given, its engine trusting `certificates` beside the
    /// system's authorities.
    pub fn with_accounts(
        accounts: Vec<MailAccount>,
        certificates: Vec<rustls::pki_types::CertificateDer<'static>>,
    ) -> Self {
        let host = Host::new(actor(), || None).expect("the host opens");
        host.load_builtin(SOURCE, "").expect("the package compiles");
        let shared = Arc::new(Mutex::new(accounts));
        let read = Arc::clone(&shared);
        let source: AccountSource = Box::new(move || {
            read.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        });
        // The engine wakes the origin when it has an answer, as a connector process does over
        // its socket: without it nothing that a server said is ever taken in. The same wake is
        // the worker's, which rings for every request it answers.
        let woken = Arc::new(AtomicBool::new(true));
        let wakes = Arc::new(Wakes::default());
        let awake = Arc::clone(&woken);
        let ringing = Arc::clone(&wakes);
        let ring: mail_engine::engine::Wake = Arc::new(move || {
            awake.store(true, std::sync::atomic::Ordering::Release);
            ringing.ring();
        });
        let engine = mail_engine::engine::live::LiveEngine::trusting(
            tilo_content::ContentStore::memory(),
            Arc::clone(&ring),
            certificates,
        )
        .expect("the mail engine opens");
        let (requests, events) = mail_engine::engine::spawn_waking(Box::new(engine), ring);
        let connector = EngineConnector::new(source, requests, events);
        let origin = Rc::new(RefCell::new(Box::new(ImapOrigin::new(
            Box::new(connector),
            woken,
        ))));
        // What the app's `outside()` effects settle to: the origin this test polls, as the
        // instance's sink hands them to its origins.
        host.runtime().set_sink(OriginSink {
            origin: Rc::clone(&origin),
        });
        Self {
            host,
            origin,
            accounts: shared,
            wakes,
        }
    }

    /// One poll of the origin and one settle of the outbox, as a frame does: what the app asked
    /// of the outside world is handed to the origin, and what the server said is taken in.
    ///
    /// # Panics
    /// Never: a poll that writes nothing is a poll, and the outbox's settle is the host's.
    pub fn poll(&mut self) -> bool {
        let waker: tilolang_tilo::background::Waker = Arc::new(|| {});
        let cx = OriginCx {
            host: &self.host,
            app: APP,
            waker: &waker,
            now: NOW.to_owned(),
        };
        let polled = self.origin.borrow_mut().poll(&cx);
        let settled = self
            .host
            .settle_effects()
            .unwrap_or_else(|error| panic!("the effects settle: {error}"));
        polled.wrote || polled.settle || settled.written > 0 || settled.owes_settle
    }

    /// Polls until nothing is left to do: a sync answered, a write taken. A poll with something
    /// in flight waits for the engine's wake, so a busy machine takes as long as it takes.
    pub fn settle(&mut self) {
        self.settle_until("the mail to be all taken in", |rig| {
            let origin = rig.origin.borrow();
            !origin.waiting() && origin.not_taken_in().is_none()
        });
    }

    /// Polls until `reached` says the workspace shows `what`: its server's mail taken in, a
    /// message gone, an account's trouble said. While an answer is in flight the wait blocks on
    /// the engine's wake; otherwise it polls again after a pause that grows, so a poll that only
    /// rereads the workspace does not spin. The bound is only the failure.
    pub fn settle_until(&mut self, what: &str, reached: impl Fn(&Self) -> bool) {
        let deadline = Instant::now()
            .checked_add(SETTLE_BOUND)
            .expect("the settle's bound fits in an Instant");
        let mut pause = POLL_MIN;
        loop {
            let seen = self.wakes.generation();
            self.poll();
            if reached(self) {
                return;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "{what} did not show in {}s",
                SETTLE_BOUND.as_secs()
            );
            if self.origin.borrow().waiting() {
                // An answer is on its way: blocking until it comes is the condition itself, and
                // no pause shortens the engine's work.
                self.wakes.wait_past(seen, deadline, what);
                pause = POLL_MIN;
            } else {
                std::thread::sleep(pause.min(deadline.saturating_duration_since(now)));
                pause = pause.saturating_mul(2).min(POLL_MAX);
            }
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

    /// The items of the collection `field` of `at`, or of the root when none is named.
    pub fn items_at(&self, at: &ResourceRef, ty: &str, field: &str) -> Vec<ResourceRef> {
        let field = self.field(ty, field);
        match self.host.runtime().read(at, &field) {
            Ok(Value::List(items)) => items
                .iter()
                .filter_map(|value| match value {
                    Value::Ref(r) => Some((*r.target).clone()),
                    _ => None,
                })
                .collect(),
            other => panic!("{field} reads {other:?}"),
        }
    }

    /// The items of the root's collection `field`.
    pub fn items(&self, field: &str) -> Vec<ResourceRef> {
        let root = self.root();
        self.items_at(&root, "Mail", field)
    }

    /// The accounts the workspace holds.
    pub fn accounts(&self) -> Vec<ResourceRef> {
        self.items("accounts")
    }

    /// The drafts the workspace holds.
    pub fn drafts(&self) -> Vec<ResourceRef> {
        self.items("drafts")
    }

    /// The conversations the account `at` holds.
    pub fn threads(&self, at: &ResourceRef) -> Vec<ResourceRef> {
        self.items_at(at, "Account", "threads")
    }

    /// The messages the conversation `at` holds.
    pub fn messages(&self, at: &ResourceRef) -> Vec<ResourceRef> {
        self.items_at(at, "Thread", "messages")
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

/// A mail server on loopback that takes `address`'s `password`, laid out as the servers
/// people's mail is on: what a person rents, stood in for.
///
/// # Panics
/// The stand-in cannot listen.
pub fn served(address: &str, password: &str) -> Started {
    tilo_account_probe::stand_in::started(tilo_account_probe::stand_in::Serving {
        layout: tilo_account_probe::stand_in::Layout::Plain,
        seeded: true,
        ..tilo_account_probe::stand_in::Serving::local(address, password)
    })
    .expect("the stand-in mail server listens")
}

/// The account `address` signs in to `server` with `password`.
pub fn account(server: &Started, address: &str, password: &str) -> MailAccount {
    MailAccount {
        config: mail_engine::model::AccountConfig {
            display_name: address.to_owned(),
            email: address.to_owned(),
            username: address.to_owned(),
            password: password.to_owned(),
            imap_host: "127.0.0.1".into(),
            imap_port: server.imap.port(),
            smtp_host: "127.0.0.1".into(),
            smtp_port: server.smtp.port(),
            auth: mail_engine::model::AuthMethod::Password,
            ..Default::default()
        },
        refused: None,
    }
}

/// The person at the workspace.
fn actor() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}
