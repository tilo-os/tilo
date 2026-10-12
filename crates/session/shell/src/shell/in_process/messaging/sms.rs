//! The selected native SIM uses the original protected account key and grant.
use super::{
    AccountAuthority, AccountState, AccountSupervisor, Arc, Authorization,
    NativeMessagingCredential, NativeMessagingIdentity, Route, SendError,
};
use tilo_messaging_protocol::{
    Error,
    external_broker::{ExternalAuthority, ExternalBroker},
    sms_runtime::{DbusTransport, SimAuthority, SimBinding, SmsRuntime},
};

pub(super) struct Startup {
    pub identity: NativeMessagingIdentity,
    pub credential: NativeMessagingCredential,
    pub authority: Arc<dyn ExternalAuthority>,
}
struct Grant {
    startup: Startup,
    stopped: std::sync::atomic::AtomicBool,
}
impl Grant {
    fn check(&self) -> Result<(), SendError> {
        if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SendError::Revoked);
        }
        self.startup.identity.check()?;
        if self.startup.credential.owner != self.startup.identity.owner {
            return Err(SendError::Revoked);
        }
        self.startup.credential.check()
    }
    fn checked(&self, authorization: Authorization) -> Result<Authorization, SendError> {
        self.check()?;
        if authorization.binding.owner != self.startup.identity.owner {
            return Err(SendError::Revoked);
        }
        Ok(authorization)
    }
}
impl SimAuthority for Grant {
    fn check(&self) -> Result<(), Error> {
        Grant::check(self).map_err(|_| Error::WrongAccount)
    }
}
impl AccountAuthority for Grant {
    fn check(&self) -> Result<(), SendError> {
        Grant::check(self)
    }
}
impl ExternalAuthority for Grant {
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.startup.authority.authorize(route)?)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.startup.authority.incoming(recipient)?)
    }
}
/// Retained by the first-account owner. Dropping setup never grants access.
pub(super) struct Account {
    gate: Arc<Grant>,
    supervisor: Arc<AccountSupervisor>,
}
impl Account {
    pub(super) async fn activate(
        &self,
        connection: zbus::Connection,
        binding: SimBinding,
        root: std::path::PathBuf,
        scope: String,
    ) -> Result<(), SendError> {
        let result = self.assemble(connection, binding, root, scope, true).await;
        if result.is_err() {
            let state = if self.gate.check().is_ok() {
                AccountState::Failed
            } else {
                AccountState::Locked
            };
            let _ = self.supervisor.unavailable(state);
        }
        result
    }
    async fn assemble(
        &self,
        connection: zbus::Connection,
        binding: SimBinding,
        root: std::path::PathBuf,
        scope: String,
        save: bool,
    ) -> Result<(), SendError> {
        self.gate.check()?;
        if !root.is_absolute() {
            return Err(SendError::InvalidRequest);
        }
        let owner = &self.gate.startup.identity.owner;
        let transport = DbusTransport::new(connection, binding.clone(), self.gate.clone())
            .map_err(|_| SendError::Revoked)?;
        transport.validate().await.map_err(|_| SendError::Revoked)?;
        self.gate.check()?;
        if save {
            private_fs::directory(&root).map_err(|_| SendError::Unavailable)?;
            self.gate.check()?;
            binding
                .save(
                    &root.join("sim.enc"),
                    owner,
                    &self.gate.startup.credential.profile_key()?,
                )
                .map_err(|_| SendError::Unavailable)?;
            self.gate.check()?;
        }
        let journal = tilo_messaging_protocol::journal::EncryptedJournal::open(
            &root.join("journal"),
            owner,
            self.gate.startup.credential.profile_key()?,
        )
        .map_err(|_| SendError::Unavailable)?;
        self.gate.check()?;
        let history = tilo_messaging_protocol::signal_broker::History::open(
            &root.join("history"),
            owner,
            self.gate.startup.credential.profile_key()?,
        )?;
        self.gate.check()?;
        let runtime = SmsRuntime::new(owner.clone(), transport, journal)
            .map_err(|_| SendError::Unavailable)?;
        let broker = ExternalBroker::new(Box::new(runtime), history, scope, self.gate.clone())?;
        self.gate.check()?;
        self.supervisor.install_sms_receiving(&broker).await?;
        self.gate.check()
    }
    pub(super) async fn resume(
        &self,
        connection: zbus::Connection,
        root: std::path::PathBuf,
        scope: String,
    ) -> Result<(), SendError> {
        self.gate.check()?;
        let binding = SimBinding::load(
            &root.join("sim.enc"),
            &self.gate.startup.identity.owner,
            &self.gate.startup.credential.profile_key()?,
            &connection,
        )
        .await
        .map_err(|_| SendError::Revoked)?;
        self.gate.check()?;
        self.assemble(connection, binding, root, scope, false).await
    }
    pub(super) fn state(&self) -> AccountState {
        use tilo_messaging::MessagingHost as _;
        self.supervisor
            .account_state(&tilo_messaging::ServiceAccount::new(
                "sms",
                &self.gate.startup.identity.owner.account,
            ))
    }
    pub(super) fn retire(&self) {
        self.gate
            .stopped
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.supervisor.unavailable(AccountState::Locked);
    }
}
impl super::super::InProcessApps {
    pub(super) fn prepare_sms_account(
        &mut self,
        mut startup: Startup,
    ) -> Result<Arc<Account>, ui::launch::LaunchError> {
        use ui::launch::LaunchError;
        self.bind_native_messaging_identity(&mut startup.identity)?;
        let owner = &startup.identity.owner;
        if owner.principal != self.author.0
            || owner.device != self.device_id
            || owner.service != tilo_messaging_protocol::Service::Sms
            || startup.credential.owner != *owner
        {
            return Err(LaunchError::BindingMismatch);
        }
        startup
            .identity
            .check()
            .map_err(|_| LaunchError::BindingMismatch)?;
        let gate = Arc::new(Grant {
            startup,
            stopped: std::sync::atomic::AtomicBool::new(false),
        });
        let supervisor =
            AccountSupervisor::new_with_authority(&gate.startup.identity.owner, Some(gate.clone()))
                .map_err(|_| LaunchError::BindingMismatch)?;
        self.install_messaging_supervisor(&supervisor)?;
        Ok(Arc::new(Account { gate, supervisor }))
    }
}
