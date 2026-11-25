//! The file-chooser and URI-opening vocabulary: requests, outcomes and the URI
//! to path rule every backend shares.
//!
//! An app never draws its own file picker and never keys open or save off an
//! environment variable: it goes through the portal, so the host desktop
//! provides the chooser. On Tilo the picker is also where the per-workspace
//! sandbox file-access prompt lives, which makes this module the file-access
//! security boundary; a per-app copy would be a second, drifting definition of
//! which URIs may become paths. The URI mapping goes through
//! [`url::Url::to_file_path`], never a prefix strip that turns a remote host
//! into a relative path.
//!
//! The round trip opens a real host window, so it never runs on the UI thread:
//! [`PickRequest::send`] hands the request to the host's [`crate::FilePicker`],
//! which answers over an [`std::sync::mpsc`] channel the app drains once per
//! frame.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: portal and file dialogs block on their own thread"
)]

mod destination;
mod uri;
pub use destination::{DestinationChannel, DestinationOutcome, SaveDestination};
pub use uri::open_uri_scoped;
#[cfg(any(test, feature = "test-support"))]
pub use uri::{ScopedUriFixture, scoped_uri_fixture};

pub trait EffectCheck: Send + 'static {
    fn with_effect<R>(&self, effect: impl FnOnce() -> Result<R, String>) -> Result<R, String>;
}

use std::fs::File;
#[cfg(any(unix, windows))]
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalOutcome {
    Opened(PathBuf),
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultiOutcome {
    Selected(Vec<PathBuf>),
    Cancelled,
    Failed(String),
}

pub type PortalChannel = Receiver<PortalOutcome>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    Opened,
    Failed(String),
}

pub type LaunchChannel = Receiver<LaunchOutcome>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchKind {
    Open,
    Reveal,
}

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

    pub fn send(self) -> LaunchChannel {
        crate::current_picker().launch(self)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_reveal(&self) -> bool {
        self.kind == LaunchKind::Reveal
    }

    pub fn worker_name(&self) -> &str {
        &self.thread_name
    }
}

/// Open a file or folder again by the reference a choice answered
/// ([`ChosenHandle::reference`]): what a window restored on the file it showed does.
pub fn reopen(reference: &Path, write: bool) -> OpenFileChannel {
    crate::current_picker().reopen(reference, write)
}

pub fn open_uri(uri: &str) -> std::io::Result<()> {
    open_uri_with(&*crate::current_opener(), uri)
}

pub fn send_open_uri(uri: &str) -> LaunchChannel {
    let (tx, rx) = std::sync::mpsc::channel();
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

pub fn open_uri_with(opener: &dyn crate::UriOpener, uri: &str) -> std::io::Result<()> {
    if uri.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "URI contains NUL",
        ));
    }
    opener.open(uri)
}

pub struct OpenedFile {
    /// The file a native host opened.
    file: Option<File>,
    /// The bytes a page chose, where there are no descriptors to hand.
    bytes: Option<Vec<u8>>,
    display_name: String,
    /// What the chooser answers that reopens the same file: the documents-store path of a file a
    /// sandboxed app was given. `None` only for a host that answers a file it cannot name again.
    reference: Option<PathBuf>,
    /// What a save commits when its file is finished: renaming the chosen file over the one the
    /// person sees, so a save replaces it whole or not at all.
    commit: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for OpenedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedFile")
            .field("file", &self.file)
            .field("bytes", &self.bytes.as_ref().map(Vec::len))
            .field("display_name", &self.display_name)
            .field("reference", &self.reference)
            .field("commit", &self.commit.is_some())
            .finish()
    }
}

impl OpenedFile {
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// What the chooser answers to reopen the same file ([`crate::shell::ShellLink::reopen`]).
    #[must_use]
    pub fn reference(&self) -> Option<&Path> {
        self.reference.as_deref()
    }

    /// The file, and the commit that finishes a save when it is dropped.
    #[must_use]
    pub fn into_file(self) -> PickedFile {
        self.into_held()
    }

    /// The opened file and what names it again.
    #[must_use]
    pub fn into_parts(self) -> (PickedFile, Option<PathBuf>) {
        let reference = self.reference.clone();
        (self.into_held(), reference)
    }

    fn into_held(self) -> PickedFile {
        let Self {
            file,
            bytes,
            display_name: _,
            reference: _,
            commit,
        } = self;
        PickedFile {
            file,
            bytes: bytes.map(std::io::Cursor::new),
            commit,
        }
    }
}

/// A file a chooser opened, kept until what writes it is done: dropping it finishes a save. A
/// native host hands a descriptor; a page hands the bytes it read.
pub struct PickedFile {
    file: Option<File>,
    bytes: Option<std::io::Cursor<Vec<u8>>>,
    commit: Option<Box<dyn FnOnce() + Send>>,
}

impl PickedFile {
    #[allow(
        clippy::expect_used,
        reason = "a native host always opened a file; only a page holds bytes instead"
    )]
    fn file(&self) -> &File {
        self.file
            .as_ref()
            .expect("a native host opens a file; a page holds its bytes")
    }

    /// The file itself, without finishing a save when it goes.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a native host always opened a file; only a page holds bytes instead"
    )]
    pub fn into_std(mut self) -> File {
        self.commit = None;
        let file = self
            .file
            .take()
            .expect("a native host opens a file; a page holds its bytes");
        std::mem::forget(self);
        file
    }
}

impl std::fmt::Debug for PickedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickedFile")
            .field("file", &self.file)
            .field(
                "bytes",
                &self.bytes.as_ref().map(|bytes| bytes.get_ref().len()),
            )
            .field("commit", &self.commit.is_some())
            .finish()
    }
}

impl Drop for PickedFile {
    fn drop(&mut self) {
        if let Some(commit) = self.commit.take() {
            commit();
        }
    }
}

impl std::io::Read for PickedFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match (&mut self.file, &mut self.bytes) {
            (Some(file), _) => std::io::Read::read(file, buf),
            (None, Some(bytes)) => std::io::Read::read(bytes, buf),
            (None, None) => Err(no_file()),
        }
    }
}

impl std::io::Write for PickedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match (&mut self.file, &mut self.bytes) {
            (Some(file), _) => std::io::Write::write(file, buf),
            (None, Some(bytes)) => std::io::Write::write(bytes, buf),
            (None, None) => Err(no_file()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match (&mut self.file, &mut self.bytes) {
            (Some(file), _) => std::io::Write::flush(file),
            (None, Some(bytes)) => std::io::Write::flush(bytes),
            (None, None) => Err(no_file()),
        }
    }
}

impl std::io::Seek for PickedFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match (&mut self.file, &mut self.bytes) {
            (Some(file), _) => std::io::Seek::seek(file, pos),
            (None, Some(bytes)) => std::io::Seek::seek(bytes, pos),
            (None, None) => Err(no_file()),
        }
    }
}

#[cfg(unix)]
impl std::os::fd::AsRawFd for PickedFile {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(self.file())
    }
}

fn no_file() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this host holds no open file",
    )
}

impl PickedFile {
    /// Metadata of the file a native host opened; a page's bytes hold none.
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        match &self.file {
            Some(file) => file.metadata(),
            None => Err(no_file()),
        }
    }

    /// A file a page chose: its bytes, held as the file's content.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            file: None,
            bytes: Some(std::io::Cursor::new(bytes)),
            commit: None,
        }
    }
}

#[derive(Debug)]
pub enum OpenFileOutcome {
    Opened(OpenedFile),
    Cancelled,
    Failed(String),
}

pub type OpenFileChannel = Receiver<OpenFileOutcome>;

pub type MultiChannel = Receiver<MultiOutcome>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperTarget {
    Background,
    Lockscreen,
    Both,
}

pub type WallpaperChannel = Receiver<Result<(), String>>;

pub fn send_set_wallpaper(file: File, target: WallpaperTarget) -> WallpaperChannel {
    #[cfg(all(target_os = "linux", feature = "portal-client"))]
    {
        crate::xdg::set_wallpaper(file, target)
    }
    #[cfg(all(target_os = "linux", not(feature = "portal-client")))]
    {
        let _ = (file, target);
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(Err(
            "setting the wallpaper needs the desktop portal client".to_string()
        ));
        rx
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFilter {
    pub(crate) name: String,
    pub(crate) globs: Vec<String>,
    pub(crate) mimetypes: Vec<String>,
}

impl FileFilter {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn globs(&self) -> &[String] {
        &self.globs
    }

    #[must_use]
    pub fn glob(mut self, g: &str) -> Self {
        self.globs.push(g.to_string());
        self
    }

    #[must_use]
    pub fn mimetype(mut self, m: &str) -> Self {
        self.mimetypes.push(m.to_string());
        self
    }

    pub fn all_files() -> Self {
        Self::new("All files").glob("*")
    }

    #[must_use]
    pub fn mimetypes(&self) -> &[String] {
        &self.mimetypes
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub const SAVE_DIRECTORY_ENV: &str = "TILO_SAVE_DIRECTORY";

pub const WRITABLE_DIRECTORY_CHOICE: &str = "tilo-writable-directory";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickKind {
    Open { multiple: bool },
    Directory,
    Save { suggested_name: String },
}

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
    pub fn open(title: &str, accept_label: &str) -> Self {
        Self::new(title, accept_label, PickKind::Open { multiple: false })
    }

    pub fn open_multiple(title: &str, accept_label: &str) -> Self {
        Self::new(title, accept_label, PickKind::Open { multiple: true })
    }

    pub fn directory(title: &str, accept_label: &str) -> Self {
        Self::new(title, accept_label, PickKind::Directory)
    }

    pub fn writable_directory(title: &str, accept_label: &str) -> Self {
        Self {
            writable_directory: true,
            ..Self::directory(title, accept_label)
        }
    }

    pub fn is_writable_directory(&self) -> bool {
        self.writable_directory
    }

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

    #[must_use]
    pub fn initial_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.initial_directory = Some(directory.into());
        self
    }

    pub fn directory_hint(&self) -> Option<&Path> {
        self.initial_directory.as_deref()
    }

    /// Bound what the app reads of a choice into memory: an Open read past this fails rather
    /// than growing without limit. A folder's files count together.
    #[must_use]
    pub fn max_contents_bytes(mut self, limit: usize) -> Self {
        self.max_contents_bytes = Some(limit);
        self
    }

    #[must_use]
    pub fn contents_byte_limit(&self) -> Option<usize> {
        self.max_contents_bytes
    }

    #[must_use]
    pub fn thread_name(mut self, name: &str) -> Self {
        self.thread_name = name.to_string();
        self
    }

    #[must_use]
    pub fn filter(mut self, f: FileFilter) -> Self {
        self.filters.push(f);
        self
    }

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

    pub fn is_open(&self) -> bool {
        matches!(self.kind, PickKind::Open { .. })
    }

    pub fn filter_names(&self) -> Vec<&str> {
        self.filters.iter().map(FileFilter::name).collect()
    }

    pub fn suggested_name(&self) -> Option<&str> {
        match &self.kind {
            PickKind::Save { suggested_name } => Some(suggested_name.as_str()),
            PickKind::Open { .. } | PickKind::Directory => None,
        }
    }

    pub fn accept_label(&self) -> &str {
        &self.accept_label
    }

    pub fn filters(&self) -> &[FileFilter] {
        &self.filters
    }

    pub fn is_directory(&self) -> bool {
        matches!(self.kind, PickKind::Directory)
    }

    pub fn kind(&self) -> &PickKind {
        &self.kind
    }

    pub fn worker_name(&self) -> &str {
        &self.thread_name
    }

    pub fn send(self) -> PortalChannel {
        crate::current_picker().pick(self)
    }

    pub fn send_scoped(self, scope: impl EffectCheck) -> PortalChannel {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(backend) = scoped_picker_fixture_backend() {
            return scoped_pick_worker(scope, move || backend(self));
        }
        let files = crate::current_picker();
        scoped_pick_worker(scope, move || {
            files.pick(self).recv().unwrap_or_else(|_| {
                PortalOutcome::Failed("The file chooser closed before you chose. Try again.".into())
            })
        })
    }

    /// The SDK's handle path only: an app reads a chosen file or walks a chosen folder through
    /// [`ChosenHandle`].
    #[doc(hidden)]
    pub fn send_open_handle(self) -> OpenFileChannel {
        crate::current_picker().open_handle(self)
    }

    pub fn send_save_file(self) -> OpenFileChannel {
        crate::current_picker().save_file(self)
    }

    pub fn send_multiple(self) -> MultiChannel {
        crate::current_picker().pick_multiple(self)
    }

    /// The SDK's save path only: an app saves through `EffectCx::choose_destination`,
    /// whose job waits for the answer and tells the person when the chooser fails.
    #[doc(hidden)]
    pub fn send_save_destination(self) -> DestinationChannel {
        crate::current_picker().save_destination(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    Cancelled,
    Failed(String),
}

pub type SaveChannel = Receiver<SaveOutcome>;

const CHOOSER_GONE: &str = "The file chooser closed before you chose. Try again.";

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

fn scoped_pick_worker(
    scope: impl EffectCheck,
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

#[cfg(any(windows, target_os = "android"))]
pub fn spawn_worker(name: &str, work: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(work);
}

#[cfg(any(unix, windows))]
pub fn to_local_path(uri: &url::Url) -> Option<PathBuf> {
    use std::path::Component;

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

    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return None;
    }

    let segments = uri.path_segments()?.filter(|s| !s.is_empty()).count();
    let components = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    #[cfg(windows)]
    let components = components.checked_add(1)?;
    if components != segments {
        return None;
    }

    Some(path)
}

#[cfg(not(any(unix, windows)))]
#[must_use]
pub fn to_local_path(_uri: &url::Url) -> Option<PathBuf> {
    None
}

pub fn map_uri(uris: &[url::Url]) -> PortalOutcome {
    let Some(uri) = uris.first() else {
        return PortalOutcome::Cancelled;
    };
    match to_local_path(uri) {
        Some(path) => PortalOutcome::Opened(path),
        None => PortalOutcome::Failed(format!("not a local file: {uri}")),
    }
}

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

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[cfg_attr(
    any(target_os = "linux", windows),
    allow(
        dead_code,
        reason = "a Linux host opens chosen files by descriptor; the others by URI"
    )
)]
pub(crate) fn open_selected_uri(uri: &url::Url) -> OpenFileOutcome {
    let Some(path) = to_local_path(uri) else {
        return OpenFileOutcome::Failed(format!("not a local file: {uri}"));
    };
    match open_regular_file(&path) {
        Ok(file) => OpenFileOutcome::Opened(OpenedFile::at(file, &path)),
        Err(error) => OpenFileOutcome::Failed(format!("could not open selected file: {error}")),
    }
}

pub(crate) fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "selected file".to_string())
}

/// The files under a chosen folder, walked through the folder's own descriptor: what an app's
/// folder chooser hands on, never a host path. Each is named by where it stands in the folder,
/// symlinks and pipes are skipped, and the walk stops with an error once more than `limit` bytes
/// would be read.
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one walk keeps the descriptor discipline a reader checks in one place"
)]
pub fn walk_chosen_folder(folder: &File, limit: usize) -> Result<Vec<(String, Vec<u8>)>, String> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    /// The most files one folder choice hands over.
    const MAX_FILES: usize = 10_000;

    struct Dir(*mut libc::DIR);
    impl Drop for Dir {
        fn drop(&mut self) {
            // SAFETY: the directory stream was opened by `fdopendir` and is closed once.
            unsafe { libc::closedir(self.0) };
        }
    }

    fn walk(
        fd: std::os::fd::RawFd,
        prefix: &str,
        limit: &mut usize,
        out: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        // SAFETY: duplicating a descriptor this walk owns; the copy is closed by `Dir`.
        let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if dup < 0 {
            return Err("the chosen folder can't be read".to_owned());
        }
        // SAFETY: `dup` is a fresh descriptor to a directory, and `fdopendir` takes it over.
        let raw = unsafe { libc::fdopendir(dup) };
        if raw.is_null() {
            // SAFETY: `fdopendir` failed, so the descriptor is still this call's to close.
            unsafe { libc::close(dup) };
            return Err("the chosen folder can't be read".to_owned());
        }
        let dir = Dir(raw);
        loop {
            // SAFETY: `readdir` reads the stream; the entry it answers is valid until the next
            // call, and is copied out before one.
            let entry = unsafe { libc::readdir(dir.0) };
            if entry.is_null() {
                break;
            }
            // SAFETY: `d_name` is a NUL-terminated name inside the entry.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let Ok(cname) = std::ffi::CString::new(name.as_str()) else {
                continue;
            };
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if out.len() >= MAX_FILES {
                return Err("the chosen folder holds too many files".to_owned());
            }
            // The entry's own type, or the file's when the filesystem does not say.
            let mut kind = unsafe { (*entry).d_type };
            if kind == libc::DT_UNKNOWN {
                // SAFETY: `fstatat` writes the buffer it is given, for the name at `fd`.
                let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
                let asked = unsafe {
                    libc::fstatat(fd, cname.as_ptr(), &raw mut stat, libc::AT_SYMLINK_NOFOLLOW)
                };
                if asked != 0 {
                    continue;
                }
                kind = if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                    libc::DT_DIR
                } else if stat.st_mode & libc::S_IFMT == libc::S_IFREG {
                    libc::DT_REG
                } else {
                    libc::DT_LNK
                };
            }
            if kind == libc::DT_DIR {
                // SAFETY: opening a name inside the chosen folder, never following a link out.
                let sub = unsafe {
                    libc::openat(
                        fd,
                        cname.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if sub < 0 {
                    continue;
                }
                let walked = walk(sub, &relative, limit, out);
                // SAFETY: the descriptor was opened by this walk and nothing else holds it.
                unsafe { libc::close(sub) };
                walked?;
            } else if kind == libc::DT_REG {
                // SAFETY: opening a regular name inside the chosen folder, never a link.
                let file = unsafe {
                    libc::openat(
                        fd,
                        cname.as_ptr(),
                        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if file < 0 {
                    continue;
                }
                // SAFETY: `file` is a fresh descriptor this walk owns.
                let mut file = unsafe { File::from_raw_fd(file) };
                let Ok(metadata) = file.metadata() else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
                if size > *limit {
                    return Err(
                        "Those files are too large for the app to read. Choose fewer or smaller files."
                            .to_owned(),
                    );
                }
                let mut bytes = Vec::with_capacity(size);
                if std::io::Read::read_to_end(&mut file, &mut bytes).is_err() {
                    continue;
                }
                *limit = limit.saturating_sub(bytes.len());
                out.push((relative, bytes));
            }
        }
        Ok(())
    }

    let mut limit = limit;
    let mut out = Vec::new();
    walk(folder.as_raw_fd(), "", &mut limit, &mut out)?;
    Ok(out)
}

/// [`walk_chosen_folder`] on Windows: the folder the handle is open on, walked by its final
/// path. Links and junctions are skipped, never followed out of the folder.
#[cfg(windows)]
pub fn walk_chosen_folder(folder: &File, limit: usize) -> Result<Vec<(String, Vec<u8>)>, String> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    /// The most files one folder choice hands over.
    const MAX_FILES: usize = 10_000;

    fn walk(
        dir: &Path,
        prefix: &str,
        limit: &mut usize,
        out: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        let entries =
            std::fs::read_dir(dir).map_err(|_| "the chosen folder can't be read".to_owned())?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if out.len() >= MAX_FILES {
                return Err("the chosen folder holds too many files".to_owned());
            }
            // The entry itself, never what a link or junction points at.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                walk(&entry.path(), &relative, limit, out)?;
            } else if kind.is_file() {
                let Ok(mut file) = File::open(entry.path()) else {
                    continue;
                };
                let Ok(metadata) = file.metadata() else {
                    continue;
                };
                let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
                if size > *limit {
                    return Err(
                        "Those files are too large for the app to read. Choose fewer or smaller files."
                            .to_owned(),
                    );
                }
                let mut bytes = Vec::with_capacity(size);
                if std::io::Read::read_to_end(&mut file, &mut bytes).is_err() {
                    continue;
                }
                *limit = limit.saturating_sub(bytes.len());
                out.push((relative, bytes));
            }
        }
        Ok(())
    }

    let mut wide = vec![0_u16; 32_768];
    // SAFETY: the folder's live handle and a buffer of the length given.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            folder.as_raw_handle(),
            wide.as_mut_ptr(),
            u32::try_from(wide.len()).unwrap_or(u32::MAX),
            0,
        )
    };
    let length = usize::try_from(length).unwrap_or(usize::MAX);
    if length == 0 || length >= wide.len() {
        return Err("the chosen folder can't be read".to_owned());
    }
    wide.truncate(length);
    let path =
        PathBuf::from(<std::ffi::OsString as std::os::windows::ffi::OsStringExt>::from_wide(&wide));
    let mut limit = limit;
    let mut out = Vec::new();
    walk(&path, "", &mut limit, &mut out)?;
    Ok(out)
}

/// The chosen folder `path`, kept open for reading: what a host hands an app that walks a folder
/// it was given. Opening the folder itself, never a link to one.
#[cfg(unix)]
pub fn open_directory_handle(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "selected path is not a folder",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
pub fn open_directory_handle(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "selected path is not a folder",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "a page opens no folder, and answers the same fallible call"
)]
pub fn open_directory_handle(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this host opens no folders",
    ))
}

/// Where the chooser names what it answers with: every reference is a path under it, whether the
/// choice is a file of a workspace's tree or one kept in the device's records.
pub const DOCUMENTS_DIR: &str = "/run/tilo/chosen";

/// Whether `path` names a chosen document rather than a file on the host: what a sandboxed app
/// is handed, and what a session holding the choices can reopen after a restart.
#[must_use]
pub fn is_document_reference(path: &Path) -> bool {
    let Some(rest) = path.strip_prefix(DOCUMENTS_DIR).ok() else {
        return false;
    };
    let mut parts = rest.components();
    // A token names the document; at least the document's own name follows it.
    matches!(parts.next(), Some(std::path::Component::Normal(_)))
        && matches!(parts.next(), Some(std::path::Component::Normal(_)))
}

pub fn save_sibling(destination: &Path) -> Option<PathBuf> {
    let name = destination.file_name()?;
    let mut sibling = std::ffi::OsString::from(".");
    sibling.push(name);
    sibling.push(".tilo-save");
    Some(destination.with_file_name(sibling))
}

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
    #[must_use]
    pub fn new(file: File, display_name: impl Into<String>) -> Self {
        Self {
            file: Some(file),
            bytes: None,
            display_name: display_name.into(),
            reference: None,
            commit: None,
        }
    }

    /// A file opened at `path` on a host whose files have paths: the path names it again.
    #[must_use]
    pub fn at(file: File, path: &Path) -> Self {
        Self {
            file: Some(file),
            bytes: None,
            display_name: file_name_of(path),
            reference: Some(path.to_path_buf()),
            commit: None,
        }
    }

    /// A file a page chose: the bytes it read, named by the name the chooser answered. A page
    /// keeps no descriptor, so the bytes are the handle's form.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>, display_name: impl Into<String>, reference: PathBuf) -> Self {
        Self {
            file: None,
            bytes: Some(bytes),
            display_name: display_name.into(),
            reference: Some(reference),
            commit: None,
        }
    }

    /// A file or folder the chooser reopened by its reference, called what the chooser calls it.
    #[must_use]
    pub(crate) fn named(file: File, display_name: impl Into<String>, reference: PathBuf) -> Self {
        Self {
            file: Some(file),
            bytes: None,
            display_name: display_name.into(),
            reference: Some(reference),
            commit: None,
        }
    }

    /// What a save commits when its file is finished: renaming the file it writes over the one
    /// the person chose, so it is replaced whole or not at all.
    #[cfg(any(unix, windows))]
    #[must_use]
    pub(crate) fn keeping(
        file: File,
        display_name: impl Into<String>,
        reference: PathBuf,
        commit: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            file: Some(file),
            bytes: None,
            display_name: display_name.into(),
            reference: Some(reference),
            commit: Some(Box::new(commit)),
        }
    }
}

/// A chosen file or folder, kept open: what a host hands an app for a file it reads in place.
#[derive(Debug)]
pub struct ChosenHandle {
    file: PickedFile,
    reference: PathBuf,
    name: String,
}

impl ChosenHandle {
    /// A handle on a file the host keeps open, named by what reopens it.
    #[must_use]
    pub fn new(file: File, reference: PathBuf, name: impl Into<String>) -> Self {
        Self {
            file: PickedFile {
                file: Some(file),
                bytes: None,
                commit: None,
            },
            reference,
            name: name.into(),
        }
    }

    /// [`Self::new`] for the file a chooser answered, with what finishes it when it goes.
    #[must_use]
    pub fn from_picked(file: PickedFile, reference: PathBuf, name: impl Into<String>) -> Self {
        Self {
            file,
            reference,
            name: name.into(),
        }
    }

    /// The opened file, or the opened folder.
    #[must_use]
    pub fn file(&self) -> &File {
        self.file.as_ref()
    }

    #[must_use]
    pub fn into_file(self) -> PickedFile {
        self.file
    }

    /// What the chooser answers to reopen the same file or folder.
    #[must_use]
    pub fn reference(&self) -> &Path {
        &self.reference
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl PickedFile {
    fn as_ref(&self) -> &File {
        self.file()
    }
}

impl PortalOutcome {
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

    tilo_test_env::scenario! {
        fn a_request_records_what_the_app_asked_for_and_an_outcome_what_was_picked;

        phase writable_directory_is_explicit_and_preserves_directory_shape;
        {
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

        phase directory_request_is_single_select_and_has_no_filename;
        {
            let request = PickRequest::directory("Choose folder", "Choose");
            assert!(!request.is_multiple());
            assert_eq!(request.suggested_name(), None);
            assert!(matches!(request.kind, PickKind::Directory));
        }

        phase directory_hint_preserves_the_requested_folder_without_granting_it;
        {
            let home = local_path("/Users/you/Recognized home");
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

        phase launch_request_records_open_and_reveal_without_changing_the_path;
        {
            let open = LaunchRequest::open("/tmp/archive.zip");
            assert_eq!(open.path, Path::new("/tmp/archive.zip"));
            assert_eq!(open.kind, LaunchKind::Open);
            let reveal = LaunchRequest::reveal("/tmp/archive.zip");
            assert_eq!(reveal.path, Path::new("/tmp/archive.zip"));
            assert_eq!(reveal.kind, LaunchKind::Reveal);
        }

        phase outcome_path_accessor_reports_only_a_real_pick;
        {
            assert_eq!(
                PortalOutcome::Opened(PathBuf::from("/tmp/a")).path(),
                Some(Path::new("/tmp/a"))
            );
            assert_eq!(PortalOutcome::Cancelled.path(), None);
            assert_eq!(PortalOutcome::Failed("x".into()).path(), None);
        }

        phase builder_records_the_per_app_delta;
        {
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

    tilo_test_env::scenario! {
        fn a_file_uri_becomes_a_local_path_only_when_it_names_one;

        phase rejects_a_uri_that_names_a_remote_host;
        {
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

        phase a_rejected_host_never_yields_a_relative_path;
        {
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

        phase localhost_is_the_local_machine;
        {
            assert_eq!(
                map_uri(&[u("file://localhost/home/alice/a.mkv")]),
                PortalOutcome::Opened(local_path("/home/alice/a.mkv"))
            );
        }

        phase percent_escapes_in_filenames_are_decoded;
        {
            assert_eq!(
                map_uri(&[u("file:///home/alice/My%20Clip.mp4")]),
                PortalOutcome::Opened(local_path("/home/alice/My Clip.mp4"))
            );
            assert_eq!(
                map_uri(&[u("file:///home/alice/%C3%A9t%C3%A9%20caf%C3%A9.mp3")]),
                PortalOutcome::Opened(local_path("/home/alice/été café.mp3"))
            );
            assert_eq!(
                map_uri(&[u("file:///home/alice/a%23b%3Fc%25d.txt")]),
                PortalOutcome::Opened(local_path("/home/alice/a#b?c%d.txt"))
            );
        }

        phase a_plus_in_a_path_stays_a_plus;
        {
            assert_eq!(
                map_uri(&[u("file:///home/alice/my+song.mp3")]),
                PortalOutcome::Opened(local_path("/home/alice/my+song.mp3"))
            );
        }

        phase an_escaped_separator_does_not_become_a_new_path_component;
        {
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

        phase a_parent_hop_hidden_inside_one_segment_is_rejected;
        {
            for s in [
                "file:///home/alice/%2E%2E%2F%2E%2E%2Fetc/shadow",
                "file:///var/workspace/%2E%2E%2Fetc/shadow",
            ] {
                assert_eq!(to_local_path(&u(s)), None, "{s} must not resolve");
                assert!(matches!(map_uri(&[u(s)]), PortalOutcome::Failed(_)), "{s}");
            }
        }

        phase a_dot_dot_segment_is_resolved_by_uri_normalisation_not_smuggled;
        {
            let uri = u("file:///home/alice/%2E%2E/%2E%2E/etc/shadow");
            assert_eq!(
                to_local_path(&uri),
                Some(local_path("/etc/shadow")),
                "the URI normalises on parse"
            );
            assert_eq!(to_local_path(&uri), Some(local_path("/etc/shadow")));
        }

        phase ordinary_nested_paths_still_resolve;
        {
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

        phase workspace_paths_resolve_through_the_fuse_mount;
        {
            assert_eq!(
                map_uri(&[u("file:///var/workspace/media/a.mkv")]),
                PortalOutcome::Opened(local_path("/var/workspace/media/a.mkv"))
            );
        }
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

    tilo_test_env::scenario! {
        fn a_selection_maps_to_the_local_files_it_names;

        phase empty_is_cancel;
        {
            assert_eq!(map_uri(&[]), PortalOutcome::Cancelled);
            assert_eq!(map_uris(&[]), MultiOutcome::Cancelled);
        }

        phase single_takes_the_first_local_file;
        {
            let uris = [
                u("file:///home/alice/photo.png"),
                u("file:///home/alice/other.png"),
            ];
            assert_eq!(
                map_uri(&uris),
                PortalOutcome::Opened(local_path("/home/alice/photo.png"))
            );
        }

        phase multi_collects_every_local_file;
        {
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

        phase multi_skips_remote_but_keeps_local;
        {
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

        phase multi_all_remote_is_failure;
        {
            assert!(matches!(
                map_uris(&[
                    u("https://example.com/a.vcf"),
                    u("https://example.com/b.vcf")
                ]),
                MultiOutcome::Failed(_)
            ));
        }
    }

    tilo_test_env::scenario! {
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        fn descriptor_open_holds_the_file_it_opened_and_refuses_a_directory;

        phase descriptor_open_retains_the_opened_file_and_a_separate_display_name;
        {
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
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, b"replacement bytes").unwrap();
            let mut contents = String::new();
            let mut file = opened.into_file();
            file.read_to_string(&mut contents).unwrap();
            assert_eq!(contents, "installer bytes");
        }

        phase descriptor_open_refuses_a_non_regular_file;
        {
            let dir = tempfile::tempdir().unwrap();
            let uri = url::Url::from_file_path(dir.path()).unwrap();

            assert!(matches!(
                open_selected_uri(&uri),
                OpenFileOutcome::Failed(_)
            ));
        }
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
}
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
