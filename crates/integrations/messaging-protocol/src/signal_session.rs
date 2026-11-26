//! Stream adapter for a broker-owned signal-cli multi-account connection.

use std::io::{BufRead, Write};

use zeroize::Zeroizing;

use crate::signal::{self, Acknowledgment, Incoming, MAX_FRAME_BYTES};
use crate::{Error, Route};

pub const MAX_INTERLEAVED_EVENTS: usize = 64;

pub trait SendJournal {
    /// Atomically reserve a never-before-used operation and persist its exact
    /// route and payload as STARTED before returning. Reuse, including reuse
    /// after restart/uncertainty, must fail. A failed call permits no wire write.
    /// The payload contains private text and needs host-owned private storage.
    fn begin(&mut self, operation: &str, route: &Route, payload: &[u8])
    -> Result<(), JournalError>;

    /// Durably store the observed result. Errors after submission mean uncertain
    /// state to the caller; they never permit another initial submission.
    fn complete(&mut self, operation: &str, result: Completion) -> Result<(), JournalError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalError {
    AlreadyReserved,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    Submitted(crate::ProtocolReference),
    Acknowledged(crate::ProtocolReference),
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    BeforeSubmission(Error),
    Journal(JournalError),
    /// Write, read, correlation, protocol or post-send durability failed. The
    /// original operation must be reconciled; do not submit it again.
    Uncertain,
}

pub struct SendContext<'a, J> {
    pub accounts: &'a accounts::person::AccountList,
    pub expected_owner: &'a crate::journal::Owner,
    pub live_binding: &'a crate::routing::LiveBinding,
    pub journal: &'a mut J,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckedSendError {
    Routing(crate::routing::Refusal),
    Transport(SendError),
}

/// Account-bound adapter. Incoming events remain available while a send waits
/// for its matching acknowledgment. Unsupported events are not discarded.
pub struct Session<R, W> {
    account: String,
    signal_account: String,
    wire_account: String,
    reader: R,
    writer: W,
    events: Vec<Zeroizing<Vec<u8>>>,
    usable: bool,
    subscription: Option<u64>,
}

impl<R: BufRead, W: Write> Session<R, W> {
    pub(crate) fn signal_account(&self) -> &str {
        &self.signal_account
    }

    pub(crate) fn wire_account(&self) -> &str {
        &self.wire_account
    }

    pub(crate) fn bind_number(&mut self, number: &str) -> Result<(), Error> {
        if self.wire_account != self.signal_account && self.wire_account != number {
            return Err(Error::WrongAccount);
        }
        self.wire_account = number.into();
        Ok(())
    }

    /// The broker entry point: check canonical account and current linking,
    /// then persist the exact operation before the first stream write.
    pub fn send_checked<J: SendJournal>(
        &mut self,
        context: &mut SendContext<'_, J>,
        route: &Route,
        operation: &str,
        text: &str,
    ) -> Result<Acknowledgment, CheckedSendError> {
        crate::routing::check(
            context.accounts,
            route,
            context.expected_owner,
            context.live_binding,
        )
        .map_err(CheckedSendError::Routing)?;
        self.send(context.journal, route, operation, text)
            .map_err(CheckedSendError::Transport)
    }
    pub fn new(account: String, reader: R, writer: W) -> Result<Self, Error> {
        Self::for_account(account.clone(), account, reader, writer)
    }

    /// Bind a Tilo account ID to its host-verified Signal phone number or ACI.
    /// Changing the binding requires a new host linking generation/journal.
    pub fn for_account(
        account: String,
        signal_account: String,
        reader: R,
        writer: W,
    ) -> Result<Self, Error> {
        if !crate::identifier(&account) || !crate::identifier(&signal_account) {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            account,
            wire_account: signal_account.clone(),
            signal_account,
            reader,
            writer,
            events: Vec::new(),
            usable: true,
            subscription: None,
        })
    }

    /// Send on exactly this account after the host's durable check has opened.
    pub fn send<J: SendJournal>(
        &mut self,
        journal: &mut J,
        route: &Route,
        operation: &str,
        text: &str,
    ) -> Result<Acknowledgment, SendError> {
        if !self.usable {
            return Err(SendError::BeforeSubmission(Error::ConnectionUnavailable));
        }
        if route.account() != self.account {
            return Err(SendError::BeforeSubmission(Error::WrongAccount));
        }
        if self.events.len() >= MAX_INTERLEAVED_EVENTS {
            return Err(SendError::BeforeSubmission(Error::FrameTooLarge));
        }
        let payload = signal::send_on(route, &self.wire_account, operation, text)
            .map_err(SendError::BeforeSubmission)?;
        journal
            .begin(operation, route, &payload)
            .map_err(SendError::Journal)?;
        let result = self.exchange(&payload, operation);
        if result.is_err() {
            self.usable = false;
        }
        let completion = match result {
            Ok(ack) => Completion::Acknowledged(crate::ProtocolReference::Signal {
                timestamp: ack.timestamp,
            }),
            Err(_) => Completion::Uncertain,
        };
        if journal.complete(operation, completion).is_err() {
            self.usable = false;
            return Err(SendError::Uncertain);
        }
        result.map_err(|_| SendError::Uncertain)
    }

    pub(crate) fn probe(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        if !self.usable || !["listAccounts", "listIdentities"].contains(&method) {
            return Err(Error::Unsupported);
        }
        self.command(method, params)
    }

    /// Manual-mode receive subscription is an explicit operation, separate
    /// from the read-only account/identity probes.
    pub(crate) fn subscribe_receive(&mut self) -> Result<(), Error> {
        let account = self.wire_account.clone();
        let result = self.command("subscribeReceive", &serde_json::json!({"account":account}))?;
        let subscription = result.as_u64().ok_or(Error::InvalidFrame)?;
        self.subscription = Some(subscription);
        for event in &self.events {
            check_notification(event, &self.wire_account, self.subscription)?;
        }
        Ok(())
    }

    fn command(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        if !self.usable {
            return Err(Error::ConnectionUnavailable);
        }
        let operation = format!("tilo-probe-{}", uuid::Uuid::new_v4());
        let payload =
            serde_json::json!({"jsonrpc":"2.0","id":operation,"method":method,"params":params});
        let mut bytes =
            Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| Error::InvalidRequest)?);
        bytes.push(b'\n');
        let result = (|| {
            self.writer
                .write_all(&bytes)
                .map_err(|_| Error::ConnectionUnavailable)?;
            self.writer
                .flush()
                .map_err(|_| Error::ConnectionUnavailable)?;
            loop {
                if self.events.len() >= MAX_INTERLEAVED_EVENTS {
                    return Err(Error::FrameTooLarge);
                }
                let frame = read_frame(&mut self.reader)?;
                let value: serde_json::Value =
                    serde_json::from_slice(&frame).map_err(|_| Error::InvalidFrame)?;
                if value.get("method").is_some() {
                    check_notification(&frame, &self.wire_account, self.subscription)?;
                    self.events.push(frame);
                    continue;
                }
                if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
                    || value.get("id").and_then(serde_json::Value::as_str) != Some(&operation)
                {
                    return Err(Error::WrongRequest);
                }
                if value.get("error").is_some() {
                    return Err(Error::RemoteRejected);
                }
                return value.get("result").cloned().ok_or(Error::InvalidFrame);
            }
        })();
        if result.is_err() {
            self.usable = false;
        }
        result
    }

    /// Raw notifications for dispatch to richer codecs; bodies are private.
    /// The account binding is checked before any event enters this queue.
    pub fn take_events(&mut self) -> Vec<Zeroizing<Vec<u8>>> {
        std::mem::take(&mut self.events)
    }

    /// Receive supported direct text outside a send operation. Unsupported
    /// events remain in the queue for a different explicitly supported handler.
    pub fn receive(&mut self) -> Result<Incoming, Error> {
        if !self.usable {
            return Err(Error::ConnectionUnavailable);
        }
        if self.events.len() >= MAX_INTERLEAVED_EVENTS {
            return Err(Error::FrameTooLarge);
        }
        let frame = read_frame(&mut self.reader).inspect_err(|_| self.usable = false)?;
        check_notification(&frame, &self.wire_account, self.subscription)
            .inspect_err(|_| self.usable = false)?;
        match signal::incoming(&frame, &self.wire_account) {
            Ok(mut incoming) => {
                incoming.id.account.clone_from(&self.account);
                Ok(incoming)
            }
            Err(error) => {
                if self.events.len() < MAX_INTERLEAVED_EVENTS {
                    self.events.push(frame);
                }
                Err(error)
            }
        }
    }

    fn exchange(&mut self, payload: &[u8], operation: &str) -> Result<Acknowledgment, Error> {
        self.writer
            .write_all(payload)
            .map_err(|_| Error::InvalidFrame)?;
        self.writer.flush().map_err(|_| Error::InvalidFrame)?;
        loop {
            if self.events.len() >= MAX_INTERLEAVED_EVENTS {
                return Err(Error::FrameTooLarge);
            }
            let frame = read_frame(&mut self.reader)?;
            let value: serde_json::Value =
                serde_json::from_slice(&frame).map_err(|_| Error::InvalidFrame)?;
            if value.get("method").is_some() {
                check_notification(&frame, &self.wire_account, self.subscription)?;
                self.events.push(frame);
            } else {
                return signal::acknowledgment(&frame, operation);
            }
        }
    }
}

fn check_notification(frame: &[u8], account: &str, subscription: Option<u64>) -> Result<(), Error> {
    let value: serde_json::Value =
        serde_json::from_slice(frame).map_err(|_| Error::InvalidFrame)?;
    if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || value.get("method").and_then(serde_json::Value::as_str) != Some("receive")
    {
        return Err(Error::Unsupported);
    }
    let params = value.get("params").ok_or(Error::InvalidFrame)?;
    if let Some(expected) = subscription
        && (params
            .get("subscription")
            .and_then(serde_json::Value::as_u64)
            != Some(expected)
            || params.get("result").is_none())
    {
        return Err(Error::WrongRequest);
    }
    let received = params.get("result").unwrap_or(params);
    if received.get("account").and_then(serde_json::Value::as_str) != Some(account) {
        return Err(Error::WrongAccount);
    }
    Ok(())
}

pub(crate) fn read_frame(reader: &mut impl BufRead) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut frame = Zeroizing::new(Vec::new());
    let count = std::io::Read::take(reader, (MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut frame)
        .map_err(|_| Error::InvalidFrame)?;
    if count > MAX_FRAME_BYTES {
        return Err(Error::FrameTooLarge);
    }
    if frame.last() != Some(&b'\n') {
        return Err(Error::InvalidFrame);
    }
    Ok(frame)
}
