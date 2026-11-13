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

pub trait EffectGate: Send + 'static {
    fn with_effect<R>(&self, effect: impl FnOnce() -> Result<R, String>) -> Result<R, String>;
}

use std::fs::File;
#[cfg(any(unix, windows))]
use std::fs::OpenOptions;
use std::io::Read;
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

#[derive(Debug)]
pub struct OpenedFile {
    file: File,
    display_name: String,
}

impl OpenedFile {
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub fn into_file(self) -> File {
        self.file
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

    #[must_use]
    pub fn max_contents_bytes(mut self, limit: usize) -> Self {
        self.max_contents_bytes = Some(limit);
        self
    }

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

    pub fn send_scoped(self, scope: impl EffectGate) -> PortalChannel {
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

    pub fn send_open_file(self) -> OpenFileChannel {
        crate::current_picker().open_file(self)
    }

    pub fn send_save_file(self) -> OpenFileChannel {
        crate::current_picker().save_file(self)
    }

    pub fn send_multiple(self) -> MultiChannel {
        crate::current_picker().pick_multiple(self)
    }

    pub fn send_contents(self) -> ContentsChannel {
        crate::current_picker().pick_contents(self)
    }

    pub fn send_save_contents(self, bytes: Vec<u8>) -> SaveChannel {
        crate::current_picker().save_contents(self, bytes)
    }

    pub fn send_save_destination(self) -> DestinationChannel {
        crate::current_picker().save_destination(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedContents {
    pub name: String,
    pub media_type: Option<String>,
    pub bytes: Vec<u8>,
    pub location: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    Contents(PickedContents),
    NotRead {
        name: String,
        size: u64,
        why: NotRead,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotRead {
    TooLarge,
    TooMany,
}

thread_local! {
    static DROPPED: std::cell::RefCell<Option<Dropped>> =
        const { std::cell::RefCell::new(None) };
}

pub fn dropping<R>(dropped: Dropped, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<Dropped>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            DROPPED.with_borrow_mut(|slot| *slot = previous);
        }
    }
    let _restore = Restore(DROPPED.with_borrow_mut(|slot| slot.replace(dropped)));
    run()
}

pub fn dropped() -> Option<Dropped> {
    DROPPED.with_borrow_mut(Option::take)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentsOutcome {
    Picked(Vec<PickedContents>),
    Cancelled,
    Failed(String),
}

pub type ContentsChannel = Receiver<ContentsOutcome>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    Cancelled,
    Failed(String),
}

pub type SaveChannel = Receiver<SaveOutcome>;

const CHOOSER_GONE: &str = "The file chooser closed before you chose. Try again.";

pub(crate) enum PickedPaths {
    One(PortalChannel),
    Many(MultiChannel),
}

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
    file_name_of(path)
}

pub(crate) fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "selected file".to_string())
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
            file,
            display_name: display_name.into(),
        }
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
        assert_eq!(
            map_uri(&[u("file://localhost/home/alice/a.mkv")]),
            PortalOutcome::Opened(local_path("/home/alice/a.mkv"))
        );
    }

    #[test]
    fn percent_escapes_in_filenames_are_decoded() {
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

    #[test]
    fn a_plus_in_a_path_stays_a_plus() {
        assert_eq!(
            map_uri(&[u("file:///home/alice/my+song.mp3")]),
            PortalOutcome::Opened(local_path("/home/alice/my+song.mp3"))
        );
    }

    #[test]
    fn an_escaped_separator_does_not_become_a_new_path_component() {
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

#[cfg(test)]
#[test]
fn a_dropped_files_contents_are_taken_once_while_its_event_is_handled() {
    let file = PickedContents {
        name: "notes.txt".into(),
        media_type: Some("text/plain".into()),
        bytes: b"hi".to_vec(),
        location: None,
    };
    assert_eq!(dropped(), None, "nothing outside a drop");
    let file = Dropped::Contents(file);
    let taken = dropping(file.clone(), || (dropped(), dropped()));
    assert_eq!(taken, (Some(file), None));
    assert_eq!(dropped(), None, "nothing outlives the dispatch");
}
