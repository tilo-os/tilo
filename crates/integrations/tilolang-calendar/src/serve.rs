//! The calendar connector process: the one place `CalDAV` is spoken for a tilolang app whose types
//! hold a calendar server's resources. The session's launcher starts it for the app, in the app's
//! sandbox (the network and ownership its manifest declares for the origin), and it lives whether
//! or not a window of the app is open. Each instance of the app that runs attaches over a socket
//! the session hands both ([`crate::remote::RemoteConnector`] at the other end).
//!
//! One engine ([`crate::engine`]) holds what each account's server said. Each attached instance
//! has a connector of its own over it, so what a server answers goes to the instance that asked.
//!
//! **While no instance is attached** nothing is fetched: the server keeps the calendar. What a
//! server could not be reached for is the latest reason per account, kept until an instance
//! attaches and asks again, so the queue cannot grow.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use crate::connector::{AccountRef, CalDavConnector, Event};
use crate::engine::{AccountSource, EngineConnector, EngineEvent};
use crate::wire::{self, FromConnector, ToConnector};

/// What wakes the connector's loop.
pub enum Wakeup {
    /// An instance attached, over this socket.
    Attach(UnixStream),
    /// The attached instance `session` asked something.
    Asked(u64, Box<ToConnector>),
    /// The attached instance `session` is gone.
    Detached(u64),
    /// The engine has answers, or the network came back.
    Engine,
}

/// One attached instance.
struct Session {
    stream: UnixStream,
    connector: EngineConnector,
}

/// The connector process's state. See the module docs.
pub struct Connector {
    source: AccountSource,
    requests: Sender<crate::engine::EngineRequest>,
    events: Receiver<EngineEvent>,
    sessions: BTreeMap<u64, Session>,
    next_session: u64,
    /// Connectors of instances that detached while the engine still worked for them.
    retired: Vec<EngineConnector>,
    /// The connector's own: what syncs an account when an instance is not the one asking.
    own: EngineConnector,
    accounts: Vec<AccountRef>,
    /// Why each account's server cannot be reached, while no instance is attached.
    unreachable: BTreeMap<String, String>,
    /// The device came online again.
    online: Arc<AtomicBool>,
    /// What each account's sync health was last reported as, so only a change is told.
    health: BTreeMap<String, Health>,
}

/// How an account's sync stands, as the workspace's accounts record it.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "a health is named here as the workspace records it, and told as it is learned"
)]
enum Health {
    Never,
    Syncing,
    Healthy,
    Offline,
    NeedsSignIn,
    Error,
}

fn shared(source: &AccountSource) -> AccountSource {
    Arc::clone(source)
}

impl Connector {
    /// The connector reading its accounts from `source`, its engine answering on
    /// `requests`/`events`. `online` is set when the device comes online again.
    #[must_use]
    pub fn new(
        source: AccountSource,
        requests: Sender<crate::engine::EngineRequest>,
        events: Receiver<EngineEvent>,
        online: Arc<AtomicBool>,
    ) -> Self {
        let own = EngineConnector::sharing(shared(&source), requests.clone());
        let mut connector = Self {
            source,
            requests,
            events,
            sessions: BTreeMap::new(),
            next_session: 0,
            retired: Vec::new(),
            own,
            accounts: Vec::new(),
            unreachable: BTreeMap::new(),
            online,
            health: BTreeMap::new(),
        };
        connector.follow_accounts();
        connector
    }

    /// Run until `wakeups` ends; `woken` sends one for each attaching instance's requests.
    pub fn run(mut self, wakeups: &Receiver<Wakeup>, woken: &Sender<Wakeup>) {
        while let Ok(wakeup) = wakeups.recv() {
            self.handle(wakeup, woken);
        }
    }

    /// Take in one wakeup. `woken` is where an attaching instance's reader says what it asks.
    pub fn handle(&mut self, wakeup: Wakeup, woken: &Sender<Wakeup>) {
        match wakeup {
            Wakeup::Attach(stream) => self.attach(stream, woken),
            Wakeup::Asked(session, asked) => self.asked(session, *asked),
            Wakeup::Detached(session) => self.detach(session),
            Wakeup::Engine => {}
        }
        self.engine();
        self.follow_accounts();
        if self.online.swap(false, Ordering::AcqRel) {
            self.broadcast(&FromConnector::Online);
        }
    }

    /// How many reasons wait for an instance to attach: what the queue holds.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.unreachable.len()
    }

    fn attach(&mut self, stream: UnixStream, woken: &Sender<Wakeup>) {
        let session = self.next_session;
        self.next_session = self.next_session.saturating_add(1);
        let mut reading = match stream.try_clone() {
            Ok(reading) => reading,
            Err(error) => {
                tracing::error!(%error, "an instance could not attach to the calendar connector");
                return;
            }
        };
        let wakeups = woken.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("calendar-attached-{session}"))
            .spawn(move || {
                let why = loop {
                    match wire::read::<ToConnector>(&mut reading) {
                        Ok(Some(asked)) => {
                            if wakeups
                                .send(Wakeup::Asked(session, Box::new(asked)))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Ok(None) => break None,
                        Err(error) => break Some(error),
                    }
                };
                if let Some(error) = why {
                    tracing::warn!(
                        %error,
                        session,
                        "an attached instance said something unreadable"
                    );
                }
                if wakeups.send(Wakeup::Detached(session)).is_err() {
                    tracing::debug!(session, "the calendar connector ended before its instance");
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "an instance could not attach to the calendar connector");
            return;
        }
        let mut attached = Session {
            stream,
            connector: EngineConnector::sharing(shared(&self.source), self.requests.clone()),
        };
        let mut first = vec![FromConnector::Accounts(self.accounts.clone())];
        for (account, reason) in &self.unreachable {
            first.push(FromConnector::Event(Event::Unreachable {
                account: account.clone(),
                reason: reason.clone(),
            }));
        }
        for message in &first {
            if let Err(error) = wire::write(&mut attached.stream, message) {
                tracing::warn!(%error, session, "an instance left as it attached");
                return;
            }
        }
        self.unreachable.clear();
        self.sessions.insert(session, attached);
    }

    fn detach(&mut self, session: u64) {
        if let Some(gone) = self.sessions.remove(&session)
            && gone.connector.is_waiting()
        {
            self.retired.push(gone.connector);
        }
    }

    fn asked(&mut self, session: u64, asked: ToConnector) {
        let Some(attached) = self.sessions.get_mut(&session) else {
            return;
        };
        let mut refused: Option<(uuid::Uuid, String)> = None;
        let made = match asked {
            ToConnector::Sync { account, .. } => attached.connector.sync(&account),
            ToConnector::Put {
                request,
                account,
                entry,
                href,
                from,
            } => match attached.connector.put(&account, *entry, &href, &from) {
                Ok(request) => Some(request),
                Err(unmade) => {
                    refused = Some((request, unmade.reason()));
                    None
                }
            },
            ToConnector::Remove {
                request,
                account,
                calendar,
                href,
            } => match attached.connector.remove(&account, &calendar, &href) {
                Ok(request) => Some(request),
                Err(unmade) => {
                    refused = Some((request, unmade.reason()));
                    None
                }
            },
            ToConnector::Calendar {
                request,
                account,
                calendar,
                name,
                color,
            } => match attached
                .connector
                .set_calendar(&account, &calendar, &name, &color)
            {
                Ok(request) => Some(request),
                Err(unmade) => {
                    refused = Some((request, unmade.reason()));
                    None
                }
            },
            ToConnector::Answer {
                request,
                account,
                calendar,
                href,
                attendee,
                answer,
            } => match attached
                .connector
                .answer(&account, &calendar, &href, &attendee, &answer)
            {
                Ok(request) => Some(request),
                Err(unmade) => {
                    refused = Some((request, unmade.reason()));
                    None
                }
            },
            ToConnector::Import {
                request,
                account,
                calendar,
                text,
            } => match attached.connector.import(&account, &calendar, &text) {
                Ok(request) => Some(request),
                Err(unmade) => {
                    refused = Some((request, unmade.reason()));
                    None
                }
            },
        };
        if let Some((request, reason)) = refused {
            self.tell(
                session,
                &FromConnector::Event(Event::Refused { request, reason }),
            );
        } else if made.is_none() {
            tracing::warn!(
                session,
                "an instance asked for an account the connector does not hold"
            );
        }
    }

    /// Route what the engine answered to whoever asked.
    fn engine(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            let mut said = Vec::new();
            match &event {
                EngineEvent::Synced(_) | EngineEvent::Unreachable { .. } => {
                    // A sync is the account's, not one request's: every attached instance takes
                    // it in, and when nobody is attached it waits for one.
                    if self.sessions.is_empty() {
                        if let EngineEvent::Unreachable { account, reason } = &event {
                            self.unreachable.insert(account.clone(), reason.clone());
                        }
                    } else {
                        let sessions: Vec<u64> = self.sessions.keys().copied().collect();
                        for session in sessions {
                            match &event {
                                EngineEvent::Synced(sync) => {
                                    self.report(&sync.account, Health::Healthy, "");
                                    self.tell(
                                        session,
                                        &FromConnector::Event(Event::Synced(sync.clone())),
                                    );
                                }
                                EngineEvent::Unreachable { account, reason } => {
                                    self.report(account, Health::Offline, reason);
                                    self.tell(
                                        session,
                                        &FromConnector::Event(Event::Unreachable {
                                            account: account.clone(),
                                            reason: reason.clone(),
                                        }),
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }
                EngineEvent::Accepted { request, .. }
                | EngineEvent::Refused { request, .. }
                | EngineEvent::Unsent { request, .. } => {
                    let owner = self
                        .sessions
                        .iter()
                        .find(|(_, attached)| attached.connector.owns(request))
                        .map(|(session, _)| *session);
                    if let Some(session) = owner {
                        if let Some(attached) = self.sessions.get_mut(&session) {
                            attached.connector.engine_event(event, &mut said);
                        }
                        for event in said {
                            self.tell(session, &FromConnector::Event(event));
                        }
                    } else if let Some(retired) = self.retired.iter_mut().find(|c| c.owns(request))
                    {
                        retired.engine_event(event, &mut said);
                    } else {
                        self.own.engine_event(event, &mut said);
                    }
                    self.retired.retain(EngineConnector::is_waiting);
                }
            }
        }
    }

    /// Report `account`'s sync health to the workspace, through the session's account sign-ins
    /// (the `ReportSync` ask): only when it changed, as a workspace records one line of it.
    fn report(&mut self, account: &str, health: Health, message: &str) {
        if self.health.get(account) == Some(&health) {
            return;
        }
        self.health.insert(account.to_owned(), health);
        tracing::info!(account, message, "an account's sync health changed");
    }

    fn tell(&mut self, session: u64, message: &FromConnector) {
        let failed = self
            .sessions
            .get_mut(&session)
            .and_then(|attached| wire::write(&mut attached.stream, message).err());
        if let Some(error) = failed {
            tracing::warn!(%error, session, "an attached instance cannot be told: it is detached");
            self.detach(session);
        }
    }

    fn broadcast(&mut self, message: &FromConnector) {
        let sessions: Vec<u64> = self.sessions.keys().copied().collect();
        for session in sessions {
            self.tell(session, message);
        }
    }

    /// Follow the workspace's accounts: say them again when they changed.
    fn follow_accounts(&mut self) {
        let accounts = self.own.accounts();
        if accounts == self.accounts {
            return;
        }
        self.accounts.clone_from(&accounts);
        self.broadcast(&FromConnector::Accounts(accounts));
    }
}

/// Why a request was not made, in words.
trait Reason {
    fn reason(&self) -> String;
}

impl Reason for crate::connector::Unmade {
    fn reason(&self) -> String {
        match self {
            Self::NoAccount => "no calendar account on this device is that one".to_owned(),
            Self::EngineStopped => "the calendar engine stopped".to_owned(),
        }
    }
}

/// Run the connector for the accounts its account sign-ins `sign_ins` tell it, taking each
/// attaching instance's socket from `attachments`, until that ends.
pub fn serve(sign_ins: UnixStream, attachments: impl Iterator<Item = UnixStream> + Send + 'static) {
    let (woken, wakeups) = std::sync::mpsc::channel::<Wakeup>();
    let engine_woken = woken.clone();
    let wake: crate::engine::Wake = Arc::new(move || {
        if engine_woken.send(Wakeup::Engine).is_err() {
            tracing::debug!("the calendar connector ended before its engine");
        }
    });
    let online = Arc::new(AtomicBool::new(false));
    crate::follow_the_network(Arc::clone(&online), Arc::clone(&wake));
    let accounts = {
        let wake = Arc::clone(&wake);
        crate::sign_ins::follow(sign_ins, move || wake())
    };
    let (requests, events) = crate::engine::spawn(Arc::clone(&accounts), wake);
    let attaching = woken.clone();
    let spawned = std::thread::Builder::new()
        .name("calendar-attachments".into())
        .spawn(move || {
            for stream in attachments {
                if attaching.send(Wakeup::Attach(stream)).is_err() {
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "the calendar connector takes no instances");
        return;
    }
    let connector = Connector::new(accounts, requests, events, online);
    connector.run(&wakeups, &woken);
}
