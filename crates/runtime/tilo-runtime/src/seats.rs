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
//! The account a connection comes from is the kernel's account of its peer ([`peer_uid`]), and
//! the daemon's admission ([`tilo_api::daemon::admission`]) says what it acts as. Which person an
//! account is, root says when the account signs in, and the daemon never guesses it from the uid:
//! it keeps that person for the account until root says the account left. What root hands over
//! is read from the account's identity directory, so it comes with the person's local person
//! proof ([`tilo_identity::local_person`]), signed by that person for this device's key and this
//! account: an account cannot name a person it is not.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::Child;
use std::sync::{Arc, Mutex, OnceLock};

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
/// Say an account signed in: `{person, uid, proof, importing?}`, the person's key and its proof
/// as hex, and whether the account's session hands over what its desktop kept.
const SIGNED_IN: &str = "signed-in";
/// Say an account's session ended: `{uid}`.
const LEFT: &str = "left";
/// Who this connection is: `{person?, device, label?, claimed}`.
const WHO: &str = "account#who";
/// Start the person's shell: `{args, env}`, the shell's arguments and environment, answered
/// `{pid}`.
const START: &str = "shells#start";
/// Wait for a shell the daemon started to end: `{pid}`, answered `{code?, signal?}` once it ended.
const WAIT: &str = "shells#wait";
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
                ActionParam::required("person", ParamType::Text, "The person's key, as hex."),
                uid(),
                ActionParam::required(
                    "proof",
                    ParamType::Text,
                    "The account's local person proof, as hex.",
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
    ]
}

/// How a connection's account is read: the kernel's account of its peer, or a test's stand-in.
pub(crate) type UidOf = fn(&UnixStream) -> io::Result<u32>;

/// The account at the other end of `stream`, as the kernel says.
///
/// # Errors
/// The kernel does not say.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        Ok(rustix::net::sockopt::socket_peercred(stream)?.uid.as_raw())
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        use std::os::fd::AsRawFd as _;
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid writes the two ids it is handed and reads nothing else.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// One connection's account, and what it is admitted as.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seat {
    pub(crate) uid: u32,
    pub(crate) admitted: Admitted,
}

/// What the daemon does for the person its seats admit, once their services run.
pub(crate) trait Shells: Send + Sync {
    /// The person signed in on a seat: find them their workspace, unless their session hands
    /// over the ones their desktop kept (`importing`), and make the link their shell is admitted
    /// by.
    ///
    /// # Errors
    /// No link could be made.
    fn arrived(&self, importing: bool) -> io::Result<UnixStream>;

    /// Start the person's shell with `args` and `env`, handed its link.
    ///
    /// # Errors
    /// The shell did not start.
    fn start(&self, args: Vec<OsString>, env: Vec<(OsString, OsString)>) -> io::Result<Child>;
}

/// The daemon's seats: its admission, and the device it answers for.
#[derive(Clone)]
pub(crate) struct Seats {
    admission: &'static Admission,
    device: DeviceOnly,
    uid_of: UidOf,
    /// What the daemon does for its person once their services run.
    shells: Arc<OnceLock<Arc<dyn Shells>>>,
    /// The shells this daemon started, by pid, until one is waited for.
    started: Arc<Mutex<HashMap<u32, Child>>>,
}

impl Seats {
    pub(crate) fn new(admission: &'static Admission, device: DeviceOnly, uid_of: UidOf) -> Self {
        Self {
            admission,
            device,
            uid_of,
            shells: Arc::default(),
            started: Arc::default(),
        }
    }

    /// Serve the person's shells with `shells` from now on: what a sign-in and a shell's start do.
    pub(crate) fn serve(&self, shells: Arc<dyn Shells>) {
        let _ = self.shells.set(shells);
    }

    /// Who the connection `stream` is. One whose account the kernel does not say is a stranger.
    pub(crate) fn seat(&self, stream: &UnixStream) -> Seat {
        match (self.uid_of)(stream) {
            Ok(uid) => Seat {
                uid,
                admitted: self.admission.admit(uid),
            },
            Err(error) => {
                tracing::debug!(%error, "a native connection's account is not known");
                Seat {
                    uid: u32::MAX,
                    admitted: Admitted::Stranger,
                }
            }
        }
    }

    /// What the seats answer `request` with, when it is theirs: who the connection is
    /// (`account#who`), or a shell started or waited for (`shells#start`, `shells#wait`); `None`
    /// for any other request.
    pub(crate) fn answer(&self, seat: Seat, request: &Value) -> Option<Answer<'static>> {
        let ready = |answer: Value| Answer::InOrder(Box::pin(async move { Ok(Some(answer)) }));
        let refused = |why: String| ready(json!({"error": why}));
        match request.get("native").and_then(Value::as_str)? {
            WHO => Some(ready(self.who(seat))),
            START => Some(
                self.start(seat, request)
                    .map_or_else(refused, |pid| ready(json!({"pid": pid}))),
            ),
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
    fn starts_shells(&self, seat: Seat) -> Result<&Arc<dyn Shells>, String> {
        if self.admission.system() {
            return Err("this computer starts its person's shell when they sign in".to_owned());
        }
        if seat.admitted != Admitted::Device {
            return Err("only the account this daemon runs as starts its shell".to_owned());
        }
        self.shells
            .get()
            .ok_or_else(|| "this daemon's person's services are still starting".to_owned())
    }

    /// Start the person's shell as `request` says (`{args, env}`): its pid.
    fn start(&self, seat: Seat, request: &Value) -> Result<u32, String> {
        let shells = self.starts_shells(seat)?;
        let args = request
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(OsString::from)
            .collect();
        let env = request
            .get("env")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| Some((name.into(), value.as_str()?.into())))
            .collect();
        let child = shells
            .start(args, env)
            .map_err(|error| format!("the shell did not start: {error}"))?;
        let pid = child.id();
        self.started.lock_recover().insert(pid, child);
        Ok(pid)
    }

    /// Wait for the shell `request` names (`{pid}`) to end: how it ended.
    fn wait(&self, seat: Seat, request: &Value) -> Result<Answer<'static>, String> {
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
            let signal = std::os::unix::process::ExitStatusExt::signal(&status);
            Ok(Some(json!({"code": status.code(), "signal": signal})))
        })))
    }

    /// The seat action `action` with `args`, as the account `seat` asks it: root's alone. A
    /// descriptor the answer carries is left in `handed`, to go with it.
    fn act(
        &self,
        seat: Seat,
        action: &str,
        args: &Value,
        handed: &Mutex<Option<OwnedFd>>,
    ) -> Result<Value, String> {
        if seat.uid != ROOT {
            return Err(ROOT_ALONE.to_owned());
        }
        match action {
            SIGNED_IN => self.signed_in(seat, args, handed),
            LEFT => uid_in(args).and_then(|uid| {
                self.admission
                    .left(seat.uid, uid)
                    .map(|was| json!({"left": was}))
            }),
            other => Err(format!("seats has no action {other}")),
        }
    }

    fn signed_in(
        &self,
        seat: Seat,
        request: &Value,
        handed: &Mutex<Option<OwnedFd>>,
    ) -> Result<Value, String> {
        let uid = uid_in(request)?;
        let hex_in = |field: &str| {
            request
                .get(field)
                .and_then(Value::as_str)
                .and_then(|value| hex::decode(value).ok())
        };
        let person = hex_in("person")
            .and_then(|person| <[u8; 32]>::try_from(person).ok())
            .ok_or("a sign-in names the person's key, as hex")?;
        let proof =
            hex_in("proof").ok_or("a sign-in names the account's local person proof, as hex")?;
        self.person_proven(person, uid, &proof)?;
        self.admission.signed_in(seat.uid, uid, person)?;
        let importing = request.get("importing").and_then(Value::as_bool) == Some(true);
        // The link the session root starts is admitted by: none while the person's services
        // still start, and that session then reaches nothing of theirs.
        let link = self.shells.get().and_then(|shells| {
            shells
                .arrived(importing)
                .inspect_err(|error| {
                    tracing::warn!(%error, "the person who signed in has no link for their shell");
                })
                .ok()
        });
        let shell = link.is_some();
        *handed.lock_recover() = link.map(OwnedFd::from);
        Ok(json!({"person": hex::encode(person), "shell": shell}))
    }

    /// Whether `person` is the one this device serves, and `proof` theirs for the account `uid`.
    fn person_proven(&self, person: [u8; 32], uid: u32, proof: &[u8]) -> Result<(), String> {
        let (bound, device) = tilo_identity::local_person::device_binding(self.device.keys())
            .map_err(|error| format!("this device is bound to no person: {error}"))?;
        if device != self.device.public_key() {
            return Err("this device's certificate names another key than its own".to_owned());
        }
        if person != bound {
            return Err("this device serves another person".to_owned());
        }
        if !tilo_identity::local_person::proves(&person, &device, uid, proof) {
            return Err(format!(
                "account {uid} holds no proof that it is this person"
            ));
        }
        Ok(())
    }

    fn who(&self, seat: Seat) -> Value {
        let person = match seat.admitted {
            Admitted::Person(person) => Some(person),
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
        // Where the device enters its person's workspaces, to the person's own accounts alone.
        if person.is_some()
            && let Some(home) = crate::device::home()
        {
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

/// A connection's seat and the daemon's seats, as a native connection is served with them.
#[derive(Clone)]
pub(crate) struct Seated {
    pub(crate) seat: Seat,
    pub(crate) seats: Arc<Seats>,
    /// A descriptor an answer on this connection carries, until it is written with it.
    pub(crate) handed: Arc<Mutex<Option<OwnedFd>>>,
}

impl Seated {
    /// `seat`, served by `seats`.
    pub(crate) fn new(seat: Seat, seats: Arc<Seats>) -> Self {
        Self {
            seat,
            seats,
            handed: Arc::default(),
        }
    }

    /// The seat action `action` with `args`, as this connection asks it.
    pub(crate) fn act(&self, action: &str, args: &Value) -> Result<Value, String> {
        self.seats.act(self.seat, action, args, &self.handed)
    }

    /// What the seats answer `request` with on this connection, when it is theirs.
    pub(crate) fn answer(&self, request: &Value) -> Option<Answer<'static>> {
        self.seats.answer(self.seat, request)
    }
}

/// A connection's writer that sends the descriptor an answer carries ([`Seated::handed`]) with
/// the first bytes written after it.
pub(crate) struct Handing<'a> {
    pub(crate) stream: &'a UnixStream,
    pub(crate) handed: Option<&'a Mutex<Option<OwnedFd>>>,
}

impl io::Write for Handing<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let fd = self
            .handed
            .filter(|_| !bytes.is_empty())
            .and_then(|handed| handed.lock_recover().take());
        match fd {
            Some(fd) => {
                runtime_portal::fd_transport::send_with_fd(self.stream, bytes, &fd)?;
                Ok(bytes.len())
            }
            None => io::Write::write(&mut &*self.stream, bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut &*self.stream)
    }
}
