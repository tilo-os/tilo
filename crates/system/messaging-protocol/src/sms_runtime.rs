//! SMS over a supervisor-supplied, authenticated `ModemManager` bus connection.
//!
//! The supervisor authorizes the account and holds its modem/SIM lease during
//! each operation. This adapter discovers no modem, unlocks no SIM, changes no
//! default and connects to no ambient bus. It pins the daemon's unique owner,
//! modem path, SIM path and ICCID, rechecking before Create and Send. D-Bus has
//! no atomic conditional Send against an ICCID: physical hot-swap during that
//! method remains a host/hardware acceptance limit, not a guarantee made here.

#[cfg(feature = "signal-broker")]
mod inbox;
mod profile;
#[cfg(test)]
mod tests;
mod transport;
#[cfg(feature = "signal-broker")]
pub use inbox::SmsInbox;

use crate::journal::{EncryptedJournal, Owner, Status};
use crate::routing::LiveBinding;
use crate::signal_session::{CheckedSendError, Completion, SendError, SendJournal};
use crate::{Error, ProtocolReference, Route, Service};
use tilo_clock::Duration;
pub use transport::{DbusTransport, SimAuthority, SimBinding};

/// A bounded complete text read from this runtime's pinned modem/SIM.
/// No Debug implementation: incoming content and addresses are private.
pub struct ReceivedSms {
    pub id: String,
    pub number: String,
    pub text: zeroize::Zeroizing<String>,
    pub timestamp_ms: u64,
}

pub struct SmsRuntime {
    transport: DbusTransport,
    journal: EncryptedJournal,
    owner: Owner,
    usable: bool,
}

impl SmsRuntime {
    /// Bind host-custodied journal and modem authority without creating a grant.
    ///
    /// # Errors
    /// Refuses a non-SMS or mismatched journal owner.
    pub fn new(
        owner: Owner,
        transport: DbusTransport,
        journal: EncryptedJournal,
    ) -> Result<Self, Error> {
        if owner.service != Service::Sms || journal.owner() != &owner {
            return Err(Error::WrongAccount);
        }
        Ok(Self {
            transport,
            journal,
            owner,
            usable: true,
        })
    }

    /// Freeze and durably reserve one exact SMS before its first mutating RPC.
    /// A successful Send method only means Submitted, never recipient delivery.
    /// Call on the broker worker while holding the host's modem/SIM grant lease.
    pub async fn send(
        &mut self,
        accounts: &accounts::AccountStore,
        binding: &LiveBinding,
        route: &Route,
        operation: &str,
        text: &str,
        timeout: Duration,
    ) -> Result<ProtocolReference, CheckedSendError> {
        crate::routing::check(accounts, route, &self.owner, binding)
            .map_err(CheckedSendError::Routing)?;
        if !self.usable {
            return Err(before(Error::ConnectionUnavailable));
        }
        if timeout.is_zero() || timeout > Duration::from_secs(60) || !crate::identifier(operation) {
            return Err(before(Error::InvalidRequest));
        }
        let properties = crate::sms::create_text(route, text).map_err(before)?;
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| before(Error::InvalidRequest))?;
        tokio::time::timeout_at(deadline, self.transport.check_binding())
            .await
            .map_err(|_| before(Error::ConnectionUnavailable))?
            .map_err(before)?;
        let frozen = self.transport.freeze(&properties).map_err(before)?;
        self.transport.check_permission().map_err(before)?;
        self.journal
            .begin(operation, route, &frozen)
            .map_err(|error| CheckedSendError::Transport(SendError::Journal(error)))?;
        let result = tokio::time::timeout_at(deadline, self.submit(&properties)).await;
        if self.transport.check_permission().is_err() {
            self.usable = false;
            // A native operation may already have reached the modem. Keep its
            // durable reservation uncertain and perform no post-revocation write.
            return Err(CheckedSendError::Transport(SendError::Uncertain));
        }
        let Ok(Ok(reference)) = result else {
            self.usable = false;
            let _ = self.journal.complete(operation, Completion::Uncertain);
            return Err(CheckedSendError::Transport(SendError::Uncertain));
        };
        if self
            .journal
            .complete(operation, Completion::Submitted(reference.clone()))
            .is_err()
        {
            self.usable = false;
            return Err(CheckedSendError::Transport(SendError::Uncertain));
        }
        Ok(reference)
    }

    async fn submit(
        &self,
        properties: &crate::sms::CreateText,
    ) -> Result<ProtocolReference, Error> {
        // Journal persistence can take time; revalidate after that boundary,
        // immediately before the first modem mutation.
        self.transport.check_binding().await?;
        let message = self.transport.create(properties).await?;
        self.transport.verify_created(&message, properties).await?;
        self.transport.check_binding().await?;
        self.transport.send(&message).await?;
        Ok(self.transport.reference(&message))
    }

    /// Frozen native owner used by the trusted mounting host.
    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    pub fn status(&self, operation: &str) -> Option<Status> {
        self.journal.status(operation)
    }

    /// Read-only observation from this exact host-owned modem/SIM assembly.
    #[cfg(feature = "signal-broker")]
    pub fn inbox(&self) -> SmsInbox {
        SmsInbox::new(self.owner.clone(), self.transport.clone())
    }

    /// Read one complete inbound SMS under the exact reviewed recipient route.
    /// The host retains its current receive/SIM lease. This neither creates,
    /// sends, stores, deletes nor acknowledges a modem object.
    /// # Errors
    /// Refuses stale account/SIM/daemon authority, another sender, incomplete
    /// or unsupported PDUs and invalid/oversized private content.
    pub async fn read_received(
        &self,
        accounts: &accounts::AccountStore,
        binding: &LiveBinding,
        route: &Route,
        object: &str,
        timeout: Duration,
    ) -> Result<ReceivedSms, Error> {
        crate::routing::check(accounts, route, &self.owner, binding)
            .map_err(|_| Error::WrongAccount)?;
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(Error::InvalidRequest);
        }
        tokio::time::timeout(timeout, self.transport.read_received(route, object))
            .await
            .map_err(|_| Error::ConnectionUnavailable)?
    }
}

fn before(error: Error) -> CheckedSendError {
    CheckedSendError::Transport(SendError::BeforeSubmission(error))
}
