//! The mail connector process: the one place IMAP and SMTP are spoken for a tilolang app whose
//! types hold a mail server's resources. The session's launcher starts it for the app, in the
//! app's sandbox (the network and custody its manifest declares for the origin), and it lives
//! whether or not a window of the app is open. Each instance of the app that runs attaches over
//! a socket the session hands both ([`crate::remote::RemoteConnector`] at the other end).
//!
//! One engine (`mail::engine`) holds the connections. Each attached instance has a connector of
//! its own over it, which knows what that instance's record holds, so what a server answers goes
//! to the instance that asked. The connector also watches each account's inbox itself.
//!
//! **While no instance is attached** nothing is fetched: the server keeps the mail. What the
//! servers say on their own is kept until one attaches, and the queue cannot grow without bound
//! or lose anything: new mail is one mark per account (the instance asks what changed when it
//! attaches, and the server holds every message), why a server cannot be reached is the latest
//! reason per account, and the outcome of a message an instance handed over and then detached
//! from is kept per message until an instance asks for it. So the queue holds at most two
//! entries per account and one per message sent.
//!
//! **A message is sent once.** Sends are kept by their content: one asked for again while it is
//! being sent, or after, is answered with its outcome rather than sent twice; only one that
//! never reached its server (unreachable) is sent again when asked.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use mail::engine::{EngineEvent, EngineRequest};

use crate::connector::{AccountRef, Event, MailConnector, Request};
use crate::engine::{AccountSource, EngineConnector};
use crate::wire::{self, FromConnector, ToConnector};

/// What wakes the connector's loop.
pub enum Wakeup {
    /// An instance attached, over this socket.
    Attach(UnixStream),
    /// The attached instance `session` asked something.
    Asked(u64, ToConnector),
    /// The attached instance `session` is gone.
    Detached(u64),
    /// The engine has answers, or the network came back.
    Engine,
}

/// How a send ended, as the server said.
#[derive(Clone)]
enum Sent {
    Accepted,
    Refused(String),
    Unsent(String),
}

impl Sent {
    fn event(&self, request: Request) -> Event {
        match self {
            Self::Accepted => Event::Accepted(request),
            Self::Refused(reason) => Event::Refused {
                request,
                reason: reason.clone(),
            },
            Self::Unsent(reason) => Event::Unsent {
                request,
                reason: reason.clone(),
            },
        }
    }
}

/// A message being sent, or sent: its outcome once known, and who waits for it (an attached
/// instance and the id it asked under).
#[derive(Default)]
struct Sending {
    outcome: Option<Sent>,
    waiters: Vec<(u64, Request)>,
}

/// One attached instance.
struct Session {
    stream: UnixStream,
    connector: EngineConnector,
    /// The instance's id of each request this connector made for it, by the engine's id.
    asked: HashMap<Request, Request>,
}

/// The connector process's state. See the module docs.
pub struct Connector {
    source: Rc<AccountSource>,
    requests: Sender<EngineRequest>,
    events: Receiver<EngineEvent>,
    sessions: BTreeMap<u64, Session>,
    next_session: u64,
    /// Connectors of instances that detached while the engine still worked for them.
    retired: Vec<EngineConnector>,
    /// The connector's own: what watches each account's inbox.
    own: EngineConnector,
    watched: BTreeSet<String>,
    accounts: Vec<AccountRef>,
    /// New mail per account, while no instance is attached.
    arrived: BTreeSet<String>,
    /// Why each account's server cannot be reached, while no instance is attached.
    unreachable: BTreeMap<String, String>,
    /// Each message being sent, by its content.
    sends: HashMap<String, Sending>,
    /// The content of each send the engine is working on, by the engine's id.
    send_of: HashMap<Request, String>,
    /// The device came online again.
    online: Arc<AtomicBool>,
}

fn shared(source: &Rc<AccountSource>) -> AccountSource {
    let source = Rc::clone(source);
    Box::new(move || source())
}

impl Connector {
    /// The connector reading its accounts from `source`, its engine answering on
    /// `requests`/`events`. `online` is set when the device comes online again.
    #[must_use]
    pub fn new(
        source: AccountSource,
        requests: Sender<EngineRequest>,
        events: Receiver<EngineEvent>,
        online: Arc<AtomicBool>,
    ) -> Self {
        let source = Rc::new(source);
        let own = EngineConnector::sharing(shared(&source), requests.clone());
        let mut connector = Self {
            source,
            requests,
            events,
            sessions: BTreeMap::new(),
            next_session: 0,
            retired: Vec::new(),
            own,
            watched: BTreeSet::new(),
            accounts: Vec::new(),
            arrived: BTreeSet::new(),
            unreachable: BTreeMap::new(),
            sends: HashMap::new(),
            send_of: HashMap::new(),
            online,
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
            Wakeup::Asked(session, asked) => self.asked(session, asked),
            Wakeup::Detached(session) => self.detach(session),
            Wakeup::Engine => {}
        }
        self.engine();
        self.follow_accounts();
        if self.online.swap(false, Ordering::AcqRel) {
            self.broadcast(&FromConnector::Online);
        }
    }

    /// How many messages wait for an instance to attach: what the queue holds.
    #[must_use]
    pub fn queued(&self) -> usize {
        let settled = self
            .sends
            .values()
            .filter(|sending| sending.outcome.is_some())
            .count();
        [self.arrived.len(), self.unreachable.len(), settled]
            .into_iter()
            .sum()
    }

    fn attach(&mut self, stream: UnixStream, woken: &Sender<Wakeup>) {
        let session = self.next_session;
        self.next_session = self.next_session.saturating_add(1);
        let mut reading = match stream.try_clone() {
            Ok(reading) => reading,
            Err(error) => {
                tracing::error!(%error, "an instance could not attach to the mail connector");
                return;
            }
        };
        let wakeups = woken.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("mail-attached-{session}"))
            .spawn(move || {
                let why = loop {
                    match wire::read::<ToConnector>(&mut reading) {
                        Ok(Some(asked)) => {
                            // The loop is gone only when the connector ends.
                            if wakeups.send(Wakeup::Asked(session, asked)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break None,
                        Err(error) => break Some(error),
                    }
                };
                if let Some(error) = why {
                    tracing::warn!(%error, session, "an attached instance said something unreadable");
                }
                if wakeups.send(Wakeup::Detached(session)).is_err() {
                    tracing::debug!(session, "the mail connector ended before its instance");
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "an instance could not attach to the mail connector");
            return;
        }
        let mut attached = Session {
            stream,
            connector: EngineConnector::sharing(shared(&self.source), self.requests.clone()),
            asked: HashMap::new(),
        };
        let mut first = vec![FromConnector::Accounts(self.accounts.clone())];
        // What the servers said while nobody was attached: given up only once it is told.
        for account in &self.arrived {
            first.push(FromConnector::Event(Event::Arrived {
                account: account.clone(),
            }));
        }
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
        self.arrived.clear();
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
        if let ToConnector::Send {
            request,
            account,
            message,
        } = asked
        {
            self.send(session, request, &account, message);
            return;
        }
        let Some(attached) = self.sessions.get_mut(&session) else {
            return;
        };
        let write = matches!(asked, ToConnector::Flag { .. } | ToConnector::Move { .. });
        let (host, engine) = match asked {
            ToConnector::Changes {
                request,
                account,
                mailbox,
            } => (request, attached.connector.changes(&account, &mailbox)),
            ToConnector::Flag {
                request,
                account,
                key,
                unread,
                flagged,
            } => (
                request,
                attached.connector.flag(&account, &key, unread, flagged),
            ),
            ToConnector::Move {
                request,
                account,
                key,
                mailbox,
            } => (
                request,
                attached.connector.move_to(&account, &key, &mailbox),
            ),
            ToConnector::Search {
                request,
                account,
                text,
            } => (request, attached.connector.search(&account, &text)),
            ToConnector::Watch { account } => {
                if self.watched.insert(account.clone()) {
                    self.own.watch(&account);
                }
                return;
            }
            ToConnector::Send { .. } => return,
        };
        match engine {
            // Only a write is answered under its id (taken or refused); what a fetch or a search
            // found names its account.
            Some(engine) if write => {
                attached.asked.insert(engine, host);
            }
            Some(_) => {}
            None => {
                let refused = FromConnector::Event(Event::Refused {
                    request: host,
                    reason: "the account is not one of the settings' mail accounts".to_owned(),
                });
                self.tell(session, &refused);
            }
        }
    }

    /// Send `message` for the instance `session`, which knows it as `request`: once, whoever
    /// asks and however often.
    fn send(
        &mut self,
        session: u64,
        request: Request,
        account: &str,
        message: crate::connector::Outgoing,
    ) {
        let key = match serde_json::to_string(&(account, &message)) {
            Ok(key) => key,
            Err(error) => {
                let refused = Event::Refused {
                    request,
                    reason: format!("the message cannot be kept: {error}"),
                };
                self.tell(session, &FromConnector::Event(refused));
                return;
            }
        };
        let known = self.sends.get(&key).map(|sending| sending.outcome.clone());
        match known {
            // Under way: told when it ends.
            Some(None) => {
                if let Some(sending) = self.sends.get_mut(&key) {
                    sending.waiters.push((session, request));
                }
                return;
            }
            // It never reached its server: it goes again.
            Some(Some(Sent::Unsent(_))) | None => {}
            // Done: the outcome, and it is not kept further.
            Some(Some(outcome)) => {
                self.sends.remove(&key);
                self.tell(session, &FromConnector::Event(outcome.event(request)));
                return;
            }
        }
        let Some(attached) = self.sessions.get_mut(&session) else {
            return;
        };
        match attached.connector.send(account, message) {
            Ok(engine) => {
                self.send_of.insert(engine, key.clone());
                self.sends.insert(
                    key,
                    Sending {
                        outcome: None,
                        waiters: vec![(session, request)],
                    },
                );
            }
            Err(crate::connector::Unmade::NoAccount) => {
                let refused = Event::Refused {
                    request,
                    reason: format!("no mail account {account} in the settings"),
                };
                self.tell(session, &FromConnector::Event(refused));
            }
            // The engine never got it: it waits to go online.
            Err(crate::connector::Unmade::EngineStopped) => {
                let unsent = Event::Unsent {
                    request,
                    reason: "the mail engine stopped".to_owned(),
                };
                self.tell(session, &FromConnector::Event(unsent));
            }
        }
    }

    /// Route what the engine answered to whoever asked.
    fn engine(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            let id = event.request_id();
            let mut said = Vec::new();
            let owner = self
                .sessions
                .iter()
                .find(|(_, attached)| attached.connector.owns(&id))
                .map(|(session, _)| *session);
            if let Some(session) = owner {
                if let Some(attached) = self.sessions.get_mut(&session) {
                    attached.connector.engine_event(event, &mut said);
                }
                for event in said {
                    self.answer(Some(session), event);
                }
                continue;
            }
            if let Some(retired) = self.retired.iter_mut().find(|c| c.owns(&id)) {
                retired.engine_event(event, &mut said);
            } else {
                self.own.engine_event(event, &mut said);
            }
            self.retired.retain(EngineConnector::is_waiting);
            for event in said {
                self.answer(None, event);
            }
        }
    }

    /// Say `event` to the instance `session` that asked, or, when nobody attached asked, to
    /// every instance attached, or keep it until one attaches.
    fn answer(&mut self, session: Option<u64>, event: Event) {
        let finished = match &event {
            Event::Accepted(request) => Some((*request, Sent::Accepted)),
            Event::Refused { request, reason } => Some((*request, Sent::Refused(reason.clone()))),
            Event::Unsent { request, reason } => Some((*request, Sent::Unsent(reason.clone()))),
            _ => None,
        };
        if let Some((engine, outcome)) = finished {
            if let Some(key) = self.send_of.remove(&engine) {
                self.sent(&key, outcome);
                return;
            }
            // A flag or a move: the instance that asked, under its own id.
            let Some(session) = session else {
                return;
            };
            let host = self
                .sessions
                .get_mut(&session)
                .and_then(|attached| attached.asked.remove(&engine));
            if let Some(host) = host {
                self.tell(session, &FromConnector::Event(outcome.event(host)));
            }
            return;
        }
        match (session, event) {
            (Some(session), event) => self.tell(session, &FromConnector::Event(event)),
            (None, Event::Arrived { account }) if self.sessions.is_empty() => {
                self.arrived.insert(account);
            }
            (None, Event::Unreachable { account, reason }) if self.sessions.is_empty() => {
                self.unreachable.insert(account, reason);
            }
            (None, event @ (Event::Arrived { .. } | Event::Unreachable { .. })) => {
                self.broadcast(&FromConnector::Event(event));
            }
            // What answered an instance that is gone: its record catches up when it attaches.
            (None, _) => {}
        }
    }

    /// The send `key` ended with `outcome`: tell whoever waits, and keep it for one who asks
    /// later when nobody did.
    fn sent(&mut self, key: &str, outcome: Sent) {
        let Some(mut sending) = self.sends.remove(key) else {
            return;
        };
        let mut told = false;
        for (session, request) in std::mem::take(&mut sending.waiters) {
            if self.sessions.contains_key(&session) {
                self.tell(session, &FromConnector::Event(outcome.event(request)));
                told = true;
            }
        }
        if !told {
            sending.outcome = Some(outcome);
            self.sends.insert(key.to_owned(), sending);
        }
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

    /// Follow the settings' accounts: say them again when they changed, and watch each inbox.
    fn follow_accounts(&mut self) {
        let accounts = self.own.accounts();
        if accounts == self.accounts {
            return;
        }
        for account in &accounts {
            if self.watched.insert(account.id.clone()) {
                self.own.watch(&account.id);
            }
        }
        self.accounts.clone_from(&accounts);
        self.broadcast(&FromConnector::Accounts(accounts));
    }
}

/// Run the connector for the settings' accounts under `config_root`, taking each attaching
/// instance's socket from `attachments`, until that ends.
pub fn serve(
    config_root: Option<std::path::PathBuf>,
    attachments: impl Iterator<Item = UnixStream> + Send + 'static,
) {
    let (woken, wakeups) = std::sync::mpsc::channel::<Wakeup>();
    let engine_woken = woken.clone();
    let wake: mail::engine::Wake = Arc::new(move || {
        if engine_woken.send(Wakeup::Engine).is_err() {
            tracing::debug!("the mail connector ended before its engine");
        }
    });
    let online = Arc::new(AtomicBool::new(false));
    crate::follow_the_network(Arc::clone(&online), Arc::clone(&wake));
    let (requests, events) = mail::engine::spawn(tilo_content::ContentStore::memory(), wake);
    let attaching = woken.clone();
    let spawned = std::thread::Builder::new()
        .name("mail-attachments".into())
        .spawn(move || {
            for stream in attachments {
                if attaching.send(Wakeup::Attach(stream)).is_err() {
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "the mail connector takes no instances");
        return;
    }
    let connector = Connector::new(
        crate::settings_accounts(config_root),
        requests,
        events,
        online,
    );
    connector.run(&wakeups, &woken);
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use mail::engine::EngineRequestKind;
    use mail::model::AccountConfig;

    use super::*;
    use crate::connector::Outgoing;

    const ME: &str = "sam@rivera.example";

    /// A connector over an engine the test plays: what it was asked arrives on `asked`, and
    /// the test answers on `answer`.
    struct Rig {
        connector: Connector,
        asked: Receiver<EngineRequest>,
        answer: Sender<EngineEvent>,
        woken: Sender<Wakeup>,
        wakeups: Receiver<Wakeup>,
    }

    fn rig() -> Rig {
        let (requests, asked) = channel();
        let (answer, events) = channel();
        let (woken, wakeups) = channel();
        let source: AccountSource = Box::new(|| {
            vec![AccountConfig {
                email: ME.into(),
                ..AccountConfig::default()
            }]
        });
        let connector = Connector::new(source, requests, events, Arc::default());
        Rig {
            connector,
            asked,
            answer,
            woken,
            wakeups,
        }
    }

    impl Rig {
        /// An instance attaching: its end of the socket, reading with a deadline.
        fn attach(&mut self) -> UnixStream {
            let (ours, theirs) = UnixStream::pair().unwrap();
            ours.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            self.connector.handle(Wakeup::Attach(theirs), &self.woken);
            ours
        }

        /// Take in what the attached instances asked, as the loop does.
        fn turn(&mut self) {
            while let Ok(wakeup) = self.wakeups.recv_timeout(Duration::from_millis(200)) {
                self.connector.handle(wakeup, &self.woken);
            }
            self.connector.handle(Wakeup::Engine, &self.woken);
        }

        /// The engine requests made since last asked.
        fn engine_requests(&self) -> Vec<EngineRequest> {
            self.asked.try_iter().collect()
        }

        fn answer(&mut self, event: EngineEvent) {
            self.answer.send(event).unwrap();
            self.connector.handle(Wakeup::Engine, &self.woken);
        }
    }

    fn heard(stream: &mut UnixStream) -> FromConnector {
        wire::read(stream)
            .unwrap()
            .expect("the connector said something")
    }

    fn ask(stream: &mut UnixStream, asked: &ToConnector) {
        wire::write(stream, asked).unwrap();
    }

    fn sending(request: Request, message: &Outgoing) -> ToConnector {
        ToConnector::Send {
            request,
            account: ME.into(),
            message: message.clone(),
        }
    }

    fn sends(requests: Vec<EngineRequest>) -> Vec<EngineRequest> {
        requests
            .into_iter()
            .filter(|r| r.kind() == EngineRequestKind::Send)
            .collect()
    }

    #[test]
    fn new_mail_while_nobody_is_attached_is_one_mark_per_account_given_to_the_next_attached() {
        let mut rig = rig();
        // The connector watches the account's inbox itself.
        assert!(
            rig.engine_requests()
                .iter()
                .any(|r| r.kind() == EngineRequestKind::Watch)
        );
        for _ in 0..50 {
            rig.answer(EngineEvent::Arrived {
                request_id: uuid::Uuid::new_v4(),
                account_id: ME.into(),
            });
        }
        assert_eq!(rig.connector.queued(), 1, "fifty arrivals are one mark");
        let mut instance = rig.attach();
        assert!(matches!(heard(&mut instance), FromConnector::Accounts(a) if a.len() == 1));
        assert_eq!(
            heard(&mut instance),
            FromConnector::Event(Event::Arrived { account: ME.into() })
        );
        assert_eq!(rig.connector.queued(), 0, "given to the instance");
    }

    #[test]
    fn a_message_two_instances_send_goes_once_and_both_hear_how_it_went() {
        let mut rig = rig();
        let mut ada = rig.attach();
        let mut again = rig.attach();
        let _accounts = (heard(&mut ada), heard(&mut again));
        let _ = rig.engine_requests();
        let message = Outgoing {
            to: "ada@analytical.example".into(),
            subject: "Re: Launch plan".into(),
            ..Outgoing::default()
        };
        let (first, second) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        ask(&mut ada, &sending(first, &message));
        ask(&mut again, &sending(second, &message));
        rig.turn();
        let sent = sends(rig.engine_requests());
        assert_eq!(sent.len(), 1, "sent once");
        rig.answer(EngineEvent::Completed {
            request_id: sent[0].request_id(),
            kind: EngineRequestKind::Send,
        });
        assert_eq!(
            heard(&mut ada),
            FromConnector::Event(Event::Accepted(first))
        );
        assert_eq!(
            heard(&mut again),
            FromConnector::Event(Event::Accepted(second))
        );
    }

    #[test]
    fn the_outcome_of_a_send_whose_instance_left_waits_for_the_next_one_to_ask() {
        let mut rig = rig();
        let mut ada = rig.attach();
        let _accounts = heard(&mut ada);
        let _ = rig.engine_requests();
        let message = Outgoing {
            to: "ada@analytical.example".into(),
            ..Outgoing::default()
        };
        ask(&mut ada, &sending(uuid::Uuid::new_v4(), &message));
        rig.turn();
        let sent = sends(rig.engine_requests());
        assert_eq!(sent.len(), 1, "handed to the engine");
        // The instance goes before the server answers.
        drop(ada);
        rig.turn();
        rig.answer(EngineEvent::Completed {
            request_id: sent[0].request_id(),
            kind: EngineRequestKind::Send,
        });
        assert_eq!(rig.connector.queued(), 1, "kept for whoever asks");
        // The next instance asks what became of it: told, not sent again.
        let mut next = rig.attach();
        let _accounts = heard(&mut next);
        let asked = uuid::Uuid::new_v4();
        ask(&mut next, &sending(asked, &message));
        rig.turn();
        assert!(sends(rig.engine_requests()).is_empty(), "not sent twice");
        assert_eq!(
            heard(&mut next),
            FromConnector::Event(Event::Accepted(asked))
        );
        assert_eq!(rig.connector.queued(), 0);
    }

    #[test]
    fn a_message_that_never_reached_its_server_goes_again_when_asked() {
        let mut rig = rig();
        let mut ada = rig.attach();
        let _accounts = heard(&mut ada);
        let _ = rig.engine_requests();
        let message = Outgoing {
            to: "ada@analytical.example".into(),
            ..Outgoing::default()
        };
        let first = uuid::Uuid::new_v4();
        ask(&mut ada, &sending(first, &message));
        rig.turn();
        let sent = sends(rig.engine_requests());
        rig.answer(EngineEvent::Error {
            request_id: sent[0].request_id(),
            kind: EngineRequestKind::Send,
            context: mail::engine::SMTP_UNREACHED.into(),
            message: "connection refused".into(),
        });
        assert_eq!(
            heard(&mut ada),
            FromConnector::Event(Event::Unsent {
                request: first,
                reason: "connection refused".into()
            })
        );
        ask(&mut ada, &sending(uuid::Uuid::new_v4(), &message));
        rig.turn();
        assert_eq!(sends(rig.engine_requests()).len(), 1, "tried again");
    }
}
