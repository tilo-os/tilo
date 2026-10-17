//! The file-chooser and URI-opening vocabulary: requests, outcomes and the
//! URI → path rule every backend shares.
//!
//! # Why this is shared
//!
//! Per the Tilo HIG an app must **never draw its own file picker and never key
//! open/save off an environment variable**: it goes through the portal so the
//! *host desktop* provides the chooser — on GNOME the user sees GNOME's file
//! dialog; on Tilo the compositor implements the portal backend and renders
//! Tilo's picker. Windows uses the host's Open, Save, and folder dialogs.
//! On Tilo, the picker is also where the per-workspace sandbox
//! file-access prompt lives, which makes this module **the file-access
//! security boundary**. A per-app copy of it is not merely duplicated code: it
//! is a second, independently-drifting definition of which URIs are allowed to
//! become paths the app then reads or writes.
//!
//! One such drift was live: mapping URIs with `uri.strip_prefix("file://")`
//! instead of [`url::Url::to_file_path`] turns `file://evil-host/etc/passwd`
//! into the **relative** path `evil-host/etc/passwd` — silently resolved
//! against the process's working directory, from a URI that explicitly named
//! a *remote host*. This module validates the host explicitly before converting
//! the URL; see `file_uris_resolve_only_to_local_normalised_paths`.
//!
//! # Threading contract
//!
//! The portal round-trip is asynchronous and opens a real host window, so it
//! must **never** run on the UI thread. [`PickRequest::send`] hands the request
//! to the current host's [`crate::FilePicker`], which answers over an
//! [`std::sync::mpsc`] channel — the Linux portal from the app runtime, the
//! macOS panels from the main run loop, the Windows dialogs from a COM
//! apartment on a worker, the browser from a promise. The app drains that
//! channel once per frame; the UI thread never blocks. Tests answer requests
//! through a recording host (`ui::host::HostServices::recording`).
//!
//! # Backends
//!
//! The crate selects the backend per target (see the crate docs). The pure
//! URI→path mapping ([`map_uris`], [`map_uri`]) and the channel types are the
//! same everywhere, so the security rule stays under test on every target.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: portal and file dialogs block on their own thread"
)]

mod uri;
pub use uri::open_uri_scoped;
#[cfg(any(test, feature = "test-support"))]
pub use uri::{ScopedUriFixture, scoped_uri_fixture};

/// Custody of a delayed host effect. A scoped chooser or URI request carries
/// the caller that asked for it, and the worker rechecks that caller
/// immediately before the host effect runs; an expired caller never reaches
/// the effect.
pub trait EffectGate: Send + 'static {
    /// Run `effect` only while the original caller is still admitted.
    ///
    /// # Errors
    ///
    /// The caller's refusal when it is no longer admitted, or `effect`'s own
    /// error.
    fn with_effect<R>(&self, effect: impl FnOnce() -> Result<R, String>) -> Result<R, String>;
}

use std::fs::File;
#[cfg(any(unix, windows))]
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

/// The result of a **single-file** portal interaction (open, or a Save
/// destination), delivered back to the UI thread over a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalOutcome {
    /// The user picked this local path. A `/var/workspace/...` pick resolves
    /// through the FUSE mount and hydrates on read exactly like a local file.
    Opened(PathBuf),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored (no desktop portal running, a
    /// non-local URI, a D-Bus failure). Carries a human-readable reason.
    Failed(String),
}

/// The result of a **multi-select** portal open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultiOutcome {
    /// The user picked one or more local paths, in the order the portal
    /// returned them. Never empty.
    Selected(Vec<PathBuf>),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored. Carries a human-readable reason.
    Failed(String),
}

/// A pending single-file request: the receiver the UI thread drains each
/// frame. `Some(rx)` means a picker is in flight; `None` means idle.
pub type PortalChannel = Receiver<PortalOutcome>;

/// Outcome of asking the desktop to open a path with an appropriate app, or
/// reveal its containing directory. The request is fire-and-observe: callers
/// drain this channel without ever blocking the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    Opened,
    Failed(String),
}

/// A pending desktop-open request.
pub type LaunchChannel = Receiver<LaunchOutcome>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchKind {
    Open,
    Reveal,
}

/// Open a local file, or reveal its containing folder, through the XDG
/// `OpenURI` portal. The portal receives an already-open descriptor rather than
/// a string path, preserving the same capability boundary as file choosing.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub(crate) path: PathBuf,
    pub(crate) kind: LaunchKind,
    pub(crate) thread_name: String,
}

impl LaunchRequest {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: LaunchKind::Open,
            thread_name: "tilo-portal-open-path".into(),
        }
    }

    pub fn reveal(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: LaunchKind::Reveal,
            thread_name: "tilo-portal-reveal-path".into(),
        }
    }

    #[must_use]
    pub fn thread_name(mut self, name: &str) -> Self {
        self.thread_name = name.to_string();
        self
    }

    /// Hand the request to the current host's [`crate::FilePicker`].
    pub fn send(self) -> LaunchChannel {
        crate::current_picker().launch(self)
    }

    /// The path to open, or whose containing folder to reveal.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this reveals the containing folder rather than opening the
    /// path itself.
    pub fn is_reveal(&self) -> bool {
        self.kind == LaunchKind::Reveal
    }

    /// The name of the worker thread a native launcher runs on.
    pub fn worker_name(&self) -> &str {
        &self.thread_name
    }
}

/// Hand a URI (a web page, a `mailto:` draft, a `settings:` pane) to the
/// current host's [`crate::UriOpener`].
///
/// On the native desktop, Unix builds with the `portal` feature go through the
/// XDG `OpenURI` portal: inside the app sandbox there is no host `xdg-open` to
/// spawn, and the portal is where the desktop applies its own handler choice
/// and policy. Other native builds use the platform opener (`ShellExecute` on
/// Windows).
///
/// Blocks until the handler is launched or the request is refused, so call it
/// from a worker thread; [`open_uri_detached`] is the fire-and-forget form.
///
/// # Errors
/// Returns invalid input for a URI containing NUL (which native APIs would
/// otherwise silently truncate), and the host's error when it cannot parse or
/// refuses the request.
pub fn open_uri(uri: &str) -> std::io::Result<()> {
    open_uri_with(&*crate::current_opener(), uri)
}

/// Non-blocking [`open_uri`]: the outcome arrives on the returned channel,
/// which the UI thread drains like any other portal result. The request runs
/// through the host's [`tilo_rt::background::Background`]; a host that refuses
/// background work reports it as a failed launch.
pub fn send_open_uri(uri: &str) -> LaunchChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    // Resolve the host here: a scoped host is only visible on this thread.
    let opener = crate::current_opener();
    let uri = uri.to_string();
    let refused = tx.clone();
    let spawned =
        tilo_rt::background::Background::current().spawn("tilo-portal-open-uri", move || {
            let _ = tx.send(match open_uri_with(&*opener, &uri) {
                Ok(()) => LaunchOutcome::Opened,
                Err(error) => LaunchOutcome::Failed(error.to_string()),
            });
        });
    if spawned.is_err() {
        let _ = refused.send(LaunchOutcome::Failed(
            "this host runs no background work".to_owned(),
        ));
    }
    rx
}

/// [`open_uri`] through the host's [`tilo_rt::background::Background`]. A launch
/// failure is logged, not surfaced.
pub fn open_uri_detached(uri: &str) {
    let opener = crate::current_opener();
    let uri = uri.to_string();
    let spawned =
        tilo_rt::background::Background::current().spawn("tilo-portal-open-uri", move || {
            if let Err(error) = open_uri_with(&*opener, &uri) {
                tracing::warn!("failed to open {uri:?}: {error}");
            }
        });
    if spawned.is_err() {
        tracing::warn!("not opening a URI: this host runs no background work");
    }
}

/// [`open_uri`] through an opener the caller resolved itself, for work that
/// runs away from the thread whose host it belongs to: resolve
/// `ui::host::current().opener` there, then call this on the worker.
///
/// # Errors
/// As [`open_uri`].
pub fn open_uri_with(opener: &dyn crate::UriOpener, uri: &str) -> std::io::Result<()> {
    if uri.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "URI contains NUL",
        ));
    }
    opener.open(uri)
}

/// A file selected by the portal and opened by its worker before the result is
/// handed to the UI. The descriptor, rather than the path used to obtain it,
/// is the authority to read the selected file.
#[derive(Debug)]
pub struct OpenedFile {
    file: File,
    display_name: String,
}

impl OpenedFile {
    /// The name shown to the user. This is deliberately separate from the
    /// descriptor: consumers must not recover a path and reopen it.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Consume this portal result and transfer its already-open descriptor.
    #[must_use]
    pub fn into_file(self) -> File {
        self.file
    }
}

/// The result of a single-file portal open which retains the selected
/// descriptor. Unlike [`PortalOutcome`], this is specifically for consumers
/// that must hand an exact, already-open file to another security boundary.
#[derive(Debug)]
pub enum OpenFileOutcome {
    /// The portal worker opened a regular local file without following the
    /// final path component, and retained its display name separately. From
    /// [`PickRequest::send_save_file`] the file is open for writing.
    Opened(OpenedFile),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable, returned an invalid URI, or the selected
    /// path could not safely be opened.
    Failed(String),
}

/// A pending descriptor-retaining portal open, drained by the UI thread.
pub type OpenFileChannel = Receiver<OpenFileOutcome>;

/// A pending multi-select request: the receiver the UI thread drains each
/// frame.
pub type MultiChannel = Receiver<MultiOutcome>;

/// Where a wallpaper is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperTarget {
    Background,
    Lockscreen,
    Both,
}

/// The outcome of asking the desktop to use a picture as the wallpaper. The
/// desktop previews it and asks the user to confirm; `Err` carries why it did
/// not happen, including the user declining.
pub type WallpaperChannel = Receiver<Result<(), String>>;

/// Ask the desktop's Wallpaper portal to use `file`, an already-open picture
/// (typically from [`PickRequest::send_open_file`]), for `target`. Never
/// blocks: the D-Bus round trip runs on the app runtime.
pub fn send_set_wallpaper(file: File, target: WallpaperTarget) -> WallpaperChannel {
    #[cfg(target_os = "linux")]
    {
        crate::xdg::set_wallpaper(file, target)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, target);
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(Err(
            "setting the wallpaper needs the desktop portal build".to_string()
        ));
        rx
    }
}

/// One entry in the chooser's file-type dropdown.
///
/// Globs and MIME types are both accepted because the portal backends differ
/// in which they honour; supplying both is the portable choice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFilter {
    pub(crate) name: String,
    pub(crate) globs: Vec<String>,
    pub(crate) mimetypes: Vec<String>,
}

impl FileFilter {
    /// A filter labelled `name` in the chooser's type dropdown.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// Match files by shell glob, e.g. `*.png`.
    /// The globs this filter offers, so an app can assert that its chooser
    /// advertises everything it can open — a filter narrower than
    /// the reader behind it hides a user's file with no way to discover the
    /// app would have taken it.
    #[must_use]
    pub fn globs(&self) -> &[String] {
        &self.globs
    }

    #[must_use]
    pub fn glob(mut self, g: &str) -> Self {
        self.globs.push(g.to_string());
        self
    }

    /// Match files by MIME type, e.g. `application/pdf`.
    #[must_use]
    pub fn mimetype(mut self, m: &str) -> Self {
        self.mimetypes.push(m.to_string());
        self
    }

    /// The "All files" escape hatch every picker should offer, so the user is
    /// never trapped by a file with an unusual MIME type.
    pub fn all_files() -> Self {
        Self::new("All files").glob("*")
    }

    /// The MIME types this filter offers.
    #[must_use]
    pub fn mimetypes(&self) -> &[String] {
        &self.mimetypes
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Names the directory a session nobody sits at (a provider's guest) saves
/// into: a Save chooser answers there under its suggested name, and an Open
/// chooser is refused, since no one is present to choose. Whether the app may
/// write there is still its file grants'.
pub const SAVE_DIRECTORY_ENV: &str = "TILO_SAVE_DIRECTORY";

/// Standard `FileChooser` `choices` identifier carrying an explicit writable
/// directory request through the XDG frontend (which filters unknown options).
/// The backend acknowledges it only after the destination chooser is accepted.
pub const WRITABLE_DIRECTORY_CHOICE: &str = "tilo-writable-directory";

/// Which chooser to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickKind {
    Open { multiple: bool },
    Directory,
    Save { suggested_name: String },
}

/// A portal file-chooser request, built up and then [`send`](PickRequest::send)
/// on a worker thread.
///
/// The per-app delta between what used to be nine hand-written copies of this
/// module is exactly what this struct captures: a title, an accept label, a
/// filter set, and a worker-thread name.
#[derive(Debug, Clone)]
pub struct PickRequest {
    pub(crate) title: String,
    pub(crate) accept_label: String,
    pub(crate) thread_name: String,
    pub(crate) filters: Vec<FileFilter>,
    pub(crate) kind: PickKind,
    pub(crate) max_contents_bytes: Option<usize>,
    pub(crate) initial_directory: Option<PathBuf>,
    writable_directory: bool,
}

impl PickRequest {
    /// A single-file open chooser.
    pub fn open(title: &str, accept_label: &str) -> Self {
        Self::new(title, accept_label, PickKind::Open { multiple: false })
    }

    /// A multi-select open chooser: the user may pick arbitrarily many files
    /// at once and every local one comes back.
    pub fn open_multiple(title: &str, accept_label: &str) -> Self {
        Self::new(title, accept_label, PickKind::Open { multiple: true })
    }

    /// Pick one existing folder. Directory authority still comes from the
    /// desktop portal; callers should not invent a path or build a private
    /// filesystem browser.
    pub fn directory(title: &str, accept_label: &str) -> Self {
        Self::new(title, accept_label, PickKind::Directory)
    }

    /// Pick one existing destination folder with explicit write consent.
    ///
    /// The title and accept label describe the destination operation, not consent.
    /// The portal host must display its own explicit read/write wording before
    /// confirmation. A document portal exports only the selected directory;
    /// the native shell chooser instead requires existing workspace write access.
    /// Cancellation grants nothing; workspace/default grants are never widened.
    pub fn writable_directory(title: &str, accept_label: &str) -> Self {
        Self {
            writable_directory: true,
            ..Self::directory(title, accept_label)
        }
    }

    /// Whether this directory request explicitly asks for write authority.
    /// Hosts must preserve this distinction or refuse the request, never silently
    /// substitute a read-only directory selection.
    pub fn is_writable_directory(&self) -> bool {
        self.writable_directory
    }

    /// A Save chooser, seeding the filename field with `suggested_name`.
    pub fn save(title: &str, accept_label: &str, suggested_name: &str) -> Self {
        Self::new(
            title,
            accept_label,
            PickKind::Save {
                suggested_name: suggested_name.to_string(),
            },
        )
    }

    fn new(title: &str, accept_label: &str, kind: PickKind) -> Self {
        Self {
            title: title.to_string(),
            accept_label: accept_label.to_string(),
            thread_name: "tilo-portal".to_string(),
            filters: Vec::new(),
            kind,
            max_contents_bytes: None,
            initial_directory: None,
            writable_directory: false,
        }
    }

    /// Suggest the folder shown when the host chooser opens. This is a
    /// navigation hint, never a file-access grant. Hosts may ignore it;
    /// callers must still validate the actual chosen directory.
    #[must_use]
    pub fn initial_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.initial_directory = Some(directory.into());
        self
    }

    pub fn directory_hint(&self) -> Option<&Path> {
        self.initial_directory.as_deref()
    }

    /// Bound the total contents returned across all chosen files. Oversized
    /// selections fail without reading their full contents into memory.
    /// Applies to `send_contents`, not path or descriptor requests. With no
    /// override, the host's existing contents policy applies.
    #[must_use]
    pub fn max_contents_bytes(mut self, limit: usize) -> Self {
        self.max_contents_bytes = Some(limit);
        self
    }

    /// The requested aggregate contents budget, when explicitly bounded.
    pub fn contents_byte_limit(&self) -> Option<usize> {
        self.max_contents_bytes
    }

    /// Name the worker thread, so a stuck picker is identifiable in a
    /// backtrace or `top -H`. Pickers that run on a dedicated thread (the
    /// Windows dialogs) use it; the Unix portal's D-Bus requests run on the
    /// shared app runtime instead.
    #[must_use]
    pub fn thread_name(mut self, name: &str) -> Self {
        self.thread_name = name.to_string();
        self
    }

    /// Add a file-type filter. Order is the dropdown order; the first is the
    /// default.
    #[must_use]
    pub fn filter(mut self, f: FileFilter) -> Self {
        self.filters.push(f);
        self
    }

    /// Append the "All files" escape hatch.
    #[must_use]
    pub fn all_files(self) -> Self {
        self.filter(FileFilter::all_files())
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn is_multiple(&self) -> bool {
        matches!(self.kind, PickKind::Open { multiple: true })
    }

    /// Whether this opens existing files (single or multi-select).
    pub fn is_open(&self) -> bool {
        matches!(self.kind, PickKind::Open { .. })
    }

    /// The filter dropdown labels, in order. Lets an app assert on the chooser
    /// it configured without opening one.
    pub fn filter_names(&self) -> Vec<&str> {
        self.filters.iter().map(FileFilter::name).collect()
    }

    /// The filename this request seeds a Save dialog with, if it is one.
    pub fn suggested_name(&self) -> Option<&str> {
        match &self.kind {
            PickKind::Save { suggested_name } => Some(suggested_name.as_str()),
            PickKind::Open { .. } | PickKind::Directory => None,
        }
    }

    /// The chooser's accept-button label.
    pub fn accept_label(&self) -> &str {
        &self.accept_label
    }

    /// The file-type filters, in dropdown order.
    pub fn filters(&self) -> &[FileFilter] {
        &self.filters
    }

    /// Whether this chooses a folder rather than files.
    pub fn is_directory(&self) -> bool {
        matches!(self.kind, PickKind::Directory)
    }

    /// The chooser kind this request opens.
    pub fn kind(&self) -> &PickKind {
        &self.kind
    }

    /// The name of the worker thread a native picker runs on, when it runs
    /// on one (see [`Self::thread_name`]).
    pub fn worker_name(&self) -> &str {
        &self.thread_name
    }

    /// Hand the request to the current host's [`crate::FilePicker`].
    /// The UI thread is never blocked: the native picker runs the D-Bus
    /// round-trip and the host dialog entirely on a worker. A multi-select
    /// request should go through [`send_multiple`](Self::send_multiple)
    /// instead, so every pick is delivered.
    pub fn send(self) -> PortalChannel {
        crate::current_picker().pick(self)
    }

    /// Retain the original admitted caller through the actual chooser. The
    /// caller is rechecked on a worker immediately before the host picker
    /// runs; an expired caller never reaches the chooser effect.
    pub fn send_scoped(self, scope: impl EffectGate) -> PortalChannel {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(backend) = scoped_picker_fixture_backend() {
            return scoped_pick_worker(scope, move || backend(self));
        }
        let files = crate::current_picker();
        scoped_pick_worker(scope, move || {
            files.pick(self).recv().unwrap_or_else(|_| {
                PortalOutcome::Failed("the file chooser closed without answering".into())
            })
        })
    }

    /// Open one selected regular file on the portal worker and deliver its
    /// descriptor to the UI. This is intentionally a separate outcome path so
    /// normal path-based open/save callers retain their existing API.
    ///
    /// This is valid only for a single-file Open request. Calling it for Save
    /// or multi-select requests reports a normal failed outcome.
    pub fn send_open_file(self) -> OpenFileChannel {
        crate::current_picker().open_file(self)
    }

    /// Save counterpart of [`send_open_file`](Self::send_open_file): the
    /// destination the user chose, created and open for writing. Android's
    /// chooser creates a document with no path the app may write, so a Save
    /// there must come through here; hosts that answer Save with a path
    /// refuse it. Valid only for a Save request.
    pub fn send_save_file(self) -> OpenFileChannel {
        crate::current_picker().save_file(self)
    }

    /// Multi-select counterpart of [`send`](Self::send).
    pub fn send_multiple(self) -> MultiChannel {
        crate::current_picker().pick_multiple(self)
    }

    /// Deliver the chosen files' contents instead of paths, for callers that
    /// only read what the user picked. Works where no local path exists (a
    /// browser page). Open requests only; multi-select delivers every file.
    pub fn send_contents(self) -> ContentsChannel {
        crate::current_picker().pick_contents(self)
    }

    /// Save `bytes` where the user chooses. Save requests only. A browser
    /// page downloads them under the suggested name.
    pub fn send_save_contents(self, bytes: Vec<u8>) -> SaveChannel {
        crate::current_picker().save_contents(self, bytes)
    }
}

/// One chosen file's contents, as [`PickRequest::send_contents`] delivers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedContents {
    /// The name shown to the user; never a path to reopen.
    pub name: String,
    /// The MIME type the host reported, when it did.
    pub media_type: Option<String>,
    pub bytes: Vec<u8>,
    /// Where the chosen file is on this device, when the host chose one
    /// with a path: for recognising the same file chosen again, never for
    /// showing. `None` where files have no paths (a browser page).
    pub location: Option<std::path::PathBuf>,
}

/// The result of [`PickRequest::send_contents`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentsOutcome {
    /// The chosen files, in the host's order. Never empty.
    Picked(Vec<PickedContents>),
    Cancelled,
    Failed(String),
}

/// A pending contents pick, drained by the UI thread.
pub type ContentsChannel = Receiver<ContentsOutcome>;

/// The result of [`PickRequest::send_save_contents`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    Cancelled,
    Failed(String),
}

/// A pending contents save, drained by the UI thread.
pub type SaveChannel = Receiver<SaveOutcome>;

const CHOOSER_GONE: &str = "the file chooser went away";

/// A path-based picker's pending answer.
pub(crate) enum PickedPaths {
    One(PortalChannel),
    Many(MultiChannel),
}

/// The paths a path-based picker answered, read on the host's
/// [`tilo_rt::background::Background`]: the default
/// [`crate::FilePicker::pick_contents`] for hosts with local files.
pub(crate) fn read_picked(
    paths: Option<PickedPaths>,
    max_contents_bytes: Option<usize>,
) -> ContentsChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let Some(paths) = paths else {
        let _ = tx.send(ContentsOutcome::Failed(
            "only an Open chooser delivers contents".to_owned(),
        ));
        return rx;
    };
    let refused = tx.clone();
    let spawned = tilo_rt::background::Background::current().spawn("tilo-portal-read", move || {
        let paths = match paths {
            PickedPaths::One(rx) => match rx.recv() {
                Ok(PortalOutcome::Opened(path)) => Ok(vec![path]),
                Ok(PortalOutcome::Cancelled) => Err(ContentsOutcome::Cancelled),
                Ok(PortalOutcome::Failed(reason)) => Err(ContentsOutcome::Failed(reason)),
                Err(_) => Err(ContentsOutcome::Failed(CHOOSER_GONE.to_owned())),
            },
            PickedPaths::Many(rx) => match rx.recv() {
                Ok(MultiOutcome::Selected(paths)) => Ok(paths),
                Ok(MultiOutcome::Cancelled) => Err(ContentsOutcome::Cancelled),
                Ok(MultiOutcome::Failed(reason)) => Err(ContentsOutcome::Failed(reason)),
                Err(_) => Err(ContentsOutcome::Failed(CHOOSER_GONE.to_owned())),
            },
        };
        let outcome = paths.map_or_else(
            |outcome| outcome,
            |paths| {
                let mut remaining = max_contents_bytes;
                paths
                    .into_iter()
                    .map(|path| {
                        let bytes = read_contents(&path, remaining)?;
                        if let Some(remaining) = &mut remaining {
                            *remaining = remaining
                                .checked_sub(bytes.len())
                                .ok_or_else(|| "chosen contents exceed byte limit".to_owned())?;
                        }
                        let name = path
                            .file_name()
                            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                        Ok(PickedContents {
                            name,
                            media_type: None,
                            bytes,
                            location: Some(path),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()
                    .map_or_else(ContentsOutcome::Failed, ContentsOutcome::Picked)
            },
        );
        let _ = tx.send(outcome);
    });
    if spawned.is_err() {
        let _ = refused.send(ContentsOutcome::Failed(
            "this host runs no background work".to_owned(),
        ));
    }
    rx
}

fn read_contents(path: &Path, limit: Option<usize>) -> Result<Vec<u8>, String> {
    let read = || -> std::io::Result<Vec<u8>> {
        match limit {
            None => std::fs::read(path),
            Some(limit) => {
                let mut bytes = Vec::new();
                File::open(path)?
                    .take((limit as u64).saturating_add(1))
                    .read_to_end(&mut bytes)?;
                Ok(bytes)
            }
        }
    };
    let bytes = read().map_err(|error| format!("{}: {error}", path.display()))?;
    if limit.is_some_and(|limit| bytes.len() > limit) {
        return Err(format!(
            "{}: chosen contents exceed the byte limit",
            path.display()
        ));
    }
    Ok(bytes)
}

/// Write `bytes` to the path a Save chooser answers, on the host's
/// [`tilo_rt::background::Background`]: the default
/// [`crate::FilePicker::save_contents`] for hosts with local files.
pub(crate) fn write_picked(destination: Option<PortalChannel>, bytes: Vec<u8>) -> SaveChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let Some(destination) = destination else {
        let _ = tx.send(SaveOutcome::Failed(
            "only a Save chooser saves contents".to_owned(),
        ));
        return rx;
    };
    let refused = tx.clone();
    let spawned =
        tilo_rt::background::Background::current().spawn("tilo-portal-write", move || {
            let _ = tx.send(match destination.recv() {
                Ok(PortalOutcome::Opened(path)) => match std::fs::write(&path, bytes) {
                    Ok(()) => SaveOutcome::Saved,
                    Err(error) => SaveOutcome::Failed(format!("{}: {error}", path.display())),
                },
                Ok(PortalOutcome::Cancelled) => SaveOutcome::Cancelled,
                Ok(PortalOutcome::Failed(reason)) => SaveOutcome::Failed(reason),
                Err(_) => SaveOutcome::Failed(CHOOSER_GONE.to_owned()),
            });
        });
    if spawned.is_err() {
        let _ = refused.send(SaveOutcome::Failed(
            "this host runs no background work".to_owned(),
        ));
    }
    rx
}

/// Run one scoped chooser request on a background worker, checking the
/// original caller's effect scope there immediately before `pick`.
fn scoped_pick_worker(
    scope: impl EffectGate,
    pick: impl FnOnce() -> PortalOutcome + Send + 'static,
) -> PortalChannel {
    let (sender, receiver) = std::sync::mpsc::channel();
    let failed = sender.clone();
    let spawned = tilo_rt::background::Background::current().spawn("ui-portal-scoped", move || {
        let outcome = scope
            .with_effect(|| Ok(pick()))
            .unwrap_or_else(PortalOutcome::Failed);
        let _ = sender.send(outcome);
    });
    if spawned.is_err() {
        let _ = failed.send(PortalOutcome::Failed(
            "this host runs no background work for a scoped chooser".into(),
        ));
    }
    receiver
}

/// Spawn the portal worker. The UI may have moved on (window closed) by the
/// time it finishes, so a dead channel is ignored by the closure itself.
///
/// If the OS can't spawn the thread, `work` (and whatever sender it closed
/// over) is simply dropped: the caller's receiver observes a closed
/// channel, the same outward signal it must already handle for a worker
/// that spawned but whose send lost the race with a dropped receiver.
///
/// Windows dialogs and the Android Storage Access Framework answer on a
/// worker; the Unix XDG portal uses the shared app runtime instead.
#[cfg(any(windows, target_os = "android"))]
pub fn spawn_worker(name: &str, work: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(work);
}

// ---------------------------------------------------------------------------
// URI → path mapping. Compiled unconditionally: this is the security rule, and
// it stays under test whether or not the `portal` feature is on.
// ---------------------------------------------------------------------------

/// Map one portal-returned URI to a local path, or `None` if it does not name
/// a file on this machine.
///
/// Builds on [`url::Url::to_file_path`], which is already much stricter than a
/// `strip_prefix("file://")`: it requires the `file` scheme, rejects a URI
/// naming any host other than the empty host or `localhost`, and requires an
/// absolute path.
///
/// It is not strict enough on its own, though. `to_file_path` percent-decodes
/// each segment and then joins, so a segment the portal presented as a single
/// **file name** can decode into several path components: `a%2Fb.txt` becomes
/// `a/b.txt`, and `%2E%2E` becomes a parent-directory hop. A chosen name would
/// then reach outside the directory the user actually browsed. So we also
/// require that the decoded path has exactly as many components as the URI had
/// segments, and that none of them is `..` or `.`.
#[cfg(any(unix, windows))]
pub fn to_local_path(uri: &url::Url) -> Option<PathBuf> {
    use std::path::Component;

    // Windows URL conversion also accepts UNC hosts. A remote share is not
    // a local selection and must not silently cross this boundary.
    if !matches!(uri.host_str(), None | Some("" | "localhost")) {
        return None;
    }
    let path = uri.to_file_path().ok()?;
    if !path.is_absolute() {
        return None;
    }
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_)))
    {
        return None;
    }

    // No parent/current-directory hops: the portal returns resolved picks, so
    // one appearing here came from an escape in a file name.
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return None;
    }

    // One URI segment must yield exactly one path component. An escaped
    // separator would make the decoded path longer than the URI it came from.
    let segments = uri.path_segments()?.filter(|s| !s.is_empty()).count();
    let components = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    // A Windows drive is a URI segment but a Prefix, not a Normal component.
    #[cfg(windows)]
    let components = components.checked_add(1)?;
    if components != segments {
        return None;
    }

    Some(path)
}

/// A host without a native filesystem has no local paths: every URI stays
/// remote.
#[cfg(not(any(unix, windows)))]
#[must_use]
pub fn to_local_path(_uri: &url::Url) -> Option<PathBuf> {
    None
}

/// Map the portal's returned URIs for a **single-file** interaction: the first
/// local `file://` path wins, an empty list is a cancel, and a URI that is not
/// a local file is a failure.
///
/// Pure, so the channel/threading contract can be tested without a portal.
pub fn map_uri(uris: &[url::Url]) -> PortalOutcome {
    let Some(uri) = uris.first() else {
        return PortalOutcome::Cancelled;
    };
    match to_local_path(uri) {
        Some(path) => PortalOutcome::Opened(path),
        None => PortalOutcome::Failed(format!("not a local file: {uri}")),
    }
}

/// Map the portal's returned URIs for a **multi-select** open: every local
/// `file://` path is collected, an empty list is a cancel, and a selection
/// with no local file at all is a failure.
///
/// A mixed selection keeps the local entries and drops the rest — the user
/// asked for those files, and refusing all of them because one was remote
/// would be surprising.
pub fn map_uris(uris: &[url::Url]) -> MultiOutcome {
    if uris.is_empty() {
        return MultiOutcome::Cancelled;
    }
    let paths: Vec<PathBuf> = uris.iter().filter_map(to_local_path).collect();
    if paths.is_empty() {
        return MultiOutcome::Failed(match uris {
            [only] => format!("not a local file: {only}"),
            _ => "no local files in selection".to_string(),
        });
    }
    MultiOutcome::Selected(paths)
}

/// Turn an already validated portal URI into a descriptor-retaining outcome.
/// This runs on the portal worker, immediately after the picker returns, so
/// no UI-frame delay can turn an accepted path into a later reopen.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) fn open_selected_uri(uri: &url::Url) -> OpenFileOutcome {
    let Some(path) = to_local_path(uri) else {
        return OpenFileOutcome::Failed(format!("not a local file: {uri}"));
    };
    match open_regular_file(&path) {
        Ok(file) => OpenFileOutcome::Opened(OpenedFile {
            file,
            display_name: display_name(&path),
        }),
        Err(error) => OpenFileOutcome::Failed(format!("could not open selected file: {error}")),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "selected file".to_string())
}

/// The one temporary file beside a Save destination an app may write and
/// then rename over it: `<folder>/.<name>.tilo-save`. A Seatbelt-confined
/// macOS app is granted exactly this name with the destination, and nothing
/// else in the folder. `None` for a path that names no file.
pub fn save_sibling(destination: &Path) -> Option<PathBuf> {
    let name = destination.file_name()?;
    let mut sibling = std::ffi::OsString::from(".");
    sibling.push(name);
    sibling.push(".tilo-save");
    Some(destination.with_file_name(sibling))
}

/// Replace the Save destination `destination` with `bytes` atomically:
/// write [`save_sibling`], flush it to disk, and rename it over the
/// destination, so a crash leaves the old file or the new one.
///
/// # Errors
///
/// Writing the sibling or renaming it failed; the destination is untouched.
pub fn replace_saved(destination: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let sibling = save_sibling(destination).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the destination names no file",
        )
    })?;
    let written = File::create(&sibling)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&sibling, destination));
    if written.is_err() {
        let _ = std::fs::remove_file(&sibling);
    }
    written
}

/// Open a chosen local path without following its final component and verify
/// the resulting descriptor is a regular file. Checking metadata on the open
/// descriptor, rather than the path, keeps the type check bound to exactly
/// what is handed to the caller.
///
/// Android's SAF view path also opens an already-chosen local file this way
/// before handing the descriptor to the system viewer.
///
/// # Errors
/// Returns when the path cannot be opened without following a final symlink,
/// or when the opened descriptor is not a regular file.
#[cfg(unix)]
pub fn open_regular_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "selected path is not a regular file",
        ));
    }
    Ok(file)
}

/// Bind the type and reparse checks to the opened Windows handle, so a link
/// cannot be substituted between validation and opening the selected file.
///
/// # Errors
/// Returns when the path cannot be opened, or when the opened handle is not a
/// regular file (including reparse points).
#[cfg(windows)]
pub fn open_regular_file(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "selected path is not a regular file",
        ));
    }
    Ok(file)
}

impl OpenedFile {
    /// Wrap a file a host has already opened and checked. The native picker
    /// opens without following the final path component and verifies a
    /// regular file; another host must uphold the same rule.
    #[must_use]
    pub fn new(file: File, display_name: impl Into<String>) -> Self {
        Self {
            file,
            display_name: display_name.into(),
        }
    }
}

impl PortalOutcome {
    /// The chosen path, if the interaction produced one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            PortalOutcome::Opened(p) => Some(p.as_path()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> url::Url {
        #[cfg(windows)]
        let s = &s
            .replace("file:///", "file:///C:/")
            .replace("file://localhost/", "file://localhost/C:/");
        url::Url::parse(s).unwrap()
    }

    fn local_path(s: &str) -> PathBuf {
        #[cfg(windows)]
        return PathBuf::from(format!("C:{s}"));
        #[cfg(not(windows))]
        PathBuf::from(s)
    }

    #[test]
    fn writable_directory_is_explicit_and_preserves_directory_shape() {
        let read = PickRequest::directory("Read folder", "Choose");
        let write = PickRequest::writable_directory("Import into folder", "Import here");
        assert!(!read.is_writable_directory());
        assert!(write.is_writable_directory());
        assert!(write.is_directory());
        assert!(!write.is_open());
        assert!(!write.is_multiple());
        assert_eq!(write.suggested_name(), None);
        assert_eq!(write.accept_label(), "Import here");
    }

    #[test]
    fn directory_request_is_single_select_and_has_no_filename() {
        let request = PickRequest::directory("Choose folder", "Choose");
        assert!(!request.is_multiple());
        assert_eq!(request.suggested_name(), None);
        assert!(matches!(request.kind, PickKind::Directory));
    }

    #[test]
    fn directory_hint_preserves_the_requested_folder_without_granting_it() {
        let home = local_path("/Users/Recognized home");
        let request =
            PickRequest::directory("Allow this folder", "Allow").initial_directory(home.clone());
        assert_eq!(request.directory_hint(), Some(home.as_path()));
        assert!(request.is_directory());
        assert_eq!(request.suggested_name(), None);
        assert_eq!(
            PickRequest::directory("Choose", "Allow").directory_hint(),
            None
        );
    }

    // ---- the security rule -------------------------------------------

    #[test]
    fn rejects_a_uri_that_names_a_remote_host() {
        let uri = u("file://evil-host.example.com/etc/passwd");

        assert_eq!(
            to_local_path(&uri),
            None,
            "a foreign host is not a local path"
        );
        assert!(
            matches!(
                map_uri(std::slice::from_ref(&uri)),
                PortalOutcome::Failed(_)
            ),
            "a foreign-host URI must not become an openable path"
        );
        assert!(matches!(map_uris(&[uri]), MultiOutcome::Failed(_)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_paths_roundtrip_without_accepting_unc_or_escaped_separators() {
        let uri = url::Url::parse("file:///C:/Users/Alice/My%20notes.txt").unwrap();
        assert_eq!(
            to_local_path(&uri),
            Some(PathBuf::from("C:/Users/Alice/My notes.txt"))
        );
        for rejected in [
            "file://server/share/notes.txt",
            "file:///C:/Users/Alice/a%5Cb.txt",
            "file:///C:/Users/Alice/a%2Fb.txt",
            "file:///Users/Alice/notes.txt",
        ] {
            assert_eq!(
                to_local_path(&url::Url::parse(rejected).unwrap()),
                None,
                "{rejected}"
            );
        }
    }

    #[test]
    fn a_rejected_host_never_yields_a_relative_path() {
        // Independent of the enum: whatever we hand back must never be a path
        // that would resolve against the working directory.
        for s in [
            "file://evil-host.example.com/etc/passwd",
            "file://192.0.2.1/srv/secret",
            "https://example.com/remote.png",
            "smb://fileserver/share/doc.odt",
        ] {
            let out = map_uri(&[u(s)]);
            if let PortalOutcome::Opened(p) = &out {
                panic!("{s} was accepted as {p:?}");
            }
            assert!(matches!(out, PortalOutcome::Failed(_)), "{s} must fail");
        }
    }

    #[test]
    fn localhost_is_the_local_machine() {
        // `file://localhost/...` is the spec's spelling of "this machine" and
        // is equivalent to an empty host.
        assert_eq!(
            map_uri(&[u("file://localhost/home/alice/a.mkv")]),
            PortalOutcome::Opened(local_path("/home/alice/a.mkv"))
        );
    }

    // ---- escaped filenames, which the nine copies disagreed about -----

    #[test]
    fn percent_escapes_in_filenames_are_decoded() {
        assert_eq!(
            map_uri(&[u("file:///home/alice/My%20Clip.mp4")]),
            PortalOutcome::Opened(local_path("/home/alice/My Clip.mp4"))
        );
        // Multi-byte UTF-8 escapes must survive too.
        assert_eq!(
            map_uri(&[u("file:///home/alice/%C3%A9t%C3%A9%20caf%C3%A9.mp3")]),
            PortalOutcome::Opened(local_path("/home/alice/été café.mp3"))
        );
        assert_eq!(
            map_uri(&[u("file:///home/alice/a%23b%3Fc%25d.txt")]),
            PortalOutcome::Opened(local_path("/home/alice/a#b?c%d.txt"))
        );
    }

    #[test]
    fn a_plus_in_a_path_stays_a_plus() {
        // `+` means space only in query strings, not in a path component.
        assert_eq!(
            map_uri(&[u("file:///home/alice/my+song.mp3")]),
            PortalOutcome::Opened(local_path("/home/alice/my+song.mp3"))
        );
    }

    #[test]
    fn an_escaped_separator_does_not_become_a_new_path_component() {
        // A bare `to_file_path` maps this to `/home/alice/a/b.txt` — one URI
        // segment silently becoming two path components.
        assert_eq!(
            to_local_path(&u("file:///home/alice/a%2Fb.txt")),
            None,
            "an escaped separator must not add a path component"
        );
        assert!(matches!(
            map_uri(&[u("file:///home/alice/a%2Fb.txt")]),
            PortalOutcome::Failed(_)
        ));
    }

    #[test]
    fn a_parent_hop_hidden_inside_one_segment_is_rejected() {
        // The URI has three segments, the decoded path would have five: what
        // the portal presented as one file name would climb two levels.
        for s in [
            "file:///home/alice/%2E%2E%2F%2E%2E%2Fetc/shadow",
            "file:///var/workspace/%2E%2E%2Fetc/shadow",
        ] {
            assert_eq!(to_local_path(&u(s)), None, "{s} must not resolve");
            assert!(matches!(map_uri(&[u(s)]), PortalOutcome::Failed(_)), "{s}");
        }
    }

    #[test]
    fn a_dot_dot_segment_is_resolved_by_uri_normalisation_not_smuggled() {
        // `%2E%2E` as a whole segment is not an escape hatch: RFC 3986 path
        // normalisation happens when the URI is *parsed*, so the URI's own
        // path is already `/etc/shadow`. Whether the app may read it is the
        // sandbox's question, not this module's.
        let uri = u("file:///home/alice/%2E%2E/%2E%2E/etc/shadow");
        assert_eq!(
            to_local_path(&uri),
            Some(local_path("/etc/shadow")),
            "the URI normalises on parse"
        );
        assert_eq!(to_local_path(&uri), Some(local_path("/etc/shadow")));
    }

    #[test]
    fn ordinary_nested_paths_still_resolve() {
        // The hardening must not reject the everyday case it sits next to.
        for (uri, want) in [
            ("file:///a.txt", "/a.txt"),
            (
                "file:///home/alice/Documents/report.pdf",
                "/home/alice/Documents/report.pdf",
            ),
            (
                "file:///var/workspace/deep/er/still/x.odp",
                "/var/workspace/deep/er/still/x.odp",
            ),
            (
                "file:///home/alice/dot.name.with.dots.tar.gz",
                "/home/alice/dot.name.with.dots.tar.gz",
            ),
        ] {
            assert_eq!(
                to_local_path(&u(uri)),
                Some(local_path(want)),
                "{uri} must resolve normally"
            );
        }
    }

    // ---- the shape every copy implemented -----------------------------

    #[test]
    fn empty_is_cancel() {
        assert_eq!(map_uri(&[]), PortalOutcome::Cancelled);
        assert_eq!(map_uris(&[]), MultiOutcome::Cancelled);
    }

    #[test]
    fn single_takes_the_first_local_file() {
        let uris = [
            u("file:///home/alice/photo.png"),
            u("file:///home/alice/other.png"),
        ];
        assert_eq!(
            map_uri(&uris),
            PortalOutcome::Opened(local_path("/home/alice/photo.png"))
        );
    }

    #[test]
    fn multi_collects_every_local_file() {
        let uris = [
            u("file:///home/alice/work.vcf"),
            u("file:///home/alice/family.vcard"),
        ];
        assert_eq!(
            map_uris(&uris),
            MultiOutcome::Selected(vec![
                local_path("/home/alice/work.vcf"),
                local_path("/home/alice/family.vcard"),
            ])
        );
    }

    #[test]
    fn multi_skips_remote_but_keeps_local() {
        let uris = [
            u("https://example.com/remote.vcf"),
            u("file:///home/alice/local.vcf"),
        ];
        assert_eq!(
            map_uris(&uris),
            MultiOutcome::Selected(vec![local_path("/home/alice/local.vcf")]),
            "a mixed multi-selection keeps the files the user can actually read"
        );
    }

    #[test]
    fn multi_all_remote_is_failure() {
        assert!(matches!(
            map_uris(&[
                u("https://example.com/a.vcf"),
                u("https://example.com/b.vcf")
            ]),
            MultiOutcome::Failed(_)
        ));
    }

    #[test]
    fn workspace_paths_resolve_through_the_fuse_mount() {
        assert_eq!(
            map_uri(&[u("file:///var/workspace/media/a.mkv")]),
            PortalOutcome::Opened(local_path("/var/workspace/media/a.mkv"))
        );
    }

    #[test]
    fn launch_request_records_open_and_reveal_without_changing_the_path() {
        let open = LaunchRequest::open("/tmp/archive.zip");
        assert_eq!(open.path, Path::new("/tmp/archive.zip"));
        assert_eq!(open.kind, LaunchKind::Open);
        let reveal = LaunchRequest::reveal("/tmp/archive.zip");
        assert_eq!(reveal.path, Path::new("/tmp/archive.zip"));
        assert_eq!(reveal.kind, LaunchKind::Reveal);
    }

    #[test]
    fn outcome_path_accessor_reports_only_a_real_pick() {
        assert_eq!(
            PortalOutcome::Opened(PathBuf::from("/tmp/a")).path(),
            Some(Path::new("/tmp/a"))
        );
        assert_eq!(PortalOutcome::Cancelled.path(), None);
        assert_eq!(PortalOutcome::Failed("x".into()).path(), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn descriptor_open_retains_the_opened_file_and_a_separate_display_name() {
        use std::io::{Read, Write};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original-installer.msi");
        let mut source = File::create(&path).unwrap();
        source.write_all(b"installer bytes").unwrap();
        source.flush().unwrap();
        let uri = url::Url::from_file_path(&path).unwrap();

        let opened = match open_selected_uri(&uri) {
            OpenFileOutcome::Opened(opened) => opened,
            OpenFileOutcome::Cancelled | OpenFileOutcome::Failed(_) => {
                panic!("regular temporary file must open")
            }
        };
        assert_eq!(opened.display_name(), "original-installer.msi");
        // A later path-based reopen would observe these replacement bytes;
        // the portal outcome instead keeps the descriptor it already opened.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"replacement bytes").unwrap();
        let mut contents = String::new();
        let mut file = opened.into_file();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "installer bytes");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_open_refuses_a_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("installer.exe");
        std::fs::write(&target, b"installer bytes").unwrap();
        let link = dir.path().join("selected.exe");
        symlink(&target, &link).unwrap();
        let uri = url::Url::from_file_path(&link).unwrap();

        assert!(matches!(
            open_selected_uri(&uri),
            OpenFileOutcome::Failed(_)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn descriptor_open_refuses_a_non_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let uri = url::Url::from_file_path(dir.path()).unwrap();

        assert!(matches!(
            open_selected_uri(&uri),
            OpenFileOutcome::Failed(_)
        ));
    }

    // ---- the request builder ------------------------------------------

    #[test]
    fn builder_records_the_per_app_delta() {
        let r = PickRequest::open("Insert image", "Insert")
            .thread_name("wp-portal-open")
            .filter(
                FileFilter::new("Images")
                    .glob("*.png")
                    .mimetype("image/png"),
            )
            .all_files();
        assert_eq!(r.title(), "Insert image");
        assert!(!r.is_multiple());
        assert_eq!(r.filters.len(), 2);
        assert_eq!(r.filters[0].name(), "Images");
        assert_eq!(r.filters[0].globs, ["*.png"]);
        assert_eq!(r.filters[0].mimetypes, ["image/png"]);
        assert_eq!(
            r.filters[1],
            FileFilter::all_files(),
            "escape hatch is last"
        );
        assert_eq!(r.thread_name, "wp-portal-open");

        assert!(PickRequest::open_multiple("Import contacts", "Import").is_multiple());
        let s = PickRequest::save("Export contacts", "Export", "contacts.vcf");
        assert!(!s.is_multiple());
        assert_eq!(
            s.kind,
            PickKind::Save {
                suggested_name: "contacts.vcf".into()
            }
        );
    }
}
// A fixture is selected only by the current test UI thread. The selected Arc is
// retained by this exact dispatched request, never looked up on the worker.
#[cfg(any(test, feature = "test-support"))]
type ScopedPickerBackend = std::sync::Arc<dyn Fn(PickRequest) -> PortalOutcome + Send + Sync>;
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SCOPED_PICKER_FIXTURE: std::cell::RefCell<Option<ScopedPickerBackend>> = const { std::cell::RefCell::new(None) };
}
#[cfg(any(test, feature = "test-support"))]
pub struct ScopedPickerFixture {
    _owner_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
/// Select a test backend for scoped chooser requests on this exact UI thread.
/// Dispatch retains the backend even after the guard leaves scope; all real
/// effect checks still run immediately before invoking it on the worker.
///
/// # Errors
///
/// When this UI thread already owns a picker fixture.
#[cfg(any(test, feature = "test-support"))]
pub fn scoped_picker_fixture(backend: ScopedPickerBackend) -> Result<ScopedPickerFixture, String> {
    SCOPED_PICKER_FIXTURE.with(|current| {
        let mut current = current.borrow_mut();
        if current.is_some() {
            return Err("this UI thread already owns a picker fixture".into());
        }
        *current = Some(backend);
        Ok(ScopedPickerFixture {
            _owner_thread: std::marker::PhantomData,
        })
    })
}
#[cfg(any(test, feature = "test-support"))]
fn scoped_picker_fixture_backend() -> Option<ScopedPickerBackend> {
    SCOPED_PICKER_FIXTURE.with(|current| current.borrow().clone())
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for ScopedPickerFixture {
    fn drop(&mut self) {
        SCOPED_PICKER_FIXTURE.with(|current| *current.borrow_mut() = None);
    }
}

#[cfg(test)]
#[test]
fn writer_scoped_picker_fixture_is_thread_local_and_released_by_guard() {
    use std::sync::Arc;
    assert!(scoped_picker_fixture_backend().is_none());
    let guard = scoped_picker_fixture(Arc::new(|_| PortalOutcome::Cancelled)).unwrap();
    assert!(scoped_picker_fixture_backend().is_some());
    assert!(scoped_picker_fixture(Arc::new(|_| PortalOutcome::Cancelled)).is_err());
    assert!(
        std::thread::spawn(|| scoped_picker_fixture_backend().is_none())
            .join()
            .unwrap()
    );
    drop(guard);
    assert!(scoped_picker_fixture_backend().is_none());
}
