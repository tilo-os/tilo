#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test says what it needs in a sentence when what it needs is not there"
)]

//! The mail origin's harness: the Mail app's record shape over a real store, its origin over
//! Mail's engine, and a stand-in mail server on loopback. The package run here is the app's
//! types and its send, kept small: what a window of the app sees of its account and its mail is
//! what these records say.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_account_probe::stand_in::Started;
use tilolang_runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::origin::{Origin, OriginCx};
use tilolang_tilo::publish::PlanRequest;

use crate::engine::{AccountSource, EngineConnector, MailAccount};
use crate::origin::ImapOrigin;

/// The app the tests run.
pub const APP: &str = "so.tilo.mail.test";

/// Its source.
pub const SOURCE: &str = include_str!("../tests/app.tl");

/// The moment every poll happens at.
pub const NOW: &str = "2026-11-23T09:00:00Z";

/// How long a settle waits for the engine before it says the mail never came.
const SETTLE_BOUND: Duration = Duration::from_secs(30);
/// How long between two polls of a settle.
const BETWEEN_POLLS: Duration = Duration::from_millis(10);
/// How many polls in a row write nothing before the workspace has settled.
const QUIET_POLLS: u32 = 3;

/// One test's workspace: the package's records, the origin over Mail's engine, and the person
/// at it.
pub struct Rig {
    pub host: Host,
    pub origin: Box<ImapOrigin>,
    pub accounts: Arc<Mutex<Vec<MailAccount>>>,
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
        let engine = mail_engine::engine::live::LiveEngine::trusting(
            tilo_content::ContentStore::memory(),
            Arc::new(|| {}),
            certificates,
        )
        .expect("the mail engine opens");
        let (requests, events) = mail_engine::engine::spawn_with(Box::new(engine));
        let connector = EngineConnector::new(source, requests, events);
        Self {
            host,
            origin: Box::new(ImapOrigin::new(
                Box::new(connector),
                Arc::new(AtomicBool::new(true)),
            )),
            accounts: shared,
        }
    }

    /// One poll of the origin, as a frame does.
    ///
    /// # Panics
    /// Never: a poll that writes nothing is a poll.
    pub fn poll(&mut self) -> bool {
        let waker: tilolang_tilo::background::Waker = Arc::new(|| {});
        let cx = OriginCx {
            host: &self.host,
            app: APP,
            waker: &waker,
            now: NOW.to_owned(),
        };
        let polled = self.origin.poll(&cx);
        polled.wrote || polled.settle
    }

    /// Polls until nothing is left to do: a sync answered, a write taken.
    pub fn settle(&mut self) {
        let mut quiet = 0;
        for _ in 0..(SETTLE_BOUND.as_millis() / BETWEEN_POLLS.as_millis()) {
            let busy = self.poll() || self.origin.waiting();
            quiet = if busy { 0 } else { quiet + 1 };
            if quiet >= QUIET_POLLS {
                return;
            }
            std::thread::sleep(BETWEEN_POLLS);
        }
        panic!("the mail settles in {}s", SETTLE_BOUND.as_secs());
    }

    /// Polls until `reached` says the workspace shows what a test waits for: its server's mail
    /// taken in, a message gone, an account's trouble said.
    ///
    /// # Panics
    /// It never does: the mail settles in [`SETTLE_BOUND`].
    pub fn settle_until(&mut self, reached: impl Fn(&Self) -> bool) {
        let deadline = std::time::Instant::now() + SETTLE_BOUND;
        loop {
            self.poll();
            if reached(self) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the mail settles in {}s",
                SETTLE_BOUND.as_secs()
            );
            std::thread::sleep(BETWEEN_POLLS);
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
