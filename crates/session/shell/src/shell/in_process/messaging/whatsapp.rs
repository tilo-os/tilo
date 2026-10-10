//! Existing signed admission and credential grants for the actual `WhatsApp` SDK.
use super::{
    AccountAuthority, AccountState, AccountSupervisor, Arc, AtomicU8, Authorization, LaunchError,
    NativeMessagingCredential, NativeMessagingIdentity, Ordering, Owner, Route, SendError,
};
use std::future::Future;
use tilo_messaging_protocol::{
    external_broker::{ExternalAuthority, ExternalBroker},
    supervisor::whatsapp::{Network, WhatsappBootstrap},
};

/// The current native host and reviewed Contacts authority, never account labels.
pub struct WhatsappAccountStartup {
    pub identity: NativeMessagingIdentity,
    pub credential: NativeMessagingCredential,
    pub authority: Arc<dyn ExternalAuthority>,
}

/// Existing private custody and explicitly authorized native network providers.
/// The factory does not discover endpoints, connect, grant access or log in.
pub struct NativeWhatsappHandles {
    pub private_root: std::path::PathBuf,
    pub scope: String,
    pub network: Network,
}

struct GrantedWhatsappAuthority(WhatsappAccountStartup);
impl GrantedWhatsappAuthority {
    fn check(&self) -> Result<(), SendError> {
        self.0.identity.check()?;
        if self.0.credential.owner != self.0.identity.owner {
            return Err(SendError::Revoked);
        }
        self.0.credential.check()
    }
    fn checked(&self, authorization: Authorization) -> Result<Authorization, SendError> {
        self.check()?;
        if authorization.binding.owner != self.0.identity.owner {
            return Err(SendError::Revoked);
        }
        Ok(authorization)
    }
}
impl ExternalAuthority for GrantedWhatsappAuthority {
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.0.authority.authorize(route)?)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.0.authority.incoming(recipient)?)
    }
}
impl AccountAuthority for GrantedWhatsappAuthority {
    fn check(&self) -> Result<(), SendError> {
        GrantedWhatsappAuthority::check(self)
    }
}

/// Retained by the native host; apps receive only its existing Messages loan.
/// A new grant can activate this exact account without replacing open chats.
pub struct NativeWhatsappAccount {
    gate: Arc<GrantedWhatsappAuthority>,
    supervisor: Arc<AccountSupervisor>,
    activation: AtomicU8,
}

struct Activation<'a> {
    account: &'a NativeWhatsappAccount,
    finished: bool,
}
impl Drop for Activation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // A cancelled factory drops its local SDK/broker before this guard.
            // No new generation, credentials or automatic retry is created.
            let state = if self.account.gate.check().is_err() {
                AccountState::Locked
            } else {
                AccountState::Failed
            };
            let _ = self.account.supervisor.unavailable(state);
            self.account.activation.store(0, Ordering::Release);
        }
    }
}

impl NativeWhatsappAccount {
    pub fn client(&self) -> tilo_messaging::MessagingClient {
        self.supervisor.client()
    }
    /// Assemble on the native worker, retaining exact current authority before
    /// and after async SDK construction. Duplicate activation performs no I/O.
    /// Cancellation or initial failure permits only an explicit recovery attempt.
    /// # Errors
    /// Refuses revoked authority, duplicate activation or invalid SDK assembly.
    pub async fn activate<F, A>(&self, assemble: A) -> Result<(), SendError>
    where
        A: FnOnce(Arc<dyn ExternalAuthority>) -> F + Send,
        F: Future<Output = Result<(Arc<ExternalBroker>, WhatsappBootstrap), SendError>> + Send,
    {
        self.activation
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SendError::AlreadyReserved)?;
        let mut activation = Activation {
            account: self,
            finished: false,
        };
        self.gate.check()?;
        let (broker, bootstrap) = assemble(self.gate.clone()).await?;
        self.gate.check()?;
        self.supervisor.install_whatsapp(broker, bootstrap).await?;
        self.activation.store(2, Ordering::Release);
        activation.finished = true;
        drop(activation);
        Ok(())
    }
    /// Use existing OS-keychain state with the host's authorized network factory.
    /// No network connection, login, provisioning or permission UI is started.
    /// # Errors
    /// Refuses relative storage, missing custody, revoked or duplicate activation.
    pub async fn activate_native(&self, handles: NativeWhatsappHandles) -> Result<(), SendError> {
        if !handles.private_root.is_absolute() {
            return Err(SendError::InvalidRequest);
        }
        let handles = NativeWhatsappHandles {
            network: handles.network.guarded(self.gate.clone()),
            ..handles
        };
        self.activate(|authority| {
            assemble_native(
                &self.gate.0.identity.owner,
                handles,
                authority,
                self.gate.clone(),
            )
        })
        .await
    }
}

async fn assemble_native(
    owner: &Owner,
    handles: NativeWhatsappHandles,
    authority: Arc<dyn ExternalAuthority>,
    account_authority: Arc<dyn AccountAuthority>,
) -> Result<(Arc<ExternalBroker>, WhatsappBootstrap), SendError> {
    let bootstrap = WhatsappBootstrap::open_system(
        &handles.private_root.join("sdk"),
        owner.clone(),
        handles.network,
        account_authority,
    )
    .await?;
    let journal = tilo_messaging_protocol::journal::EncryptedJournal::open_system(
        &handles.private_root.join("journal"),
        owner,
    )
    .map_err(|_| SendError::Unavailable)?;
    let runtime = tilo_messaging_protocol::whatsapp_runtime::WhatsappRuntime::new(
        owner.clone(),
        bootstrap.client(),
        journal,
    )
    .map_err(|_| SendError::Unavailable)?;
    let broker = ExternalBroker::open_system(
        &handles.private_root,
        Box::new(runtime),
        handles.scope,
        authority,
    )?;
    Ok((Arc::new(broker), bootstrap))
}

impl super::super::InProcessApps {
    /// Register the current signed account through the normal Messages mount.
    /// Missing grants stay locked without calling the factory. The native host
    /// retains the returned handle for an explicit grant/recovery operation.
    /// # Errors
    /// Refuses another persona/profile/service, credential binding or duplicate.
    pub fn prepare_whatsapp_account(
        &mut self,
        mut startup: WhatsappAccountStartup,
    ) -> Result<Arc<NativeWhatsappAccount>, LaunchError> {
        self.bind_native_messaging_identity(&mut startup.identity)?;
        let owner = &startup.identity.owner;
        if owner.principal != self.author.0
            || owner.device != self.device_id
            || owner.service != tilo_messaging_protocol::Service::Whatsapp
            || startup.credential.owner != *owner
        {
            return Err(LaunchError::BindingMismatch);
        }
        startup
            .identity
            .check()
            .map_err(|_| LaunchError::BindingMismatch)?;
        let gate = Arc::new(GrantedWhatsappAuthority(startup));
        let supervisor =
            AccountSupervisor::new_with_authority(&gate.0.identity.owner, Some(gate.clone()))
                .map_err(|_| LaunchError::BindingMismatch)?;
        self.install_messaging_supervisor(&supervisor)?;
        Ok(Arc::new(NativeWhatsappAccount {
            gate,
            supervisor,
            activation: AtomicU8::new(0),
        }))
    }
}
