//! Who each connection to the native endpoint is, and the device's answers about it: which
//! account signed in as which person (the actions `seats#signed-in` and `seats#left`, root's
//! alone to call), and who a connection is (`{"native": "account#who"}`).
//!
//! A person's shell reaches the daemon only by a link the daemon made for it
//! ([`crate::device::services::Daemon::shell_token`], redeemed with `{"native": "shells#link"}`).
//! Under the system unit root starts each session, so `seats#signed-in` answers the one-use secret
//! beside its reply, and root hands it to the session it starts. A daemon that runs as the
//! person's own account starts the shell itself when that account asks
//! (`{"native": "shells#start"}`), and answers when it ended (`{"native": "shells#wait"}`): the
//! secret passes from the daemon to the shell and to no other process. A link that ended is asked
//! for again for the same seat, whose account and process the kernel names and whose link this
//! daemon admitted before, so a restart leaves no dead desktop; a seat has one link at a time.
//!
//! The account a connection comes from is the kernel's word for its peer ([`peer_of`]), and
//! the daemon's admission ([`tilo_api::daemon::admission`]) says what it acts as. Which person an
//! account is, root says when the account signs in, and the daemon never guesses it from the uid:
//! it keeps that person for the account until root says the account left. The account that set
//! the computer up is the device's person, as the daemon recorded when it made or restored them
//! ([`tilo_api::daemon::admission::Admission::founded_by`]): an account it has no such record of
//! is never that person.
//!
//! The device serves one person, who is bound to it. Another person who signs in on the computer
//! acts as a key of their own on this device: the daemon makes it when they sign in and answers
//! its public half, the person's own session certifies it with their root (or their identity's
//! issuer) as any device key of theirs is certified, and hands the certificate back
//! (`seats#certified`, the account's own to call). From then on the account acts as that key, and
//! only as it, until it left; the key's secret never leaves the daemon.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::process::Child;
use std::sync::{Arc, Mutex};

use runtime_portal::process_link::Descriptor;
use tilo_socket::Stream;

use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;

use serde_json::{Value, json};
use tilo_actions::remote::{
    CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource, CatalogueRow,
};
use tilo_actions::{ActionAddress, ActionParam, ParamType};
use tilo_api::daemon::admission::{Admission, Admitted, ROOT};
use tilo_lock::MutexRecover as _;
use tilo_mcp::lines::Answer;

use crate::device::unclaimed::DeviceOnly;

/// The resource the seats' actions are on, and the app they are listed as.
pub(crate) const SEATS: &str = "seats";
/// The provider the device answers the seats' actions as.
pub(crate) const PROVIDER: &str = "tilo.seats";
/// What the seats' catalogue is.
const REVISION: &str = "tilo.seats/1";
/// Say an account signed in: `{uid, person?, importing?}`, the person the account holds, as hex,
/// when it holds one, and whether the account's session hands over what its desktop kept.
const SIGNED_IN: &str = "signed-in";
/// Say an account's session ended: `{uid}`.
const LEFT: &str = "left";
/// Hand the certificate of the key an account acts as: `{cert}`, its canonical bytes as hex.
/// The account's own to call.
pub(crate) const CERTIFIED: &str = "certified";
/// Who this connection is: `{person?, device, label?, claimed}`.
const WHO: &str = "account#who";
/// Start the person's shell: `{args, env}`, the shell's arguments and environment, answered
/// `{pid}`.
const START: &str = "shells#start";
/// Wait for a shell the daemon started to end: `{pid}`, answered `{code?, signal?}` once it ended.
const WAIT: &str = "shells#wait";
/// The one request that establishes a shell link, on the daemon's own socket: `{token?}`, the
/// one-use secret the daemon gave the shell the first time, answered the link as a descriptor.
const LINK: &str = runtime_collab::wire::shell::SHELL_LINK;
/// Hand the device's tilolang processes an instance: `{secret}`, the one-use secret the daemon
/// answered the shell's link with, and the packet socket as the line's one descriptor.
const PROCESSES: &str = runtime_collab::wire::shell::SHELL_PROCESSES;
/// Why anyone but root is refused the seats' actions.
pub(crate) const ROOT_ALONE: &str = "only root says who signed in on this computer, or who left";

/// The seats as the device lists them beside its own actions: a provider of their own.
pub(crate) fn provider() -> CatalogueProvider {
    CatalogueProvider {
        provider: PROVIDER.to_owned(),
        app_id: SEATS.to_owned(),
        workspace: String::new(),
        browsable: true,
        ..CatalogueProvider::default()
    }
}

/// The page `query` asks of the seats' actions.
pub(crate) fn catalogue(query: &CatalogueQuery) -> CataloguePage {
    CataloguePage::from_rows(REVISION.to_owned(), rows(), query)
}

/// The seats' actions.
fn rows() -> Vec<CatalogueRow> {
    let resource = CatalogueResource {
        id: SEATS.to_owned(),
        kind: "seat".to_owned(),
        label: "Seats".to_owned(),
        presentation: None,
    };
    let row = |action| CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: SEATS.to_owned(),
        parent_resource: String::new(),
        resource: resource.clone(),
        action,
        ..CatalogueRow::default()
    };
    let uid = || ActionParam::required("uid", ParamType::Integer, "The account's uid.");
    let action = |name: &str, label: &str, description: &str, params: &[ActionParam]| {
        let mut descriptor = crate::device::ticket::descriptor(name, label, description, params);
        descriptor.address = ActionAddress::new(SEATS, name);
        "seat".clone_into(&mut descriptor.resource_kind);
        "Seats".clone_into(&mut descriptor.resource_label);
        Some(descriptor)
    };
    vec![
        row(None),
        row(action(
            SIGNED_IN,
            "An account signed in",
            "Serve the account as its person until it leaves. Root's alone to say.",
            &[
                uid(),
                ActionParam::optional(
                    "person",
                    ParamType::Text,
                    "The person the account holds, their key as hex.",
                ),
                ActionParam::optional(
                    "importing",
                    ParamType::Boolean,
                    "Whether the account's session hands over the workspaces its desktop kept.",
                ),
            ],
        )),
        row(action(
            LEFT,
            "An account left",
            "Serve the account the device's own actions alone again. Root's alone to say.",
            &[uid()],
        )),
        row(action(
            CERTIFIED,
            "Certify my key here",
            "Act on this computer as the key it made for me, which my root certified.",
            &[ActionParam::required(
                "cert",
                ParamType::Text,
                "The key's certificate, its canonical bytes as hex.",
            )],
        )),
    ]
}

/// How a connection's peer is read: the kernel's account and process at its other end, or a
/// test's stand-in.
pub(crate) type PeerOf = fn(&Stream) -> io::Result<Peer>;

/// The process at the other end of a connection, as the kernel names it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Peer {
    pub(crate) uid: u32,
    /// `None` for a process outside this daemon's view of processes.
    pub(crate) pid: Option<u32>,
}

impl Peer {
    /// The kernel's word for `stream`'s peer.
    ///
    /// # Errors
    /// The kernel does not say.
    pub(crate) fn of(stream: &Stream) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        {
            let facts = peer_facts::facts(stream)?;
            Ok(Self {
                uid: facts.uid,
                pid: facts.pid,
            })
        }
        // A pipe of the daemon's admits its own account's processes alone (`tilo_socket`).
        #[cfg(windows)]
        {
            Ok(Self {
                uid: own_account(),
                pid: stream.peer_pid().ok(),
            })
        }
    }
}

/// This daemon's own account, as the admission names accounts
/// ([`tilo_api::daemon::admission::Admission`]): its uid on Unix. Windows names an account by a
/// SID rather than a number, and a pipe of the daemon's admits only its own account's processes,
/// so every peer there is this one account, which the admission knows by this number.
pub(crate) fn own_account() -> u32 {
    #[cfg(unix)]
    {
        rustix::process::geteuid().as_raw()
    }
    #[cfg(windows)]
    {
        OWN_WINDOWS_ACCOUNT
    }
}

/// The number the admission knows the daemon's own Windows account by: any but [`ROOT`]'s.
#[cfg(windows)]
const OWN_WINDOWS_ACCOUNT: u32 = 1;

/// The process at the other end of `stream`, as the kernel says.
///
/// # Errors
/// The kernel does not say.
pub(crate) fn peer_of(stream: &Stream) -> io::Result<Peer> {
    Peer::of(stream)
}

/// What finds the app and workspace the daemon started process `pid` as: the daemon's launch
/// records, walked up the process's parents where a sandbox put it below its launcher.
pub(crate) type AppOf = Arc<dyn Fn(u32) -> Option<(String, String)> + Send + Sync>;

/// An app the daemon started, and the workspace it runs in: what a connection from it is
/// admitted as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppAt {
    pub(crate) app: String,
    pub(crate) workspace: String,
    /// The process the daemon recorded, which a release of a vault item is answered to
    /// ([`crate::device::vault_release`]).
    pub(crate) pid: u32,
}

/// One connection's account, and what it is admitted as.
#[derive(Clone, Debug)]
pub(crate) struct Seat {
    pub(crate) uid: u32,
    /// The process at the connection's other end, as the kernel names it: what a shell link is
    /// tied to, so only the same session reaches the person's workspaces again.
    pub(crate) pid: Option<u32>,
    pub(crate) admitted: Admitted,
    /// The app the process was started as, when it is one the daemon started.
    pub(crate) app: Option<AppAt>,
    /// Whether this process is the session the daemon answered a founding link: it asks this
    /// device's own actions about the person it made or joined, and nothing of a person's.
    pub(crate) founding: bool,
}

/// What the daemon does for the person its seats admit, once their services run.
pub(crate) trait Shells: Send + Sync {
    /// The person signed in on a seat: find them their workspace, unless their session hands
    /// over the ones their desktop kept (`importing`), and make the one-use secret their shell
    /// redeems for its link (`shells#link`).
    ///
    /// # Errors
    /// No secret could be made.
    fn token(&self, importing: bool) -> io::Result<[u8; 32]>;

    /// Who the one-use secret `token` was made for, without taking it.
    fn token_holder(&self, token: &[u8; 32]) -> Option<[u8; 32]>;

    /// The connection `secret`, a secret the daemon made, admits on the broker's socket as the
    /// link of the seat `seat`.
    ///
    /// # Errors
    /// The dispatch's socket does not take the connection.
    fn redeem(&self, secret: [u8; 32], seat: u64) -> io::Result<Stream>;

    /// A link for `person`'s shell on `seat`: refused while the seat has one.
    ///
    /// # Errors
    /// The seat is linked already, or the dispatch's socket does not take the connection.
    fn link_for(&self, person: [u8; 32], seat: u64) -> io::Result<Stream>;

    /// Whether the seat has a shell link standing now.
    fn live(&self, seat: u64) -> bool;

    /// Whether this daemon acts for `person`: the person whose link it makes and whose services
    /// it runs. The token it makes names that person, so the device's own account (and root)
    /// redeems it as the person this daemon serves, never as another.
    fn serves(&self, person: [u8; 32]) -> bool;

    /// Hand the device's tilolang processes `packets`, which a connection of the account `uid`
    /// (the kernel's word for its peer) carried with `secret`
    /// (`tilo_app_host::shell::ShellLinks::hand_processes`).
    ///
    /// # Errors
    /// Why the hand-over is refused.
    fn hand_processes(
        &self,
        uid: u32,
        secret: &[u8; 32],
        packets: Descriptor,
    ) -> Result<(), String>;

    /// Start the person's shell with `args` and `env`, handed its link and, when the session that
    /// asked sent one (`window`), the connection it draws its window on.
    ///
    /// # Errors
    /// The shell did not start.
    fn start(
        &self,
        args: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
        window: Option<Descriptor>,
    ) -> io::Result<Child>;
}

/// What a seat's link was admitted for, kept here for as long as the daemon runs: the process a
/// sign-in's founding link was redeemed from, or the process whose shell link became a person's.
#[derive(Clone, Copy)]
enum SeatLink {
    /// A sign-in with no person yet answered this process a founding link: it may make a person
    /// or join one until it is the person's.
    Founding,
    /// The process acts for `person`: its connections may reach them, and its shell asks for
    /// their link again after a restart. `device` is whether the link was made for the person this
    /// device itself serves, whose own endpoint answers the seat rather than a key's.
    Person { person: [u8; 32], device: bool },
}

/// What this daemon's person's services are, as the seats find them: still starting, running, or
/// ended in the failure that kept them from starting.
#[derive(Clone)]
enum Services {
    /// Nothing serves the person yet: a request that needs them waits for their start.
    Starting,
    /// The person's services run, and answer for them through these shells.
    Running(Arc<dyn Shells>),
    /// They did not start, and the daemon ends: a request that waited for them is answered this
    /// rather than waiting for a part that will not come.
    Failed(String),
}

/// The daemon's seats: its admission, and the device it answers for.
#[derive(Clone)]
pub(crate) struct Seats {
    admission: &'static Admission,
    device: DeviceOnly,
    peer_of: PeerOf,
    /// What finds the app the daemon started a peer process as, once the daemon keeps its
    /// launch records.
    app_of: Arc<Mutex<Option<AppOf>>>,
    /// What the daemon does for its person, and whether their services started: `Starting` until
    /// they run, and why they did not when their start failed.
    shells: Arc<tokio::sync::watch::Sender<Services>>,
    /// The shells this daemon started, by pid, until one is waited for.
    started: Arc<Mutex<HashMap<u32, Child>>>,
    /// The key each account signed in as another person than the device's acts as, by uid,
    /// until it left.
    keys: Arc<Mutex<HashMap<u32, SigningKey>>>,
    /// The one-use secrets a founding sign-in answers, each for the account that may redeem it,
    /// until a process redeems it as its link.
    founding: Arc<Mutex<HashMap<[u8; 32], u32>>>,
    /// The seats whose links were admitted, by account and process, for as long as this daemon
    /// runs.
    links: Arc<Mutex<HashMap<(u32, u32), SeatLink>>>,
}

impl Seats {
    pub(crate) fn new(admission: &'static Admission, device: DeviceOnly, peer_of: PeerOf) -> Self {
        Self {
            admission,
            device,
            peer_of,
            app_of: Arc::default(),
            shells: Arc::new(tokio::sync::watch::channel(Services::Starting).0),
            started: Arc::default(),
            keys: Arc::default(),
            founding: Arc::default(),
            links: Arc::default(),
        }
    }

    /// Find the app the daemon started process `pid` as with `app_of` from now on: what admits a
    /// connection from a process the daemon started as the app it runs, and never as the person.
    pub(crate) fn follow_launches(&self, app_of: AppOf) {
        *self
            .app_of
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(app_of);
    }

    /// The key the account `seat` acts as, with its certificate: an account signed in as
    /// another person than the device's, once their certificate for it came.
    pub(crate) fn key(&self, seat: &Seat) -> Option<(SigningKey, DeviceCert)> {
        let Admitted::Key { key, .. } = seat.admitted else {
            return None;
        };
        let cert = self.admission.key_of(seat.uid)?;
        let signing = self.keys.lock_recover().get(&seat.uid).cloned()?;
        (signing.verifying_key().to_bytes() == key && cert.device_public_key == key)
            .then_some((signing, cert))
    }

    /// Whether the account `seat` may make a person ([`FOUND`]) and has none yet: `Ok(false)`
    /// when it has one already, and refused when it is no account that makes one. Under the
    /// system unit the account's session makes its person by the founding link its sign-in
    /// answered; a person's own daemon makes one for its own account while the device has none.
    /// Root under the system unit, an account nobody signed in, a program of the account and a
    /// stranger to a person's own daemon make none.
    ///
    /// [`FOUND`]: tilo_device_identity::login::FOUND
    pub(crate) fn may_found(&self, seat: &Seat) -> Result<bool, &'static str> {
        const NOT_SIGNED_IN: &str =
            "only an account signed in on this computer starts a person on it";
        const NOT_THE_LINK: &str = "only the session this computer signed in, with the link its sign-in answered, \
             starts a person on it";
        if seat.uid == u32::MAX || seat.app.is_some() {
            return Err(NOT_SIGNED_IN);
        }
        if !self.admission.system() {
            return if seat.admitted == Admitted::Device {
                Ok(!self.device.claimed())
            } else {
                Err(NOT_SIGNED_IN)
            };
        }
        // The account's session reaches the daemon by the founding link its sign-in answered:
        // nothing else of the account makes a person, as no shell program and no app is that
        // session.
        if seat.uid == ROOT
            || seat.admitted != Admitted::Device
                && !matches!(seat.admitted, Admitted::Founding { .. })
        {
            return Err(NOT_THE_LINK);
        }
        Ok(!self.admission.has_person(seat.uid))
    }

    /// Whether this daemon serves every account of the computer, each with its own person.
    pub(crate) fn system(&self) -> bool {
        self.admission.system()
    }

    /// Record that the account `seat` made the person `person`: the first person a computer
    /// makes is the one it serves, and every account after it acts as a key of its own.
    pub(crate) fn founded_by(&self, seat: &Seat, person: [u8; 32]) -> Result<(), String> {
        self.admission.founded_by(seat.uid, person)?;
        // The key the account signed in with acts for it from now on, as its certificate comes.
        self.admission.founded(seat.uid, person)?;
        // The session that made them is theirs from now on: its next calls, and its link, reach
        // the person without a restart, though it is not certified yet (that still reaches this
        // device's own actions through `seats#certified`).
        if let Some(pid) = seat.pid
            && matches!(
                self.links.lock_recover().get(&(seat.uid, pid)),
                Some(SeatLink::Founding)
            )
        {
            let device = self.device.person() == Some(person);
            self.record_person(seat.uid, pid, person, device);
        }
        Ok(())
    }

    /// Record that `uid` joined the person `person` its computer now serves: the person the
    /// account made here for its setup is retired, and the seats the sign-in linked act as the
    /// joined person from now on.
    pub(crate) fn adopted_login(&self, uid: u32, person: [u8; 32]) {
        if let Err(error) = self.admission.adopted(uid, person) {
            tracing::warn!(%error, "this computer did not keep whose person its account joined");
        }
        self.keys.lock_recover().remove(&uid);
        for ((held, _), link) in self.links.lock_recover().iter_mut() {
            if *held == uid && matches!(link, SeatLink::Founding) {
                *link = SeatLink::Person {
                    person,
                    device: true,
                };
            }
        }
    }

    /// A one-use secret the sign-in of the account `uid` answers for its session's founding
    /// link: the process that redeems it may make a person or join one, and reaches nothing else
    /// of a person's until it does.
    pub(crate) fn founding_token(&self, uid: u32) -> [u8; 32] {
        let token: [u8; 32] = rand::random();
        self.founding.lock_recover().insert(token, uid);
        token
    }

    /// The account `token`, a founding one-use secret, was made for, without taking it.
    fn founding_holder(&self, token: &[u8; 32]) -> Option<u32> {
        self.founding.lock_recover().get(token).copied()
    }

    /// The account `token`, a founding one-use secret, was made for, once: the secret admits one
    /// founding link and no other.
    fn redeem_founding(&self, token: &[u8; 32]) -> Option<u32> {
        self.founding.lock_recover().remove(token)
    }

    /// Keep that the seat `(uid, pid)` redeems a founding link: the process this daemon admits
    /// as the account's session.
    fn record_founding(&self, uid: u32, pid: u32) {
        self.links
            .lock_recover()
            .insert((uid, pid), SeatLink::Founding);
    }

    /// Keep that the seat `(uid, pid)` acts for `person` from now on: the session its founding
    /// link made, through the endpoint of the person `device` names when this daemon serves them.
    fn record_person(&self, uid: u32, pid: u32, person: [u8; 32], device: bool) {
        self.links
            .lock_recover()
            .insert((uid, pid), SeatLink::Person { person, device });
    }

    /// The person the seat `(uid, pid)` acts for, when a founding link or a person's link was
    /// admitted for it here: the person its account certified once it made one, or the one its
    /// shell link became.
    pub(crate) fn seated_person(&self, uid: u32, pid: u32) -> Option<[u8; 32]> {
        match self.links.lock_recover().get(&(uid, pid)) {
            Some(SeatLink::Person { person, .. }) => Some(*person),
            // A founding session acts for the person it made once its certified key is theirs.
            Some(SeatLink::Founding) => self.admission.person_of(uid),
            None => None,
        }
    }

    /// Serve the person's shells with `shells` from now on: what a sign-in and a shell's start do.
    pub(crate) fn serve(&self, shells: Arc<dyn Shells>) {
        self.shells.send_if_modified(|served| {
            let first = matches!(served, Services::Starting);
            if first {
                *served = Services::Running(shells);
            }
            first
        });
    }

    /// Say that the person's services did not start, as `why` says: a link that waited for them is
    /// answered that, rather than waiting for a part that will not come. A daemon whose person's
    /// services ran once keeps them: a later part that fails does not take them away.
    pub(crate) fn did_not_start(&self, why: String) {
        self.shells.send_if_modified(|served| {
            let starting = matches!(served, Services::Starting);
            if starting {
                *served = Services::Failed(why);
            }
            starting
        });
    }

    /// Who the connection `stream` is. One whose account the kernel does not say is a stranger.
    pub(crate) fn seat(&self, stream: &Stream) -> Seat {
        let peer = match (self.peer_of)(stream) {
            Ok(peer) => peer,
            Err(error) => {
                tracing::debug!(%error, "a native connection's account is not known");
                return Seat {
                    uid: u32::MAX,
                    pid: None,
                    admitted: Admitted::Stranger,
                    app: None,
                    founding: false,
                };
            }
        };
        let mut admitted = self.admission.admit(peer.uid);
        // A process the daemon started, or one descending from it, is the app it runs, with that
        // app's grants and no more: never the account it runs as. Any other process of the
        // account is that account alone.
        let app = peer
            .pid
            .and_then(|pid| {
                let app_of = self
                    .app_of
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                app_of
                    .and_then(|app_of| app_of(pid))
                    .map(|(app, workspace)| (app, workspace, pid))
            })
            .filter(|(app, _, _)| !app.is_empty())
            .map(|(app, workspace, pid)| AppAt {
                app,
                workspace,
                pid,
            });
        // The session a sign-in answered a founding link is its own connection, never an app's
        // or another program's of the account: it reaches the device's actions about the person
        // it made or joined, and once it is that person, theirs.
        let mut founding = false;
        if let Some(pid) = peer.pid.filter(|_| app.is_none()) {
            let link = self.links.lock_recover().get(&(peer.uid, pid)).copied();
            match link {
                Some(SeatLink::Founding) => {
                    founding = true;
                    // Until the account acts as a person, its session reaches only what makes
                    // or joins one. Once its certified key or a login serves it, the account's
                    // own admission stands, and this process only keeps the device's setup
                    // actions.
                    if !admitted.served() {
                        let key = self
                            .admission
                            .founding(peer.uid)
                            .or_else(|| self.admission.awaiting(peer.uid).map(|(_, key)| key))
                            .unwrap_or_default();
                        admitted = Admitted::Founding { key };
                    }
                }
                Some(SeatLink::Person {
                    person,
                    device: true,
                }) => {
                    // The person's certified key, once it came, is what the account acts as;
                    // until then the session reaches the person it made through this link.
                    if !matches!(admitted, Admitted::Key { person: certified, .. } if certified == person)
                    {
                        admitted = Admitted::Person(person);
                    }
                    founding = true;
                }
                // Another person's link: its own admission (the key it certified) answers it.
                Some(SeatLink::Person { .. }) => {
                    founding = true;
                }
                None => {}
            }
        }
        Seat {
            uid: peer.uid,
            pid: peer.pid,
            admitted,
            app,
            founding,
        }
    }

    /// What the seats answer `request` with, when it is theirs: who the connection is
    /// (`account#who`), or a shell started or waited for (`shells#start`, `shells#wait`); `None`
    /// for any other request. `received` holds the descriptors the connection's client sent with
    /// its first line, which a shell's start hands the shell it starts.
    pub(crate) fn answer(
        &self,
        seat: &Seat,
        request: &Value,
        received: &Mutex<Vec<Descriptor>>,
        handed: &Hand,
    ) -> Option<Answer<'static>> {
        let ready = |answer: Value| Answer::InOrder(Box::pin(async move { Ok(Some(answer)) }));
        let refused = |why: String| ready(json!({"error": why}));
        match request.get("native").and_then(Value::as_str)? {
            WHO => Some(ready(self.who(seat))),
            START => Some(self.start(seat, request, received)),
            WAIT => Some(self.wait(seat, request).unwrap_or_else(refused)),
            LINK => Some(self.link(seat, request, handed)),
            PROCESSES => Some(ready(
                self.hand_processes(seat, request, received)
                    .map_or_else(|why| json!({"error": why}), |()| json!({"handed": true})),
            )),
            _ => None,
        }
    }

    /// Whether `request` is the seats' own, answered by [`Self::answer`].
    pub(crate) fn asks(request: &Value) -> bool {
        matches!(
            request.get("native").and_then(Value::as_str),
            Some(WHO | START | WAIT | LINK | PROCESSES)
        )
    }

    /// Whether the account `seat` starts its person's shell through this daemon: the account a
    /// person's own daemon runs as. The system's daemon starts none; root starts each session,
    /// with the link `seats#signed-in` answers.
    fn starts_shells(&self, seat: &Seat) -> Result<(), String> {
        if self.admission.system() {
            return Err("this computer starts its person's shell when they sign in".to_owned());
        }
        if seat.admitted != Admitted::Device {
            return Err("only the account this daemon runs as starts its shell".to_owned());
        }
        Ok(())
    }

    /// What the daemon does for its person once their services run, waiting for them to start:
    /// their shells, or the failure their start ended in. The wait is this request's alone, on
    /// the connection's own task, never the daemon's other callers.
    async fn shells(&self) -> Result<Arc<dyn Shells>, String> {
        let mut served = self.shells.subscribe();
        loop {
            let state = served.borrow_and_update().clone();
            match state {
                Services::Running(shells) => return Ok(shells),
                Services::Failed(why) => return Err(why),
                Services::Starting => {}
            }
            served
                .changed()
                .await
                .map_err(|_| "this daemon is stopping".to_owned())?;
        }
    }

    /// Start the person's shell as `request` says (`{args, env}`), once their services run:
    /// answered with its pid. The shell is handed its link and, when the client sent one with the
    /// request, the connection the asking session draws its window on (`WAYLAND_SOCKET`).
    fn start(
        &self,
        seat: &Seat,
        request: &Value,
        received: &Mutex<Vec<Descriptor>>,
    ) -> Answer<'static> {
        if let Err(why) = self.starts_shells(seat) {
            return Answer::InOrder(Box::pin(async move { Ok(Some(json!({"error": why}))) }));
        }
        let args: Vec<OsString> = request
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(OsString::from)
            .collect();
        let env: Vec<(OsString, OsString)> = request
            .get("env")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| Some((name.into(), value.as_str()?.into())))
            .collect();
        let window = received.lock_recover().pop();
        let seats = self.clone();
        Answer::InOrder(Box::pin(async move {
            let started = seats.shells().await.and_then(|shells| {
                shells
                    .start(args, env, window)
                    .map_err(|error| format!("the shell did not start: {error}"))
            });
            Ok(Some(match started {
                Ok(child) => {
                    let pid = child.id();
                    seats.started.lock_recover().insert(pid, child);
                    json!({"pid": pid})
                }
                Err(why) => json!({"error": why}),
            }))
        }))
    }

    /// The one request that establishes a shell link: `{token?}`, the one-use secret a daemon gave
    /// this shell the first time; without one, the seat a link was admitted for before. The link is
    /// answered as a descriptor, and kept for the seat it was made for. It waits for the person's
    /// services when they have not started, and answers why when their start failed.
    fn link(&self, seat: &Seat, request: &Value, handed: &Hand) -> Answer<'static> {
        let handed = Arc::clone(handed);
        let token = request
            .get("token")
            .and_then(Value::as_str)
            .and_then(|token| hex::decode(token).ok())
            .and_then(|token| <[u8; 32]>::try_from(token).ok());
        let seats = self.clone();
        let (uid, pid, admitted) = (seat.uid, seat.pid, seat.admitted);
        let is_app = seat.app.is_some();
        Answer::InOrder(Box::pin(async move {
            let refusal = |why: String| Ok(Some(json!({"error": why})));
            let Some(pid) = pid else {
                return refusal(
                    "this connection's process is not known, so no shell link can be tied to it"
                        .to_owned(),
                );
            };
            let seat_id = seat_id(uid, pid);
            // A founding link is the sign-in's, not a shell's: the session that redeems it is
            // tied to the account that signed in, and it reaches the device's own actions about
            // the person it makes or joins. An app is never that session, and never spends its
            // secret.
            if let Some(token) = token
                && let Some(holder) = seats.founding_holder(&token)
            {
                if is_app {
                    return refusal(
                        "an app is no session this computer signed in: it gets no founding link"
                            .to_owned(),
                    );
                }
                if holder != uid {
                    return refusal(
                        "this founding link was made for another account's sign-in".to_owned(),
                    );
                }
                let _ = seats.redeem_founding(&token);
                seats.record_founding(uid, pid);
                return Ok(Some(json!({"founding": true, "shell": false})));
            }
            // Whose shell this is: a token is the daemon's own word for it, and without one the
            // record of a link this daemon admitted for the same process before is. A token is
            // kept by the person's services, so a token waits for them before it is read; a
            // process with no record is no seat at all, and is refused at once, never after a
            // wait.
            let (person, shells) = if let Some(token) = token {
                let shells = match seats.shells().await {
                    Ok(shells) => shells,
                    Err(why) => return refusal(why),
                };
                let Some(holder) = shells.token_holder(&token) else {
                    return refusal(
                        "this token is no shell link the daemon made, or it was taken already"
                            .to_owned(),
                    );
                };
                (holder, shells)
            } else {
                let Some(person) = seats
                    .seated_person(uid, pid)
                    .or_else(|| seats.recorded_seat(uid, pid))
                else {
                    return refusal(
                        "this process is no seat a shell link was admitted for: ask for one with the token the daemon gave it"
                            .to_owned(),
                    );
                };
                // The link itself is the person's services' to make: it is answered once they
                // run, however long their start takes, and a start that failed answers why. The
                // wait is this request's alone, on the connection's own task.
                let shells = match seats.shells().await {
                    Ok(shells) => shells,
                    Err(why) => return refusal(why),
                };
                (person, shells)
            };
            if !Self::acts_as(&shells, admitted, person) {
                return refusal("this account does not act as the seat's person now".to_owned());
            }
            if shells.live(seat_id) {
                return refusal("this seat's shell is linked already".to_owned());
            }
            let made = match token {
                Some(token) => shells.redeem(token, seat_id),
                None => shells.link_for(person, seat_id),
            };
            let stream = match made {
                Ok(stream) => stream,
                Err(error) => return refusal(format!("no shell link: {error}")),
            };
            if let Err(error) = seats.remember_seat(uid, pid, person) {
                tracing::warn!(%error, "a seat's shell link is admitted but not kept");
            }
            seats.record_person(uid, pid, person, shells.serves(person));
            *handed.lock_recover() = Some(runtime_portal::packet::descriptor_of(stream));
            Ok(Some(json!({"shell": true, "pid": pid})))
        }))
    }

    /// Hand the device's tilolang processes the packet socket the connection sent with its
    /// request (`{secret}`), admitted by the account the kernel names for the connection's peer
    /// and by the one-use secret the daemon answered that account's shell on its link. The
    /// account comes from the kernel alone: nothing the request says names it, and a file's mode
    /// admits nothing here, since every account reaches this endpoint.
    fn hand_processes(
        &self,
        seat: &Seat,
        request: &Value,
        received: &Mutex<Vec<Descriptor>>,
    ) -> Result<(), String> {
        let mut received = std::mem::take(&mut *received.lock_recover());
        if seat.app.is_some() {
            return Err("an app is no shell: it hands the device's processes nothing".to_owned());
        }
        let secret = request
            .get("secret")
            .and_then(Value::as_str)
            .and_then(|secret| hex::decode(secret).ok())
            .and_then(|secret| <[u8; 32]>::try_from(secret).ok())
            .ok_or("a hand-over names the secret its shell's link was answered")?;
        let (Some(packets), true) = (received.pop(), received.is_empty()) else {
            return Err("a hand-over sends one packet socket with its request".to_owned());
        };
        let state = self.shells.borrow().clone();
        let shells = match state {
            Services::Running(shells) => shells,
            // A hand-over follows the link a shell's secret was answered, so it comes only once
            // the person's services run: a start that failed is answered why, and one still
            // under way serves no process yet.
            Services::Failed(why) => return Err(why),
            Services::Starting => {
                return Err("this daemon serves no person's processes yet".to_owned());
            }
        };
        shells.hand_processes(seat.uid, &secret, packets)
    }

    /// Whether a connection admitted as `admitted` acts as `person` now, as `shells` serves them:
    /// a link for a person is only handed to that person's account, or to the device itself,
    /// which makes the token for the person it serves.
    fn acts_as(shells: &Arc<dyn Shells>, admitted: Admitted, person: [u8; 32]) -> bool {
        match admitted {
            Admitted::Person(held) | Admitted::Key { person: held, .. } => held == person,
            // The daemon's own account, and root, are the device: it acts as the person its own
            // services run for, which is the person it made the token for.
            Admitted::Device => shells.serves(person),
            // A founding link reaches the person only once it is theirs, by their account's
            // certified key: the same connection asks again for its person's link then.
            Admitted::Founding { .. } | Admitted::Stranger => false,
        }
    }

    /// Where a seat's link is kept between this daemon and the next: the profile's `seats`.
    fn seat_record(&self, uid: u32, pid: u32) -> Option<std::path::PathBuf> {
        let profile = self.device.profile();
        (!profile.as_os_str().is_empty())
            .then(|| profile.join("seats").join(format!("{uid}-{pid}.json")))
    }

    /// Keep that this daemon admitted a shell link for `person` on the seat `(uid, pid)`, so the
    /// same process reaches them again after a restart.
    fn remember_seat(&self, uid: u32, pid: u32, person: [u8; 32]) -> Result<(), String> {
        let Some(path) = self.seat_record(uid, pid) else {
            return Ok(());
        };
        // A pid is reused; the process's launch time is not.
        let start = crate::apps::process_start_time(pid);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|error| format!("the seat's link cannot be kept: {error}"))?;
        }
        let record = json!({"uid": uid, "pid": pid, "start": start, "person": hex::encode(person)});
        tilo_files::native::atomic::write_private(&path, record.to_string().as_bytes())
            .map_err(|error| format!("the seat's link cannot be kept: {error}"))
    }

    /// Forget every seat of the account `uid`, which left: the links its processes were admitted
    /// for, here and kept for the next daemon, so none of them acts for a person any more.
    fn forget_seats(&self, uid: u32) {
        self.links
            .lock_recover()
            .retain(|(held, _), _| *held != uid);
        let Some(dir) = self
            .seat_record(uid, 0)
            .and_then(|path| path.parent().map(std::path::Path::to_owned))
        else {
            return;
        };
        let prefix = format!("{uid}-");
        let Ok(records) = std::fs::read_dir(&dir) else {
            return;
        };
        for record in records.flatten() {
            if record.file_name().to_string_lossy().starts_with(&prefix)
                && let Err(error) = std::fs::remove_file(record.path())
            {
                tracing::warn!(%error, "a seat of an account that left is still kept");
            }
        }
    }

    /// The person a shell link was admitted for on the seat `(uid, pid)`, when this daemon (or the
    /// one before it) admitted one and that process is still the same one.
    fn recorded_seat(&self, uid: u32, pid: u32) -> Option<[u8; 32]> {
        let path = self.seat_record(uid, pid)?;
        let bytes = std::fs::read(path).ok()?;
        let record: Value = serde_json::from_slice(&bytes).ok()?;
        let record = record.as_object()?;
        if record.get("uid").and_then(Value::as_u64) != Some(u64::from(uid))
            || record.get("pid").and_then(Value::as_u64) != Some(u64::from(pid))
        {
            return None;
        }
        let start = record.get("start").and_then(Value::as_u64);
        if crate::apps::process_start_time(pid) != start {
            return None;
        }
        hex::decode(record.get("person")?.as_str()?)
            .ok()
            .and_then(|person| <[u8; 32]>::try_from(person).ok())
    }

    /// Wait for the shell `request` names (`{pid}`) to end: how it ended.
    fn wait(&self, seat: &Seat, request: &Value) -> Result<Answer<'static>, String> {
        self.starts_shells(seat)?;
        let pid = request
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or("a wait names the shell by its pid")?;
        let mut child = self
            .started
            .lock_recover()
            .remove(&pid)
            .ok_or_else(|| format!("this daemon started no shell {pid}"))?;
        Ok(Answer::Overlapping(Box::pin(async move {
            let status = tokio::task::spawn_blocking(move || child.wait())
                .await
                .map_err(io::Error::other)??;
            #[cfg(unix)]
            let signal = std::os::unix::process::ExitStatusExt::signal(&status);
            // A Windows process ends with a code alone.
            #[cfg(windows)]
            let signal = None::<i32>;
            Ok(Some(json!({"code": status.code(), "signal": signal})))
        })))
    }

    /// The seat action `action` with `args`, as the account `seat` asks it: root's alone. A
    /// descriptor the answer carries is left in `handed`, to go with it.
    fn act(
        &self,
        seat: &Seat,
        action: &str,
        args: &Value,
        handed: &Mutex<Option<Descriptor>>,
    ) -> Result<Value, String> {
        if action == CERTIFIED {
            return self.certified(seat, args);
        }
        if seat.uid != ROOT {
            return Err(ROOT_ALONE.to_owned());
        }
        match action {
            SIGNED_IN => self.signed_in(seat, args, handed),
            LEFT => uid_in(args).and_then(|uid| {
                let was = self.admission.left(seat.uid, uid)?;
                self.keys.lock_recover().remove(&uid);
                self.forget_seats(uid);
                Ok(json!({"left": was}))
            }),
            other => Err(format!("seats has no action {other}")),
        }
    }

    /// Take the certificate `request` hands (`{cert}`) for the key the account `seat` signed in
    /// to act as: its own account alone hands it, and it must be its person's for that key. An
    /// account whose person this computer keeps the root of asks with none (`{}`), and the
    /// daemon signs it with that person's root: no root ever signs for another account.
    fn certified(&self, seat: &Seat, request: &Value) -> Result<Value, String> {
        let handed = request
            .get("cert")
            .and_then(Value::as_str)
            .and_then(|cert| hex::decode(cert).ok());
        let (ours, cert) = match handed {
            Some(bytes) => {
                let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&bytes)
                    .map_err(|error| format!("the certificate does not read: {error}"))?;
                (cert.device_public_key, cert)
            }
            None => self.certify_here(seat)?,
        };
        let key = self
            .keys
            .lock_recover()
            .get(&seat.uid)
            .map(|key| key.verifying_key().to_bytes());
        if key != Some(ours) {
            return Err(format!(
                "this computer made account {} no key that certificate names",
                seat.uid
            ));
        }
        let key = cert.device_public_key;
        self.admission.certified(seat.uid, cert)?;
        Ok(json!({"key": hex::encode(key)}))
    }

    /// The certificate of the key the account `seat` signed in to act as, signed here with the
    /// root this computer keeps for that account alone.
    fn certify_here(&self, seat: &Seat) -> Result<([u8; 32], DeviceCert), String> {
        let (person, key) = self
            .admission
            .awaiting(seat.uid)
            .ok_or("this account is to certify no key on this computer")?;
        let root = crate::device::services::root_for(seat.uid)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "this computer keeps no root of this person's: certify the key from a device of \
                 theirs that does"
                    .to_owned()
            })?;
        if root.group_public_key() != person {
            return Err(
                "the root this computer keeps is another person's than the account's".to_owned(),
            );
        }
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: key,
            device_label: tilo_user_identity::UNNAMED_SHARED_DEVICE.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root)
            .map_err(|error| format!("the certificate is not signed: {error}"))?;
        Ok((key, cert))
    }

    fn signed_in(
        &self,
        seat: &Seat,
        request: &Value,
        _handed: &Mutex<Option<Descriptor>>,
    ) -> Result<Value, String> {
        let uid = uid_in(request)?;
        let named = request
            .get("person")
            .and_then(Value::as_str)
            .and_then(|person| hex::decode(person).ok())
            .and_then(|person| <[u8; 32]>::try_from(person).ok());
        let recorded = self.admission.recorded_person(uid);
        if let Some(held) = recorded
            && let Some(named) = named
            && named != held.person
        {
            return Err(format!(
                "account {uid} made another person on this computer than the one it names"
            ));
        }
        // The person this account is: the one it made here, else the one it names.
        let held = if let Some(held) = recorded {
            held
        } else {
            let Some(named) = named else {
                // No identity yet: the account may make one here, then certify its key. Its
                // session is given the founding link that lets it do so and nothing more.
                let key = SigningKey::from_bytes(&rand::random());
                self.admission
                    .signed_in_founding(seat.uid, uid, key.verifying_key().to_bytes())?;
                self.keys.lock_recover().insert(uid, key);
                let token = self.founding_token(uid);
                return Ok(json!({
                    "founding": true,
                    "shell": true,
                    "token": hex::encode(token),
                }));
            };
            // The device's own person is the account recorded as having set them up here, never
            // one that only names them: naming them would act as them.
            if self.device.person() == Some(named) {
                return Err(format!(
                    "account {uid} is not the account this computer's person was set up from"
                ));
            }
            // A person whose identity another device of theirs holds: this computer records that
            // the account made that person, and the account acts as a key of theirs once their
            // root certified it.
            let held = tilo_api::daemon::admission::AccountPerson {
                person: named,
                device: false,
            };
            self.admission
                .founded_by_as(uid, held.person, held.device)?;
            held
        };
        // The account the computer was set up from is the person its own services serve.
        if !held.device {
            return self.another_person(seat, uid, held.person);
        }
        self.admission.signed_in(seat.uid, uid, held.person)?;
        self.keys.lock_recover().remove(&uid);
        let importing = request.get("importing").and_then(Value::as_bool) == Some(true);
        // The secret the session root starts redeems for its link: none while the person's
        // services have not started, and the session asks for its link itself, which is answered
        // once they run.
        let state = self.shells.borrow().clone();
        let served = match state {
            Services::Running(shells) => Some(shells),
            Services::Starting | Services::Failed(_) => None,
        };
        let token = served.and_then(|shells| {
            shells
                .token(importing)
                .inspect_err(|error| {
                    tracing::warn!(%error, "the person who signed in has no token for their shell");
                })
                .ok()
        });
        let shell = token.is_some();
        Ok(json!({
            "person": hex::encode(held.person),
            "shell": shell,
            "token": token.map(hex::encode),
        }))
    }

    /// The account `uid` signed in as `person`, another person than the device's: it acts as a
    /// key of theirs this daemon makes it now, once they certified it. Answers the key to certify.
    fn another_person(&self, seat: &Seat, uid: u32, person: [u8; 32]) -> Result<Value, String> {
        let key = self
            .keys
            .lock_recover()
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| SigningKey::from_bytes(&rand::random()));
        let public = key.verifying_key().to_bytes();
        self.admission
            .signed_in_as_key(seat.uid, uid, person, public)?;
        self.keys.lock_recover().insert(uid, key);
        Ok(json!({
            "person": hex::encode(person),
            "certify": hex::encode(public),
            "shell": false,
        }))
    }

    fn who(&self, seat: &Seat) -> Value {
        let person = match seat.admitted {
            Admitted::Person(person) | Admitted::Key { person, .. } => Some(person),
            // A person's own service is that person's; the system's daemon and root are none.
            Admitted::Device if !self.admission.system() => self.device.person(),
            Admitted::Device | Admitted::Founding { .. } | Admitted::Stranger => None,
        };
        // The person and the label only where there is one: a computer nobody set up is its
        // device alone.
        let mut who = serde_json::Map::new();
        if let Some(person) = person {
            who.insert("person".into(), json!(hex::encode(person)));
        }
        who.insert(
            "device".into(),
            json!(hex::encode(self.device.public_key())),
        );
        if let Some(label) = self.device.label() {
            who.insert("label".into(), json!(label));
        }
        who.insert("claimed".into(), json!(self.device.claimed()));
        // Whether this computer keeps the account's person's root, to that account alone: what a
        // backup and the recovery words are made from.
        if person.is_some() && seat.uid != u32::MAX {
            who.insert(
                "holds_root".into(),
                json!(crate::device::services::holds_a_root(seat.uid)),
            );
        }
        match seat.admitted {
            Admitted::Key { key, .. } => {
                who.insert("key".into(), json!(hex::encode(key)));
            }
            // The session a sign-in answered a founding link: it may make its person, and once
            // it has, certifies the key this computer made it.
            Admitted::Founding { key } => {
                if let Some((person, key)) = self.admission.awaiting(seat.uid) {
                    who.insert(
                        "certify".into(),
                        json!({"person": hex::encode(person), "key": hex::encode(key)}),
                    );
                } else {
                    who.insert("founding".into(), json!(true));
                    who.insert("key".into(), json!(hex::encode(key)));
                }
            }
            // An account that signed in as another person, and has yet to certify its key.
            Admitted::Stranger => {
                if let Some((person, key)) = self.admission.awaiting(seat.uid) {
                    who.insert(
                        "certify".into(),
                        json!({"person": hex::encode(person), "key": hex::encode(key)}),
                    );
                } else if let Some(key) = self.admission.founding(seat.uid) {
                    // An account that signed in with no person yet: it may make one here.
                    who.insert("founding".into(), json!(true));
                    who.insert("key".into(), json!(hex::encode(key)));
                }
            }
            // The account a person's own daemon runs as, whose computer has no person yet: it
            // may make one here.
            Admitted::Device if !self.admission.system() && self.device.person().is_none() => {
                who.insert("founding".into(), json!(true));
            }
            // An account that made its person here holds a key of theirs this device made, which
            // still waits for the certificate that makes the account act as it.
            Admitted::Device | Admitted::Person(_) => {
                if let Some((person, key)) = self.admission.awaiting(seat.uid) {
                    who.insert(
                        "certify".into(),
                        json!({"person": hex::encode(person), "key": hex::encode(key)}),
                    );
                }
            }
        }
        // Where this computer enters its person's workspaces, to the person's own account alone:
        // the account a person's own daemon runs as is that person's, and another person acts as
        // their key.
        let own_account = match seat.admitted {
            Admitted::Person(_) => true,
            Admitted::Device => !self.admission.system() && self.device.person().is_some(),
            Admitted::Key { .. } | Admitted::Founding { .. } | Admitted::Stranger => false,
        };
        if own_account && let Some(home) = crate::device::home() {
            who.insert("home".into(), tilo_api::daemon::who::home_answer(&home));
        }
        Value::Object(who)
    }
}

/// The number a seat is known by, to the daemon and to the socket that serves its link: the
/// account and the process, kernel-attested at both ends.
pub(crate) fn seat_id(uid: u32, pid: u32) -> u64 {
    (u64::from(uid) << 32) | u64::from(pid)
}

/// The account a seats request names.
fn uid_in(request: &Value) -> Result<u32, String> {
    request
        .get("uid")
        .and_then(Value::as_u64)
        .and_then(|uid| u32::try_from(uid).ok())
        .ok_or_else(|| "a seats request names the account by its uid".to_owned())
}

/// Where a descriptor an answer on one connection carries waits until it is written with it
/// ([`Handing`]).
pub(crate) type Hand = Arc<Mutex<Option<Descriptor>>>;

std::thread_local! {
    /// The hand of the connection whose session is being made on this thread ([`making_for`]).
    static MAKING_FOR: std::cell::RefCell<Option<Hand>> = const { std::cell::RefCell::new(None) };
}

/// `make` a session for the connection whose answers' descriptors wait in `hand`: an action that
/// answers a descriptor (a seat's link, a hosted terminal's stream) finds it there
/// ([`connection_hand`]). A session is made on its connection's own thread, before any of its
/// requests is answered.
pub(crate) fn making_for<T>(hand: Option<Hand>, make: impl FnOnce() -> T) -> T {
    let before = MAKING_FOR.with(|slot| slot.replace(hand));
    let session = make();
    MAKING_FOR.with(|slot| *slot.borrow_mut() = before);
    session
}

/// The hand of the connection whose session is being made now ([`making_for`]): `None` for a
/// session no native connection serves.
pub(crate) fn connection_hand() -> Option<Hand> {
    MAKING_FOR.with(|making| making.borrow().clone())
}

/// A connection's seat and the daemon's seats, as a native connection is served with them.
#[derive(Clone)]
pub(crate) struct Seated {
    pub(crate) seat: Seat,
    pub(crate) seats: Arc<Seats>,
    /// A descriptor an answer on this connection carries, until it is written with it.
    pub(crate) handed: Hand,
    /// The descriptors the client sent with its first line: a nested session's window
    /// connection, which a shell started on this connection is handed
    /// ([`Shells::start`]).
    pub(crate) received: Arc<Mutex<Vec<Descriptor>>>,
}

impl Seated {
    /// `seat`, served by `seats`.
    pub(crate) fn new(seat: Seat, seats: Arc<Seats>) -> Self {
        Self {
            seat,
            seats,
            handed: Arc::default(),
            received: Arc::default(),
        }
    }

    /// Keep `descriptors`, which the client sent with its first line, for the call that hands
    /// them on.
    pub(crate) fn receives(&self, descriptors: Vec<Descriptor>) {
        *self.received.lock_recover() = descriptors;
    }

    /// The seat action `action` with `args`, as this connection asks it.
    pub(crate) fn act(&self, action: &str, args: &Value) -> Result<Value, String> {
        self.seats.act(&self.seat, action, args, &self.handed)
    }

    /// What the seats answer `request` with on this connection, when it is theirs.
    pub(crate) fn answer(&self, request: &Value) -> Option<Answer<'static>> {
        self.seats
            .answer(&self.seat, request, &self.received, &self.handed)
    }
}

/// A connection's writer that sends the descriptor an answer carries ([`Seated::handed`]) with
/// the first bytes written after it: a seat action's, or any action's of the session the
/// connection is served ([`making_for`]).
pub(crate) struct Handing<'a> {
    pub(crate) stream: &'a Stream,
    pub(crate) handed: Option<&'a Mutex<Option<Descriptor>>>,
}

impl io::Write for Handing<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let fd = self
            .handed
            .filter(|_| !bytes.is_empty())
            .and_then(|handed| handed.lock_recover().take());
        match fd {
            #[cfg(unix)]
            Some(fd) => {
                runtime_portal::fd_transport::send_with_fd(self.stream, bytes, &fd)?;
                Ok(bytes.len())
            }
            // Only Unix sockets carry a descriptor: no action hands one on Windows.
            #[cfg(windows)]
            Some(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a Windows connection carries no descriptor",
            )),
            None => io::Write::write(&mut &*self.stream, bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut &*self.stream)
    }
}
