//! The process link: the one connection between the session and an app process, the process an
//! app runs in.
//!
//! The session keeps one end of a packet socket pair (`crate::packet`) and hands the other to the
//! process it starts, as `TILO_SVC_PROCESS_FD` at [`CHILD_FD`]. Everything the session says to a
//! running process is a [`ToProcess`] on it, with the descriptors it carries, and everything the
//! process says back is a [`FromProcess`]:
//!
//! - A native app serves every window of the app in a workspace from one process: the session
//!   asks it for another window ([`ToProcess::Window`]) instead of starting a second process,
//!   which could never hold the app's data, and hands a tilolang app's window its patch
//!   connection ([`ToProcess::Patch`]). The app says it reads its link ([`FromProcess::Serves`])
//!   before it maps its first window.
//! - A tilolang process runs one workspace and author's tilolang app instances. It has no app
//!   identity of its own: every instance ([`ToProcess::Instance`]), agent task
//!   ([`ToProcess::Task`]), later build ([`ToProcess::Build`]) and change of the person's formats
//!   or access arrives on its link, and it says when an instance ended
//!   ([`FromProcess::Ended`]) and, last, why it exits ([`FromProcess::Exiting`]).
//!
//! Only the Linux session starts processes this way; elsewhere there is no link and every call
//! says so.

use std::io;

use serde::{Deserialize, Serialize};
use tilo_action_policy::MemberAccess;
use tilo_formats::RegionFormats;

pub use crate::packet::{Descriptor, Packet, pair};

/// The service name the launcher hands the link under: `TILO_SVC_PROCESS_FD`.
pub const SERVICE: &str = "process";

/// The variable the process finds its link's descriptor in.
pub const FD_ENV: &str = "TILO_SVC_PROCESS_FD";

/// The descriptor the launcher places the link at in the process.
pub const CHILD_FD: i32 = 36;

/// The one switch a launch hands a window it opens for a visitor.
pub const VISITOR_WINDOW_FLAG: &str = "--visitor";

/// Where a tilolang process keeps the storage of its workspace and author.
pub const STORAGE_ENV: &str = "TILO_TILOLANG_PROCESS_STORAGE";

/// Where a tilolang process reads what an earlier version of its app kept on this device: a store
/// the session granted it read-only, holding only that app's documents in the process's workspace.
pub const EARLIER_ENV: &str = "TILO_TILOLANG_PROCESS_EARLIER";

/// Set on a launch whose window shows an instance a tilolang process runs.
pub const HOSTED_ENV: &str = "TILO_TILOLANG_HOSTED";

/// The largest module or app list a message may name.
pub const MAX_MODULE: u64 = 64 << 20;

/// What the session says to a process.
#[derive(Debug, Serialize, Deserialize)]
pub enum ToProcess {
    /// Open one more window of the app, with the window's own connections to the session.
    Window {
        request: WindowRequest,
        #[serde(with = "crate::packet::optional_fd")]
        collaboration: Option<Descriptor>,
        #[serde(with = "crate::packet::optional_fd")]
        shell_ui: Option<Descriptor>,
    },
    /// The patch connection of the window `instance`, whose instance a tilolang process runs:
    /// sent before the window is asked for.
    Patch {
        instance: u64,
        #[serde(with = "crate::packet::fd")]
        patch: Descriptor,
    },
    /// Run one instance of a tilolang app.
    Instance {
        request: Box<InstanceRequest>,
        fds: InstanceFds,
    },
    /// Run an agent task's programs against the apps they are compiled for.
    Task { request: TaskRequest, fds: TaskFds },
    /// The person's formats or language changed.
    Locale(Locale),
    /// What the person may do in one app changed.
    Access(AppAccess),
    /// A later build of an app the process runs, with its module in a sealed memfd.
    Build {
        build: AppBuild,
        #[serde(with = "crate::packet::fd")]
        module: Descriptor,
    },
}

impl Packet for ToProcess {
    fn check(&self) -> Result<(), &'static str> {
        let over = match self {
            Self::Instance { request, .. } => request.module_len > MAX_MODULE,
            Self::Task { request, .. } => request.apps_len > MAX_MODULE,
            Self::Build { build, .. } => build.module_len > MAX_MODULE,
            Self::Window { .. } | Self::Patch { .. } | Self::Locale(_) | Self::Access(_) => false,
        };
        if over {
            Err("a module over the link's limit")
        } else {
            Ok(())
        }
    }
}

/// What a process says to the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromProcess {
    /// The app opens the windows the session asks for on this link. An app that maps a window
    /// without having said it gets a process of its own for each window.
    Serves,
    /// The instance of this window ended (its window closed, or it never opened).
    Ended(u64),
    /// Said last, as the process exits: why, in words its supervisor logs and shows. A process
    /// gone without saying it ended unheard (killed, or crashed).
    Exiting(String),
}

impl Packet for FromProcess {}

impl FromProcess {
    /// [`FromProcess::Exiting`] with `reason`, cut to fit a packet.
    #[must_use]
    pub fn exiting(reason: &str) -> Self {
        let fit = crate::packet::MAX_PACKET.saturating_sub(16);
        let end = (0..=reason.len().min(fit))
            .rev()
            .find(|end| reason.is_char_boundary(*end))
            .unwrap_or(0);
        Self::Exiting(reason.get(..end).unwrap_or_default().to_owned())
    }
}

/// The session's request to open one more window of an app that is already running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRequest {
    pub instance: u64,
    /// The item the window opens on.
    pub reveal: Option<String>,
    /// The file the window opens, when the app was opened with one.
    pub open: Option<String>,
}

/// The person's region formats and language, as the session hands them to a process whose
/// instances are already running ([`ToProcess::Locale`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Locale {
    pub formats: RegionFormats,
    pub language: String,
}

/// What the person may do in one app of the process's workspace, said again when it changes
/// ([`ToProcess::Access`]): a role given or taken back reaches the windows already open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppAccess {
    pub app: String,
    /// `None` once the session holds no policy for the workspace: the broker alone decides.
    #[serde(with = "policy_text")]
    pub access: Option<MemberAccess>,
}

/// A file an instance was opened with: the hash its content has in the session's content store,
/// and the name of the file it is where the one handing it over knew it (`plan.md`). The instance
/// reads the bytes by the hash and hands the action the same `File` value a chooser gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenedFile {
    pub hash: String,
    /// The name of the file, where the app that handed it over knew it: the bytes alone say an
    /// image is a PNG, never that text is a note rather than a table.
    ///
    /// Always written, as every field of a packet is: postcard reads fields by position, so a
    /// field left out reads the next one in its place.
    pub name: Option<String>,
}

/// A later build of one app the process runs ([`ToProcess::Build`]): its instances take it over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppBuild {
    pub app: String,
    pub module_len: u64,
}

/// What a call shows: a view of the app's tree where someone uses it, or nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shows {
    /// A view someone uses: the app's code draws it, hears what they do, and offers to bring it
    /// forward.
    #[default]
    View,
    /// Nothing: the app's code runs for whoever calls it — an agent's step, an app's `start` — and
    /// nothing is drawn. A call that shows nothing has no view connection, offers no verb to bring
    /// a view forward, and stays until the session ends it.
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRequest {
    pub app: String,
    pub workspace: String,
    pub instance: u64,
    pub person: String,
    pub module_len: u64,
    /// Whether the call shows a view, or nothing ([`Shows`]).
    pub shows: Shows,
    /// The person's formats (`locale` in their settings): the clock a time of day reads by and the
    /// style a date does, for what the instance writes in a sentence (`t.clock()`, a change's
    /// before and after) and its window shows.
    pub formats: RegionFormats,
    /// The person's language (`de-DE`), the one their windows read in: the choice in their
    /// settings, else the locale their device runs in, as their session resolved it. Each window
    /// speaks it where its package ships it, and the package's source language otherwise
    /// (`tilo_history::window_language`), so the process resolves every word the window paints in
    /// the window's language rather than in its own process's. A session that names none (`""`)
    /// lets the process's own environment speak.
    pub language: String,
    /// The document the window was asked to open, by its id, when its launch named one: the
    /// instance opens only the document its route names (the workspace's, or the person's own), so
    /// a window asked for another is refused rather than shown that one.
    pub document: Option<String>,
    pub reveal: Option<String>,
    /// The file the app was opened with, when its launch named one: the instance runs the action
    /// its package declares for the file's kind with it, as a chooser hands a file to an action.
    /// The instance reads the bytes from the content store by their hash; `None` for a window
    /// opened without one.
    pub open: Option<OpenedFile>,
    /// The tilolang apps installed here, by id, each with the id its documents are kept under: a
    /// package the app uses that is one of them is another app, whose documents the instance reads
    /// from that app's own document and whose changes it asks of that app's link. A package it uses
    /// that is not installed as an app keeps its documents in the app's own.
    pub installed: std::collections::BTreeMap<String, String>,
    /// What the person may do in the app, as the workspace's policy says it: the instance's views
    /// offer what it denies them disabled, and refuse it before it reaches the broker. `None`
    /// where the session holds no policy for the workspace; the broker decides either way.
    #[serde(with = "policy_text")]
    pub access: Option<MemberAccess>,
}

/// The descriptors one instance arrives with.
#[derive(Debug, Serialize, Deserialize)]
pub struct InstanceFds {
    #[serde(with = "crate::packet::fd")]
    pub broker: Descriptor,
    #[serde(with = "crate::packet::fd")]
    pub identity: Descriptor,
    #[serde(with = "crate::packet::fd")]
    pub module: Descriptor,
    /// The instance's end of its view's patch connection. `None` for a call that shows nothing
    /// ([`Shows::Nothing`]).
    #[serde(with = "crate::packet::optional_fd")]
    pub window: Option<Descriptor>,
    /// The instance's connection to its app's origin connector, when its types hold an outside
    /// service's resources.
    #[serde(with = "crate::packet::optional_fd")]
    pub origin: Option<Descriptor>,
    /// The instance's connection to the device's content store, scoped to its app in its
    /// workspace: what a file it chooses or keeps, and the pictures its document holds, are read
    /// and written through. `None` on a device that keeps no content store.
    #[serde(with = "crate::packet::optional_fd")]
    pub content: Option<Descriptor>,
    /// The instance's connection to the person's chooser: what `file.choose` and `file.save`
    /// ask. `None` on a session that serves none.
    #[serde(with = "crate::packet::optional_fd")]
    pub chooser: Option<Descriptor>,
    /// The instance's connection to the person's clipboard: what `clipboard.copy` offers and
    /// `clipboard.paste` reads, since its process holds no display to do either on. `None` on a
    /// session that serves none.
    #[serde(with = "crate::packet::optional_fd")]
    pub clipboard: Option<Descriptor>,
}

#[cfg(unix)]
impl InstanceFds {
    /// The same connections, each a descriptor of its own: to hand the instance to another
    /// process when the one it went to has ended.
    ///
    /// # Errors
    /// The system's error duplicating one.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            broker: self.broker.try_clone()?,
            identity: self.identity.try_clone()?,
            module: self.module.try_clone()?,
            window: self
                .window
                .as_ref()
                .map(Descriptor::try_clone)
                .transpose()?,
            origin: self
                .origin
                .as_ref()
                .map(Descriptor::try_clone)
                .transpose()?,
            content: self
                .content
                .as_ref()
                .map(Descriptor::try_clone)
                .transpose()?,
            chooser: self
                .chooser
                .as_ref()
                .map(Descriptor::try_clone)
                .transpose()?,
            clipboard: self
                .clipboard
                .as_ref()
                .map(Descriptor::try_clone)
                .transpose()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRequest {
    pub task: String,
    pub workspace: String,
    pub person: Option<String>,
    pub apps_len: u64,
    pub documents_under: std::collections::BTreeMap<String, String>,
    pub changed_at_document: std::collections::BTreeSet<String>,
}

/// The descriptors a task arrives with: its program connection and a memfd of the apps its
/// programs are compiled against ([`encode_apps`]).
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskFds {
    #[serde(with = "crate::packet::fd")]
    pub program: Descriptor,
    #[serde(with = "crate::packet::fd")]
    pub apps: Descriptor,
}

/// One app a task's programs may name, as the session hands it to the task's process: a tilolang
/// app's compiled module, or a native app's published surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    postcard::to_stdvec(apps).unwrap_or_default()
}

/// The apps `bytes` hold.
///
/// # Errors
/// Bytes that are not a list of apps.
pub fn decode_apps(bytes: &[u8]) -> io::Result<Vec<App>> {
    whole(bytes, "not a list of tilolang apps")
}

/// What one instance runs: the package's compiled module, its ids lock and the compiled modules of
/// the packages it uses, as the package's payload holds them (`app.wasm`, `app.ids`,
/// `imports/*.wasm`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    #[serde(with = "serde_bytes")]
    pub wasm: Vec<u8>,
    pub ids: String,
    #[serde(with = "byte_lists")]
    pub imports: Vec<Vec<u8>>,
}

impl Module {
    /// The module the package payload at `payload` holds.
    ///
    /// # Errors
    /// The payload's read error.
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
        postcard::to_stdvec(self).unwrap_or_default()
    }

    /// The module `bytes` hold.
    ///
    /// # Errors
    /// Bytes that are not a module.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        whole(bytes, "not a tilolang module")
    }
}

/// The one value `bytes` hold, and nothing after it.
fn whole<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &'static str) -> io::Result<T> {
    match postcard::take_from_bytes(bytes) {
        Ok((value, [])) => Ok(value),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, what)),
    }
}

/// Byte strings written whole rather than byte by byte.
mod byte_lists {
    use serde::{Deserialize as _, Deserializer, Serializer};

    #[allow(
        clippy::ptr_arg,
        reason = "serde's `with` hands the field by reference"
    )]
    pub fn serialize<S: Serializer>(
        lists: &Vec<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(lists.iter().map(|list| serde_bytes::Bytes::new(list)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Vec<u8>>, D::Error> {
        Ok(Vec::<serde_bytes::ByteBuf>::deserialize(deserializer)?
            .into_iter()
            .map(serde_bytes::ByteBuf::into_vec)
            .collect())
    }
}

/// A workspace's policy as its JSON text: the policy types leave out empty fields when they are
/// written, which a positional encoding cannot read back, and the session already bounds that
/// text.
mod policy_text {
    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer};
    use tilo_action_policy::MemberAccess;

    #[allow(
        clippy::ref_option,
        reason = "serde's `with` hands the field by reference"
    )]
    pub fn serialize<S: Serializer>(
        access: &Option<MemberAccess>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        access
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<MemberAccess>, D::Error> {
        use serde::de::Error as _;
        Option::<String>::deserialize(deserializer)?
            .map(|text| serde_json::from_str(&text))
            .transpose()
            .map_err(D::Error::custom)
    }
}

/// `descriptor` as the raw descriptor it owns, handed on to whatever closes it.
#[must_use]
pub fn into_raw(descriptor: Descriptor) -> crate::RawFd {
    #[cfg(unix)]
    {
        std::os::fd::IntoRawFd::into_raw_fd(descriptor)
    }
    #[cfg(not(unix))]
    match descriptor {}
}

/// A second handle on `link`, for the reader a process keeps beside the one it
/// serves.
///
/// # Errors
///
/// The descriptor could not be duplicated, or [`io::ErrorKind::Unsupported`] where there
/// are no descriptors: no caller can hold a link to duplicate there.
pub fn try_clone(link: &Descriptor) -> io::Result<Descriptor> {
    #[cfg(unix)]
    {
        link.try_clone()
    }
    #[cfg(not(unix))]
    {
        let _ = link;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "only the Linux session links its processes",
        ))
    }
}

/// The link the launcher handed this process, taken once.
#[must_use]
pub fn take() -> Option<Descriptor> {
    #[cfg(target_os = "linux")]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let fd = crate::service_fd(SERVICE)?;
        if TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the launcher handed this descriptor to the process for the link alone, and the
        // flag above lets it be owned exactly once.
        Some(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Start `command` holding the other end of a new link, as its stdin, named by [`FD_ENV`]: a
/// tilolang host reads it there, and a launcher takes it off stdin and places it at [`CHILD_FD`]
/// in the sandbox it starts. Answers this end and the child. Every tilolang host is handed its
/// link this way, whatever starts it.
///
/// Stdin, rather than a descriptor placed in a `pre_exec` step: that step makes
/// `Command::spawn` `fork()` the caller, which for the compositor's session is the whole desktop.
///
/// # Errors
/// No link could be made, or the program did not start.
#[cfg(unix)]
pub fn spawn(mut command: std::process::Command) -> io::Result<(Descriptor, std::process::Child)> {
    let (ours, theirs) = pair()?;
    command
        .env(FD_ENV, "0")
        .stdin(std::process::Stdio::from(theirs));
    let child = command.spawn()?;
    Ok((ours, child))
}

/// Close `link` for good, both ways: whatever the other end sends from now on fails at once
/// instead of waiting unread in a process that no longer reads it.
///
/// # Errors
/// The socket's error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn close(link: &Descriptor) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        Ok(rustix::net::shutdown(link, rustix::net::Shutdown::Both)?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = link;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "only the Linux session links its processes",
        ))
    }
}

/// Whether the process behind `link` let go of it: it closed its end as it stopped taking
/// windows ([`close`]), or it is gone. Never waits.
#[must_use]
pub fn hung_up(link: &Descriptor) -> bool {
    #[cfg(target_os = "linux")]
    {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut polled = [PollFd::new(link, PollFlags::RDHUP)];
        let now = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        poll(&mut polled, Some(&now)).is_ok_and(|_| {
            polled[0]
                .revents()
                .intersects(PollFlags::RDHUP | PollFlags::HUP | PollFlags::ERR)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = link;
        false
    }
}

/// Whether the process behind `link` said it serves windows: every message waiting is read, and
/// none blocks.
///
/// # Errors
/// The socket's error, or a message that is not one.
pub fn take_serves(link: &Descriptor) -> io::Result<bool> {
    let mut serves = false;
    while let Some(message) = FromProcess::try_receive(link)? {
        serves |= message == FromProcess::Serves;
    }
    Ok(serves)
}

#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use super::*;

    fn connection() -> Descriptor {
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.into()
    }

    fn instance_request() -> InstanceRequest {
        InstanceRequest {
            app: "so.tilo.notes".into(),
            workspace: "7a110d00-0000-4000-8000-000000000001".into(),
            instance: 7,
            person: "ada".into(),
            module_len: 12,
            shows: Shows::View,
            formats: RegionFormats {
                time: tilo_formats::TimeStyle::H12,
                ..RegionFormats::default()
            },
            language: "de-DE".into(),
            document: Some("uuid:7a110d00-0000-4000-8000-000000000009".into()),
            reveal: Some("tl.so.tilo.notes.Note:7".into()),
            open: Some(OpenedFile {
                hash: "ab".repeat(32),
                name: Some("plan.md".into()),
            }),
            installed: [("so.tilo.lang.tasks".into(), "so.tilo.lang.todo".into())].into(),
            access: Some(MemberAccess::person(
                tilo_action_policy::WorkspacePolicy::new(
                    "7a110d00-0000-4000-8000-000000000001",
                    "bob",
                ),
                "ada",
            )),
        }
    }

    tilo_test_env::scenario! {
        fn messages_across_the_process_link;

        phase a_window_request_arrives_with_its_own_connections;
        {
            let (session, app) = pair().unwrap();
            let request = WindowRequest {
                instance: 41,
                reveal: Some("chat/links/c_ada".into()),
                open: None,
            };
            ToProcess::Window {
                request: request.clone(),
                collaboration: Some(connection()),
                shell_ui: None,
            }
            .send(&session)
            .unwrap();
            let Some(ToProcess::Window { request: received, collaboration, shell_ui }) =
                ToProcess::receive(&app).unwrap()
            else {
                panic!("a window request arrives as one");
            };
            assert_eq!(received, request);
            assert!(collaboration.is_some() && shell_ui.is_none());
            drop(session);
            assert!(ToProcess::receive(&app).unwrap().is_none(), "a closed link ends");
        }

        phase an_instance_crosses_the_link_with_its_connections;
        {
            let (session, process) = pair().unwrap();
            let opened = instance_request();
            // Most windows open without a file, and a file handed over may have no name: each
            // crosses as it was sent, the fields after it read as theirs.
            let plain = InstanceRequest { open: None, ..opened.clone() };
            let unnamed = InstanceRequest {
                open: Some(OpenedFile { hash: "cd".repeat(32), name: None }),
                ..opened.clone()
            };
            // An instance that shows nothing crosses with no window connection.
            let nothing = InstanceRequest { shows: Shows::Nothing, ..opened.clone() };
            for (request, window, (origin, content, chooser)) in [
                (opened.clone(), Some(connection()), (None, None, None)),
                (opened, Some(connection()), (Some(connection()), Some(connection()), Some(connection()))),
                (plain.clone(), Some(connection()), (None, None, None)),
                (plain, Some(connection()), (Some(connection()), Some(connection()), Some(connection()))),
                (unnamed, Some(connection()), (None, Some(connection()), None)),
                (nothing, None, (None, None, None)),
            ] {
                let clipboard = chooser.is_some().then(connection);
                let had = (origin.is_some(), content.is_some(), chooser.is_some());
                ToProcess::Instance {
                    request: Box::new(request.clone()),
                    fds: InstanceFds {
                        broker: connection(),
                        identity: connection(),
                        module: connection(),
                        window,
                        origin,
                        content,
                        chooser,
                        clipboard,
                    },
                }
                .send(&session)
                .unwrap();
                let Some(ToProcess::Instance { request: received, fds }) =
                    ToProcess::receive(&process).unwrap()
                else {
                    panic!("an instance arrives as one");
                };
                assert_eq!(*received, request);
                assert_eq!(
                    fds.window.is_some(),
                    request.shows == Shows::View,
                    "a window crosses with the instance that draws one"
                );
                assert_eq!(
                    (fds.origin.is_some(), fds.content.is_some(), fds.chooser.is_some()),
                    had,
                    "the instance's own chooser and content store cross with it"
                );
                assert_eq!(
                    fds.clipboard.is_some(),
                    had.2,
                    "the instance's clipboard connection crosses with it"
                );
            }
        }

        phase what_the_person_may_do_in_an_app_crosses_the_link_whole;
        {
            let (session, process) = pair().unwrap();
            // A workspace of many members: the policy is far over a page.
            let mut policy = tilo_action_policy::WorkspacePolicy::new(
                "7a110d00-0000-4000-8000-000000000001",
                "bob",
            );
            for member in 0..400 {
                policy
                    .members
                    .insert(format!("member-{member}"), tilo_action_policy::Role::Viewer);
            }
            let access = AppAccess {
                app: "so.tilo.notes".into(),
                access: Some(MemberAccess::person(policy, "member-7")),
            };
            ToProcess::Access(access.clone()).send(&session).unwrap();
            assert!(matches!(
                ToProcess::receive(&process).unwrap(),
                Some(ToProcess::Access(received)) if received == access
            ));
            let locale = Locale {
                formats: RegionFormats {
                    date: tilo_formats::DateStyle::Iso,
                    ..RegionFormats::default()
                },
                language: "de-DE".into(),
            };
            ToProcess::Locale(locale.clone()).send(&session).unwrap();
            assert!(matches!(
                ToProcess::receive(&process).unwrap(),
                Some(ToProcess::Locale(received)) if received == locale
            ));
        }

        phase a_module_over_the_limit_is_refused;
        {
            let (session, process) = pair().unwrap();
            ToProcess::Build {
                build: AppBuild { app: "a".into(), module_len: MAX_MODULE + 1 },
                module: connection(),
            }
            .send(&session)
            .unwrap();
            assert!(ToProcess::receive(&process).is_err());
        }

        phase the_process_says_it_serves_ended_and_why_it_exits;
        {
            let (session, process) = pair().unwrap();
            assert!(!take_serves(&session).unwrap(), "a process that said nothing");
            FromProcess::Serves.send(&process).unwrap();
            assert!(take_serves(&session).unwrap());
            FromProcess::Ended(42).send(&process).unwrap();
            assert_eq!(FromProcess::receive(&session).unwrap(), Some(FromProcess::Ended(42)));
            let long = "why ".repeat(crate::packet::MAX_PACKET);
            FromProcess::exiting(&long).send(&process).unwrap();
            assert!(matches!(
                FromProcess::receive(&session).unwrap(),
                Some(FromProcess::Exiting(reason)) if long.starts_with(&reason)
            ));
            drop(process);
            assert_eq!(FromProcess::receive(&session).unwrap(), None, "the process is gone");
        }

        phase apps_and_modules_are_their_bytes_again;
        {
            let apps = vec![
                App::Module(Module {
                    wasm: vec![0, 97, 115, 109, 1],
                    ids: "Note = 1\n".into(),
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
            let Some(App::Module(module)) = apps.first() else { panic!("the first app is a module") };
            assert_eq!(&Module::decode(&module.encode()).unwrap(), module);
            let mut cut = module.encode();
            cut.truncate(cut.len() - 1);
            assert!(Module::decode(&cut).is_err());
        }
    }
}
