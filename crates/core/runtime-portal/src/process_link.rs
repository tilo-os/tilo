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
//! The Linux session and the device on Linux, macOS and Windows start processes this way
//! ([`spawn`]); a Windows link is a message pipe whose handles go down to the process alone, and a
//! macOS link a stream socket its packets are framed on (`crate::packet`).

use std::io;

use serde::{Deserialize, Serialize};
use tilo_action_policy::MemberAccess;
use tilo_formats::RegionFormats;

pub use crate::packet::{Descriptor, Packet, pair};

#[cfg(windows)]
mod windows;

/// A process [`spawn`] started.
#[cfg(unix)]
pub type Child = std::process::Child;
#[cfg(windows)]
pub use windows::{Child, Confinement};

/// The service name the launcher hands the link under: `TILO_SVC_PROCESS_FD`.
pub const SERVICE: &str = "process";

/// The variable the process finds its link's descriptor in.
pub const FD_ENV: &str = "TILO_SVC_PROCESS_FD";

/// The descriptor the launcher places the link at in the process.
pub const CHILD_FD: i32 = 36;

/// The variable a window shown on a web page reads its two descriptors from, as `<patch>,<page>`:
/// its instance's patch connection and its end of the page's link. It answers that link and draws
/// nothing.
pub const PAGE_ENV: &str = "TILO_TILOLANG_PAGE";

/// The descriptor the launcher places a page window's end of the page's link at; its patch
/// connection is at [`CHILD_FD`].
pub const PAGE_FD: i32 = 37;

/// The one switch a launch hands a window it opens for a visitor.
pub const VISITOR_WINDOW_FLAG: &str = "--visitor";

/// Where a tilolang process keeps the storage of its workspace and author.
pub const STORAGE_ENV: &str = "TILO_TILOLANG_PROCESS_STORAGE";

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
    /// What the system's media controls ask of the app's Playback (`FromProcess::Media`): the
    /// instance that told the session what it plays, and the control a person pressed.
    MediaCommand {
        instance: u64,
        command: MediaCommand,
    },
    /// Nothing more comes: the process lets its link go and ends once it runs nothing. This
    /// is the session's half of closing the link, which a Windows pipe cannot close one way.
    Finish,
    /// Stop running engine job `job`: the call that started it was cancelled, and the daemon
    /// ends it. The process reports the job cancelled as its engine lets it go.
    CancelJob { job: String },
}

impl Packet for ToProcess {
    fn check(&self) -> Result<(), &'static str> {
        let over = match self {
            Self::Instance { request, .. } => request.module_len > MAX_MODULE,
            Self::Task { request, .. } => request.apps_len > MAX_MODULE,
            Self::Build { build, .. } => build.module_len > MAX_MODULE,
            Self::Window { .. }
            | Self::Patch { .. }
            | Self::Locale(_)
            | Self::Access(_)
            | Self::MediaCommand { .. }
            | Self::CancelJob { .. }
            | Self::Finish => false,
        };
        if over {
            Err("a module over the link's limit")
        } else {
            Ok(())
        }
    }
}

/// What a process says to the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FromProcess {
    /// The app opens the windows the session asks for on this link. An app that maps a window
    /// without having said it gets a process of its own for each window.
    Serves,
    /// The instance of this window ended (its window closed, or it never opened).
    Ended(u64),
    /// Said last, as the process exits: why, in words its supervisor logs and shows. A process
    /// gone without saying it ended unheard (killed, or crashed).
    Exiting(String),
    /// What the instance `instance` plays, as its Playback stands now, or that it plays nothing
    /// (`None`): what the system's media controls show, and the board a bare media key reaches.
    /// Said only when it moved.
    Media {
        instance: u64,
        view: Option<MediaView>,
    },
    /// The engine job `job` moved, as `record` says: its `{id, app, workspace, invocation, by,
    /// op, state, detail}` and, where the app asked for them, its `done` and `progress` handlers.
    /// The daemon keeps it as the device's job record (`devices/<device>/jobs/<job>`), naming the
    /// app and workspace from this process's own identity rather than from what the record says.
    Job {
        job: String,
        record: serde_json::Value,
    },
}

/// What one instance plays, as the system's media controls show it: the Playback's own reading,
/// so the board and its now-playing surface name what a person hears without reading an app's
/// records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaView {
    /// The Playback's id: what a command comes back naming.
    pub id: String,
    /// The app playing, as the desktop's widget shows it ("Video", "Music").
    pub identity: String,
    pub title: String,
    pub artist: String,
    /// Where its artwork is, or empty.
    pub art_url: String,
    pub playing: bool,
    /// Where the playhead stood when this was read, microseconds.
    pub position_us: i64,
    /// How long what plays runs, microseconds; zero when it does not say.
    pub length_us: i64,
    pub volume: f64,
    pub rate: f64,
    pub can_next: bool,
    pub can_prev: bool,
}

/// What a media control asks a Playback to do: the same actions a person or an agent calls on
/// the resource.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum MediaCommand {
    Play,
    Pause,
    /// Play if paused, pause if playing.
    Toggle,
    Stop,
    Next,
    Previous,
    /// Move by this many microseconds, positive forward.
    SeekBy(i64),
    /// Move to this position, microseconds from the start.
    SeekTo(i64),
    /// Set the volume, `0.0..=1.0`.
    SetVolume(f64),
    /// Silence, or bring back what was silent.
    Mute,
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

/// What an instance was opened with: a file of the workspace, or a link of a scheme its app
/// claims (`tube:watch?v=…`). A file's bytes are the content store's, and the instance runs its
/// package's action for that kind of file; a link is text, and the instance runs its package's
/// `open-link` with it, so it shows what the link names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenRequest {
    /// A file, as `file.choose` hands one to an action.
    File(OpenedFile),
    /// A link, as a shell hands one to the app that claims its scheme.
    Uri(String),
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
    /// What the app was opened with, when its launch named one: a file's bytes are read from the
    /// content store by their hash and given to the action its package declares for the file's
    /// kind, and a link is given to the app's `open-link`; `None` for a window opened without
    /// one.
    pub open: Option<OpenRequest>,
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
    /// Where the workspace's resources open on the web, as the session knows it
    /// (`tilolang_tilo::share::LinkHome`, as JSON): what the instance's host mints a resource's
    /// `share-link` under. `None` where the workspace's host serves no page, and then the link is
    /// refused with the system's own words.
    ///
    /// Always written, as every field of a packet is.
    pub link_home: Option<String>,
    /// What the providers this device registered publish at fixed addresses, as packages
    /// (`tilo_actions::remote::ProvidedPackage`, as a JSON list; empty for none): a package the
    /// app uses that is one of them is called at the addresses its provider registered.
    pub provided: String,
}

/// Declares the connections a packet of one shape carries, once: its fields, which the packet
/// writes and reads in that order, and how many descriptors it carries at most
/// ([`InstanceFds::CONNECTIONS`]). The link sizes its descriptor buffers from that count, so a
/// connection added to the list below is one more the buffers hold, never one they leave out.
macro_rules! connections {
    ($(#[$doc:meta])* $name:ident {
        $(
            $(#[$field_doc:meta])*
            $field:ident: $ty:ty [with = $with:literal],
        )*
    }) => {
        $(#[$doc])*
        #[derive(Debug, Serialize, Deserialize)]
        pub struct $name {
            $(
                $(#[$field_doc])*
                #[serde(with = $with)]
                pub $field: $ty,
            )*
        }

        impl $name {
            /// The most descriptors a packet of this shape carries: one for each connection, since
            /// every one of them may be carried.
            pub const CONNECTIONS: usize = [$(stringify!($field)),*].len();
        }
    };
}

connections! {
    /// The descriptors one instance arrives with.
    InstanceFds {
        broker: Descriptor [with = "crate::packet::fd"],
        identity: Descriptor [with = "crate::packet::fd"],
        module: Descriptor [with = "crate::packet::fd"],
        /// The instance's end of its view's patch connection. `None` for a call that shows nothing
        /// ([`Shows::Nothing`]).
        window: Option<Descriptor> [with = "crate::packet::optional_fd"],
        /// The instance's connection to its app's origin connector, when its types hold an outside
        /// service's resources.
        origin: Option<Descriptor> [with = "crate::packet::optional_fd"],
        /// The instance's connection to the device's content store, scoped to its app in its
        /// workspace: what a file it chooses or keeps, and the pictures its document holds, are
        /// read and written through. `None` on a device that keeps no content store.
        content: Option<Descriptor> [with = "crate::packet::optional_fd"],
        /// The instance's connection to the person's chooser: what `file.choose` and `file.save`
        /// ask. `None` on a session that serves none.
        chooser: Option<Descriptor> [with = "crate::packet::optional_fd"],
        /// The instance's connection to the person's clipboard: what `clipboard.copy` offers and
        /// `clipboard.paste` reads, since its process holds no display to do either on. `None` on a
        /// session that serves none.
        clipboard: Option<Descriptor> [with = "crate::packet::optional_fd"],
        /// The instance's connection to the session's tools service, made for this instance: what
        /// `File.open` asks which app opens a file and starts it. `None` on a session that serves
        /// none.
        tools: Option<Descriptor> [with = "crate::packet::optional_fd"],
    }
}

/// The most descriptors one message on the link carries: every connection an instance arrives
/// with ([`InstanceFds::CONNECTIONS`]), the widest message. A window request's two, a patch's and a
/// build's one, and a task's connections ([`TaskFds::CONNECTIONS`]) are fewer.
pub const MAX_FDS: usize = max(InstanceFds::CONNECTIONS, TaskFds::CONNECTIONS);

const fn max(left: usize, right: usize) -> usize {
    if left > right { left } else { right }
}

#[cfg(any(unix, windows))]
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
            tools: self.tools.as_ref().map(Descriptor::try_clone).transpose()?,
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

connections! {
    /// The descriptors a task arrives with: its program connection and a memfd of the apps its
    /// programs are compiled against ([`encode_apps`]).
    TaskFds {
        program: Descriptor [with = "crate::packet::fd"],
        apps: Descriptor [with = "crate::packet::fd"],
    }
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
pub(crate) mod policy_text {
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
#[cfg(not(windows))]
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
/// The descriptor could not be duplicated.
pub fn try_clone(link: &Descriptor) -> io::Result<Descriptor> {
    #[cfg(any(unix, windows))]
    {
        link.try_clone()
    }
    // A descriptor exists only where the platform has descriptors, so elsewhere `link` cannot be
    // one; the type is empty there and nothing reaches this.
    #[cfg(not(any(unix, windows)))]
    {
        let _ = link;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// The link the launcher handed this process, taken once.
///
/// A link handed as stdin ([`spawn`]) is moved off it, and stdin reads `/dev/null` from then on:
/// a child this process starts sets up its stdin before anything else, which would otherwise
/// replace the link before the child is handed it, and a child left the default stdin would
/// share the link.
#[must_use]
pub fn take() -> Option<Descriptor> {
    #[cfg(unix)]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let fd = crate::service_fd(SERVICE)?;
        if TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the launcher handed this descriptor to the process for the link alone, and the
        // flag above lets it be owned exactly once.
        let link: Descriptor = unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) };
        if fd != 0 {
            return Some(link);
        }
        match off_stdin(link) {
            Ok(link) => Some(link),
            Err(error) => {
                eprintln!("tilo-portal: the process link could not be moved off stdin: {error}");
                None
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawHandle as _, OwnedHandle, RawHandle};
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let value: usize = std::env::var(FD_ENV).ok()?.parse().ok()?;
        if TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the process that started this one made this handle inheritable for the link
        // alone and named it here, and the flag above lets it be owned exactly once.
        let handle = unsafe { OwnedHandle::from_raw_handle(value as RawHandle) };
        tilo_socket::Stream::from_handle(handle)
            .ok()
            .map(Descriptor::from_starter)
    }
    #[cfg(not(any(unix, windows)))]
    None
}

/// `link`, handed as descriptor 0, at a descriptor of its own, with `/dev/null` at 0.
#[cfg(unix)]
fn off_stdin(link: Descriptor) -> io::Result<Descriptor> {
    use std::os::fd::{AsRawFd as _, IntoRawFd as _};
    let moved = link.try_clone()?;
    let null = std::fs::File::open("/dev/null")?;
    // SAFETY: both are this process's own descriptors; dup2 replaces the link at 0 with
    // `/dev/null` in one step, so 0 is never closed in between.
    if unsafe { libc::dup2(null.as_raw_fd(), 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // Descriptor 0 is `/dev/null` now, stdin's own: dropping `link` would close it.
    let _ = link.into_raw_fd();
    Ok(moved)
}

/// Start `command` with the other end of a new link as its stdin, named by [`FD_ENV`] ([`take`]
/// reads it): not a `pre_exec` step, which would make the spawn `fork()` the whole caller.
///
/// On Windows the link's handle is named by [`FD_ENV`] instead, and the process inherits only it,
/// the null device as its input and output, and a pipe its error output goes to
/// ([`Child::stderr`]); it is started suspended and runs once its link names it.
///
/// # Errors
/// No link could be made, or the program did not start.
#[cfg(windows)]
pub fn spawn(command: std::process::Command) -> io::Result<(Descriptor, Child)> {
    windows::spawn(&command, None)
}

/// [`spawn`], with the process run in the `AppContainer` and the job `confinement` names. The
/// container's grants are its maker's (the Windows launcher); this only starts the process in it.
///
/// # Errors
/// As [`spawn`], and when this Windows cannot run an `AppContainer` (Wine cannot).
#[cfg(windows)]
pub fn spawn_confined(
    command: std::process::Command,
    confinement: &Confinement<'_>,
) -> io::Result<(Descriptor, Child)> {
    windows::spawn(&command, Some(confinement))
}

/// As above, on Unix.
///
/// # Errors
/// No link could be made, or the program did not start.
#[cfg(unix)]
pub fn spawn(mut command: std::process::Command) -> io::Result<(Descriptor, Child)> {
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
/// The socket's error, or [`io::ErrorKind::Unsupported`] off Unix and Windows.
pub fn close(link: &Descriptor) -> io::Result<()> {
    #[cfg(unix)]
    {
        Ok(rustix::net::shutdown(link, rustix::net::Shutdown::Both)?)
    }
    #[cfg(windows)]
    {
        link.stream()?.shutdown(std::net::Shutdown::Both)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = link;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "processes are linked on Unix and Windows only",
        ))
    }
}

/// Whether the process behind `link` let go of it: it closed its end as it stopped taking
/// windows ([`close`]), or it is gone. Never waits.
#[must_use]
pub fn hung_up(link: &Descriptor) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
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
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    {
        crate::packet::hung_up(link)
    }
    #[cfg(windows)]
    {
        link.stream().is_ok_and(tilo_socket::Stream::hung_up)
    }
    #[cfg(not(any(unix, windows)))]
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

#[cfg(all(test, unix))]
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
            open: Some(OpenRequest::File(OpenedFile {
                hash: "ab".repeat(32),
                name: Some("plan.md".into()),
            })),
            installed: [("so.tilo.lang.tasks".into(), "so.tilo.lang.todo".into())].into(),
            link_home: None,
            provided: String::new(),
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
                open: Some(OpenRequest::File(OpenedFile { hash: "cd".repeat(32), name: None })),
                ..opened.clone()
            };
            // An instance that shows nothing crosses with no window connection.
            let nothing = InstanceRequest { shows: Shows::Nothing, ..opened.clone() };
            for (request, window, (origin, content, chooser, tools)) in [
                (opened.clone(), Some(connection()), (None, None, None, None)),
                (
                    opened,
                    Some(connection()),
                    (
                        Some(connection()),
                        Some(connection()),
                        Some(connection()),
                        Some(connection()),
                    ),
                ),
                (plain.clone(), Some(connection()), (None, None, None, None)),
                (
                    plain,
                    Some(connection()),
                    (
                        Some(connection()),
                        Some(connection()),
                        Some(connection()),
                        Some(connection()),
                    ),
                ),
                // The clipboard and the tools service are their own connections: each crosses
                // whether the other does.
                (unnamed, Some(connection()), (None, Some(connection()), None, Some(connection()))),
                (nothing, None, (None, None, None, None)),
            ] {
                let clipboard = chooser.is_some().then(connection);
                let had = (
                    origin.is_some(),
                    content.is_some(),
                    chooser.is_some(),
                    tools.is_some(),
                );
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
                        tools,
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
                    (
                        fds.origin.is_some(),
                        fds.content.is_some(),
                        fds.chooser.is_some(),
                        fds.tools.is_some(),
                    ),
                    had,
                    "each connection the session made crosses with the instance"
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

    tilo_test_env::scenario! {
        fn a_window_opens_a_link_over_the_process_link;

        phase a_link_crosses_as_text_its_instance_shows;
        {
            // What an app was opened with crosses as the `open-link` text its instance shows,
            // never as bytes a file's kind names: the request's shape carries either.
            let (session, process) = pair().unwrap();
            let request = InstanceRequest {
                open: Some(OpenRequest::Uri("tube:watch?v=abc".into())),
                ..instance_request()
            };
            ToProcess::Instance {
                request: Box::new(request.clone()),
                fds: InstanceFds {
                    broker: connection(),
                    identity: connection(),
                    module: connection(),
                    window: Some(connection()),
                    origin: None,
                    content: None,
                    chooser: None,
                    clipboard: None,
                    tools: None,
                },
            }
            .send(&session)
            .unwrap();
            let Some(ToProcess::Instance { request: received, .. }) =
                ToProcess::receive(&process).unwrap()
            else {
                panic!("an instance arrives as one");
            };
            assert_eq!(*received, request);
        }
    }
}
