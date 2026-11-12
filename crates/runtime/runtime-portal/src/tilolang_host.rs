//! The tilolang host's door: where the session hands a tilolang host each app
//! instance it runs.
//!
//! A tilolang host is one process per room and author that runs the tilolang
//! apps of that author in that room, each as an instance of its own
//! (`docs/designs/app-language/ui-host.md`). It has no app identity of its own:
//! everything an instance is arrives over this door, from the session, which
//! made it as that app's launch:
//!
//! 1. the instance's broker connection, which the session attested against a
//!    launch record of the instance carrying the app's creation record and
//!    launch ticket, exactly as for a launched app;
//! 2. its identity-service connection;
//! 3. its verified module, in a sealed memfd ([`Module`]);
//! 4. its window: the patch connection to the window's process, which shows
//!    what the instance sends and reports what the person did;
//! 5. for an app whose types hold an outside service's resources, its connection
//!    to the app's origin connector (`crate::origin_connector`).
//!
//! A message is one `SOCK_SEQPACKET` packet in the framing of `jobs`: the verb
//! [`INSTANCE_VERB`] and the request ([`InstanceRequest`], JSON), with the four
//! descriptors in that order.
//!
//! A host whose author is an agent task runs no app instance: the session hands it
//! the task instead ([`TASK_VERB`], [`TaskRequest`]), with the task's program
//! connection to the agent door and every app its programs are compiled against:
//! a tilolang app's verified module, and a native app's signed surface
//! (`docs/designs/app-language/agents.md`, "How a program runs"). The host never names an app to the session: the
//! broker and the compositor attribute each connection from the session's own
//! records. An instance ends when its window's connection closes.

use std::io;

use serde::{Deserialize, Serialize};
use tilo_formats::RegionFormats;

pub use crate::windows::Descriptor;

/// The variable the host reads its door's descriptor from.
pub const DOOR_FD_ENV: &str = "TILO_SVC_TILOLANG_HOST_FD";

/// The variable naming the host's storage: the one directory of its room and
/// author, keyed by package inside.
pub const STORAGE_ENV: &str = "TILO_TILOLANG_HOST_STORAGE";

/// The variable the launcher sets for a tilolang app's window whose instance a tilolang host
/// runs: the window presents what the instance sends over the patch connection its window door
/// carries (`crate::windows::PATCH_VERB`), and holds none of the app's data.
pub const HOSTED_ENV: &str = "TILO_TILOLANG_HOSTED";

pub const INSTANCE_VERB: &[u8] = b"instance";

/// The verb of the person's formats, as the session hands them to a host whose instances are
/// already running: which clock their dates and times read by changed in Settings. The request is
/// the [`RegionFormats`], JSON; the door carries no descriptor with it.
pub const FORMATS_VERB: &[u8] = b"formats";

/// The verb of a task handed to a task's host.
pub const TASK_VERB: &[u8] = b"task";

/// The verb the host says on its door when an instance ended (its window closed, or it never
/// opened): the window's instance, in decimal. The session stops a host once its last instance
/// ended; the host names no app.
pub const ENDED_VERB: &[u8] = b"ended";

/// The verb the host says on its door last, as it exits: why, in words its supervisor logs and
/// shows. A host gone without saying it ended unheard (killed, or crashed).
pub const EXITING_VERB: &[u8] = b"exiting";

/// What a host says on its door.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromHost {
    /// The instance of this window ended (its window closed, or it never opened).
    Ended(u64),
    /// The host exits, for this reason.
    Exiting(String),
}

/// The largest request: a few names and numbers.
pub const MAX_MESSAGE: usize = 4096;

/// The largest module the door carries.
pub const MAX_MODULE: u64 = 64 << 20;

/// The session's request to run one more app instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRequest {
    /// The app's package id, as the session launched it.
    pub app: String,
    /// The room the instance runs in.
    pub room: String,
    /// The window's instance: the durable id its tab keeps.
    pub instance: u64,
    /// The participant at the window, whose steps the instance's undo takes
    /// back.
    pub person: String,
    /// The length of the module in its memfd.
    pub module_len: u64,
    /// The id whose room record the instance opens, when it is not the app's own: an app renamed
    /// keeps using the record of its earliest id, as the installed catalogue believes the same
    /// publisher signed it. The broker admits the instance's connection to that record only on
    /// the same belief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_under: Option<String>,
    /// The instance applies one change to the record at its origin instead of running a
    /// window: an agent task's invocation the broker admitted (`docs/designs/app-language/agents.md`,
    /// "Reading records outside their app"). The descriptor in the window's place is the change's
    /// connection: the session writes the [`Change`] on it as one JSON line, and the host runs the
    /// action's code against the record as it is then, applies what it wrote as the invoker's
    /// step and answers on it with one JSON line. No window and no instance run. `instance` is
    /// then the change's own id, never a window's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub applies_change: bool,
    /// The person's formats (`locale` in their settings): the clock a time of day reads by and the
    /// style a date does, for what the instance writes in a sentence (`t.clock()`, a change's
    /// before and after) and its window shows. A host without them answers a 24-hour clock.
    #[serde(default)]
    pub formats: RegionFormats,
    /// The person's language (`de-DE`), the one their windows read in: the choice in their
    /// settings, else the locale their device runs in, as their session resolved it. Each window
    /// speaks it where its package ships it, and the package's source language otherwise
    /// (`tilo_history::window_language`), so the host resolves every word the window paints in the
    /// window's language rather than in its own process's. A session that names none (`""`) lets
    /// the host's own environment speak.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    /// The record the window was asked to open, by its id, when its launch named one: the
    /// instance opens only the record its route names (the workspace's, or the person's own), so
    /// a window asked for another is refused rather than shown that one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// The item the window opens on, by its anchor (a search hit's, an agent's `open`, the
    /// conversation a send landed in): the instance shows it as its `open` would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<String>,
    /// The tilolang apps installed here, by id, each with the id its records are kept under: a
    /// package the app uses that is one of them is another app, whose records the instance reads
    /// from that app's own record and whose changes it asks of that app's door. A package it uses
    /// that is not installed as an app keeps its records in the app's own.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub installed: std::collections::BTreeMap<String, String>,
}

/// An action to run against a record at its origin, as the broker admitted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The action's subject resource and the action, as the app publishes them.
    pub resource: String,
    pub action: String,
    /// The arguments, by parameter name, as the caller gave them.
    #[serde(default)]
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// Who invokes (`tilo_actions::ActionInvoker`), as the broker admitted them: an agent acting
    /// for its person.
    pub invoker: serde_json::Value,
    /// The run the change is a step of (`tilo_actions::ActionContext`), as the broker set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// What the task's preview planned, when it planned: the applied result is compared with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<serde_json::Value>,
}

/// The session's request to run an agent task's programs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRequest {
    /// The task: the agent run whose programs the host runs, their code's author.
    pub task: String,
    /// The room the task works in.
    pub room: String,
    /// The person the task acts for, as its programs' `me`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// The length of the apps in their memfd ([`encode_apps`]).
    pub apps_len: u64,
    /// The id whose records an app keeps, by the app's id, for each app that keeps another's:
    /// an app renamed keeps using the record of its earliest id
    /// ([`InstanceRequest::records_under`]).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub records_under: std::collections::BTreeMap<String, String>,
    /// The apps whose records the session changes itself, the packages it runs: a program's
    /// change of one goes to its record. A change of any other app is the app's own action,
    /// invoked where the app runs.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub changed_at_record: std::collections::BTreeSet<String>,
}

/// The descriptors a task arrives with, in the order the door carries them.
#[derive(Debug)]
pub struct TaskFds {
    /// The program connection to the agent door: the task's programs come in on it, and what
    /// they read and invoke goes out on it, as the task.
    pub program: Descriptor,
    /// Every app the task's programs may name, in a sealed memfd ([`encode_apps`]).
    pub apps: Descriptor,
}

/// What the session hands a host.
#[derive(Debug)]
pub enum Handed {
    Instance(InstanceRequest, InstanceFds),
    Task(TaskRequest, TaskFds),
    /// The person's formats, which their instance's clock and dates read by from now on.
    Formats(RegionFormats),
}

const APPS_MAGIC: &[u8; 8] = b"tlappsl1";

/// One app a task's programs may name, as the session hands it to the task's host: a tilolang
/// app's compiled module, or a native app's published surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum App {
    /// A tilolang app the session runs: its compiled module, its package's description embedded
    /// (`app.wasm`, `app.ids`, `imports/*.wasm`; [`Module`]).
    Module(Module),
    /// A native app: its package id, the name people call it, and the bytes of its signed
    /// `surface.toml` (the session reads them with `app_catalogue::apps::signed_surfaces`).
    Surface {
        app: String,
        title: String,
        surface: String,
    },
}

/// `apps` as the bytes a task's memfd holds.
#[must_use]
pub fn encode_apps(apps: &[App]) -> Vec<u8> {
    let mut out = APPS_MAGIC.to_vec();
    for app in apps {
        let encoded = app.encode();
        out.extend_from_slice(&(encoded.len() as u64).to_le_bytes());
        out.extend_from_slice(&encoded);
    }
    out
}

/// The apps `bytes` hold.
pub fn decode_apps(bytes: &[u8]) -> io::Result<Vec<App>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not a list of tilolang apps");
    let mut rest = bytes.strip_prefix(APPS_MAGIC).ok_or_else(invalid)?;
    let mut apps = Vec::new();
    while !rest.is_empty() {
        let (len, after) = rest.split_at_checked(8).ok_or_else(invalid)?;
        let len = u64::from_le_bytes(len.try_into().map_err(|_| invalid())?);
        let len = usize::try_from(len).map_err(|_| invalid())?;
        let (app, after) = after.split_at_checked(len).ok_or_else(invalid)?;
        apps.push(App::decode(app)?);
        rest = after;
    }
    Ok(apps)
}

/// What one instance runs: the package's compiled module, its ids lock and the
/// compiled modules of the packages it uses, as the package's payload holds them
/// (`app.wasm`, `app.ids`, `imports/*.wasm`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub wasm: Vec<u8>,
    pub ids: String,
    pub imports: Vec<Vec<u8>>,
}

const MODULE_MAGIC: &[u8; 8] = b"tlmodul1";

impl Module {
    /// The module of the package whose payload is `payload`.
    pub fn read(payload: &std::path::Path) -> io::Result<Self> {
        let wasm = std::fs::read(payload.join("app.wasm"))?;
        let ids = match std::fs::read_to_string(payload.join("app.ids")) {
            Ok(ids) => ids,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let mut paths: Vec<std::path::PathBuf> = match std::fs::read_dir(payload.join("imports")) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "wasm"))
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        paths.sort();
        let imports = paths
            .iter()
            .map(std::fs::read)
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self { wasm, ids, imports })
    }

    /// The module as the bytes its memfd holds.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            self.wasm
                .len()
                .saturating_add(self.ids.len())
                .saturating_add(64),
        );
        out.extend_from_slice(MODULE_MAGIC);
        let mut field = |bytes: &[u8]| {
            out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(bytes);
        };
        field(&self.wasm);
        field(self.ids.as_bytes());
        for import in &self.imports {
            field(import);
        }
        out
    }

    /// The module `bytes` hold.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not a tilolang module");
        let mut rest = bytes.strip_prefix(MODULE_MAGIC).ok_or_else(invalid)?;
        let mut fields = Vec::new();
        while !rest.is_empty() {
            let (len, after) = rest.split_at_checked(8).ok_or_else(invalid)?;
            let len = u64::from_le_bytes(len.try_into().map_err(|_| invalid())?);
            let len = usize::try_from(len).map_err(|_| invalid())?;
            let (field, after) = after.split_at_checked(len).ok_or_else(invalid)?;
            fields.push(field.to_vec());
            rest = after;
        }
        let mut fields = fields.into_iter();
        let wasm = fields.next().ok_or_else(invalid)?;
        let ids = String::from_utf8(fields.next().ok_or_else(invalid)?).map_err(|_| invalid())?;
        Ok(Self {
            wasm,
            ids,
            imports: fields.collect(),
        })
    }
}

impl App {
    /// The app as the bytes the task's memfd holds: a tag, then its fields, each with its
    /// length.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        fn field(out: &mut Vec<u8>, bytes: &[u8]) {
            out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        let mut out = Vec::new();
        match self {
            Self::Module(module) => {
                out.push(b'm');
                field(&mut out, &module.encode());
            }
            Self::Surface {
                app,
                title,
                surface,
            } => {
                out.push(b's');
                field(&mut out, app.as_bytes());
                field(&mut out, title.as_bytes());
                field(&mut out, surface.as_bytes());
            }
        }
        out
    }

    /// The app `bytes` hold.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not a tilolang app");
        let (tag, mut rest) = bytes.split_first().ok_or_else(invalid)?;
        let mut fields = Vec::new();
        while !rest.is_empty() {
            let (len, after) = rest.split_at_checked(8).ok_or_else(invalid)?;
            let len = u64::from_le_bytes(len.try_into().map_err(|_| invalid())?);
            let len = usize::try_from(len).map_err(|_| invalid())?;
            let (field, after) = after.split_at_checked(len).ok_or_else(invalid)?;
            fields.push(field.to_vec());
            rest = after;
        }
        let mut fields = fields.into_iter();
        match tag {
            b'm' => {
                let encoded = fields.next().ok_or_else(invalid)?;
                if fields.next().is_some() {
                    return Err(invalid());
                }
                Ok(Self::Module(Module::decode(&encoded)?))
            }
            b's' => {
                let mut text =
                    || String::from_utf8(fields.next().ok_or_else(invalid)?).map_err(|_| invalid());
                let app = text()?;
                let title = text()?;
                let surface = text()?;
                if fields.next().is_some() {
                    return Err(invalid());
                }
                Ok(Self::Surface {
                    app,
                    title,
                    surface,
                })
            }
            _ => Err(invalid()),
        }
    }
}

/// The descriptors one instance arrives with, in the order the door carries
/// them.
#[derive(Debug)]
pub struct InstanceFds {
    pub broker: Descriptor,
    pub identity: Descriptor,
    pub module: Descriptor,
    pub window: Descriptor,
    /// The connection to the app's origin connector, for an app with one.
    pub origin: Option<Descriptor>,
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "only the Linux shell runs tilolang hosts",
    )
}

/// A connected pair of door ends: the session keeps one and hands the other
/// to the host it starts.
///
/// # Errors
/// The socket error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn pair() -> io::Result<(Descriptor, Descriptor)> {
    crate::windows::pair()
}

/// The door the session handed this process at its start, once.
#[must_use]
pub fn take_door() -> Option<Descriptor> {
    #[cfg(target_os = "linux")]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let raw = std::env::var(DOOR_FD_ENV).ok()?;
        let Ok(fd) = raw.parse::<i32>() else {
            eprintln!(
                "tilo-portal: the session's door descriptor {raw:?} is not a number; this host has no door"
            );
            return None;
        };
        if fd < 0 || TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the session handed this descriptor to the process for the
        // door alone, and the flag above lets it be owned exactly once.
        Some(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Hand the host behind `door` the instance `request` with its descriptors.
///
/// # Errors
/// The socket's error, a request that does not encode, or
/// [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_instance(
    door: &Descriptor,
    request: &InstanceRequest,
    fds: &InstanceFds,
) -> io::Result<()> {
    let encoded = serde_json::to_vec(request).map_err(io::Error::other)?;
    if encoded.len() > MAX_MESSAGE {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        crate::door::send(
            door.as_fd(),
            &[INSTANCE_VERB, &encoded],
            &[
                fds.broker.as_fd(),
                fds.identity.as_fd(),
                fds.module.as_fd(),
                fds.window.as_fd(),
            ]
            .into_iter()
            .chain(fds.origin.as_ref().map(std::os::fd::AsFd::as_fd))
            .collect::<Vec<_>>(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, fds);
        Err(unsupported())
    }
}

/// Say on the host behind `door` that the person's formats changed: the clock and the date style
/// their instances write in a sentence, and their windows show, from now on.
///
/// # Errors
/// The socket's error, a request that does not encode, or [`io::ErrorKind::Unsupported`] off
/// Linux.
pub fn send_formats(door: &Descriptor, formats: &RegionFormats) -> io::Result<()> {
    let encoded = serde_json::to_vec(formats).map_err(io::Error::other)?;
    if encoded.len() > MAX_MESSAGE {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        crate::door::send(door.as_fd(), &[FORMATS_VERB, &encoded], &[])
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(unsupported())
    }
}

/// Hand the host behind `door` the task `request`: its program connection and the apps its
/// programs are compiled against.
///
/// # Errors
/// The socket's error, a request that does not encode, or
/// [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_task(door: &Descriptor, request: &TaskRequest, fds: &TaskFds) -> io::Result<()> {
    let encoded = serde_json::to_vec(request).map_err(io::Error::other)?;
    if encoded.len() > MAX_MESSAGE {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        crate::door::send(
            door.as_fd(),
            &[TASK_VERB, &encoded],
            &[fds.program.as_fd(), fds.apps.as_fd()],
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, fds);
        Err(unsupported())
    }
}

/// The next instance on `door` and its descriptors; `None` at end of file,
/// when the session closed its end.
///
/// # Errors
/// The socket's error; a message that is no instance request or does not
/// carry exactly its four descriptors.
pub fn receive_instance(door: &Descriptor) -> io::Result<Option<(InstanceRequest, InstanceFds)>> {
    match receive(door)? {
        None => Ok(None),
        Some(Handed::Instance(request, fds)) => Ok(Some((request, fds))),
        Some(Handed::Task(..) | Handed::Formats(..)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the door carried something other than an instance",
        )),
    }
}

/// The next instance or task on `door`, with its descriptors; `None` at end of
/// file, when the session closed its end.
///
/// # Errors
/// The socket's error; a message that is neither, or does not carry exactly
/// its descriptors.
pub fn receive(door: &Descriptor) -> io::Result<Option<Handed>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        // The verb, its separator and the request.
        let mut buffer = vec![
            0_u8;
            MAX_MESSAGE
                .saturating_add(INSTANCE_VERB.len())
                .saturating_add(1)
        ];
        let Some((length, fds)) = crate::door::receive(door.as_fd(), &mut buffer)? else {
            return Ok(None);
        };
        let message = buffer.get(..length).unwrap_or_default();
        let [verb, request] = crate::door::decode(message)[..] else {
            return Err(invalid("the door carried something other than an instance"));
        };
        if verb == TASK_VERB {
            let request: TaskRequest = serde_json::from_slice(request)
                .map_err(|_| invalid("a task request that does not decode"))?;
            let [program, apps] = <[Descriptor; 2]>::try_from(fds)
                .map_err(|_| invalid("a task without its two connections"))?;
            if request.apps_len > MAX_MODULE {
                return Err(invalid("apps over the door's limit"));
            }
            return Ok(Some(Handed::Task(request, TaskFds { program, apps })));
        }
        if verb == FORMATS_VERB {
            let formats: RegionFormats = serde_json::from_slice(request)
                .map_err(|_| invalid("formats that do not decode"))?;
            if !fds.is_empty() {
                return Err(invalid("formats carrying a connection"));
            }
            return Ok(Some(Handed::Formats(formats)));
        }
        if verb != INSTANCE_VERB {
            return Err(invalid("the door carried something other than an instance"));
        }
        let request: InstanceRequest = serde_json::from_slice(request)
            .map_err(|_| invalid("an instance request that does not decode"))?;
        let mut fds = fds.into_iter();
        let (Some(broker), Some(identity), Some(module), Some(window)) =
            (fds.next(), fds.next(), fds.next(), fds.next())
        else {
            return Err(invalid("an instance without its four connections"));
        };
        let origin = fds.next();
        if fds.next().is_some() {
            return Err(invalid("an instance with more connections than it has"));
        }
        if request.module_len > MAX_MODULE {
            return Err(invalid("a module over the door's limit"));
        }
        Ok(Some(Handed::Instance(
            request,
            InstanceFds {
                broker,
                identity,
                module,
                window,
                origin,
            },
        )))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(unsupported())
    }
}

/// Say on the host's end of `door` that the instance of the window `instance` ended.
///
/// # Errors
/// The socket's error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_ended(door: &Descriptor, instance: u64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let instance = instance.to_string();
        crate::door::send(door.as_fd(), &[ENDED_VERB, instance.as_bytes()], &[])
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, instance);
        Err(unsupported())
    }
}

/// Say on the host's end of `door`, last, why the host exits.
///
/// # Errors
/// The socket's error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_exiting(door: &Descriptor, reason: &str) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        // Cut on a character boundary to what the door carries.
        let room = MAX_MESSAGE.saturating_sub(EXITING_VERB.len().saturating_add(1));
        let end = (0..=reason.len().min(room))
            .rev()
            .find(|end| reason.is_char_boundary(*end))
            .unwrap_or(0);
        // A NUL separates the door's fields.
        let reason = reason.get(..end).unwrap_or_default().replace('\0', " ");
        crate::door::send(door.as_fd(), &[EXITING_VERB, reason.as_bytes()], &[])
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, reason);
        Err(unsupported())
    }
}

/// The next thing the host behind the session's `door` says; `None` at end of file, when the
/// host is gone.
pub fn receive_from_host(door: &Descriptor) -> io::Result<Option<FromHost>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        let mut buffer = [0_u8; MAX_MESSAGE];
        let Some((length, _fds)) = crate::door::receive(door.as_fd(), &mut buffer)? else {
            return Ok(None);
        };
        let message = buffer.get(..length).unwrap_or_default();
        match crate::door::decode(message)[..] {
            [verb, instance] if verb == ENDED_VERB => std::str::from_utf8(instance)
                .ok()
                .and_then(|instance| instance.parse().ok())
                .map(|instance| Some(FromHost::Ended(instance)))
                .ok_or_else(|| invalid("an ended instance that is no instance")),
            [verb, reason] if verb == EXITING_VERB => std::str::from_utf8(reason)
                .map(|reason| Some(FromHost::Exiting(reason.to_owned())))
                .map_err(|_| invalid("an exit whose reason is no text")),
            _ => Err(invalid(
                "the host said something other than an ended instance or its exit",
            )),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(unsupported())
    }
}

#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use super::*;

    fn connection() -> Descriptor {
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.into()
    }

    #[test]
    fn an_instance_crosses_the_door_with_its_four_connections() {
        let (shell, host) = pair().unwrap();
        let request = InstanceRequest {
            app: "so.tilo.lang.notes".into(),
            room: "7a110d00-0000-4000-8000-000000000001".into(),
            instance: 7,
            person: "ada".into(),
            module_len: 12,
            records_under: Some("so.tilo.lang.older-notes".into()),
            applies_change: true,
            formats: RegionFormats {
                time: tilo_formats::TimeStyle::H12,
                ..RegionFormats::default()
            },
            language: "de-DE".into(),
            record: Some("uuid:7a110d00-0000-4000-8000-000000000009".into()),
            reveal: Some("tl.so.tilo.lang.notes.Note:7".into()),
            installed: [("so.tilo.lang.tasks".into(), "so.tilo.lang.todo".into())].into(),
        };
        let fds = InstanceFds {
            broker: connection(),
            identity: connection(),
            module: connection(),
            window: connection(),
            origin: None,
        };
        send_instance(&shell, &request, &fds).unwrap();
        let (received, received_fds) = receive_instance(&host).unwrap().unwrap();
        assert_eq!(received, request);
        assert!(
            received_fds.origin.is_none(),
            "an app without an origin has no connection"
        );
        // An app with an origin brings its connection to the connector as the fifth.
        let fds = InstanceFds {
            origin: Some(connection()),
            ..fds
        };
        send_instance(&shell, &request, &fds).unwrap();
        let (_, received_fds) = receive_instance(&host).unwrap().unwrap();
        assert!(received_fds.origin.is_some());
        drop(shell);
        assert!(
            receive_instance(&host).unwrap().is_none(),
            "closed door ends"
        );
    }

    #[test]
    fn the_persons_formats_cross_the_door_between_instances() {
        let (shell, host) = pair().unwrap();
        let formats = RegionFormats {
            date: tilo_formats::DateStyle::Iso,
            time: tilo_formats::TimeStyle::H12,
            ..RegionFormats::default()
        };
        send_formats(&shell, &formats).unwrap();
        assert_eq!(
            receive(&host).unwrap().map(|handed| match handed {
                Handed::Formats(formats) => formats,
                other => panic!("a formats message arrives as formats: {other:?}"),
            }),
            Some(formats),
            "the person's clock and date style reach the host as they set them"
        );
        // Said again while a host waits on an instance: it arrives as formats, never as one.
        send_formats(&shell, &formats).unwrap();
        assert!(
            receive_instance(&host).is_err(),
            "a host waiting on an instance does not take formats for one"
        );
    }

    #[test]
    fn an_ended_instance_crosses_the_door_back() {
        let (shell, host) = pair().unwrap();
        send_ended(&host, 42).unwrap();
        assert_eq!(
            receive_from_host(&shell).unwrap(),
            Some(FromHost::Ended(42))
        );
        send_exiting(&host, "the session closed its door and no instance runs").unwrap();
        assert_eq!(
            receive_from_host(&shell).unwrap(),
            Some(FromHost::Exiting(
                "the session closed its door and no instance runs".into()
            ))
        );
        drop(host);
        assert_eq!(receive_from_host(&shell).unwrap(), None, "the host is gone");
    }

    #[test]
    fn a_message_without_its_connections_is_refused() {
        use std::os::fd::AsFd as _;
        let (shell, host) = pair().unwrap();
        let request = br#"{"app":"a","room":"r","instance":1,"person":"p","module_len":1}"#;
        let one = connection();
        crate::door::send(shell.as_fd(), &[INSTANCE_VERB, request], &[one.as_fd()]).unwrap();
        assert!(receive_instance(&host).is_err());
        crate::door::send(shell.as_fd(), &[b"open", request], &[]).unwrap();
        assert!(receive_instance(&host).is_err());
    }

    #[test]
    fn a_task_crosses_the_door_with_its_program_connection_and_apps() {
        let (shell, host) = pair().unwrap();
        let request = TaskRequest {
            task: "door:1".into(),
            room: "7a110d00-0000-4000-8000-000000000001".into(),
            person: Some("ada".into()),
            apps_len: 3,
            records_under: [("so.tilo.lang.tasks".into(), "so.tilo.lang.todo".into())].into(),
            changed_at_record: ["so.tilo.shell".into()].into(),
        };
        let fds = TaskFds {
            program: connection(),
            apps: connection(),
        };
        send_task(&shell, &request, &fds).unwrap();
        let Some(Handed::Task(received, _fds)) = receive(&host).unwrap() else {
            panic!("a task arrives as a task");
        };
        assert_eq!(received, request);
    }

    #[test]
    fn apps_are_their_bytes_again() {
        let apps = vec![
            App::Module(Module {
                wasm: vec![0, 97, 115, 109, 1],
                ids: "Note = 1\n".into(),
                imports: Vec::new(),
            }),
            App::Module(Module {
                wasm: vec![0, 97, 115, 109, 2],
                ids: String::new(),
                imports: vec![vec![1, 2]],
            }),
            App::Surface {
                app: "so.tilo.chat".into(),
                title: "Chat".into(),
                surface: "[[resources]]\nkind = \"chat.workspace\"\n".into(),
            },
        ];
        assert_eq!(decode_apps(&encode_apps(&apps)).unwrap(), apps);
        assert!(decode_apps(b"garbage").is_err());
        let mut empty = App::Surface {
            app: String::new(),
            title: String::new(),
            surface: String::new(),
        }
        .encode();
        empty.truncate(empty.len() - 1);
        assert!(App::decode(&empty).is_err());
    }

    #[test]
    fn a_module_is_its_bytes_again() {
        let module = Module {
            wasm: vec![0, 97, 115, 109, 1],
            ids: "Note = 1\n".into(),
            imports: vec![vec![0, 97, 115, 109, 1], vec![0, 97, 115, 109, 2]],
        };
        assert_eq!(Module::decode(&module.encode()).unwrap(), module);
        assert!(Module::decode(b"garbage").is_err());
        let mut cut = module.encode();
        cut.truncate(cut.len() - 1);
        assert!(Module::decode(&cut).is_err());
    }
}
