//! The connector as an instance's mail origin reaches it: the mail connector process the system
//! runs for the app ([`crate::serve`]), over the socket the session handed the instance. The
//! instance's process holds no network and no credential; it asks, and takes in what the servers
//! answered.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use crate::connector::{AccountRef, Event, MailConnector, Outgoing, Request, Unmade};
use crate::wire::{self, FromConnector, ToConnector};

/// What the reading thread hands the origin: a message, or that the connector is gone.
enum Heard {
    Said(FromConnector),
    Gone(String),
}

/// [`MailConnector`] over the socket to the mail connector process.
pub struct RemoteConnector {
    stream: UnixStream,
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
    ///
    /// # Errors
    /// The socket cannot be shared with the reading thread.
    pub fn new(
        stream: UnixStream,
        woken: Arc<AtomicBool>,
        wake: tilolang_tilo::background::Waker,
        reconnected: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let mut reading = stream.try_clone()?;
        let (said, heard) = channel();
        std::thread::Builder::new()
            .name("mail-connector".into())
            .spawn(move || {
                loop {
                    let message = match wire::read::<FromConnector>(&mut reading) {
                        Ok(Some(message)) => Heard::Said(message),
                        Ok(None) => Heard::Gone("the mail connector stopped".to_owned()),
                        Err(error) => Heard::Gone(format!("the mail connector failed: {error}")),
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

    /// Ask the connector `request`, under a fresh id; `None` for an account it does not hold
    /// or once it is gone.
    fn ask(
        &mut self,
        account: &str,
        request: impl FnOnce(Request) -> ToConnector,
    ) -> Option<Request> {
        if self.gone.is_some() || !self.accounts.iter().any(|a| a.id == account) {
            return None;
        }
        let id = uuid::Uuid::new_v4();
        match wire::write(&mut self.stream, &request(id)) {
            Ok(()) => Some(id),
            Err(error) => {
                self.gone = Some(format!("the mail connector failed: {error}"));
                None
            }
        }
    }
}

impl MailConnector for RemoteConnector {
    fn accounts(&self) -> Vec<AccountRef> {
        self.accounts.clone()
    }

    fn changes(&mut self, account: &str, mailbox: &str) -> Option<Request> {
        self.ask(account, |request| ToConnector::Changes {
            request,
            account: account.to_owned(),
            mailbox: mailbox.to_owned(),
        })
    }

    fn flag(&mut self, account: &str, key: &str, unread: bool, flagged: bool) -> Option<Request> {
        self.ask(account, |request| ToConnector::Flag {
            request,
            account: account.to_owned(),
            key: key.to_owned(),
            unread,
            flagged,
        })
    }

    fn move_to(&mut self, account: &str, key: &str, mailbox: &str) -> Option<Request> {
        self.ask(account, |request| ToConnector::Move {
            request,
            account: account.to_owned(),
            key: key.to_owned(),
            mailbox: mailbox.to_owned(),
        })
    }

    fn search(&mut self, account: &str, text: &str) -> Option<Request> {
        self.ask(account, |request| ToConnector::Search {
            request,
            account: account.to_owned(),
            text: text.to_owned(),
        })
    }

    fn send(&mut self, account: &str, message: Outgoing) -> Result<Request, Unmade> {
        if self.gone.is_none() && !self.accounts.iter().any(|a| a.id == account) {
            return Err(Unmade::NoAccount);
        }
        // Gone, or going as it is asked: the connector never got it, so it waits to go online.
        self.ask(account, |request| ToConnector::Send {
            request,
            account: account.to_owned(),
            message,
        })
        .ok_or(Unmade::EngineStopped)
    }

    fn watch(&mut self, account: &str) {
        if self.gone.is_none() {
            let watch = ToConnector::Watch {
                account: account.to_owned(),
            };
            if let Err(error) = wire::write(&mut self.stream, &watch) {
                self.gone = Some(format!("the mail connector failed: {error}"));
            }
        }
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
