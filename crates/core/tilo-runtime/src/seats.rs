//! Who each connection to the native endpoint is, and the device's answers about it: which
//! account signed in as which person (the actions `seats#signed-in` and `seats#left`, root's
//! alone to call), and who a connection is (`{"native": "account#who"}`).
//!
//! A person's shell reaches the daemon only by a link the daemon made for it
//! ([`crate::device::services::Daemon::shell_link`]). Under the system unit root starts each
//! session, so `seats#signed-in` answers the link beside its reply, as a descriptor on the
//! connection, and root hands it to the session it starts. A daemon that runs as the person's own
//! account starts the shell itself when that account asks (`{"native": "shells#start"}`), and
//! answers when it ended (`{"native": "shells#wait"}`): the link passes from the daemon to the
//! shell and to no other process.
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
use std::time::Duration;

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
const SEATS: &str = "seats";
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
const CERTIFIED: &str = "certified";
/// Who this connection is: `{person?, device, label?, claimed}`.
const WHO: &str = "account#who";
/// Start the person's shell: `{args, env}`, the shell's arguments and environment, answered
/// `{pid}`.
const START: &str = "shells#start";
/// Wait for a shell the daemon started to end: `{pid}`, answered `{code?, signal?}` once it ended.
const WAIT: &str = "shells#wait";
/// How long a shell's start waits for the person's services, which the daemon starts after its
/// native endpoint answers: a sign-in as the computer boots asks before they run.
const SERVICES_START: Duration = Duration::from_secs(120);
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
        #[cfg(unix)]
        use std::os::fd::AsRawFd as _;
        #[cfg(unix)]
        let fd = stream.as_raw_fd();
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // Read with libc rather than rustix: the kernel says 0 for a peer in another pid
            // namespace, which is no pid this daemon can look up.
            let mut credentials = libc::ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            };
            let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
                .map_err(io::Error::other)?;
            // SAFETY: `credentials` is a writable `ucred` of `length` bytes.
            let read = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&raw mut credentials).cast(),
                    &raw mut length,
                )
            };
            if read != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                uid: credentials.uid,
                pid: u32::try_from(credentials.pid).ok().filter(|pid| *pid > 0),
            })
        }
        #[cfg(target_os = "macos")]
        {
            let (mut uid, mut gid) = (0, 0);
            // SAFETY: both are writable ids.
            if unsafe { libc::getpeereid(fd, &raw mut uid, &raw mut gid) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut pid: libc::pid_t = 0;
            let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::pid_t>())
                .map_err(io::Error::other)?;
            // SAFETY: `pid` is a writable `pid_t` of `length` bytes.
            let read = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_LOCAL,
                    libc::LOCAL_PEERPID,
                    (&raw mut pid).cast(),
                    &raw mut length,
                )
            };
            Ok(Self {
                uid,
                pid: u32::try_from(pid)
                    .ok()
                    .filter(|pid| *pid > 0)
                    .filter(|_| read == 0),
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
    pub(crate) admitted: Admitted,
    /// The app the process was started as, when it is one the daemon started.
    pub(crate) app: Option<AppAt>,
}

/// What the daemon does for the person its seats admit, once their services run.
pub(crate) trait Shells: Send + Sync {
    /// The person signed in on a seat: find them their workspace, unless their session hands
    /// over the ones their desktop kept (`importing`), and make the link their shell is admitted
    /// by.
    ///
    /// # Errors
    /// No link could be made.
    fn arrived(&self, importing: bool) -> io::Result<Stream>;

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

/// The daemon's seats: its admission, and the device it answers for.
#[derive(Clone)]
pub(crate) struct Seats {
    admission: &'static Admission,
    device: DeviceOnly,
    peer_of: PeerOf,
    /// What finds the app the daemon started a peer process as, once the daemon keeps its
    /// launch records.
    app_of: Arc<Mutex<Option<AppOf>>>,
    /// What the daemon does for its person once their services run: `None` until they do.
    shells: Arc<tokio::sync::watch::Sender<Option<Arc<dyn Shells>>>>,
    /// The shells this daemon started, by pid, until one is waited for.
    started: Arc<Mutex<HashMap<u32, Child>>>,
    /// The key each account signed in as another person than the device's acts as, by uid,
    /// until it left.
    keys: Arc<Mutex<HashMap<u32, SigningKey>>>,
}

impl Seats {
    pub(crate) fn new(admission: &'static Admission, device: DeviceOnly, peer_of: PeerOf) -> Self {
        Self {
            admission,
            device,
            peer_of,
            app_of: Arc::default(),
            shells: Arc::new(tokio::sync::watch::channel(None).0),
            started: Arc::default(),
            keys: Arc::default(),
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
    /// system unit any account signed in on the computer makes its own person; a person's own
    /// daemon makes one for its own account while the device has none. Root under the system
    /// unit, an account nobody signed in and a stranger to a person's own daemon make none.
    ///
    /// [`FOUND`]: tilo_device_identity::login::FOUND
    pub(crate) fn may_found(&self, seat: &Seat) -> Result<bool, &'static str> {
        const NOT_SIGNED_IN: &str =
            "only an account signed in on this computer starts a person on it";
        if seat.uid == u32::MAX {
            return Err(NOT_SIGNED_IN);
        }
        if !self.admission.system() {
            return if seat.admitted == Admitted::Device {
                Ok(!self.device.claimed())
            } else {
                Err(NOT_SIGNED_IN)
            };
        }
        if seat.uid == ROOT
            || seat.admitted != Admitted::Device && !self.admission.signed_in_now(seat.uid)
        {
            return Err(NOT_SIGNED_IN);
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
        self.admission.founded(seat.uid, person)
    }

    /// Serve the person's shells with `shells` from now on: what a sign-in and a shell's start do.
    pub(crate) fn serve(&self, shells: Arc<dyn Shells>) {
        self.shells.send_if_modified(|served| {
            let first = served.is_none();
            if first {
                *served = Some(shells);
            }
            first
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
                    admitted: Admitted::Stranger,
                    app: None,
                };
            }
        };
        let admitted = self.admission.admit(peer.uid);
        // A process the daemon started is the app it runs, with that app's grants and no more:
        // never the account it runs as. Any other process of the account is that account alone.
        let app = admitted
            .served()
            .then_some(peer.pid)
            .flatten()
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
        Seat {
            uid: peer.uid,
            admitted,
            app,
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
    ) -> Option<Answer<'static>> {
        let ready = |answer: Value| Answer::InOrder(Box::pin(async move { Ok(Some(answer)) }));
        let refused = |why: String| ready(json!({"error": why}));
        match request.get("native").and_then(Value::as_str)? {
            WHO => Some(ready(self.who(seat))),
            START => Some(self.start(seat, request, received)),
            WAIT => Some(self.wait(seat, request).unwrap_or_else(refused)),
            _ => None,
        }
    }

    /// Whether `request` is the seats' own, answered by [`Self::answer`].
    pub(crate) fn asks(request: &Value) -> bool {
        matches!(
            request.get("native").and_then(Value::as_str),
            Some(WHO | START | WAIT)
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

    /// What the daemon does for its person once their services run, waiting up to
    /// [`SERVICES_START`] for them to.
    async fn shells(&self) -> Result<Arc<dyn Shells>, String> {
        let mut served = self.shells.subscribe();
        let shells = tokio::time::timeout(SERVICES_START, served.wait_for(Option::is_some))
            .await
            .map_err(|_| "this daemon's person's services did not start".to_owned())?
            .map_err(|_| "this daemon is stopping".to_owned())?
            .clone();
        shells.ok_or_else(|| "this daemon's person's services did not start".to_owned())
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
        handed: &Mutex<Option<Descriptor>>,
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
                // No identity yet: the account may make one here, then certify its key.
                let key = SigningKey::from_bytes(&rand::random());
                self.admission
                    .signed_in_founding(seat.uid, uid, key.verifying_key().to_bytes())?;
                self.keys.lock_recover().insert(uid, key);
                return Ok(json!({"founding": true, "shell": false}));
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
        // The link the session root starts is admitted by: none while the person's services
        // still start, and that session then reaches nothing of theirs.
        let served = self.shells.borrow().clone();
        let link = served.and_then(|shells| {
            shells
                .arrived(importing)
                .inspect_err(|error| {
                    tracing::warn!(%error, "the person who signed in has no link for their shell");
                })
                .ok()
        });
        let shell = link.is_some();
        *handed.lock_recover() = link.map(runtime_portal::packet::descriptor_of);
        Ok(json!({"person": hex::encode(held.person), "shell": shell}))
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
            Admitted::Device | Admitted::Stranger => None,
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
            Admitted::Device | Admitted::Person(_) => {}
        }
        // Where this computer enters its person's workspaces, to the person's own account alone:
        // the account a person's own daemon runs as is that person's, and another person acts as
        // their key.
        let own_account = match seat.admitted {
            Admitted::Person(_) => true,
            Admitted::Device => !self.admission.system() && self.device.person().is_some(),
            Admitted::Key { .. } | Admitted::Stranger => false,
        };
        if own_account && let Some(home) = crate::device::home() {
            who.insert("home".into(), tilo_api::daemon::who::home_answer(&home));
        }
        Value::Object(who)
    }
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
        self.seats.answer(&self.seat, request, &self.received)
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
