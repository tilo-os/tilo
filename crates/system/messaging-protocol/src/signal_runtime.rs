//! Native I/O for a supervisor-provided signal-cli connection.
//!
//! The supervisor authenticates the daemon and supplies the connected handle.
//! This module never discovers ambient sockets, spawns a daemon, logs in or
//! acquires Signal identity keys. A single total deadline bounds each exchange,
//! including partial writes and trickling frames. The send journal stays local.

use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tilo_clock::{Duration, Instant};
use zeroize::Zeroizing;

use crate::journal::{EncryptedJournal, Owner, Status};
use crate::routing::LiveBinding;
use crate::signal::{Acknowledgment, Incoming};
use crate::signal_session::{CheckedSendError, SendContext, SendError, Session};
use crate::{Error, Route, Service};

/// The supervisor can choose a shorter budget, never an unbounded operation.
pub const MAX_OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    WrongOwner,
    CustodyUnavailable,
    TransportUnavailable,
}

/// One local owner, one authenticated daemon connection and one durable ledger.
/// No other stream or journal handle escapes this session.
pub struct SignalRuntime {
    session: Session<DeadlineReader, DeadlineStream>,
    deadline: Deadline,
    journal: EncryptedJournal,
    owner: Owner,
}

impl SignalRuntime {
    /// Use an existing OS-keychain key with the supervisor's verified handle.
    /// No persistent grant, key or Signal linking is created by this call.
    ///
    /// # Errors
    /// Refuses unavailable custody, wrong ownership or unusable socket handles.
    pub fn open_system(
        root: &Path,
        owner: Owner,
        signal_account: String,
        stream: UnixStream,
    ) -> Result<Self, OpenError> {
        if owner.service != Service::Signal {
            return Err(OpenError::WrongOwner);
        }
        let journal = EncryptedJournal::open_system(root, &owner)
            .map_err(|_| OpenError::CustodyUnavailable)?;
        Self::from_journal(owner, signal_account, stream, journal)
    }

    /// Host-injected custody, without weakening the production OS-keychain path.
    /// The journal's frozen device/account/link generation must match exactly.
    ///
    /// # Errors
    /// Refuses mismatched journal ownership and unusable socket handles.
    pub fn from_journal(
        owner: Owner,
        signal_account: String,
        stream: UnixStream,
        journal: EncryptedJournal,
    ) -> Result<Self, OpenError> {
        if owner.service != Service::Signal
            || journal.owner() != &owner
            || !valid_signal_account(&signal_account)
        {
            return Err(OpenError::WrongOwner);
        }
        let (session, deadline) = connection(owner.account.clone(), signal_account, stream)?;
        Ok(Self {
            session,
            deadline,
            journal,
            owner,
        })
    }

    /// Check an existing account on the authenticated multi-account daemon.
    /// # Errors
    /// Refuses missing/ambiguous accounts and bounded probe failures.
    pub fn verify_account(
        &mut self,
        timeout: Duration,
    ) -> Result<crate::signal_setup::SignalAccount, Error> {
        self.deadline
            .arm(timeout)
            .map_err(|_| Error::InvalidRequest)?;
        let result = self.session.probe("listAccounts", &serde_json::json!({}))?;
        let selected = self.session.signal_account().to_owned();
        let identities = self.session.probe(
            "listIdentities",
            &serde_json::json!({"account":selected,"number":selected}),
        )?;
        let account = crate::signal_setup::account(&result, &identities, &selected)?;
        self.session
            .bind_number(account.number.as_deref().ok_or(Error::InvalidIdentity)?)?;
        Ok(account)
    }
    /// Compare the current verified provider key without changing trust.
    /// # Errors
    /// Refuses unknown, changed or unverified recipient keys and probe failures.
    pub fn verify_recipient(
        &mut self,
        recipient: &str,
        revision: &str,
        timeout: Duration,
    ) -> Result<(), Error> {
        self.deadline
            .arm(timeout)
            .map_err(|_| Error::InvalidRequest)?;
        let result = self.session.probe(
            "listIdentities",
            &serde_json::json!({"account":self.session.wire_account(),"number":recipient}),
        )?;
        crate::signal_setup::identity(&result, recipient, revision)
    }
    pub fn owner(&self) -> &Owner {
        &self.owner
    }
    #[cfg(feature = "signal-broker")]
    pub(crate) fn signal_account(&self) -> &str {
        self.session.signal_account()
    }

    /// Recheck canonical account/link authority, freeze and persist the send,
    /// then submit once over the owned connection within a total I/O budget.
    /// Call on the broker worker, never on the UI/render thread.
    ///
    /// # Errors
    /// Route/custody failures write nothing. Any post-reservation failure is
    /// uncertain and cannot be retried, including after process restart.
    pub fn send(
        &mut self,
        accounts: &accounts::AccountStore,
        binding: &LiveBinding,
        route: &Route,
        operation: &str,
        text: &str,
        timeout: Duration,
    ) -> Result<Acknowledgment, CheckedSendError> {
        self.deadline.arm(timeout).map_err(|_| {
            CheckedSendError::Transport(SendError::BeforeSubmission(Error::InvalidRequest))
        })?;
        let mut context = SendContext {
            accounts,
            expected_owner: &self.owner,
            live_binding: binding,
            journal: &mut self.journal,
        };
        self.session
            .send_checked(&mut context, route, operation, text)
    }

    /// Receive a direct text notification within a total I/O budget. Rich
    /// events remain available to explicit handlers via [`Self::take_events`].
    ///
    /// # Errors
    /// Refuses invalid budgets, wrong-account, malformed or unsupported events.
    pub fn receive(&mut self, timeout: Duration) -> Result<Incoming, Error> {
        self.deadline
            .arm(timeout)
            .map_err(|_| Error::InvalidRequest)?;
        self.session.receive()
    }

    pub fn take_events(&mut self) -> Vec<Zeroizing<Vec<u8>>> {
        self.session.take_events()
    }

    pub fn status(&self, operation: &str) -> Option<Status> {
        self.journal.status(operation)
    }
    #[cfg(feature = "signal-broker")]
    pub(crate) fn receipt_operations(
        &self,
        route: &Route,
        timestamps: &[u64],
    ) -> Result<Vec<String>, Error> {
        self.journal
            .signal_operations(route, timestamps)
            .map_err(|_| Error::WrongRequest)
    }
    #[cfg(feature = "signal-broker")]
    pub(crate) fn observe_recipient(
        &mut self,
        accounts: &accounts::AccountStore,
        binding: &LiveBinding,
        route: &Route,
        operation: &str,
        evidence: crate::journal::RecipientEvidence,
    ) -> Result<(), Error> {
        crate::routing::check(accounts, route, &self.owner, binding)
            .map_err(|_| Error::WrongAccount)?;
        self.journal
            .observe_recipient(operation, route, evidence)
            .map_err(|_| Error::ConnectionUnavailable)
    }
}

pub(crate) fn private_connection(
    stream: UnixStream,
) -> Result<(DeadlineReader, DeadlineStream, Deadline), OpenError> {
    stream
        .set_nonblocking(false)
        .map_err(|_| OpenError::TransportUnavailable)?;
    let reader = stream
        .try_clone()
        .map_err(|_| OpenError::TransportUnavailable)?;
    let deadline = Deadline::default();
    let reader = DeadlineReader {
        stream: DeadlineStream {
            stream: reader,
            deadline: deadline.clone(),
        },
        buffer: Zeroizing::new(vec![0; 8192]),
        start: 0,
        end: 0,
    };
    let writer = DeadlineStream {
        stream,
        deadline: deadline.clone(),
    };
    Ok((reader, writer, deadline))
}

fn connection(
    account: String,
    signal_account: String,
    stream: UnixStream,
) -> Result<(Session<DeadlineReader, DeadlineStream>, Deadline), OpenError> {
    let (reader, writer, deadline) = private_connection(stream)?;
    let session = Session::for_account(account, signal_account, reader, writer)
        .map_err(|_| OpenError::WrongOwner)?;
    Ok((session, deadline))
}

/// Independent receive connection. Its blocking read never holds the send worker.
#[cfg(feature = "signal-broker")]
pub struct SignalReceiver {
    session: Session<DeadlineReader, DeadlineStream>,
    deadline: Deadline,
    owner: Owner,
    signal_account: String,
    scope: String,
    control: UnixStream,
}
#[cfg(feature = "signal-broker")]
impl SignalReceiver {
    /// The supervisor supplies a separately authenticated daemon connection.
    /// # Errors
    /// Refuses malformed owner/identity, unavailable stream or wrong daemon account.
    pub fn new(
        owner: Owner,
        signal_account: String,
        stream: UnixStream,
    ) -> Result<Self, OpenError> {
        if owner.service != Service::Signal
            || !owner.valid_principal()
            || !valid_signal_account(&signal_account)
        {
            return Err(OpenError::WrongOwner);
        }
        let control = stream
            .try_clone()
            .map_err(|_| OpenError::TransportUnavailable)?;
        let (mut session, deadline) =
            connection(owner.account.clone(), signal_account.clone(), stream)?;
        deadline
            .arm(Duration::from_secs(10))
            .map_err(|_| OpenError::TransportUnavailable)?;
        let result = session
            .probe("listAccounts", &serde_json::json!({}))
            .map_err(|_| OpenError::TransportUnavailable)?;
        let identities = session
            .probe(
                "listIdentities",
                &serde_json::json!({"account":signal_account,"number":signal_account}),
            )
            .map_err(|_| OpenError::TransportUnavailable)?;
        let account = crate::signal_setup::account(&result, &identities, &signal_account)
            .map_err(|_| OpenError::WrongOwner)?;
        session
            .bind_number(account.number.as_deref().ok_or(OpenError::WrongOwner)?)
            .map_err(|_| OpenError::WrongOwner)?;
        session
            .subscribe_receive()
            .map_err(|_| OpenError::TransportUnavailable)?;
        Ok(Self {
            session,
            deadline,
            owner,
            signal_account,
            scope: account.scope(),
            control,
        })
    }
    pub(crate) fn owner(&self) -> &Owner {
        &self.owner
    }
    pub(crate) fn signal_account(&self) -> &str {
        &self.signal_account
    }
    pub(crate) fn wire_account(&self) -> &str {
        self.session.wire_account()
    }
    pub(crate) fn scope(&self) -> &str {
        &self.scope
    }
    pub(crate) fn interrupt(&self) -> Result<UnixStream, OpenError> {
        self.control
            .try_clone()
            .map_err(|_| OpenError::TransportUnavailable)
    }
    pub(crate) fn receive(&mut self) -> Result<Incoming, Error> {
        self.deadline
            .arm(MAX_OPERATION_TIMEOUT)
            .map_err(|_| Error::InvalidRequest)?;
        self.session.receive()
    }
    pub(crate) fn take_events(&mut self) -> Vec<Zeroizing<Vec<u8>>> {
        self.session.take_events()
    }
}

#[derive(Clone, Default)]
pub(crate) struct Deadline(Arc<Mutex<Option<Instant>>>);

impl Deadline {
    pub(crate) fn arm(&self, timeout: Duration) -> io::Result<()> {
        if timeout.is_zero() || timeout > MAX_OPERATION_TIMEOUT {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let end = tilo_clock::now()
            .checked_add(timeout)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        *self
            .0
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))? = Some(end);
        Ok(())
    }

    fn remaining(&self) -> io::Result<Duration> {
        let end = self
            .0
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        end.checked_duration_since(tilo_clock::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
    }
}

pub(crate) struct DeadlineStream {
    stream: UnixStream,
    deadline: Deadline,
}

impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream
            .set_read_timeout(Some(self.deadline.remaining()?))?;
        self.stream.read(bytes)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(self.deadline.remaining()?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.deadline.remaining()?;
        self.stream.flush()
    }
}

fn valid_signal_account(account: &str) -> bool {
    if let Some(digits) = account.strip_prefix('+') {
        !digits.is_empty()
            && digits.len() <= 15
            && !digits.starts_with('0')
            && digits.bytes().all(|digit| digit.is_ascii_digit())
    } else {
        uuid::Uuid::parse_str(account).is_ok()
    }
}

/// Unlike a generic `BufReader`, this private receive buffer wipes on drop.
pub(crate) struct DeadlineReader {
    stream: DeadlineStream,
    buffer: Zeroizing<Vec<u8>>,
    start: usize,
    end: usize,
}

impl BufRead for DeadlineReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.stream.deadline.remaining()?;
        if self.start == self.end {
            self.end = self.stream.read(&mut self.buffer)?;
            self.start = 0;
        }
        self.buffer
            .get(self.start..self.end)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
    }

    fn consume(&mut self, amount: usize) {
        self.start = self.start.saturating_add(amount).min(self.end);
    }
}

impl Read for DeadlineReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let count = available.len().min(bytes.len());
        let source = available
            .get(..count)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let target = bytes
            .get_mut(..count)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        target.copy_from_slice(source);
        self.consume(count);
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
