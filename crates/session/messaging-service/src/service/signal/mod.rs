//! Signal through its signed provider, run by the launcher in its own
//! sandbox on a RAM working copy of the account's sealed profile.
//!
//! Enrollment links a new device in a private RAM directory before any
//! account, key or grant exists; only a provider that exited cleanly yields
//! a profile, which is sealed under the account's key once the person
//! approved it. A running account starts the provider on that profile and
//! talks to it over two separately authenticated streams.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use launcher::owned_service::{ServiceCancellation, ServiceEndpoint, ServiceLaunch};
use tilo_messaging::{AccountState, SendError};
use tilo_messaging_protocol::journal::Owner;
use tilo_messaging_protocol::signal_broker::{SignalAuthority, SignalBroker};
use tilo_messaging_protocol::signal_linking::{
    SignalEnrollment, SignalPairing, VerifiedSignalEnrollment,
};
use tilo_messaging_protocol::signal_profile::{EncryptedSignalProfile, ProfileError};
use tilo_messaging_protocol::signal_runtime::SignalReceiver;
use tilo_messaging_protocol::supervisor::{AccountAuthority, AccountSupervisor};

use super::consent::{self, Policy};
use super::gate::{Gate, Running};
use super::identity::Identity;

pub(crate) mod working_profile;

use working_profile::provision::{self, UnpublishedSignalProfile};

/// Where a running account keeps its sealed profile and history, and the
/// RAM directory its provider runs in. No app chooses these.
pub(crate) struct Storage {
    pub profile: PathBuf,
    pub history: PathBuf,
    pub runtime: PathBuf,
    pub signal_account: String,
}

/// How long a just-started provider has to accept its two connections.
const CONNECT: Duration = Duration::from_secs(20);
/// How long the service waits for a started provider's streams.
const STREAMS: Duration = Duration::from_secs(25);

const CATALOGUE_OUTPUT_LIMIT: usize = 1024 * 1024;
const CATALOGUE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The catalogue program that installs packages.
fn catalogue() -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("app")));
    beside
        .into_iter()
        .chain([
            PathBuf::from("/usr/libexec/app"),
            PathBuf::from("/usr/bin/app"),
        ])
        .find(|path| path.exists())
}

/// Install the signed provider through the catalogue's normal verification.
fn install_provider() -> io::Result<()> {
    let program = catalogue().ok_or(io::ErrorKind::NotFound)?;
    let mut command = std::process::Command::new(program);
    command.args(["install", working_profile::SERVICE_PACKAGE]);
    let output =
        bounded_command::run_prepared(command, None, CATALOGUE_OUTPUT_LIMIT, CATALOGUE_TIMEOUT)
            .map_err(io::Error::other)?;
    if !output.status.success() {
        return Err(io::ErrorKind::Other.into());
    }
    Ok(())
}

fn installed_launch() -> Result<ServiceLaunch, SendError> {
    let installed = package_store::find_installed(working_profile::SERVICE_PACKAGE)
        .map_err(|_| SendError::Unavailable)?
        .ok_or(SendError::Unavailable)?;
    Ok(ServiceLaunch {
        manifest: installed.manifest_path,
        payload: installed.payload_root,
        options: launcher::run::RunOptions {
            app_id: Some(working_profile::SERVICE_PACKAGE.into()),
            expected_package_hash: Some(installed.package_id_hex),
            mode: launcher::run::LaunchMode::Service {
                passthrough_argv: vec![],
            },
            no_services: true,
            identity: launcher::run::IdentitySource::None,
            ..launcher::run::RunOptions::default()
        },
    })
}

/// The provider's launch, installing it first when it is missing.
pub(crate) fn acquire_launch(
    check: impl Fn() -> Result<(), SendError>,
    cancellation: &ServiceCancellation,
) -> Result<ServiceLaunch, SendError> {
    check()?;
    if cancellation.is_cancelled() {
        return Err(SendError::Revoked);
    }
    if package_store::find_installed(working_profile::SERVICE_PACKAGE)
        .map_err(|_| SendError::Unavailable)?
        .is_none()
    {
        install_provider().map_err(|_| SendError::Unavailable)?;
    }
    check()?;
    if cancellation.is_cancelled() {
        return Err(SendError::Revoked);
    }
    installed_launch()
}

/// The provider's two connections, once it listens.
pub(crate) fn streams(
    endpoint: &ServiceEndpoint,
) -> io::Result<[std::os::unix::net::UnixStream; 2]> {
    let deadline = Instant::now()
        .checked_add(CONNECT)
        .ok_or(io::ErrorKind::InvalidData)?;
    let first = loop {
        match endpoint.connect() {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    };
    Ok([first, endpoint.connect()?])
}

/// Linking a new device, before the account exists: the provider runs on
/// a private RAM directory and the attempt keeps the original admission.
struct ProvisioningAuthority(Identity);

impl AccountAuthority for ProvisioningAuthority {
    fn check(&self) -> Result<(), SendError> {
        self.0.check()
    }
}

/// One enrollment's link: the provider it started and what it must stop.
pub(crate) struct Linking {
    identity: Identity,
    linker: SignalEnrollment,
    stopped: mpsc::Receiver<Result<UnpublishedSignalProfile, SendError>>,
    service: ServiceCancellation,
}

/// A link the person completed on their phone: the account it names and
/// the profile the provider left.
pub(crate) struct Linked {
    identity: Identity,
    verified: VerifiedSignalEnrollment,
    profile: UnpublishedSignalProfile,
}

impl Linking {
    /// Start a provider on a private RAM directory and begin linking. Blocks.
    pub(crate) fn start(
        identity: Identity,
        runtime: &Path,
        policy: &Policy,
    ) -> Result<(Self, SignalPairing), SendError> {
        identity.check()?;
        let provision = provision::provision(identity.clone(), runtime, policy)?;
        let linker = SignalEnrollment::new(
            identity.owner.clone(),
            Arc::new(ProvisioningAuthority(identity.clone())),
            provision.stream,
        )?;
        let pairing = tokio::runtime::Handle::current().block_on(linker.begin())?;
        Ok((
            Self {
                identity,
                linker,
                stopped: provision.stopped,
                service: policy.cancellation.clone(),
            },
            pairing,
        ))
    }

    /// Finish the link the person scanned and stop the provider. Blocks.
    pub(crate) fn finish(self, attempt: String) -> Result<Linked, SendError> {
        let verified = tokio::runtime::Handle::current().block_on(self.linker.finish(attempt))?;
        if self.identity.check().is_err() {
            self.linker.cancel();
            return Err(SendError::Revoked);
        }
        self.service.cancel();
        let profile = self
            .stopped
            .recv_timeout(STREAMS)
            .map_err(|_| SendError::Unavailable)??;
        self.identity.check()?;
        Ok(Linked {
            identity: self.identity,
            verified,
            profile,
        })
    }

    pub(crate) fn cancel(&self) {
        self.linker.cancel();
        self.service.cancel();
    }
}

impl Linked {
    pub(crate) fn number(&self) -> Option<String> {
        self.verified.account().number.clone()
    }

    pub(crate) fn scope(&self) -> String {
        self.verified.account().scope()
    }

    /// The account identity (ACI) the link verified.
    pub(crate) fn aci(&self) -> String {
        self.verified.account().aci.clone()
    }

    /// Accept the verified identity for publication, and seal the provider's
    /// profile under `account`'s key at `root`.
    pub(crate) fn seal(self, account: &Account, root: &Path) -> Result<(), SendError> {
        if self.verified.owner() != &self.identity.owner {
            return Err(SendError::Revoked);
        }
        drop(self.verified.accept()?);
        self.profile.seal(account, root)
    }
}

/// A running Signal account.
pub(crate) struct Account {
    pub(crate) gate: Arc<Gate>,
    supervisor: Arc<AccountSupervisor>,
    /// 0: not started; 1: starting; 2: started.
    activation: AtomicU8,
    /// Stops the provider this account started.
    provider: std::sync::Mutex<Option<ServiceCancellation>>,
}

impl Running for Account {
    fn gate(&self) -> &Arc<Gate> {
        &self.gate
    }
    fn supervisor(&self) -> &Arc<AccountSupervisor> {
        &self.supervisor
    }
    fn retire(&self) {
        self.gate.stop();
        let provider = self
            .provider
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(provider) = provider {
            provider.cancel();
        }
        let _ = self.supervisor.unavailable(AccountState::Locked);
    }
}

/// Creates or opens a sealed profile under a key and grant.
type UnsealProfile = fn(
    &Path,
    Owner,
    secret_vault::VaultKey,
    Arc<dyn AccountAuthority>,
) -> Result<EncryptedSignalProfile, ProfileError>;

impl Account {
    pub(crate) fn new(gate: Arc<Gate>, supervisor: Arc<AccountSupervisor>) -> Self {
        Self {
            gate,
            supervisor,
            activation: AtomicU8::new(0),
            provider: std::sync::Mutex::new(None),
        }
    }

    /// Create the account's first sealed profile; an existing one is refused.
    pub(crate) fn create_profile(&self, root: &Path) -> Result<EncryptedSignalProfile, SendError> {
        self.sealed_profile(root, EncryptedSignalProfile::create_with_key)
    }

    /// Open the account's sealed profile.
    pub(crate) fn open_profile(&self, root: &Path) -> Result<EncryptedSignalProfile, SendError> {
        self.sealed_profile(root, EncryptedSignalProfile::open_with_key)
    }

    fn sealed_profile(
        &self,
        root: &Path,
        unseal: UnsealProfile,
    ) -> Result<EncryptedSignalProfile, SendError> {
        self.gate.check()?;
        if !root.is_absolute() {
            return Err(SendError::Unavailable);
        }
        let key = self.gate.credential.profile_key()?;
        let profile = unseal(
            root,
            self.gate.identity.owner.clone(),
            key,
            self.gate.clone(),
        )
        .map_err(|_| SendError::Unavailable)?;
        self.gate.check()?;
        Ok(profile)
    }

    /// Start the provider on the account's profile and connect the account
    /// to it. At most one start runs; a failed one may be retried. Blocks.
    pub(crate) fn start(
        self: &Arc<Self>,
        storage: &Storage,
        policy: &Policy,
    ) -> Result<(), SendError> {
        if [&storage.profile, &storage.history, &storage.runtime]
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(SendError::InvalidRequest);
        }
        self.activation
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SendError::AlreadyReserved)?;
        *self
            .provider
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(policy.cancellation.clone());
        let result = self.run(storage, policy);
        self.activation
            .store(if result.is_ok() { 2 } else { 0 }, Ordering::Release);
        if result.is_err() {
            policy.cancellation.cancel();
            let state = if self.gate.check().is_err() {
                AccountState::Locked
            } else {
                AccountState::Failed
            };
            let _ = self.supervisor.unavailable(state);
        }
        result
    }

    fn run(self: &Arc<Self>, storage: &Storage, policy: &Policy) -> Result<(), SendError> {
        self.gate.check()?;
        let consent = consent::request(self.gate.identity.clone(), policy)?;
        self.gate
            .check_account_scope(&format!("signal:{}", storage.signal_account))?;
        let cancellation = policy.cancellation.clone();
        let launch = acquire_launch(|| self.gate.check(), &cancellation)?;
        let profile = self.materialize_profile(&storage.profile, &storage.runtime)?;
        let (send, receive) = mpsc::sync_channel(1);
        let service = cancellation;
        drop(tokio::runtime::Handle::current().spawn_blocking(move || {
            let result = profile.run_connected_and_checkpoint(
                launch,
                service,
                Some(consent),
                Some(Box::new(move |endpoint| {
                    let pair = streams(&endpoint)?;
                    send.send(pair)
                        .map_err(|_| io::ErrorKind::Interrupted.into())
                })),
            );
            if result.is_err() {
                tracing::debug!(target: "tilo::messaging", "the Signal provider stopped without a clean checkpoint");
            }
        }));
        let [sender, receiver] = receive
            .recv_timeout(STREAMS)
            .map_err(|_| SendError::Unavailable)?;
        self.gate.check()?;
        let (broker, receiver) =
            self.assemble(&storage.history, &storage.signal_account, sender, receiver)?;
        self.gate.check()?;
        self.gate.check_account_scope(broker.scope())?;
        self.supervisor.install_signal_receiving(&broker, receiver)
    }

    fn assemble(
        &self,
        root: &Path,
        signal_account: &str,
        sender: std::os::unix::net::UnixStream,
        receiver: std::os::unix::net::UnixStream,
    ) -> Result<(SignalBroker, SignalReceiver), SendError> {
        // Enrollment approved this account's key; nothing mints another.
        let key = self.gate.credential.profile_key()?;
        self.gate.check()?;
        let owner = &self.gate.identity.owner;
        let history = tilo_messaging_protocol::signal_broker::History::open(
            &root.join("history"),
            owner,
            key.clone(),
        )?;
        let journal = tilo_messaging_protocol::journal::EncryptedJournal::open(
            &root.join("journal"),
            owner,
            key,
        )
        .map_err(|_| SendError::Unavailable)?;
        let runtime = tilo_messaging_protocol::signal_runtime::SignalRuntime::from_journal(
            owner.clone(),
            signal_account.into(),
            sender,
            journal,
        )
        .map_err(|_| SendError::Unavailable)?;
        let authority: Arc<dyn SignalAuthority> = self.gate.clone();
        let broker = SignalBroker::new(runtime, history, authority, signal_account.into())?;
        let receiver = SignalReceiver::new(owner.clone(), signal_account.into(), receiver)
            .map_err(|_| SendError::Unavailable)?;
        Ok((broker, receiver))
    }
}

/// The account identity a published Signal account row names: its link
/// generation, scope and ACI.
pub(crate) fn published(row: &accounts::Account) -> Option<(u64, String, String)> {
    let generation = row
        .config
        .get("linking_generation")?
        .as_u64()
        .filter(|value| *value > 0)?;
    let scope = row.config.get("messaging_scope")?.as_str()?;
    let aci = scope.strip_prefix("signal:")?;
    uuid::Uuid::parse_str(aci)
        .is_ok_and(|parsed| parsed.to_string() == aci)
        .then(|| (generation, scope.into(), aci.into()))
}

/// What a Signal enrollment holds: the link in progress, then the link the
/// person completed.
pub(crate) enum Held {
    Linking(Linking),
    Linked(Linked),
}

/// Signal's steps in the one enrollment flow.
pub(crate) struct Signal;

impl Signal {
    fn storage(
        service: &super::Service,
        identity: &Identity,
        aci: String,
    ) -> Result<Storage, SendError> {
        let private = service.private_root(&identity.owner)?;
        Ok(Storage {
            profile: private.join("profile"),
            history: private.join("history"),
            runtime: super::runtime_dir()?,
            signal_account: aci,
        })
    }
}

impl super::flow::Provider for Signal {
    type Held = Held;
    type Account = Account;
    const SERVICE: tilo_messaging_protocol::Service = tilo_messaging_protocol::Service::Signal;
    const ORIGIN: &'static str = "https://signal.org";
    const ORDER: super::flow::Order = super::flow::Order::LinkThenGrant;

    fn purpose(_name: &str) -> String {
        "Private Signal messaging".into()
    }

    fn begin(
        &self,
        service: &Arc<super::Service>,
        identity: &Identity,
        held: &mut Option<Held>,
        _account: Option<&Arc<Account>>,
    ) -> Result<tilo_messaging::Pairing, SendError> {
        let runtime = super::runtime_dir()?;
        let (linking, pairing) = Linking::start(identity.clone(), &runtime, &service.policy())?;
        *held = Some(Held::Linking(linking));
        Ok(tilo_messaging::Pairing::new(
            tilo_messaging::ServiceAccount::new("signal", &identity.owner.account),
            pairing.attempt.clone(),
            identity.owner.linking_generation,
            zeroize::Zeroizing::new(pairing.qr_bytes().to_vec()),
        ))
    }

    fn finish(
        &self,
        held: &mut Option<Held>,
        attempt: String,
    ) -> Result<Option<String>, SendError> {
        let Some(Held::Linking(linking)) = held.take() else {
            return Err(SendError::Revoked);
        };
        let linked = linking.finish(attempt)?;
        let number = linked.number();
        *held = Some(Held::Linked(linked));
        Ok(number)
    }

    fn approved(
        &self,
        service: &Arc<super::Service>,
        identity: &Identity,
        credential: super::identity::Credential,
        name: &str,
        held: Option<Held>,
    ) -> Result<(Arc<Account>, String, Option<Held>), SendError> {
        let Some(Held::Linked(linked)) = held else {
            return Err(SendError::Revoked);
        };
        let aci = linked.aci();
        let scope = linked.scope();
        service.publish(
            identity,
            crate::door::Publication {
                account: identity.owner.account.clone(),
                name: name.into(),
                generation: identity.owner.linking_generation,
                kind: crate::door::PublicationKind::Signal { aci: aci.clone() },
            },
        )?;
        let account = service.account(identity, credential, &scope, Account::new)?;
        let storage = Self::storage(service, identity, aci)?;
        let private = service.private_root(&identity.owner)?;
        tilo_files::native::private::directory(&private).map_err(|_| SendError::Unavailable)?;
        linked.seal(&account, &storage.profile)?;
        account.start(&storage, &service.policy())?;
        Ok((account, scope, None))
    }

    fn restored_scope(_id: &str, row: &accounts::Account) -> Option<String> {
        published(row).map(|(_, scope, _)| scope)
    }

    fn resume(
        &self,
        service: &Arc<super::Service>,
        identity: &Identity,
        credential: super::identity::Credential,
        scope: String,
        row: &accounts::Account,
    ) -> Result<Arc<Account>, SendError> {
        let (_, _, aci) = published(row).ok_or(SendError::Revoked)?;
        let account = service.account(identity, credential, &scope, Account::new)?;
        let storage = Self::storage(service, identity, aci)?;
        if let Err(error) = account.start(&storage, &service.policy()) {
            tracing::info!(target: "tilo::messaging", ?error, "a Signal account did not start");
        }
        Ok(account)
    }

    fn cancel(held: &Held) {
        if let Held::Linking(linking) = held {
            linking.cancel();
        }
    }
}
