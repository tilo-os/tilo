//! Native account startup through existing personal admission and vault grants.
//! The host supplies its owned adapters; this module discovers no sockets,
//! creates no credentials and never asks a background permission question.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};

use accounts::{AccessRequest, CredentialBroker, CredentialScope};
use tilo_action_broker::services::CertifiedPersonContext;
use tilo_contact_directory::routing::Route;
use tilo_identity_host::custody::HostIdentity;
use tilo_messaging::{AccountState, SendError};
use tilo_messaging_protocol::{
    journal::Owner,
    signal_broker::{Authorization, SignalAuthority, SignalBroker},
    signal_runtime::SignalReceiver,
    supervisor::{AccountAuthority, AccountSupervisor},
};
use ui::launch::LaunchError;

pub(crate) mod activation;
mod enrollment;
pub(super) mod first_account;
pub(crate) mod native;
pub(crate) mod native_first;
pub(super) mod network;
pub(super) mod provider;
pub mod records;
mod working_profile;
pub use provider::NativeSignalStorage;
pub use working_profile::NativeSignalWorkingProfile;
pub(super) mod sms;
mod sms_first;
pub mod whatsapp;
pub use activation::NativeSignalActivation;
pub use enrollment::{NativeSignalEnrollment, NativeVerifiedSignalEnrollment};

/// Hand an enrollment owner loop the request `request` builds around its
/// reply channel, and wait up to `timeout` for the answer. A full or closed
/// queue answers [`SendError::Unavailable`].
fn ask_owner<R, T: Send + 'static>(
    sender: &std::sync::mpsc::SyncSender<R>,
    wake: &crate::session_runtime::Wake,
    timeout: std::time::Duration,
    request: impl FnOnce(tokio::sync::oneshot::Sender<Result<T, SendError>>) -> R,
) -> std::pin::Pin<Box<dyn Future<Output = Result<T, SendError>> + Send>> {
    let (reply, receiver) = tokio::sync::oneshot::channel();
    let submitted = sender.try_send(request(reply)).is_ok();
    wake();
    Box::pin(async move {
        if !submitted {
            return Err(SendError::Unavailable);
        }
        tokio::time::timeout(timeout, receiver)
            .await
            .map_err(|_| SendError::Unavailable)?
            .map_err(|_| SendError::Unavailable)?
    })
}

/// Signed personal admission bound to the actual native cryptographic custody
/// and the host profile's separate SDK device identifier. Neither identifier
/// is inferred from editable account metadata.
#[derive(Clone)]
pub struct NativeMessagingIdentity {
    person: CertifiedPersonContext,
    host: HostIdentity,
    owner: Owner,
    workspace: u64,
    native_session: Option<Arc<crate::shell::collab_broker::NativePersonalSession>>,
}
impl NativeMessagingIdentity {
    /// # Errors
    /// Refuses another root/device, retired admission or unavailable native custody.
    pub fn new(
        person: CertifiedPersonContext,
        host: HostIdentity,
        owner: Owner,
    ) -> Result<Self, SendError> {
        let mut person_public_key = [0; 32];
        hex::decode_to_slice(person.principal(), &mut person_public_key)
            .map_err(|_| SendError::Revoked)?;
        let room = collab_types::person_room(&person_public_key).to_string();
        let workspace =
            tilo_relay_protocol::room_workspace_id(&room).map_err(|_| SendError::Revoked)?;
        let identity = Self {
            person,
            host,
            owner,
            workspace,
            native_session: None,
        };
        identity.check()?;
        Ok(identity)
    }
    fn check(&self) -> Result<(), SendError> {
        if let Some(session) = &self.native_session {
            session.check().map_err(|_| SendError::Revoked)?;
        }
        self.person.revalidate().map_err(|_| SendError::Revoked)?;
        let (certificate, _) = self
            .host
            .current_native_material(self.workspace)
            .map_err(|_| SendError::Revoked)?;
        if self.person.principal() != self.owner.principal
            || hex::encode(certificate.user_public_key) != self.person.principal()
            || hex::encode(certificate.device_public_key) != self.person.device()
            || certificate.capabilities.foreign
        {
            return Err(SendError::Revoked);
        }
        Ok(())
    }
}

/// The native host's exact credential binding. The broker is the same owner
/// the permission UI revokes; a serialized grant or boolean is never accepted.
#[derive(Clone)]
pub struct NativeMessagingCredential {
    broker: Arc<Mutex<CredentialBroker>>,
    request: AccessRequest,
    owner: Owner,
}
fn credential_scope(owner: &Owner) -> CredentialScope {
    CredentialScope {
        principal: owner.principal.clone(),
        device: owner.device.clone(),
        service: match owner.service {
            tilo_messaging_protocol::Service::Signal => "signal",
            tilo_messaging_protocol::Service::Whatsapp => "whatsapp",
            tilo_messaging_protocol::Service::Sms => "sms",
            tilo_messaging_protocol::Service::Tilo => "tilo",
        }
        .into(),
        account: owner.account.clone(),
        generation: owner.linking_generation,
    }
}
impl NativeMessagingCredential {
    /// Bind an existing vault item to one native assembly without requesting
    /// access. The host supplies this association, never an app/wire request.
    /// # Errors
    /// Refuses another application or an unscoped credential request.
    pub fn new(
        broker: Arc<Mutex<CredentialBroker>>,
        request: AccessRequest,
        owner: Owner,
    ) -> Result<Self, SendError> {
        if request.app_id != "so.tilo.messages"
            || request.origin.is_none()
            || owner.linking_generation == 0
            || owner.account.is_empty()
        {
            return Err(SendError::Revoked);
        }
        Ok(Self {
            broker,
            request,
            owner,
        })
    }
    fn check(&self) -> Result<(), SendError> {
        // The temporary decrypted value clears itself immediately. No bearer
        // secret is copied into Contacts, the UI, logs or the authority snapshot.
        self.broker
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .request_existing_scoped(&self.request, &credential_scope(&self.owner))
            .map(drop)
            .map_err(|_| SendError::Revoked)
    }

    fn profile_key(&self) -> Result<secret_vault::VaultKey, SendError> {
        let encoded = self
            .broker
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .request_existing_profile_key(&self.request, &credential_scope(&self.owner))
            .map_err(|_| SendError::Revoked)?;
        let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(encoded.as_bytes(), bytes.as_mut())
            .map_err(|_| SendError::Unavailable)?;
        secret_vault::VaultKey::from_slice(bytes.as_ref()).map_err(|_| SendError::Unavailable)
    }
}

/// Inputs retained by startup and every native Signal I/O operation. Contacts
/// and provider fingerprint validation remain the existing authority's job.
pub struct SignalAccountStartup {
    pub identity: NativeMessagingIdentity,
    pub credential: NativeMessagingCredential,
    pub authority: Arc<dyn SignalAuthority>,
}

/// Handles already owned and authenticated by the native process supervisor.
/// The two streams remain independent; startup never discovers an ambient
/// daemon socket or spawns/logs in to an account.
pub struct NativeSignalHandles {
    pub private_root: std::path::PathBuf,
    pub signal_account: String,
    pub sender: std::os::unix::net::UnixStream,
    pub receiver: std::os::unix::net::UnixStream,
}
struct GrantedSignalAuthority {
    startup: SignalAccountStartup,
}
impl GrantedSignalAuthority {
    fn check(&self) -> Result<(), SendError> {
        self.startup.identity.check()?;
        if self.startup.credential.owner != self.startup.identity.owner {
            return Err(SendError::Revoked);
        }
        self.startup.credential.check()?;
        self.startup.authority.check_account()
    }
    fn checked(&self, auth: Authorization) -> Result<Authorization, SendError> {
        self.check()?;
        if auth.binding.owner != self.startup.identity.owner {
            return Err(SendError::Revoked);
        }
        Ok(auth)
    }
}
impl SignalAuthority for GrantedSignalAuthority {
    fn check_account(&self) -> Result<(), SendError> {
        self.check()
    }
    fn check_account_scope(&self, scope: &str) -> Result<(), SendError> {
        self.check()?;
        self.startup.authority.check_account_scope(scope)?;
        self.check()
    }
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.startup.authority.authorize(route)?)
    }
    fn incoming(&self, aci: &str) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.startup.authority.incoming(aci)?)
    }
}
impl AccountAuthority for GrantedSignalAuthority {
    fn check(&self) -> Result<(), SendError> {
        GrantedSignalAuthority::check(self)
    }
}

/// The UI host retains native custody without exposing its implementation or
/// opening vault/storage on the UI loop. Future service owners share this door.
pub(super) trait NativeAccountCustody: Send + Sync {
    fn owner(&self) -> &Owner;
}

/// Native host-owned account setup handle. Open Messages windows retain the
/// same supervisor while an explicit grant activates the existing account.
/// This handle is never serialized or lent to an application.
pub struct NativeSignalAccount {
    gate: Arc<GrantedSignalAuthority>,
    supervisor: Arc<AccountSupervisor>,
    /// 0: no installed adapter; 1: one activation; 2: installed for this generation.
    activation: AtomicU8,
}
impl NativeAccountCustody for NativeSignalAccount {
    fn owner(&self) -> &Owner {
        &self.gate.startup.identity.owner
    }
}
/// Creates or opens a sealed Signal profile under a key and grant.
type UnsealProfile = fn(
    &std::path::Path,
    Owner,
    secret_vault::VaultKey,
    Arc<dyn AccountAuthority>,
) -> Result<
    tilo_messaging_protocol::signal_profile::EncryptedSignalProfile,
    tilo_messaging_protocol::signal_profile::ProfileError,
>;
impl NativeSignalAccount {
    /// Create a first sealed profile under the original approved account key.
    /// Existing profiles are refused; neither grant nor provider is created.
    /// # Errors
    /// Refuses retired admission/grants, weaker custody or occupied storage.
    pub fn create_profile(
        &self,
        root: &std::path::Path,
    ) -> Result<tilo_messaging_protocol::signal_profile::EncryptedSignalProfile, SendError> {
        self.sealed_profile(
            root,
            tilo_messaging_protocol::signal_profile::EncryptedSignalProfile::create_with_key,
        )
    }
    /// Open this original account's sealed profile using only its existing
    /// system-custodied, scope-bound credential. This does not start a provider
    /// or make the account ready.
    /// # Errors
    /// Refuses retired admission/grants, weaker key custody or invalid storage.
    pub fn open_profile(
        &self,
        root: &std::path::Path,
    ) -> Result<tilo_messaging_protocol::signal_profile::EncryptedSignalProfile, SendError> {
        self.sealed_profile(
            root,
            tilo_messaging_protocol::signal_profile::EncryptedSignalProfile::open_with_key,
        )
    }
    /// Run `unseal` on `root` with this account's owner, profile key and
    /// grant, checking the grant both before and after.
    fn sealed_profile(
        &self,
        root: &std::path::Path,
        unseal: UnsealProfile,
    ) -> Result<tilo_messaging_protocol::signal_profile::EncryptedSignalProfile, SendError> {
        self.gate.check()?;
        if !root.is_absolute() {
            return Err(SendError::Unavailable);
        }
        let key = self.gate.startup.credential.profile_key()?;
        let profile = unseal(
            root,
            self.gate.startup.identity.owner.clone(),
            key,
            self.gate.clone(),
        )
        .map_err(|_| SendError::Unavailable)?;
        self.gate.check()?;
        Ok(profile)
    }
    /// Prepare a separate private provisioning generation after the host's
    /// setup door confirms the operation. The intended ACI is frozen before
    /// any provider I/O, and linking never publishes a ready message adapter.
    /// # Errors
    /// Refuses retired admission/grants, malformed identity or unusable handles.
    pub fn prepare_linking(
        &self,
        expected_aci: String,
        stream: std::os::unix::net::UnixStream,
    ) -> Result<Arc<tilo_messaging_protocol::signal_linking::SignalLinking>, SendError> {
        self.gate.check()?;
        let linker = tilo_messaging_protocol::signal_linking::SignalLinking::new(
            self.gate.startup.identity.owner.clone(),
            expected_aci,
            self.gate.clone(),
            stream,
        )?;
        self.supervisor.install_setup(linker.client())?;
        Ok(linker)
    }
    pub fn client(&self) -> tilo_messaging::MessagingClient {
        self.supervisor.client()
    }
    /// Activate only the retained exact owner and current grant. Concurrent or
    /// repeated activation never opens a second daemon connection or receiver.
    /// A failed initial assembly may be retried explicitly with new owned handles.
    /// # Errors
    /// Refuses revoked authority, duplicate activation or failed native assembly.
    pub fn activate(
        &self,
        assemble: impl FnOnce(
            Arc<dyn SignalAuthority>,
        ) -> Result<(SignalBroker, SignalReceiver), SendError>,
    ) -> Result<(), SendError> {
        self.activation
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SendError::AlreadyReserved)?;
        let result = self
            .gate
            .check()
            .and_then(|()| assemble(self.gate.clone()))
            .and_then(|(broker, receiver)| self.install_verified(&broker, receiver));
        self.activation
            .store(if result.is_ok() { 2 } else { 0 }, Ordering::Release);
        if result.is_err() {
            let state = if self.gate.check().is_err() {
                AccountState::Locked
            } else {
                AccountState::Failed
            };
            self.supervisor.unavailable(state)?;
        }
        result
    }
    fn install_verified(
        &self,
        broker: &SignalBroker,
        receiver: SignalReceiver,
    ) -> Result<(), SendError> {
        self.gate.check_account_scope(broker.scope())?;
        self.supervisor.install_signal_receiving(broker, receiver)
    }
    /// Resume the exact retained account through original approved credential custody.
    /// No grant, socket discovery, daemon spawn or login is performed here.
    /// # Errors
    /// Refuses relative storage, revoked authority or another activation.
    pub fn activate_native(&self, handles: NativeSignalHandles) -> Result<(), SendError> {
        if !handles.private_root.is_absolute() {
            return Err(SendError::InvalidRequest);
        }
        self.activate(|authority| assemble_native(self, handles, authority))
    }
}
fn assemble_native(
    account: &NativeSignalAccount,
    handles: NativeSignalHandles,
    authority: Arc<dyn SignalAuthority>,
) -> Result<(SignalBroker, SignalReceiver), SendError> {
    account.gate.check()?;
    // Enrollment approved this exact account key. A fresh account has no
    // unrelated messaging key in the OS keychain, and must not mint one here.
    // Existing vault formats authenticate distinct purposes under this key.
    let key = account.gate.startup.credential.profile_key()?;
    account.gate.check()?;
    let owner = &account.gate.startup.identity.owner;
    let history = tilo_messaging_protocol::signal_broker::History::open(
        &handles.private_root.join("history"),
        owner,
        key.clone(),
    )?;
    let journal = tilo_messaging_protocol::journal::EncryptedJournal::open(
        &handles.private_root.join("journal"),
        owner,
        key,
    )
    .map_err(|_| SendError::Unavailable)?;
    let runtime = tilo_messaging_protocol::signal_runtime::SignalRuntime::from_journal(
        owner.clone(),
        handles.signal_account.clone(),
        handles.sender,
        journal,
    )
    .map_err(|_| SendError::Unavailable)?;
    let broker = SignalBroker::new(runtime, history, authority, handles.signal_account.clone())?;
    let receiver = SignalReceiver::new(owner.clone(), handles.signal_account, handles.receiver)
        .map_err(|_| SendError::Unavailable)?;
    Ok((broker, receiver))
}

impl super::InProcessApps {
    /// Propose a fresh protected Passwords decision for this host's original
    /// native execution. This never approves a grant or lends vault ownership
    /// to Messages; activation still checks current account/contact metadata.
    /// # Errors
    /// Refuses foreign sessions, missing custody, invalid items or stale admission.
    pub fn propose_native_messaging_permission(
        &self,
        identity: &NativeMessagingIdentity,
        request: AccessRequest,
    ) -> Result<String, LaunchError> {
        drop(self.native_messaging_credential(identity, request.clone())?);
        let vault = self
            .credential_owner
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?;
        let retained = identity.clone();
        let id = vault
            .propose_scoped(
                request,
                credential_scope(&identity.owner),
                Arc::new(move || retained.check().is_ok()),
            )
            .map_err(|_| LaunchError::BindingMismatch)?;
        if identity.check().is_err() {
            vault
                .decide_proposal(&id, accounts::AccessDecision::Deny)
                .map_err(|_| LaunchError::BindingMismatch)?;
            return Err(LaunchError::BindingMismatch);
        }
        Ok(id)
    }
    /// Query canonical metadata on this exact native host's current UI owner.
    /// Neither a record label nor a copied file supplies personal admission.
    /// # Errors
    /// Refuses a missing wake/record owner, foreign profile or retired execution.
    pub fn native_messaging_authority(
        &mut self,
        identity: &NativeMessagingIdentity,
        credential: &NativeMessagingCredential,
        scope: &str,
    ) -> Result<Arc<records::CurrentAuthority>, LaunchError> {
        let socket = self
            .native_broker_socket
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?;
        if credential.owner != identity.owner
            || !self
                .credential_owner
                .as_ref()
                .is_some_and(|owner| owner.owns_broker(&credential.broker))
            || identity.owner.linking_generation == 0
            || identity.owner.account.is_empty()
            || scope.is_empty()
            || scope.len() > 512
            || scope.chars().any(char::is_control)
            || identity.owner.principal != self.author.0
            || identity.owner.device != self.device_id
            || !identity
                .native_session
                .as_ref()
                .is_some_and(|session| session.matches_socket(socket))
        {
            return Err(LaunchError::BindingMismatch);
        }
        identity.check().map_err(|_| LaunchError::BindingMismatch)?;
        credential
            .check()
            .map_err(|_| LaunchError::BindingMismatch)?;
        if self.messaging_records.is_none() {
            let wake = self
                .messaging_record_wake
                .clone()
                .ok_or(LaunchError::BindingMismatch)?;
            self.messaging_records = Some(
                records::Records::open(&mut self.owners, &self.author.0, &self.device_id, wake)
                    .map_err(|_| LaunchError::BindingMismatch)?,
            );
        }
        self.messaging_records
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?
            .authority(identity, credential, scope)
            .map_err(|_| LaunchError::BindingMismatch)
    }
    /// Bind the exact current native credential owner after original admission.
    /// Passwords revokes this broker; no independent vault snapshot is opened.
    /// # Errors
    /// Refuses another profile/session, missing custody or an unscoped request.
    pub fn native_messaging_credential(
        &self,
        identity: &NativeMessagingIdentity,
        request: AccessRequest,
    ) -> Result<NativeMessagingCredential, LaunchError> {
        let owner = &identity.owner;
        let vault = self
            .credential_owner
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?;
        let socket = self
            .native_broker_socket
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?;
        if request.app_id != "so.tilo.messages"
            || request.origin.is_none()
            || owner.linking_generation == 0
            || owner.account.is_empty()
            || owner.principal != self.author.0
            || owner.device != self.device_id
            || vault.principal() != owner.principal
            || vault.device() != owner.device
            || !identity
                .native_session
                .as_ref()
                .is_some_and(|session| session.matches_socket(socket))
        {
            return Err(LaunchError::BindingMismatch);
        }
        identity.check().map_err(|_| LaunchError::BindingMismatch)?;
        NativeMessagingCredential::new(
            vault.broker().map_err(|_| LaunchError::BindingMismatch)?,
            request,
            owner.clone(),
        )
        .map_err(|_| LaunchError::BindingMismatch)
    }
    /// Resolve the default native broker's current signed personal execution,
    /// retaining its compositor generation through this exact account setup.
    /// No app label, metadata record or serialized caller establishes custody.
    /// # Errors
    /// Refuses absent/locked native source or another persona/profile owner.
    pub fn native_messaging_identity(
        &self,
        owner: Owner,
    ) -> Result<NativeMessagingIdentity, LaunchError> {
        if owner.principal != self.author.0 || owner.device != self.device_id {
            return Err(LaunchError::BindingMismatch);
        }
        let socket = self
            .native_broker_socket
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?;
        let session = crate::shell::collab_broker::NativePersonalSource::admit_for_socket(
            socket,
            &self.author.0,
        )
        .map_err(|_| LaunchError::BindingMismatch)?;
        let mut identity = NativeMessagingIdentity::new(session.person(), session.host(), owner)
            .map_err(|_| LaunchError::BindingMismatch)?;
        identity.native_session = Some(session);
        Ok(identity)
    }
    fn bind_native_messaging_identity(
        &self,
        identity: &mut NativeMessagingIdentity,
    ) -> Result<(), LaunchError> {
        if let Some(socket) = &self.native_broker_socket {
            if let Some(session) = &identity.native_session {
                if !session.matches_socket(socket) {
                    return Err(LaunchError::BindingMismatch);
                }
                return session.check().map_err(|_| LaunchError::BindingMismatch);
            }
            let mut session = Some(
                crate::shell::collab_broker::NativePersonalSource::admit_for_socket(
                    socket,
                    &self.author.0,
                )
                .map_err(|_| LaunchError::BindingMismatch)?,
            );
            let person = session
                .as_ref()
                .ok_or(LaunchError::BindingMismatch)?
                .person();
            let matches = person.principal() == identity.person.principal()
                && person.device() == identity.person.device();
            drop(person);
            if !matches {
                return Err(LaunchError::BindingMismatch);
            }
            identity.native_session = session.take();
        }
        Ok(())
    }
    /// Assemble the production adapter with original approved account custody, after
    /// checking the account's signed admission and existing credential grant.
    /// Locked/missing custody has no local-file or generated-key fallback.
    /// # Errors
    /// Refuses a relative private state root or mismatched native account.
    pub fn start_native_signal_account(
        &mut self,
        startup: SignalAccountStartup,
        handles: NativeSignalHandles,
    ) -> Result<Arc<NativeSignalAccount>, LaunchError> {
        if !handles.private_root.is_absolute() {
            return Err(LaunchError::BindingMismatch);
        }
        self.start_owned_signal_account(startup, |account, wake| {
            account.activate_native_on_worker(wake, handles)
        })
    }

    /// Prepare and activate through the original native host's worker queue.
    /// The host retains the receipt and accepts it on its ordinary loop, even
    /// without an open Messages window. No result outlives that owning host.
    /// # Errors
    /// Refuses an absent native wake, a full bounded queue or invalid startup.
    pub fn start_signal_account_on_worker(
        &mut self,
        startup: SignalAccountStartup,
        assemble: impl FnOnce(
            Arc<dyn SignalAuthority>,
        ) -> Result<(SignalBroker, SignalReceiver), SendError>
        + Send
        + 'static,
    ) -> Result<Arc<NativeSignalAccount>, LaunchError> {
        self.start_owned_signal_account(startup, |account, wake| {
            account.activate_on_worker(wake, assemble)
        })
    }

    fn start_owned_signal_account(
        &mut self,
        startup: SignalAccountStartup,
        activate: impl FnOnce(
            &Arc<NativeSignalAccount>,
            crate::session_runtime::Wake,
        ) -> Result<NativeSignalActivation, SendError>,
    ) -> Result<Arc<NativeSignalAccount>, LaunchError> {
        let wake = self
            .messaging_record_wake
            .clone()
            .ok_or(LaunchError::BindingMismatch)?;
        if self.messaging_starts.is_none() {
            self.messaging_starts = Some(activation::starts());
        }
        let starts = self
            .messaging_starts
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?;
        if !starts.available() {
            return Err(LaunchError::BindingMismatch);
        }
        let account = self.prepare_signal_account(startup)?;
        if let Ok(pending) = activate(&account, wake) {
            self.messaging_starts
                .as_ref()
                .ok_or(LaunchError::BindingMismatch)?
                .push(pending)
                .map_err(|_| LaunchError::BindingMismatch)?;
        }
        Ok(account)
    }

    /// Start one account through current signed admission and an existing
    /// credential grant, and lend its lifecycle through the normal Messages
    /// mount. `assemble` receives the checked authority before any native RPC;
    /// it supplies the already owned send/receive handles and encrypted history.
    /// An ungranted account is visible as locked and never calls `assemble`.
    /// # Errors
    /// Refuses another host profile, credential binding or duplicate account.
    pub fn start_signal_account(
        &mut self,
        startup: SignalAccountStartup,
        assemble: impl FnOnce(
            Arc<dyn SignalAuthority>,
        ) -> Result<(SignalBroker, SignalReceiver), SendError>,
    ) -> Result<Arc<NativeSignalAccount>, LaunchError> {
        let account = self.prepare_signal_account(startup)?;
        // The public lifecycle reports the assembly result; the retained handle
        // allows a later explicit grant/recovery without replacing its UI loan.
        let _ = account.activate(assemble);
        Ok(account)
    }

    /// Retain an exact native account without performing provider/storage I/O.
    /// The owning UI can mount its unavailable/locked lifecycle immediately,
    /// then activate its owned handles on the existing session worker pool.
    /// # Errors
    /// Refuses another profile, retired execution or duplicate exact account.
    pub fn prepare_signal_account(
        &mut self,
        mut startup: SignalAccountStartup,
    ) -> Result<Arc<NativeSignalAccount>, LaunchError> {
        self.bind_native_messaging_identity(&mut startup.identity)?;
        let owner = &startup.identity.owner;
        if owner.principal != self.author.0
            || owner.device != self.device_id
            || owner.service != tilo_messaging_protocol::Service::Signal
            || startup.credential.owner != *owner
        {
            return Err(LaunchError::BindingMismatch);
        }
        startup
            .identity
            .check()
            .map_err(|_| LaunchError::BindingMismatch)?;
        let gate = Arc::new(GrantedSignalAuthority { startup });
        let supervisor =
            AccountSupervisor::new_with_authority(&gate.startup.identity.owner, Some(gate.clone()))
                .map_err(|_| LaunchError::BindingMismatch)?;
        self.install_messaging_supervisor(&supervisor)?;
        Ok(Arc::new(NativeSignalAccount {
            gate,
            supervisor,
            activation: AtomicU8::new(0),
        }))
    }
}

#[cfg(test)]
#[path = "messaging_tests.rs"]
mod tests;
