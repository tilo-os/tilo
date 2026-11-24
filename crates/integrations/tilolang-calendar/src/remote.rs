//! The connector as an instance's calendar origin reaches it: the calendar connector process the
//! system runs for the app ([`crate::serve`]), over the socket the session handed the instance.
//! The instance's process holds no network and no credential; it asks, and takes in what the
//! servers answered.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use tilo_socket::Stream;

use crate::connector::{AccountRef, CalDavConnector, CalendarKnown, Entry, Event, Request, Unmade};
use crate::wire::{self, FromConnector, ToConnector};

/// What the reading thread hands the origin: a message, or that the connector is gone.
enum Heard {
    Said(FromConnector),
    Gone(String),
}

/// [`CalDavConnector`] over the socket to the calendar connector process.
pub struct RemoteConnector {
    stream: Stream,
    heard: Receiver<Heard>,
    accounts: Vec<AccountRef>,
    /// Set when the connector says the device came online again.
    reconnected: Arc<AtomicBool>,
    /// Why the connector cannot be reached, once it cannot.
    gone: Option<String>,
}

impl RemoteConnector {
    /// The connector at the far end of `stream`. A thread reads what it says, sets `woken` and
    /// calls `wake` whenever it said something, and sets `reconnected` when the device is online
    /// again.
    pub fn new(
        stream: Stream,
        woken: Arc<AtomicBool>,
        wake: tilolang_tilo::background::Waker,
        reconnected: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let mut reading = stream.try_clone()?;
        let (said, heard) = channel();
        std::thread::Builder::new()
            .name("calendar-connector".into())
            .spawn(move || {
                loop {
                    let message = match wire::read::<FromConnector>(&mut reading) {
                        Ok(Some(message)) => Heard::Said(message),
                        Ok(None) => Heard::Gone("the calendar connector stopped".to_owned()),
                        Err(error) => {
                            Heard::Gone(format!("the calendar connector failed: {error}"))
                        }
                    };
                    let gone = matches!(message, Heard::Gone(_));
                    if said.send(message).is_err() {
                        return;
                    }
                    woken.store(true, Ordering::Release);
                    wake();
                    if gone {
                        return;
                    }
                }
            })?;
        Ok(Self {
            stream,
            heard,
            accounts: Vec::new(),
            reconnected,
            gone: None,
        })
    }

    /// Whether the connector holds `account`.
    fn knows(&self, account: &str) -> bool {
        self.accounts.iter().any(|a| a.id == account)
    }

    /// Ask the connector `request`, under a fresh id; `Err` for an account it does not hold or
    /// once it is gone, which is why nothing was written.
    fn ask(
        &mut self,
        account: &str,
        request: impl FnOnce(Request) -> ToConnector,
    ) -> Result<Request, Unmade> {
        if self.gone.is_some() || !self.knows(account) {
            return Err(Unmade::EngineStopped);
        }
        let id = uuid::Uuid::new_v4();
        match wire::write(&mut self.stream, &request(id)) {
            Ok(()) => Ok(id),
            Err(error) => {
                self.gone = Some(format!("the calendar connector failed: {error}"));
                Err(Unmade::EngineStopped)
            }
        }
    }

    /// A write's request: refused outright for an account the workspace does not hold.
    fn write(
        &mut self,
        account: &str,
        request: impl FnOnce(Request) -> ToConnector,
    ) -> Result<Request, Unmade> {
        if self.gone.is_none() && !self.knows(account) {
            return Err(Unmade::NoAccount);
        }
        self.ask(account, request)
    }
}

impl CalDavConnector for RemoteConnector {
    fn accounts(&self) -> Vec<AccountRef> {
        self.accounts.clone()
    }

    fn sync(&mut self, account: &str, known: Vec<CalendarKnown>) -> Option<Request> {
        self.ask(account, |request| ToConnector::Sync {
            request,
            account: account.to_owned(),
            known,
        })
        .ok()
    }

    fn put(
        &mut self,
        account: &str,
        entry: Entry,
        href: &str,
        from: &str,
    ) -> Result<Request, Unmade> {
        self.write(account, |request| ToConnector::Put {
            request,
            account: account.to_owned(),
            entry: Box::new(entry),
            href: href.to_owned(),
            from: from.to_owned(),
        })
    }

    fn remove(&mut self, account: &str, calendar: &str, href: &str) -> Result<Request, Unmade> {
        self.write(account, |request| ToConnector::Remove {
            request,
            account: account.to_owned(),
            calendar: calendar.to_owned(),
            href: href.to_owned(),
        })
    }

    fn set_calendar(
        &mut self,
        account: &str,
        calendar: &str,
        name: &str,
        color: &str,
    ) -> Result<Request, Unmade> {
        self.write(account, |request| ToConnector::Calendar {
            request,
            account: account.to_owned(),
            calendar: calendar.to_owned(),
            name: name.to_owned(),
            color: color.to_owned(),
        })
    }

    fn answer(
        &mut self,
        account: &str,
        calendar: &str,
        href: &str,
        attendee: &str,
        answer: &str,
    ) -> Result<Request, Unmade> {
        self.write(account, |request| ToConnector::Answer {
            request,
            account: account.to_owned(),
            calendar: calendar.to_owned(),
            href: href.to_owned(),
            attendee: attendee.to_owned(),
            answer: answer.to_owned(),
        })
    }

    fn import(&mut self, account: &str, calendar: &str, text: &str) -> Result<Request, Unmade> {
        self.write(account, |request| ToConnector::Import {
            request,
            account: account.to_owned(),
            calendar: calendar.to_owned(),
            text: text.to_owned(),
        })
    }

    fn events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            match self.heard.try_recv() {
                Ok(Heard::Said(FromConnector::Accounts(accounts))) => self.accounts = accounts,
                Ok(Heard::Said(FromConnector::Event(event))) => out.push(event),
                Ok(Heard::Said(FromConnector::Online)) => {
                    self.reconnected.store(true, Ordering::Release);
                }
                Ok(Heard::Gone(reason)) => {
                    // Every account's server is out of reach until the connector is back.
                    for account in &self.accounts {
                        out.push(Event::Unreachable {
                            account: account.id.clone(),
                            reason: reason.clone(),
                        });
                    }
                    self.gone = Some(reason);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        out
    }
}
