//! This computer's device before its person's services run: what the daemon serves from boot,
//! on a machine nobody has set up yet, at its greeter, and while a set-up device's person's
//! services start.
//!
//! The daemon takes the device's lock ([`ThisDevice`]) and answers its apps' native endpoint
//! with the device's own actions ([`DeviceOnly`]): what the device is, and logging it in with a
//! ticket.
//! Whatever needs a person is refused, saying the device has no person yet, or that the action
//! is the person's, whose own part answers it. A session asking for its shell's link is not
//! refused while that part starts: the request is held until it runs, or answered with the
//! failure that kept it from starting.
//!
//! A ticket handed to it as the `tilo.ticket` credential at its start is spent once
//! ([`take_handed_ticket`]): the person's root certifies this key, and the person's
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
use tilo_api::daemon::admission::Admitted;
use tilo_device_identity::login::{
    ALREADY_A_PERSONS, ANOTHER_PERSONS_WORDS, DEV_ID, Device, FOUND, LOG_IN, PROVIDER, Place,
    RESOURCE, RESTORE, SET_UP_UNLOCK,
};
use tilo_lock::MutexRecover as _;

use super::ticket::{
    INFO, device_info, device_only_catalogue, failed, failed_as, logged_in, login_asked, providers,
    succeeded,
};

/// The credential a ticket is handed to the daemon as, in `$CREDENTIALS_DIRECTORY`: what a
/// machine that is set up for a person without anyone at it (a VM, a server) logs in with.
pub const TICKET_CREDENTIAL: &str = "tilo.ticket";
/// What the agent this computer's login names is, for a login made from a handed ticket.
const HANDED_AGENT: &str = "tilo";
/// How often the daemon looks whether a person's certificate now names its key, when nothing of
/// its own made one: a certificate another program filed here between two looks.
const LOOKING: Duration = Duration::from_secs(2);
/// How long the wait for that certificate sleeps before it hears a stop or a person this
/// daemon's own founding, restoring or login took up.
const HEARING: Duration = Duration::from_millis(100);
/// How often a founding call reports the stage it is in while that stage runs: a caller that
/// follows the call sees it move, and a stage that takes long is never mistaken for a daemon
/// that stopped answering.
const REPORTING: Duration = Duration::from_secs(2);
/// What a device no person's certificate names says to a call that needs one.
const NO_PERSON: &str = "this device has no person yet: set it up with `tilo login \
                         --new-identity`, or log it in with a ticket from one of your devices \
                         (`tilo login <ticket>`) first";
/// What the session a sign-in answered a founding link is told when it asks this device anything
/// making or joining a person does not: it reaches no more of the computer than that.
pub(crate) const FOUNDING_SETUP_ONLY: &str = "this computer is setting up its person: the \
    session this sign-in started may make one or join one, and asks nothing else of the device \
    until it has";
/// Whether `action` of the provider `provider` is one a session making or joining a person asks:
/// making one, bringing one back, logging in with a ticket, what the device is, and the seats'
/// own action certifying the key this computer made the account.
fn answers_a_founding_setup(provider: &str, action: &str) -> bool {
    use tilo_device_identity::login::{FOUND, LOG_IN, RESTORE};
    if provider == crate::seats::PROVIDER {
        return action == crate::seats::CERTIFIED;
    }
    provider == PROVIDER && matches!(action, FOUND | RESTORE | LOG_IN | INFO)
}
/// What a founding call reports as it makes the person and starts their part.
pub(crate) const FOUNDING_STARTING: &str = "starting the person part";
/// What it reports while it waits for them to found their own workspace here.
pub(crate) const FOUNDING_FOUNDING: &str = "founding";
/// What it reports as the tickets the founder asked for are issued.
pub(crate) const FOUNDING_ISSUING: &str = "issuing the tickets";

/// This computer's device key, its login's place, and the lock that makes this process its one
/// daemon: taken at boot, before anything else, and handed to the person's services once they
/// start.
pub(crate) struct ThisDevice {
    key: SigningKey,
    place: Place,
    /// The device's key, taken by this daemon alone: let go of before the lock beside it, so the
    /// daemon that takes the device over finds the key free once it has the lock.
    _key: tilo_device_identity::persist::DeviceKeyLock,
    /// The daemon's profile, where founding records the account that made its person.
    profile: PathBuf,
    /// The account the daemon runs as: its own identity names the person first.
    own: u32,
    lock: advisory_lock::FileLock,
    /// The device a ticket made of this computer, once one did: kept in memory for the person's
    /// services to enter through, as nothing of it is written down.
    entered: Entered,
    /// What the person's services issue the tickets a founding asks for through, once they run.
    founding: Arc<Founding>,
    /// Told when this daemon's own founding, restoring or ticket login took this computer's
    /// person up: the wait for their services looks again then rather than at its next look.
    ready: Arc<std::sync::atomic::AtomicBool>,
    /// The person's services while they run: a login answered here is taken in by them as it is
    /// answered, not at their next start.
    serving: ServingHandle,
}

/// The calls this device took, and where each is: what a caller following one reads, whichever
/// provider serves the device by then.
pub(crate) type Calls = Arc<std::sync::Mutex<HashMap<InvocationId, InvocationState>>>;

/// The tickets a person's founding asks for, issued as part of founding them: the founding call
/// names what it needs, and is answered them once the person's own services run here, which is
/// where a ticket can be served. A person is founded once, so their first tickets are the ones
/// their own act issued; nobody else in their account gets any.
#[derive(Default)]
pub(crate) struct Founding {
    /// Whether the person's services run: the start that makes them says so, and the endpoint
    /// that serves them.
    running: std::sync::atomic::AtomicBool,
    /// The person's services, and the person they serve, once they run.
    serving: std::sync::Mutex<Option<([u8; 32], Arc<super::ticket::Tickets>)>>,
    /// Why the founding that waits for them ended without their workspace bound here, once it did:
    /// the call hears this instead of waiting for services that will not answer it.
    failed: std::sync::Mutex<Option<String>>,
    /// What sets up this device's boot key, once the person's services run.
    enrolling: std::sync::Mutex<Option<Arc<super::boot_unlock::BootUnlock>>>,
    /// Told when they do.
    announced: tokio::sync::Notify,
    /// The calls the device took while it had no person: its services answer them too, so a
    /// founding call followed after they start reads its state where it was kept.
    calls: Calls,
    /// Whether a founding this device answered at once is making its person now: a call that
    /// needs the person waits for their part meanwhile, as it does once they are made.
    under_way: std::sync::atomic::AtomicBool,
}

impl Founding {
    /// Whether a founding is making this device's person now ([`Self::under_way`]).
    pub(crate) fn under_way(&self) -> bool {
        self.under_way.load(std::sync::atomic::Ordering::Acquire)
    }

    fn set_under_way(&self, under_way: bool) {
        self.under_way
            .store(under_way, std::sync::atomic::Ordering::Release);
    }

    /// The calls the device took, and where each is: the person's services answer them too, so a
    /// call followed after they start reads where it is.
    pub(crate) fn calls(&self) -> Calls {
        Arc::clone(&self.calls)
    }

    /// Have the person's services, which serve `person`, issue the tickets a founder asks for
    /// from now on.
    pub(crate) fn serving(&self, person: [u8; 32], tickets: Arc<super::ticket::Tickets>) {
        *self.serving.lock_recover() = Some((person, tickets));
        self.person_serves();
    }

    /// The person's services run from now on: what the start that made them says, and the
    /// endpoint that serves them ([`crate::daemon`]'s `serve_the_person`). A founding call
    /// answered once they run waits for this.
    pub(crate) fn person_serves(&self) {
        self.running
            .store(true, std::sync::atomic::Ordering::Release);
        self.announced.notify_waiters();
    }

    /// End the founding that waits for this device's person with `why`, as what it was founded
    /// for did not happen: their own workspace is not bound here, so no call of theirs in it would
    /// be admitted.
    pub(crate) fn failed(&self, why: String) {
        *self.failed.lock_recover() = Some(why);
        self.announced.notify_waiters();
    }

    /// Have `enrolling` enrol this device's boot key from now on: the person's services run.
    pub(crate) fn enrolling(&self, enrolling: Arc<super::boot_unlock::BootUnlock>) {
        *self.enrolling.lock_recover() = Some(enrolling);
        self.announced.notify_waiters();
    }

    /// What sets up this device's boot unlock, once the person's services run: a boot asked
    /// while they start (a server's, asked by a unit its own boot starts beside the daemon) waits
    /// for them, and whoever follows the call sees it pending meanwhile.
    async fn boot_unlock(&self) -> Arc<super::boot_unlock::BootUnlock> {
        loop {
            let heard = self.announced.notified();
            tokio::pin!(heard);
            heard.as_mut().enable();
            let set_up = self.enrolling.lock_recover().clone();
            if let Some(unlocking) = set_up {
                return unlocking;
            }
            heard.await;
        }
    }

    /// The person's services run, as `reporting` is told the stage under way again every
    /// [`REPORTING`] meanwhile: no fixed wait ends this, the caller following the call decides
    /// when a stage has stopped reporting.
    async fn until_serving(&self, mut reporting: impl FnMut(&str)) -> Result<(), String> {
        loop {
            let heard = self.announced.notified();
            tokio::pin!(heard);
            heard.as_mut().enable();
            let failed = self.failed.lock_recover().clone();
            if let Some(why) = failed {
                return Err(why);
            }
            if self.running.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(());
            }
            if tokio::time::timeout(REPORTING, heard).await.is_err() {
                reporting(FOUNDING_FOUNDING);
            }
        }
    }

    /// The tickets of this device's services once they run, serving `person`: the person's own
    /// workspace is founded by then when the device just made their root. `reporting` is told
    /// the stage under way again every [`REPORTING`] meanwhile.
    async fn until_serving_tickets(
        &self,
        person: [u8; 32],
        mut reporting: impl FnMut(&str),
    ) -> Result<Arc<super::ticket::Tickets>, String> {
        loop {
            let heard = self.announced.notified();
            tokio::pin!(heard);
            heard.as_mut().enable();
            let failed = self.failed.lock_recover().clone();
            if let Some(why) = failed {
                return Err(why);
            }
            let serving = self.serving.lock_recover().clone();
            if let Some((serving, tickets)) = serving
                && serving == person
            {
                return Ok(tickets);
            }
            // The person's part starts: no fixed wait ends this, the caller following the call
            // decides when a stage has stopped reporting.
            if tokio::time::timeout(REPORTING, heard).await.is_err() {
                reporting(FOUNDING_FOUNDING);
            }
        }
    }

    /// The tickets `kinds` names, each issued as `person`'s own act on this device: as
    /// `[{"kind": …, "ticket": …}]`, or why they were not. `reporting` is told the stage under
    /// way again every [`REPORTING`], so whoever follows the call sees it move even while one
    /// stage takes minutes.
    pub(crate) async fn issue(
        &self,
        person: [u8; 32],
        kinds: &[String],
        mut reporting: impl FnMut(&str),
    ) -> Result<Value, String> {
        let tickets = self.until_serving_tickets(person, &mut reporting).await?;
        let mut issued = Vec::with_capacity(kinds.len());
        for kind in kinds {
            reporting(FOUNDING_ISSUING);
            let issuing = tickets.issue_first(person, kind);
            tokio::pin!(issuing);
            let ticket = loop {
                match tokio::time::timeout(REPORTING, &mut issuing).await {
                    Ok(answered) => break answered?,
                    Err(_) => reporting(FOUNDING_ISSUING),
                }
            };
            let ticket = ticket.get("ticket").cloned().unwrap_or(Value::Null);
            issued.push(serde_json::json!({ "kind": kind, "ticket": ticket }));
        }
        Ok(Value::Array(issued))
    }
}

/// Take the device's key in `keys` as this daemon, the one process that writes as the device,
/// once it has the device's lock.
///
/// # Errors
/// Another process took the key ([`io::ErrorKind::WouldBlock`]), or it cannot be read.
pub(crate) fn take(
    keys: &tilo_device_identity::IdentityPaths,
) -> io::Result<tilo_device_identity::persist::DeviceKeyLock> {
    tilo_device_identity::persist::take_device_key(keys).map_err(|error| {
        let kind = match error {
            tilo_device_identity::PersistError::KeyInUse { .. } => io::ErrorKind::WouldBlock,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, error.to_string())
    })
}

/// [`take`], once the key read before it, `read`, is the one taken.
///
/// # Errors
/// Another process took the key, or it changed since it was read.
pub(crate) fn take_key(
    keys: &tilo_device_identity::IdentityPaths,
    read: &SigningKey,
) -> io::Result<tilo_device_identity::persist::DeviceKeyLock> {
    let taken = take(keys)?;
    if taken.key().verifying_key() != read.verifying_key() {
        return Err(io::Error::other(
            "this computer's device key changed while its daemon started",
        ));
    }
    Ok(taken)
}

/// The device a ticket made of this computer while its daemon runs, once one did.
type Entered = Arc<std::sync::Mutex<Option<Device>>>;

/// The person's services, while they run: what a login answered by the device's own actions is
/// taken into as it is answered, and what a call this endpoint does not answer is handed to.
#[derive(Default)]
struct Serving {
    part: std::sync::Mutex<Option<std::sync::Weak<super::services::Daemon>>>,
    /// Told whenever [`Self::part`] is set or cleared: whoever waits for the person's part looks
    /// again then rather than at its next look.
    started: tokio::sync::Notify,
}

impl Serving {
    /// The person's services run from now on, or (`None`) no longer.
    fn set(&self, part: Option<&Arc<super::services::Daemon>>) {
        *self.part.lock_recover() = part.map(Arc::downgrade);
        self.started.notify_waiters();
    }

    /// The person's services, while they run.
    fn now(&self) -> Option<Arc<super::services::Daemon>> {
        self.part
            .lock_recover()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
    }

    /// The person's services, waiting up to `within` for them to start: `None` when they did not.
    async fn within(&self, within: Duration) -> Option<Arc<super::services::Daemon>> {
        let deadline = std::time::Instant::now().checked_add(within);
        loop {
            // Armed before the look, so a part that starts between the two is not missed.
            let heard = self.started.notified();
            tokio::pin!(heard);
            heard.as_mut().enable();
            if let Some(part) = self.now() {
                return Some(part);
            }
            let left = deadline.map_or(within, |deadline| {
                deadline.saturating_duration_since(std::time::Instant::now())
            });
            if left.is_zero() || tokio::time::timeout(left, heard).await.is_err() {
                return None;
            }
        }
    }
}

/// The person's services, shared by the device and every action answered on its behalf.
type ServingHandle = Arc<Serving>;

impl ThisDevice {
    /// Take this computer's device over `profile`, its key made here on the first start of a
    /// computer whose system made none.
    ///
    /// # Errors
    /// The key cannot be read or made, or another daemon keeps the device.
    pub(crate) fn lock(profile: PathBuf) -> io::Result<Self> {
        let keys = tilo_api::calls::device_keys();
        let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys)
            .map_err(|error| io::Error::other(format!("this computer's device key: {error}")))?;
        let place = Place::here(keys.clone()).map_err(io::Error::other)?;
        tilo_files::native::private::directory(place.dir())?;
        let daemon = tilo_api::daemon::this_computer().map_err(io::Error::other)?;
        let lock = super::serve::lock_device(&daemon)?;
        let taken = take_key(&keys, &key)?;
        Ok(Self {
            key,
            place,
            _key: taken,
            profile,
            own: crate::device::services::this_account_uid(),
            lock,
            entered: Entered::default(),
            founding: Arc::default(),
            ready: Arc::default(),
            serving: ServingHandle::default(),
        })
    }

    /// What the person's own services issue the tickets a founding asks for through.
    pub(crate) fn founding(&self) -> Arc<Founding> {
        Arc::clone(&self.founding)
    }

    /// Whether this computer's identity is one its person's services can start as: a person's
    /// certificate names this device's key and the account's identity store names that person
    /// ([`claimed`]).
    pub(crate) fn claimed(&self) -> bool {
        claimed(&self.place, &self.key, &self.profile, self.own)
    }

    /// The person's services run from now on: a login answered here is taken in by them as it is
    /// answered, by the same path a start enters through, rather than at their next start, and a
    /// call this endpoint does not answer is handed to them.
    pub(crate) fn serving(&self, daemon: &Arc<super::services::Daemon>) {
        self.serving.set(Some(daemon));
    }

    /// The person's services ended: a login answered from now on is taken up by the part that
    /// starts next.
    pub(crate) fn stopped_serving(&self) {
        self.serving.set(None);
    }

    /// The device's own actions, as its endpoints answer them before its person's services run:
    /// its package cache among them, the one it answers after a login too.
    pub(crate) fn answers(&self) -> DeviceOnly {
        let answers = DeviceOnly(
            Arc::new(Answering {
                key: self.key.clone(),
                place: self.place.clone(),
                profile: self.profile.clone(),
                own: self.own,
                entered: Arc::clone(&self.entered),
                founding: Arc::clone(&self.founding),
                ready: Arc::clone(&self.ready),
                serving: Arc::clone(&self.serving),
                runtime: tilo_rt::app().handle().clone(),
                packages: std::sync::OnceLock::new(),
                records: Arc::new(super::records::DeviceRecords::at(
                    self.place.dir(),
                    self.key.verifying_key().to_bytes(),
                )),
            }),
            None,
        );
        match super::packages::PackageCache::this_device(&self.key) {
            Ok(cache) => return answers.with_packages(cache),
            Err(why) => tracing::warn!(%why, "this device keeps no packages"),
        }
        answers
    }

    /// The device a ticket made of this computer while this daemon ran, when one did: what its
    /// person's services enter through first.
    pub(crate) fn entered(&self) -> Option<Device> {
        // The login a ticket made outlives the daemon that redeemed it: one that starts again
        // enters through the entry the login wrote down, until the copy of the person's own
        // workspace names the same server.
        self.entered
            .lock_recover()
            .take()
            .or_else(|| tilo_device_identity::login::filed_entry(&self.place))
    }

    /// This computer's device key, its lock kept here for as long as this device is, for the
    /// person's services to start under.
    pub(crate) fn device_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// This device's lock, kept for as long as this device is: one daemon serves one device.
    pub(crate) const fn device_lock(&self) -> &advisory_lock::FileLock {
        &self.lock
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
            &self.profile,
            self.own,
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

    /// Return once this computer's identity is one its person's services can start as
    /// ([`claimed`]): `true` then, and `false` when the daemon was told to stop first.
    pub(crate) fn until_ready(&self, stop: &super::serve::Stop) -> bool {
        loop {
            if self.claimed() {
                return true;
            }
            let looked = std::time::Instant::now();
            // A certificate this daemon's own action files is heard at once; one another
            // program files is looked for again every [`LOOKING`]. Each sleep hears a stop.
            while looked.elapsed() < LOOKING {
                if stop.wait(HEARING) {
                    return false;
                }
                if self.ready.swap(false, std::sync::atomic::Ordering::AcqRel) {
                    break;
                }
            }
        }
    }
}

/// Whether this computer's identity is one its person's services can start as: the account's
/// identity store names a person this device holds a certificate for, naming its key
/// ([`tilo_identity::trusted_device::serving_in`]). This is the one answer the daemon asks before
/// it starts the person's services and the services ask when they start, so a certificate no
/// identity names and an identity no certificate backs both leave the daemon serving its device
/// alone.
#[must_use]
pub(crate) fn claimed(place: &Place, key: &SigningKey, profile: &Path, own: u32) -> bool {
    let serving = crate::device::services::device_identity_dir(profile, own, place.keys())
        .and_then(|dir| tilo_identity::trusted_device::serving_in(&dir, place.keys()));
    serving.is_ok_and(|(device, _)| device.verifying_key() == key.verifying_key())
}

/// Spend the ticket in `credentials`' [`TICKET_CREDENTIAL`] to make this computer, whose device
/// key is `key`, one of the person's devices, its certificate kept in `place`: `None` when no
/// ticket was handed, or when this computer's identity is already one its person's services can
/// start as, where the ticket is left unspent. The daemon runs over `profile` as `own`.
pub(crate) async fn take_handed_ticket(
    place: &Place,
    key: &SigningKey,
    profile: &Path,
    own: u32,
    credentials: Option<&Path>,
) -> Option<anyhow::Result<Device>> {
    if claimed(place, key, profile, own) {
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
/// ([`LOG_IN`]), a development build's id ([`DEV_ID`]), its package cache
/// ([`super::packages`]), and, to root, who signed in and who left ([`crate::seats`]). Every
/// other action needs a person, and is refused saying so.
#[derive(Clone)]
pub(crate) struct DeviceOnly(Arc<Answering>, Option<crate::seats::Seated>);

struct Answering {
    key: SigningKey,
    place: Place,
    /// The daemon's profile, where founding records the account that made its person.
    profile: PathBuf,
    /// The account the daemon runs as: its own identity names the person first.
    own: u32,
    /// Where the device a ticket made of this computer is kept for its person's services.
    entered: Entered,
    /// What the person's services issue a founding's tickets through, once they run, and the
    /// calls the device took before it had a person, which they answer too.
    founding: Arc<Founding>,
    /// Told when this daemon's own founding, restoring or ticket login took this computer's
    /// person up: the wait for their services looks again then rather than at its next look.
    ready: Arc<std::sync::atomic::AtomicBool>,
    /// The person's services while they run, once they do.
    serving: ServingHandle,
    runtime: tokio::runtime::Handle,
    /// The device's package cache, which it answers before a login as after it.
    packages: std::sync::OnceLock<Arc<dyn ActionProvider + Send + Sync>>,
    /// The records the device keeps as its own: its settings, read, written and followed through
    /// the one record API before anyone claims it.
    records: Arc<super::records::DeviceRecords>,
}

impl Answering {
    /// Keep `update` as where `invocation` stands, and answer the receipt that says so: what a
    /// caller following the call reads.
    fn keep(&self, invocation: InvocationId, update: InvocationUpdate) -> InvocationReceipt {
        let state = InvocationState::new(invocation, update);
        let receipt = state.receipt();
        self.founding
            .calls()
            .lock_recover()
            .insert(invocation, state);
        receipt
    }
}

impl DeviceOnly {
    /// The device actions of the device whose key is `key`, its certificates kept in `place`:
    /// no profile and this process's own account, for a test that asks the device's own actions
    /// alone.
    #[cfg(test)]
    pub(crate) fn new(key: SigningKey, place: Place) -> Self {
        Self::at(
            key,
            place,
            PathBuf::new(),
            crate::device::services::this_account_uid(),
        )
    }

    /// The device actions of a device whose daemon runs over `profile` as `own`: the identity
    /// store its person is read from, and the profile founding records that account in.
    #[cfg(test)]
    pub(crate) fn at(key: SigningKey, place: Place, profile: PathBuf, own: u32) -> Self {
        let records = Arc::new(super::records::DeviceRecords::at(
            place.dir(),
            key.verifying_key().to_bytes(),
        ));
        Self(
            Arc::new(Answering {
                key,
                place,
                profile,
                own,
                entered: Entered::default(),
                founding: Arc::default(),
                ready: Arc::default(),
                serving: ServingHandle::default(),
                runtime: tilo_rt::app().handle().clone(),
                packages: std::sync::OnceLock::new(),
                records,
            }),
            None,
        )
    }

    /// The records this device keeps as its own.
    #[cfg(test)]
    pub(crate) fn records(&self) -> Arc<super::records::DeviceRecords> {
        Arc::clone(&self.0.records)
    }

    /// The person's services, as their part hands them to the device's own actions when it
    /// starts: what a login answered now is taken into, and what a call this endpoint does not
    /// answer is handed to.
    #[cfg(test)]
    pub(crate) fn serving(&self, daemon: &Arc<super::services::Daemon>) {
        self.0.serving.set(Some(daemon));
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
        claimed(&self.0.place, &self.0.key, &self.0.profile, self.0.own)
    }

    /// Whether a founding this device answered at once is making its person now.
    pub(crate) fn founding(&self) -> bool {
        self.0.founding.under_way()
    }

    /// What this device's own answers keep of a founding, told as the person's services run:
    /// [`Founding::person_serves`] is what the endpoint that serves them says.
    pub(crate) fn founding_record(&self) -> Arc<Founding> {
        Arc::clone(&self.0.founding)
    }

    /// The daemon's profile, where founding records the account that made its person and the
    /// seats whose shells are admitted here: empty for a device actions' value no daemon keeps.
    pub(crate) fn profile(&self) -> &std::path::Path {
        &self.0.profile
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
        tilo_device_identity::persist::read_owner_cert(keys)
            .ok()
            .flatten()
            .filter(|cert| cert.device_public_key == self.public_key())
            .map(|cert| cert.device_label)
    }

    /// Why an action that needs a person is not answered now.
    fn needs_a_person(&self) -> ActionCallError {
        // A device whose person exists answers this action through their own part, which this
        // endpoint never serves: what the person's services do is theirs, and a request for it
        // that needs waiting is held for them before it reaches here.
        let reason = if self.claimed() {
            "this action is this computer's person's, which this endpoint does not serve"
        } else {
            NO_PERSON
        };
        ActionCallError::Refused {
            reason: reason.into(),
        }
    }

    /// `request`, one this endpoint does not answer itself: the person's part answers it, once
    /// it serves. A part that is still starting is waited for within [`crate::daemon::
    /// WAITED_WITHIN`], and one that does not start is answered unavailable, never as an action
    /// this computer offers not at all. The wait is on the runtime, and opening the person's tree
    /// waits on the workspaces: it is opened on a thread of its own.
    fn answer_by_the_person(
        &self,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // A device no person's certificate names has none to answer it, and one whose
        // connection is not the person's own is answered the device's actions alone: the
        // person's tree is theirs, and no other account reaches it here.
        if !self.claimed() || !self.is_the_person() {
            return Pending::ready(Err(self.needs_a_person()));
        }
        let (done, answer) = tilo_actions::remote::pending();
        let answering = self.clone();
        self.0.runtime.spawn(async move {
            let answered = match answering
                .0
                .serving
                .within(crate::daemon::WAITED_WITHIN)
                .await
            {
                Some(part) => {
                    let made = tokio::task::spawn_blocking(move || {
                        let tree = part.connection_tree_in(None);
                        (part, tree)
                    })
                    .await;
                    match made {
                        Ok((_part, Ok((tree, caller)))) => {
                            tree.invoke(&caller, request).await.unwrap_or_else(|| {
                                Err(ActionCallError::unavailable(
                                    "this computer's person's part abandoned the answer",
                                ))
                            })
                        }
                        Ok((_part, Err(why))) => Err(ActionCallError::Unavailable {
                            reason: format!(
                                "this computer's person's services are still starting ({why})"
                            ),
                        }),
                        Err(error) => Err(ActionCallError::Unavailable {
                            reason: format!(
                                "this computer's person's services are still starting ({error})"
                            ),
                        }),
                    }
                }
                None => Err(ActionCallError::Unavailable {
                    reason: "this computer's person's services are still starting".to_owned(),
                }),
            };
            done.complete(answered);
        });
        answer
    }

    /// Whether this connection is the person's own, whose part answers as them: the device's own
    /// account on its own daemon, or the person it serves. Root of a computer's daemon, another
    /// account, a founding session and a stranger reach the device's own actions alone.
    fn is_the_person(&self) -> bool {
        let Some(seated) = &self.1 else {
            return true;
        };
        if seated.seats.system() {
            return false;
        }
        match seated.seat.admitted {
            Admitted::Device => true,
            Admitted::Person(person) => self.person() == Some(person),
            Admitted::Key { .. } | Admitted::Founding { .. } | Admitted::Stranger => false,
        }
    }

    /// [`DEV_ID`]'s answer, one of the device's own: the id a development build of an app runs
    /// under, derived from the key this part keeps.
    fn answer_dev_id(
        &self,
        invocation: InvocationId,
        caller: &tilo_actions::remote::Caller,
        argument: InvokeArgument,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let InvokeArgument::Json(argument) = argument else {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "a device action asks with its arguments".into(),
            }));
        };
        let personal = !tilo_api::daemon::system_daemon();
        let answer = match super::ticket::dev_id_answer(
            caller,
            personal,
            &Value::Object(argument),
            &self.0.key,
        ) {
            Ok(answer) => answer,
            Err(error) => return Pending::ready(Err(error)),
        };
        Pending::ready(Ok(self.keep(invocation, succeeded(invocation, answer))))
    }

    fn keep(&self, invocation: InvocationId, update: InvocationUpdate) -> InvocationReceipt {
        self.0.keep(invocation, update)
    }

    /// Report `stage` on `invocation`, as the report numbered `sequence`: what a caller
    /// following the call reads while it waits.
    fn progressing(
        &self,
        invocation: InvocationId,
        sequence: u64,
        stage: &str,
    ) -> InvocationReceipt {
        self.keep(
            invocation,
            InvocationUpdate::Progress {
                invocation,
                sequence,
                phase: FOUNDING_FOUNDING.to_owned(),
                completed: None,
                total: None,
                message: Some(stage.to_owned()),
            },
        )
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
            super::services::found_account_person(self.keys(), &name, seated.seat.uid)
                .map_err(|error| format!("this device could not make a new person: {error}").into())
        })
    }

    /// Answer the founding `invocation`, which asks for the tickets `kinds` names, at once:
    /// the person is made and their part started under it, which takes longer than a caller
    /// waits for one answer, and each stage is reported on the call as it is reached. The
    /// tickets are issued as the person's own act once their services run here, and the answer
    /// says why when they were not.
    fn founding_tickets(
        &self,
        invocation: InvocationId,
        args: Value,
        kinds: Vec<String>,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // The call stands as under way before anything is made, so a caller that asks again
        // under its id reads that same call rather than founding the person a second time.
        let receipt = self.keep(invocation, InvocationUpdate::Accepted { invocation });
        self.0.founding.set_under_way(true);
        let (runtime, answering) = (self.0.runtime.clone(), self.clone());
        runtime.spawn(async move {
            let mut sequence = 1;
            answering.progressing(invocation, sequence, FOUNDING_STARTING);
            let mut reporting = {
                let answering = answering.clone();
                move |stage: &str| {
                    sequence = sequence.saturating_add(1);
                    answering.progressing(invocation, sequence, stage);
                }
            };
            // Making the person is the account's work on disk and no async step: it runs where
            // it cannot hold the runtime the answer above was written on, reporting the stage
            // it is in for as long as it takes.
            let founding = answering.clone();
            let mut waiting = tokio::task::spawn_blocking(move || founding.found(&args));
            let made = loop {
                match tokio::time::timeout(REPORTING, &mut waiting).await {
                    Ok(joined) => break joined,
                    Err(_) => reporting(FOUNDING_STARTING),
                }
            };
            // Made or not, the person's part is the device's to say from here: started, or
            // never to start.
            answering.0.founding.set_under_way(false);
            let person = match made {
                Ok(Ok(person)) => person,
                Ok(Err(refusal)) => {
                    let update = failed_as(invocation, refusal.code, &refusal.message);
                    let _ = answering.keep(invocation, update);
                    return;
                }
                Err(error) => {
                    let update = failed(
                        invocation,
                        &format!("this device could not make a new person: {error}"),
                    );
                    let _ = answering.keep(invocation, update);
                    return;
                }
            };
            let asked = hex::decode(&person)
                .ok()
                .and_then(|key| <[u8; 32]>::try_from(key).ok());
            reporting(FOUNDING_FOUNDING);
            let outcome = match asked {
                Some(person) => answering.0.founding.issue(person, &kinds, reporting).await,
                None => Err("the person founded is no key".to_owned()),
            };
            let update = match outcome {
                Ok(tickets) => succeeded(
                    invocation,
                    serde_json::json!({ "person": person, "tickets": tickets }),
                ),
                // The person is founded either way: the tickets they asked for are not, and the
                // founder reads why and may add the computer another way.
                Err(why) => succeeded(
                    invocation,
                    serde_json::json!({ "person": person, "tickets": [], "tickets_error": why }),
                ),
            };
            let _ = answering.keep(invocation, update);
        });
        Pending::ready(Ok(receipt))
    }

    /// Answer the founding `invocation` of `person` once this device's services run: the start
    /// that follows the making of their root founds their own workspace before it serves them
    /// ([`super::services::found_account_person`]), so the founder hears back once the person has
    /// one. The call is kept as under way meanwhile: a founder whose wait for one answer ends
    /// first follows it by its id, and reads this call, never founds again as the person it
    /// just became.
    fn once_serving(
        &self,
        invocation: InvocationId,
        person: String,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let _ = self.keep(invocation, InvocationUpdate::Accepted { invocation });
        let (done, answer) = tilo_actions::remote::pending();
        let answering = self.clone();
        self.0.runtime.spawn(async move {
            match answering.0.founding.until_serving(|_| {}).await {
                Ok(()) => {
                    let update = succeeded(invocation, serde_json::json!({ "person": person }));
                    done.complete(Ok(answering.keep(invocation, update)));
                }
                // The founding ends with what kept the person's workspace from being bound:
                // answered as the refusal it is, never as a person whose workspace is here.
                Err(why) => done.complete(Err(ActionCallError::refused(why))),
            }
        });
        answer
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
            super::services::restore_account_person(self.keys(), args, seated.seat.uid).map_err(
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
        // A daemon that is not the system's serves one person, so a device a person's
        // certificate names starts no other on it.
        let system = self.1.as_ref().is_some_and(|seated| seated.seats.system());
        if !system && self.claimed() {
            return Err(format!("{ALREADY_A_PERSONS}: it serves them").into());
        }
        let Some(seated) = self.1.as_ref() else {
            return Err(
                "only an account signed in on this computer starts a person on it"
                    .to_owned()
                    .into(),
            );
        };
        if !seated
            .seats
            .may_found(&seated.seat)
            .map_err(str::to_owned)?
        {
            return Err(
                format!("{ALREADY_A_PERSONS}: this account's person is set up here").into(),
            );
        }
        let person = make(seated)?;
        // Their services start now: the wait for them looks again at once, not at its next look.
        self.0
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        seated.seats.founded_by(&seated.seat, person)?;
        // A person founded while this daemon serves has their root marked and their asks served
        // from now on, without the daemon starting again.
        super::services::person_founded(person, seated.seat.uid);
        Ok(hex::encode(person))
    }

    /// Log this device in with the ticket `args` name, as `tilo login` does.
    async fn log_in(&self, args: &Value) -> anyhow::Result<Value> {
        let (ticket, agent, label) = login_asked(args)?;
        let device =
            tilo_api::daemon::enter::log_in_here(&self.0.key, &self.0.place, ticket, agent, label)
                .await?;
        // This computer is one of the person's devices now: their services start at once.
        self.0
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        // The account joined the person this computer now serves: the person it made here for
        // its setup is retired, and the session that asked is the joined person's from now on.
        if let Some(seated) = &self.1 {
            seated
                .seats
                .adopted_login(seated.seat.uid, device.cert.user_public_key);
        }
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

/// What the tickets a founder asks for lets in, as the founding call names them: `server` for a
/// server, `device` for a computer at hand. Nothing else is issued.
fn founding_kinds(args: &Value) -> Vec<String> {
    args.get("tickets")
        .and_then(Value::as_array)
        .map(|kinds| {
            kinds
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The name a new person takes from the account `uid` that made them: its full name, else its
/// login name. Root asking for its daemon names the daemon's own account.
fn founder_name(uid: u32) -> String {
    let uid = if uid == tilo_api::daemon::admission::ROOT {
        crate::seats::own_account()
    } else {
        uid
    };
    tilo_identity::provisioning::account_display_name(uid)
}

impl DeviceOnly {
    /// Answer `action`, one a person's programs ask with the device key
    /// ([`super::key_use::FOR_A_PERSON`]): the device's own system and its person's accounts
    /// alone, whose account could read the key before the daemon alone kept it. Answered in the
    /// receipt and kept nowhere.
    fn for_a_person(
        &self,
        invocation: InvocationId,
        action: &str,
        argument: &InvokeArgument,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        use tilo_api::daemon::admission::Admitted;
        let theirs = self.1.as_ref().is_some_and(|seated| {
            matches!(seated.seat.admitted, Admitted::Device | Admitted::Person(_))
        });
        if !theirs {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "only this device's system and its person ask what the device key does"
                    .into(),
            }));
        }
        let InvokeArgument::Json(argument) = argument else {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "a device action asks with its arguments".into(),
            }));
        };
        let update =
            match super::key_use::answer(action, &Value::Object(argument.clone()), &self.0.key) {
                Ok(value) => succeeded(invocation, value),
                Err(refusal) => failed_as(invocation, refusal.code, &refusal.message),
            };
        Pending::ready(Ok(InvocationState::new(invocation, update).receipt()))
    }

    /// Renew this device's certificates of the asking account's person that are due, with the
    /// root kept here for that account ([`super::services::renew_certificates`]): the account's
    /// session asks, since only this process opens the root. Any account this device serves
    /// asks for its own person's alone.
    fn renew_certificates(
        &self,
        invocation: InvocationId,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(uid) = self
            .1
            .as_ref()
            .and_then(|seated| seated.seat.admitted.served().then_some(seated.seat.uid))
        else {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "only an account this device serves renews its certificates".into(),
            }));
        };
        let device = self.0.key.verifying_key().to_bytes();
        let update = match super::services::renew_certificates(uid, device) {
            Ok(renewed) => succeeded(invocation, serde_json::json!({ "renewed": renewed })),
            Err(error) => failed(invocation, &error),
        };
        Pending::ready(Ok(InvocationState::new(invocation, update).receipt()))
    }

    /// Answer `action`, one the device's own system asks with the device key: root, or the
    /// daemon's own account, alone.
    fn for_the_system(
        &self,
        invocation: InvocationId,
        action: &str,
        argument: &InvokeArgument,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let system = self.1.as_ref().is_some_and(|seated| {
            seated.seat.admitted == tilo_api::daemon::admission::Admitted::Device
        });
        if !system {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "only this device's system asks what the device key does".into(),
            }));
        }
        let InvokeArgument::Json(argument) = argument else {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "a device action asks with its arguments".into(),
            }));
        };
        let args = Value::Object(argument.clone());
        let ended = move |answer: Result<Value, super::key_use::Refusal>| match answer {
            Ok(value) => succeeded(invocation, value),
            Err(refusal) => failed_as(invocation, refusal.code, &refusal.message),
        };
        if action != SET_UP_UNLOCK {
            let update = ended(super::key_use::answer(action, &args, &self.0.key));
            return Pending::ready(Ok(self.keep(invocation, update)));
        }
        // A boot is set up through the person's own dispatch, which their services run: a device
        // no person's certificate names has none to start, and is told so at once.
        if !self.claimed() {
            let update = failed(invocation, NO_PERSON);
            return Pending::ready(Ok(self.keep(invocation, update)));
        }
        let receipt = self.keep(invocation, InvocationUpdate::Accepted { invocation });
        let answering = self.clone();
        let founding = Arc::clone(&self.0.founding);
        self.0.runtime.spawn(async move {
            // Within the bound every call that needs the person's part waits for it, and then
            // the same words.
            let Ok(unlocking) =
                tokio::time::timeout(crate::daemon::WAITED_WITHIN, founding.boot_unlock()).await
            else {
                let update = failed(
                    invocation,
                    "this computer's person's part did not answer its boot unlock in time",
                );
                answering.keep(invocation, update);
                return;
            };
            let update = ended(unlocking.enrol(&args).await);
            answering.keep(invocation, update);
        });
        Pending::ready(Ok(receipt))
    }

    /// A seat action asks this device before a person claims it: what the seat serving here
    /// answers, kept as the call's receipt.
    fn answer_seat(
        &self,
        invocation: InvocationId,
        address: &tilo_actions::ActionAddress,
        argument: InvokeArgument,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let InvokeArgument::Json(argument) = argument else {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "a seat action asks with its arguments".into(),
            }));
        };
        let Some(seated) = &self.1 else {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: crate::seats::ROOT_ALONE.into(),
            }));
        };
        Pending::ready(
            seated
                .act(&address.action, &Value::Object(argument))
                .map(|value| self.keep(invocation, succeeded(invocation, value)))
                .map_err(|reason| ActionCallError::Refused { reason }),
        )
    }

    /// The device's own records, its settings, written here before anyone claims it: the action
    /// `action` of one, as `caller` asks it with `argument`.
    fn answer_setting(
        &self,
        caller: &Caller,
        invocation: InvocationId,
        argument: &InvokeArgument,
        action: &str,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let answer = match argument {
            InvokeArgument::Json(args) => self.0.records.act(caller, action, args),
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
        Pending::ready(Ok(self.keep(invocation, update)))
    }
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
                page.providers.extend(providers(""));
                page.providers.push(crate::seats::provider());
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
        if let Some(state) = self.0.founding.calls().lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        // The session a sign-in answered a founding link asks only what makes or joins its
        // person, until the person exists: from then on it is the person's, and its link reaches
        // them.
        if let Some(seated) = &self.1
            && matches!(seated.seat.admitted, Admitted::Founding { .. })
            && !answers_a_founding_setup(&request.target.provider, &request.target.address.action)
        {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: FOUNDING_SETUP_ONLY.into(),
            }));
        }
        if let Some(cache) = self.packages()
            && cache.serves(&request.target.provider)
        {
            return cache.invoke(caller, request);
        }
        let address = &request.target.address;
        if request.target.provider == crate::seats::PROVIDER {
            return self.answer_seat(invocation, address, request.argument);
        }
        // The device's own records: its settings, written here before anyone claims it.
        if request.target.provider == PROVIDER && address.resource == super::records::RESOURCE {
            return self.answer_setting(caller, invocation, &request.argument, &address.action);
        }
        if request.target.provider != PROVIDER || address.resource != RESOURCE {
            return self.answer_by_the_person(request);
        }
        if super::key_use::FOR_THE_SYSTEM.contains(&address.action.as_str()) {
            return self.for_the_system(invocation, &address.action, &request.argument);
        }
        if super::key_use::FOR_A_PERSON.contains(&address.action.as_str()) {
            return self.for_a_person(invocation, &address.action, &request.argument);
        }
        if address.action == tilo_device_identity::login::RENEW_CERTIFICATES {
            return self.renew_certificates(invocation);
        }
        match address.action.as_str() {
            // The id a development build of an app runs under: the device's own, as installing
            // the build is, answered before its person's services run.
            DEV_ID => self.answer_dev_id(invocation, caller, request.argument),
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
                    // The login enters what it brought here before it answers a running part: a
                    // future that big is boxed, so the task the runtime holds stays small.
                    let update = match Box::pin(answering.log_in(&Value::Object(argument))).await {
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
                // A founder that asks for their first tickets is answered them as part of
                // founding: the call is answered at once and reports each stage, since making
                // the person and starting their part takes longer than a caller waits.
                if address.action == FOUND {
                    let kinds = founding_kinds(&args);
                    if !kinds.is_empty() {
                        return self.founding_tickets(invocation, args, kinds);
                    }
                }
                // Services that serve a person already start nobody for the one founded now.
                let serving_already = self.claimed();
                let made = if address.action == FOUND {
                    self.found(&args)
                } else {
                    self.restore(&args)
                };
                match made {
                    // The first person founded here has their workspace founded as their
                    // services start: the founding is answered once they have one.
                    Ok(person) if address.action == FOUND && !serving_already => {
                        self.once_serving(invocation, person)
                    }
                    Ok(person) => Pending::ready(Ok(self.keep(
                        invocation,
                        succeeded(invocation, serde_json::json!({ "person": person })),
                    ))),
                    Err(refusal) => Pending::ready(Ok(self.keep(
                        invocation,
                        failed_as(invocation, refusal.code, &refusal.message),
                    ))),
                }
            }
            _ => self.answer_by_the_person(request),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0
            .founding
            .calls()
            .lock_recover()
            .get(id)
            .cloned()
            .or_else(|| self.packages()?.observe(caller, id))
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(match self.0.founding.calls().lock_recover().get(id) {
            Some(_) => ActionCallError::NotCancellable {
                reason: "a login, once asked for, is the issuing device's to answer".into(),
            },
            None => ActionCallError::UnknownInvocation,
        }))
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: tilo_actions::remote::Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Some(cache) = self.packages()
            && cache.serves(&request.target.provider)
        {
            return cache.run(caller, request, admitted);
        }
        self.invoke(caller, request)
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.packages()
            .filter(|cache| cache.serves(&request.target.provider))?
            .asks_first(caller, request, admitted)
    }

    fn watch(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        match self
            .packages()
            .filter(|cache| cache.serves(&request.target.provider))
        {
            Some(cache) => cache.watch(caller, request),
            None => Err("this action has no live result here".to_owned()),
        }
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
