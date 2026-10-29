//! The one gate every running account checks before and after each I/O:
//! the account is not retired, its owner is still admitted, its grant still
//! holds, and every route goes through the person's current records.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_contact_directory::routing::Route;
use tilo_messaging::{AccountState, MessagingClient, SendError, ServiceAccount};
use tilo_messaging_protocol::external_broker::ExternalAuthority;
use tilo_messaging_protocol::signal_broker::{Authorization, SignalAuthority};
use tilo_messaging_protocol::supervisor::{AccountAuthority, AccountSupervisor};

use super::identity::{Credential, Identity, service_name};
use super::records::CurrentAuthority;

pub(crate) struct Gate {
    pub(crate) identity: Identity,
    pub(crate) credential: Credential,
    authority: CurrentAuthority,
    stopped: AtomicBool,
}

impl Gate {
    /// # Errors
    /// The grant belongs to another owner.
    pub(crate) fn new(
        identity: Identity,
        credential: Credential,
        authority: CurrentAuthority,
    ) -> Result<Arc<Self>, SendError> {
        if credential.owner != identity.owner {
            return Err(SendError::Revoked);
        }
        Ok(Arc::new(Self {
            identity,
            credential,
            authority,
            stopped: AtomicBool::new(false),
        }))
    }

    /// # Errors
    /// The account was retired, its owner's admission or its grant ended.
    pub(crate) fn check(&self) -> Result<(), SendError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(SendError::Revoked);
        }
        self.identity.check()?;
        if self.credential.owner != self.identity.owner {
            return Err(SendError::Revoked);
        }
        self.credential.check()
    }

    fn checked(&self, authorization: Authorization) -> Result<Authorization, SendError> {
        self.check()?;
        if authorization.binding.owner != self.identity.owner {
            return Err(SendError::Revoked);
        }
        Ok(authorization)
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    pub(crate) fn account(&self) -> ServiceAccount {
        ServiceAccount::new(
            service_name(self.identity.owner.service),
            &self.identity.owner.account,
        )
    }
}

impl SignalAuthority for Gate {
    fn check_account(&self) -> Result<(), SendError> {
        self.check()?;
        self.authority.check_account()?;
        self.check()
    }
    fn check_account_scope(&self, scope: &str) -> Result<(), SendError> {
        self.check()?;
        self.authority.check_account_scope(scope)?;
        self.check()
    }
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.authority.authorize(route)?)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.authority.incoming(recipient)?)
    }
}

impl ExternalAuthority for Gate {
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        SignalAuthority::authorize(self, route)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        SignalAuthority::incoming(self, recipient)
    }
}

impl AccountAuthority for Gate {
    fn check(&self) -> Result<(), SendError> {
        Gate::check(self)
    }
}

impl tilo_messaging_protocol::sms_runtime::SimAuthority for Gate {
    fn check(&self) -> Result<(), tilo_messaging_protocol::Error> {
        Gate::check(self).map_err(|_| tilo_messaging_protocol::Error::WrongAccount)
    }
}

/// A running account of any service, as the service keeps it.
pub(crate) trait Running: Send + Sync {
    fn gate(&self) -> &Arc<Gate>;
    fn supervisor(&self) -> &Arc<AccountSupervisor>;
    fn client(&self) -> MessagingClient {
        self.supervisor().client()
    }
    fn state(&self) -> AccountState {
        use tilo_messaging::MessagingHost as _;
        self.supervisor().account_state(&self.gate().account())
    }
    /// Stop the account: it fails every check from now on and reads locked.
    fn retire(&self) {
        self.gate().stop();
        let _ = self.supervisor().unavailable(AccountState::Locked);
    }
}
