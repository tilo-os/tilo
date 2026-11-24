//! This computer's device before its person's services run: what the daemon serves from boot,
//! on a machine nobody has set up yet, at its greeter, and while a set-up device's person's
//! services start.
//!
//! The daemon takes the device's lock ([`ThisDevice`]) and answers its apps' native endpoint
//! with the device's own actions ([`DeviceOnly`]): what the device is, and logging it in with a
//! ticket.
//! Whatever needs a person is refused, saying the device has no person yet, or that their
//! services are starting. A ticket handed to it as the `tilo.ticket` credential at its start is
//! spent once ([`take_handed_ticket`]): the person's root certifies this key, and the person's
//! workspaces open as on any device of theirs. Otherwise the device waits for a certificate that
//! names its key: `tilo login`, or a person founded here explicitly (`tilo login --new-identity`).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_device_identity::login::{
    ANOTHER_PERSONS_WORDS, Device, FOUND, LOG_IN, PROVIDER, Place, RESOURCE, RESTORE,
};
use tilo_lock::MutexRecover as _;

use super::ticket::{
    INFO, device_info, device_only_catalogue, failed, failed_as, logged_in, login_asked, provider,
    succeeded,
};

/// The credential a ticket is handed to the daemon as, in `$CREDENTIALS_DIRECTORY`: what a
/// machine that is set up for a person without anyone at it (a VM, a server) logs in with.
pub const TICKET_CREDENTIAL: &str = "tilo.ticket";
/// What the agent this computer's login names is, for a login made from a handed ticket.
const HANDED_AGENT: &str = "tilo";
/// How often the daemon looks whether a person's certificate now names its key.
const LOOKING: Duration = Duration::from_secs(2);

/// This computer's device key, its login's place, and the lock that makes this process its one
/// daemon: taken at boot, before anything else, and handed to the person's services once they
/// start.
pub(crate) struct ThisDevice {
    key: SigningKey,
    place: Place,
    lock: advisory_lock::FileLock,
    /// The device a ticket made of this computer, once one did: kept in memory for the person's
    /// services to enter through, as nothing of it is written down.
    entered: Entered,
}

/// The device a ticket made of this computer while its daemon runs, once one did.
type Entered = Arc<std::sync::Mutex<Option<Device>>>;

impl ThisDevice {
    /// Take this computer's device, its key made here on the first start of a computer whose
    /// system made none.
    ///
    /// # Errors
    /// The key cannot be read or made, or another daemon keeps the device.
    pub(crate) fn lock() -> io::Result<Self> {
        let keys = tilo_api::calls::device_keys();
        let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys)
            .map_err(|error| io::Error::other(format!("this computer's device key: {error}")))?;
        let place = Place::here(keys).map_err(io::Error::other)?;
        tilo_files::native::private::directory(place.dir())?;
        let daemon = tilo_api::daemon::this_computer().map_err(io::Error::other)?;
        let lock = super::serve::lock_device(&daemon)?;
        Ok(Self {
            key,
            place,
            lock,
            entered: Entered::default(),
        })
    }

    /// Whether a person's certificate names this device's key.
    pub(crate) fn claimed(&self) -> bool {
        claimed(&self.place, &self.key)
    }

    /// The device's own actions, as its endpoints answer them before its person's services run:
    /// its package cache among them, the one it answers after a login too.
    pub(crate) fn answers(&self) -> DeviceOnly {
        let answers = DeviceOnly(
            Arc::new(Answering {
                key: self.key.clone(),
                place: self.place.clone(),
                entered: Arc::clone(&self.entered),
                calls: std::sync::Mutex::default(),
                runtime: tilo_rt::app().handle().clone(),
                packages: std::sync::OnceLock::new(),
                records: Arc::new(super::records::DeviceRecords::at(
                    self.place.dir(),
                    self.key.verifying_key().to_bytes(),
                )),
            }),
            None,
        );
        #[cfg(unix)]
        match super::packages::PackageCache::this_device(&self.key.verifying_key().to_bytes()) {
            Ok(cache) => return answers.with_packages(cache),
            Err(why) => tracing::warn!(%why, "this device keeps no packages"),
        }
        answers
    }

    /// The device a ticket made of this computer while this daemon ran, when one did: what its
    /// person's services enter through first.
    pub(crate) fn entered(&self) -> Option<Device> {
        self.entered.lock_recover().take()
    }

    /// The device's key and lock, for the person's services to start under.
    pub(crate) fn into_lock(self) -> ([u8; 32], advisory_lock::FileLock) {
        (self.key.verifying_key().to_bytes(), self.lock)
    }
}

impl ThisDevice {
    /// Spend the ticket this daemon was handed at its start, when it was handed one: a ticket that
    /// does not add this computer is said in the log, and the device goes on as it was.
    pub(crate) fn spend_handed_ticket(&self) {
        let credentials = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
        let taken = tilo_rt::app().handle().block_on(take_handed_ticket(
            &self.place,
            &self.key,
            credentials.as_deref(),
        ));
        match taken {
            None => {}
            Some(Ok(device)) => {
                tracing::info!(
                    person = %device.user(),
                    label = device.label(),
                    "this computer is one of the person's devices now, with the ticket it was handed"
                );
                *self.entered.lock_recover() = Some(device);
            }
            Some(Err(error)) => tracing::error!(
                "the ticket this computer was handed did not add it: {error:#}. It may be used \
                 already or have run out: make a new one with `tilo device add` on one of the \
                 person's devices."
            ),
        }
    }

    /// Return once a person's certificate names this device's key, so the daemon opens the
    /// person's workspaces on it: `true` then, and `false` when the daemon was told to stop first.
    pub(crate) fn until_claimed(&self, stop: &super::serve::Stop) -> bool {
        while !self.claimed() {
            if stop.wait(LOOKING) {
                return false;
            }
        }
        true
    }
}

/// Whether a person's certificate names this computer's device key `key`
/// ([`tilo_device_identity::login::claimed`]).
#[must_use]
pub(crate) fn claimed(place: &Place, key: &SigningKey) -> bool {
    tilo_device_identity::login::claimed(place, &key.verifying_key().to_bytes())
}

/// Spend the ticket in `credentials`' [`TICKET_CREDENTIAL`] to make this computer, whose device
/// key is `key`, one of the person's devices, its certificate kept in `place`: `None` when no
/// ticket was handed, or when a person's certificate already names this key, where the ticket is
/// left unspent.
pub(crate) async fn take_handed_ticket(
    place: &Place,
    key: &SigningKey,
    credentials: Option<&Path>,
) -> Option<anyhow::Result<Device>> {
    if claimed(place, key) {
        return None;
    }
    let ticket = std::fs::read_to_string(credentials?.join(TICKET_CREDENTIAL)).ok()?;
    let label = tilo_paths::session::host_name();
    Some(
        tilo_api::daemon::enter::log_in_here(
            key,
            place,
            ticket.trim(),
            HANDED_AGENT,
            label.as_deref(),
        )
        .await,
    )
}

/// What a device answers on its native endpoint before its person's services run, and to every
/// account it serves no person for: what it is ([`INFO`]), logging it in with a ticket
/// ([`LOG_IN`]), its package cache ([`super::packages`]), and, to root, who signed in and who left
/// ([`crate::seats`]). Every other action needs a person, and is refused saying so.
#[derive(Clone)]
pub(crate) struct DeviceOnly(Arc<Answering>, Option<crate::seats::Seated>);

struct Answering {
    key: SigningKey,
    place: Place,
    /// Where the device a ticket made of this computer is kept for its person's services.
    entered: Entered,
    /// Each invocation taken, and where it is.
    calls: std::sync::Mutex<HashMap<InvocationId, InvocationState>>,
    runtime: tokio::runtime::Handle,
    /// The device's package cache, which it answers before a login as after it.
    packages: std::sync::OnceLock<Arc<dyn ActionProvider + Send + Sync>>,
    /// The records the device keeps as its own: its settings, read, written and followed through
    /// the one record API before anyone claims it.
    records: Arc<super::records::DeviceRecords>,
}

impl DeviceOnly {
    /// The device actions of the device whose key is `key`, its certificates kept in `place`.
    #[cfg(test)]
    pub(crate) fn new(key: SigningKey, place: Place) -> Self {
        let records = Arc::new(super::records::DeviceRecords::at(
            place.dir(),
            key.verifying_key().to_bytes(),
        ));
        Self(
            Arc::new(Answering {
                key,
                place,
                entered: Entered::default(),
                calls: std::sync::Mutex::default(),
                runtime: tilo_rt::app().handle().clone(),
                packages: std::sync::OnceLock::new(),
                records,
            }),
            None,
        )
    }

    /// The records this device keeps as its own.
    pub(crate) fn records(&self) -> Arc<super::records::DeviceRecords> {
        Arc::clone(&self.0.records)
    }

    /// These actions with the device's package cache `packages` among them: the first cache
    /// given is the one they answer.
    pub(crate) fn with_packages(self, packages: Arc<dyn ActionProvider + Send + Sync>) -> Self {
        drop(self.0.packages.set(packages));
        self
    }

    /// The device's package cache, when it keeps one.
    fn packages(&self) -> Option<&Arc<dyn ActionProvider + Send + Sync>> {
        self.0.packages.get()
    }

    /// These actions as one connection asks them: its account, which the seats' actions are
    /// answered by.
    pub(crate) fn seated(&self, seated: crate::seats::Seated) -> Self {
        Self(Arc::clone(&self.0), Some(seated))
    }

    /// Whether a person's certificate names this device's key.
    pub(crate) fn claimed(&self) -> bool {
        claimed(&self.0.place, &self.0.key)
    }

    /// This device's key and certificates.
    pub(crate) fn keys(&self) -> &tilo_device_identity::IdentityPaths {
        self.0.place.keys()
    }

    /// This device's public key.
    pub(crate) fn public_key(&self) -> [u8; 32] {
        self.0.key.verifying_key().to_bytes()
    }

    /// The person whose certificate names this device first, when one does.
    pub(crate) fn person(&self) -> Option<[u8; 32]> {
        tilo_device_identity::login::certified_by(&self.0.place, &self.public_key())
            .first()
            .copied()
    }

    /// The name its person's certificate gives this device, once one names it.
    pub(crate) fn label(&self) -> Option<String> {
        let keys = self.0.place.keys();
        [
            keys.device_cert.clone(),
            keys.device_cert_for_user(&self.person()?),
        ]
        .iter()
        .filter_map(|path| tilo_device_identity::persist::read_device_cert(path).ok())
        .find(|cert| cert.device_public_key == self.public_key())
        .map(|cert| cert.device_label)
    }

    /// Why an action that needs a person is not answered now.
    fn needs_a_person(&self) -> ActionCallError {
        let reason = if self.claimed() {
            "this device's person's services are still starting: ask again in a moment"
        } else {
            "this device has no person yet: log it in with a ticket from one of your devices \
             (`tilo login <ticket>`) first"
        };
        ActionCallError::Refused {
            reason: reason.into(),
        }
    }

    fn keep(&self, invocation: InvocationId, update: InvocationUpdate) -> InvocationReceipt {
        let state = InvocationState::new(invocation, update);
        let receipt = state.receipt();
        self.0.calls.lock_recover().insert(invocation, state);
        receipt
    }

    /// Start a new person on this device ([`FOUND`]), as its own account asks, or under the
    /// system unit the account that sets the computer up: named as `args`' `name` says, else as
    /// that account is. Their root is kept here and certifies this device's key, the daemon
    /// serves them from then on, and the account acts as them. Answers the person's id.
    fn found(&self, args: &Value) -> Result<String, Refusal> {
        self.take_a_person(|seated| {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map_or_else(|| founder_name(seated.seat.uid), str::to_owned);
            let made = if seated.seats.system() {
                super::services::found_account_person(self.keys(), &name, seated.seat.uid)
            } else {
                super::services::found_a_person(self.keys(), &name)
            };
            made.map_err(|error| format!("this device could not make a new person: {error}").into())
        })
    }

    /// Bring a person back on this device ([`RESTORE`]) from the recovery words or the backup
    /// `args` hold, as [`Self::found`] makes one: their root is kept here from then on.
    fn restore(&self, args: &Value) -> Result<String, Refusal> {
        let another = || Refusal {
            code: ANOTHER_PERSONS_WORDS,
            message: "these recovery words are another person's than this computer's".to_owned(),
        };
        // Words of the person this device serves already bring nobody new: the account takes them
        // up again. Anyone else's are refused as theirs.
        if let (Some(person), Some(words)) =
            (self.person(), args.get("words").and_then(Value::as_str))
        {
            return match tilo_identity::store::recovery_words_public_key(words) {
                Ok(key) if key == person => Ok(hex::encode(person)),
                Ok(_) => Err(another()),
                Err(problem) => Err(problem.to_string().into()),
            };
        }
        self.take_a_person(|seated| {
            let restored = if seated.seats.system() {
                super::services::restore_account_person(self.keys(), args, seated.seat.uid)
            } else {
                super::services::restore_a_person(self.keys(), args)
            };
            restored.map_err(
                |error| match tilo_identity::store::recovery_words_error(&error) {
                    Some(tilo_identity::store::RecoveryWordsError::DifferentIdentity {
                        ..
                    }) => another(),
                    _ => format!("this device could not bring the person back: {error}").into(),
                },
            )
        })
    }

    /// Make this device a person's with `make`, on a device nobody has yet, asked by the account
    /// it is set up from, which acts as them from then on: the person's id.
    fn take_a_person(
        &self,
        make: impl FnOnce(&crate::seats::Seated) -> Result<[u8; 32], Refusal>,
    ) -> Result<String, Refusal> {
        // A person is made by an account on this computer, one per account: a computer whose
        // device's person is set up starts another person for the next account that signs in.
        let Some(seated) = self
            .1
            .as_ref()
            .filter(|seated| seated.seats.founds(seated.seat.clone()))
        else {
            return Err(
                "this account already made a person on this computer, or is not signed in to it"
                    .to_owned()
                    .into(),
            );
        };
        let person = make(seated)?;
        seated.seats.founded_by(seated.seat.clone(), person)?;
        Ok(hex::encode(person))
    }

    /// Log this device in with the ticket `args` name, as `tilo login` does.
    async fn log_in(&self, args: &Value) -> anyhow::Result<Value> {
        let (ticket, agent, label) = login_asked(args)?;
        let device =
            tilo_api::daemon::enter::log_in_here(&self.0.key, &self.0.place, ticket, agent, label)
                .await?;
        let said = logged_in(&device);
        *self.0.entered.lock_recover() = Some(device);
        Ok(said)
    }
}

/// Why a person was not made or brought back on this device: the refusal's stable code, and its
/// words.
struct Refusal {
    code: &'static str,
    message: String,
}

impl From<String> for Refusal {
    fn from(message: String) -> Self {
        Self {
            code: "device-action",
            message,
        }
    }
}

/// The name a new person takes from the account `uid` that made them: its full name, else its
/// login name. Root asking for its daemon names the daemon's own account.
fn founder_name(uid: u32) -> String {
    let uid = if uid == tilo_api::daemon::admission::ROOT {
        rustix::process::geteuid().as_raw()
    } else {
        uid
    };
    tilo_identity::provisioning::account_display_name(uid)
}

impl ActionProvider for DeviceOnly {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
            || provider == crate::seats::PROVIDER
            || self.packages().is_some_and(|cache| cache.serves(provider))
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if let Some(cache) = self.packages()
            && query
                .provider
                .as_deref()
                .is_some_and(|provider| cache.serves(provider))
        {
            return cache.query(caller, query);
        }
        Pending::ready(match query.provider.as_deref() {
            // Listed as the running providers, in no workspace, as a person's daemon lists the
            // device's beside their workspace's apps.
            None => {
                let mut page = device_only_catalogue(&query);
                page.providers.push(provider(""));
                page.providers.push(crate::seats::provider());
                #[cfg(unix)]
                if self.packages().is_some() {
                    page.providers.push(CatalogueProvider {
                        provider: super::packages::PROVIDER.to_owned(),
                        app_id: super::packages::PROVIDER.to_owned(),
                        workspace: String::new(),
                        browsable: true,
                        ..CatalogueProvider::default()
                    });
                }
                Ok(page)
            }
            Some(PROVIDER) => Ok(device_only_catalogue(&query)),
            Some(crate::seats::PROVIDER) => Ok(crate::seats::catalogue(&query)),
            Some(_) => Err(self.needs_a_person()),
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if let Some(state) = self.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        if let Some(cache) = self.packages()
            && cache.serves(&request.target.provider)
        {
            return cache.invoke(caller, request);
        }
        let address = &request.target.address;
        if request.target.provider == crate::seats::PROVIDER {
            let InvokeArgument::Json(argument) = request.argument else {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: "a seat action asks with its arguments".into(),
                }));
            };
            let Some(seated) = &self.1 else {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: crate::seats::ROOT_ALONE.into(),
                }));
            };
            return Pending::ready(
                seated
                    .act(&address.action, &Value::Object(argument))
                    .map(|value| self.keep(invocation, succeeded(invocation, value)))
                    .map_err(|reason| ActionCallError::Refused { reason }),
            );
        }
        // The device's own records: its settings, written here before anyone claims it.
        if request.target.provider == PROVIDER && address.resource == super::records::RESOURCE {
            let answer = match &request.argument {
                InvokeArgument::Json(args) => self.0.records.act(caller, &address.action, args),
                _ => Err(ActionCallError::InvalidRequest {
                    reason: "a setting action asks with its arguments".to_owned(),
                }),
            };
            let update = match answer {
                Ok(value) => super::ticket::succeeded(invocation, value),
                Err(error) => {
                    super::ticket::failed_as(invocation, "device-setting", &error.to_string())
                }
            };
            return Pending::ready(Ok(self.keep(invocation, update)));
        }
        if request.target.provider != PROVIDER || address.resource != RESOURCE {
            return Pending::ready(Err(self.needs_a_person()));
        }
        match address.action.as_str() {
            INFO => Pending::ready(Ok(self.keep(
                invocation,
                succeeded(invocation, device_info(&self.0.place, &self.0.key)),
            ))),
            LOG_IN => {
                let InvokeArgument::Json(argument) = request.argument else {
                    return Pending::ready(Err(ActionCallError::Refused {
                        reason: "a device action asks with its arguments".into(),
                    }));
                };
                let receipt = self.keep(invocation, InvocationUpdate::Accepted { invocation });
                let answering = self.clone();
                self.0.runtime.spawn(async move {
                    let update = match answering.log_in(&Value::Object(argument)).await {
                        Ok(value) => succeeded(invocation, value),
                        Err(error) => failed(invocation, &format!("{error:#}")),
                    };
                    answering.keep(invocation, update);
                });
                Pending::ready(Ok(receipt))
            }
            FOUND | RESTORE => {
                let args = match &request.argument {
                    InvokeArgument::Json(argument) => Value::Object(argument.clone()),
                    _ => Value::Null,
                };
                let made = if address.action == FOUND {
                    self.found(&args)
                } else {
                    self.restore(&args)
                };
                let update = match made {
                    Ok(person) => succeeded(invocation, serde_json::json!({ "person": person })),
                    Err(refusal) => failed_as(invocation, refusal.code, &refusal.message),
                };
                Pending::ready(Ok(self.keep(invocation, update)))
            }
            _ => Pending::ready(Err(self.needs_a_person())),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0
            .calls
            .lock_recover()
            .get(id)
            .cloned()
            .or_else(|| self.packages()?.observe(caller, id))
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(match self.0.calls.lock_recover().get(id) {
            Some(_) => ActionCallError::NotCancellable {
                reason: "a login, once asked for, is the issuing device's to answer".into(),
            },
            None => ActionCallError::UnknownInvocation,
        }))
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.packages()?.content_puts()
    }

    /// The records this device keeps as its own: its settings, read and followed before anyone
    /// claims it as they are after (`devices/<device>/settings/...`).
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(self.0.records.clone())
    }
}
