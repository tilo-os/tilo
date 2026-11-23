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
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_device_identity::login::{Device, LOG_IN, PROVIDER, Place, RESOURCE};
use tilo_lock::MutexRecover as _;

use super::ticket::{
    INFO, device_info, device_only_catalogue, failed, logged_in, login_asked, provider, succeeded,
};
use crate::host;

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
}

impl ThisDevice {
    /// Take this computer's device, its key made here on the first start of a computer whose
    /// system made none.
    ///
    /// # Errors
    /// The key cannot be read or made, or another daemon keeps the device.
    pub(crate) fn lock() -> io::Result<Self> {
        let keys = tilo_api::client::device_keys();
        let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys)
            .map_err(|error| io::Error::other(format!("this computer's device key: {error}")))?;
        let place = Place::here(keys).map_err(io::Error::other)?;
        host::private_directory(place.dir())?;
        let daemon =
            tilo_api::daemon::DaemonPlace::of(place.dir(), &key.verifying_key().to_bytes())
                .map_err(io::Error::other)?;
        let lock = super::serve::lock_device(&daemon)?;
        Ok(Self { key, place, lock })
    }

    /// Whether a person's certificate names this device's key.
    pub(crate) fn claimed(&self) -> bool {
        claimed(&self.place, &self.key)
    }

    /// The device's own actions, as its endpoints answer them before its person's services run.
    pub(crate) fn answers(&self) -> DeviceOnly {
        DeviceOnly::new(self.key.clone(), self.place.clone())
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
            Some(Ok(device)) => tracing::info!(
                person = %device.user(),
                label = device.label(),
                "this computer is one of the person's devices now, with the ticket it was handed"
            ),
            Some(Err(error)) => tracing::error!(
                "the ticket this computer was handed did not add it: {error:#}. It may be used \
                 already or have run out: make a new one with `tilo device add` on one of the \
                 person's devices."
            ),
        }
    }

    /// Return once a person's certificate names this device's key, so the daemon opens the
    /// person's workspaces on it.
    pub(crate) fn until_claimed(&self) {
        while !self.claimed() {
            std::thread::sleep(LOOKING);
        }
    }
}

/// Whether a person's certificate names this computer's device key `key`
/// ([`tilo_device_identity::login::claimed`]).
#[must_use]
pub(crate) fn claimed(place: &Place, key: &SigningKey) -> bool {
    tilo_device_identity::login::claimed(place, &key.verifying_key().to_bytes())
}

/// Spend the ticket in `credentials`' [`TICKET_CREDENTIAL`] to make this computer, whose device
/// key is `key`, one of the person's devices, keeping the login in `place`: `None` when no ticket
/// was handed, or when a person's certificate already names this key, where the ticket is left
/// unspent.
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

/// What a device answers on its native endpoint before its person's services run: what it is ([`INFO`]), and logging it in with a ticket ([`LOG_IN`]). Every
/// other action needs a person, and is refused saying so.
#[derive(Clone)]
pub(crate) struct DeviceOnly(Arc<Answering>);

struct Answering {
    key: SigningKey,
    place: Place,
    /// Each invocation taken, and where it is.
    calls: std::sync::Mutex<HashMap<InvocationId, InvocationState>>,
    runtime: tokio::runtime::Handle,
}

impl DeviceOnly {
    /// The device actions of the device whose key is `key`, its login kept in `place`.
    pub(crate) fn new(key: SigningKey, place: Place) -> Self {
        Self(Arc::new(Answering {
            key,
            place,
            calls: std::sync::Mutex::default(),
            runtime: tilo_rt::app().handle().clone(),
        }))
    }

    /// Whether a person's certificate names this device's key.
    pub(crate) fn claimed(&self) -> bool {
        claimed(&self.0.place, &self.0.key)
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

    /// Log this device in with the ticket `args` name, as `tilo login` does.
    async fn log_in(&self, args: &Value) -> anyhow::Result<Value> {
        let (ticket, agent, label) = login_asked(args)?;
        let device =
            tilo_api::daemon::enter::log_in_here(&self.0.key, &self.0.place, ticket, agent, label)
                .await?;
        Ok(logged_in(&device))
    }
}

impl ActionProvider for DeviceOnly {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(match query.provider.as_deref() {
            // Listed as the one running provider, in no workspace, as a person's daemon lists it
            // beside their workspace's apps.
            None => {
                let mut page = device_only_catalogue(&query);
                page.providers.push(provider(""));
                Ok(page)
            }
            Some(PROVIDER) => Ok(device_only_catalogue(&query)),
            Some(_) => Err(self.needs_a_person()),
        })
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if let Some(state) = self.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        let address = &request.target.address;
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
            _ => Pending::ready(Err(self.needs_a_person())),
        }
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.calls.lock_recover().get(id).cloned()
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(match self.0.calls.lock_recover().get(id) {
            Some(_) => ActionCallError::NotCancellable {
                reason: "a login, once asked for, is the issuing device's to answer".into(),
            },
            None => ActionCallError::UnknownInvocation,
        }))
    }
}
