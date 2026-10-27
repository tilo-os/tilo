//! The files a person chose for an app in the system file chooser.
//!
//! Choosing a file in the session shell's chooser is the person's consent,
//! and it gives the app that asked exactly what was chosen: the file to read
//! for an Open, the file to write (created if it is new) for a Save, the
//! folder and everything under it for a folder. Nothing else.
//!
//! A sandboxed app's mount namespace is fixed when its sandbox starts, so a
//! choice made while it runs cannot add a bind to it. Instead the shell keeps
//! one filesystem, mounted before any app starts, and the launcher binds each
//! app's own view of it into the app's sandbox at [`SANDBOX_DIR`]. The view
//! starts empty; a choice adds a document to it, visible inside at once
//! (`/run/tilo/documents/<token>/<name>`). This is how `xdg-document-portal`
//! solves the same problem; `docs/architecture/security.md` ("Filesystem")
//! records why it is this and not a launch-time grant or a live re-bind.
//!
//! An app's view is keyed by the room it was launched into and its package
//! ([`AppKey`]), so the same app in another workspace sees none of it, and no
//! app can reach another's view: only its own subtree is bound. Documents
//! stay private to this device ([`Documents::kept_in`] keeps them across
//! sessions in a private file, so a recent file reopens where it was): they
//! are never written into the room's shared capability list, which would
//! replicate a private choice to every peer and widen the app's next launch.
//!
//! [`Documents`] is the table both sides share; [`host`] turns a path inside
//! an app's view into the host file it names, refusing anything a choice did
//! not cover; `mount` (feature `mount`, Linux) serves the table over FUSE.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, RwLock};

// Opening beneath a chosen item takes Linux's `openat2` (`RESOLVE_BENEATH`).
#[cfg(target_os = "linux")]
pub mod host;
#[cfg(all(target_os = "linux", any(feature = "mount", test)))]
pub mod mount;

/// Where an app finds its documents inside its sandbox.
pub const SANDBOX_DIR: &str = "/run/tilo/documents";

/// The directory under the session's runtime directory the shell mounts
/// the documents filesystem on.
const MOUNT_SUBDIR: &str = "tilo/documents";

/// Where the session's documents filesystem is mounted on the host:
/// `$XDG_RUNTIME_DIR/tilo/documents`. `None` without an absolute runtime
/// directory, where no session shell serves one.
pub fn mountpoint() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    runtime.is_absolute().then(|| runtime.join(MOUNT_SUBDIR))
}

/// The host directory that is `key`'s view, under the filesystem mounted at
/// `mountpoint`: what the launcher binds at [`SANDBOX_DIR`].
pub fn app_view(mountpoint: &Path, key: &AppKey) -> PathBuf {
    mountpoint.join(&key.room).join(&key.app)
}

/// Whose documents: the room an app was launched into and its package.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppKey {
    room: String,
    app: String,
}

impl AppKey {
    /// The key for `app` launched into `room`, or `None` when either is not
    /// a name the documents filesystem can carry as one path component (a
    /// room id is letters, digits and `-`; a package id adds `.` and `_`,
    /// and does not start with `.`).
    pub fn new(room: &str, app: &str) -> Option<Self> {
        (is_room(room) && is_app(app)).then(|| Self {
            room: room.to_owned(),
            app: app.to_owned(),
        })
    }

    pub fn room(&self) -> &str {
        &self.room
    }

    pub fn app(&self) -> &str {
        &self.app
    }
}

pub(crate) fn is_room(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub(crate) fn is_app(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// What a choice gave the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// An existing file, to read: an Open.
    Read,
    /// One file to write, created if it does not exist yet: a Save. Its
    /// save sibling (`.<name>.tilo-save`, `tilo_dialogs::portal::save_sibling`)
    /// may be written and renamed over it, so a save replaces the file
    /// whole or not at all.
    Save,
    /// A folder and everything under it: to read, or (`write`) to change.
    Folder { write: bool },
}

impl Kind {
    fn of_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "read" => Self::Read,
            "save" => Self::Save,
            "folder" => Self::Folder { write: false },
            "folder-write" => Self::Folder { write: true },
            _ => return None,
        })
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Save => "save",
            Self::Folder { write: false } => "folder",
            Self::Folder { write: true } => "folder-write",
        }
    }

    /// Whether the document may be changed.
    pub fn writes(self) -> bool {
        matches!(self, Self::Save | Self::Folder { write: true })
    }
}

/// One choice: the host file or folder, and what the app may do with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    /// The chosen file or folder on the host, resolved when it was chosen.
    /// For a [`Kind::Save`] of a new file, the resolved folder joined with
    /// the chosen name.
    pub host: PathBuf,
    pub kind: Kind,
}

impl Document {
    /// The name the app sees it by: the chosen file's or folder's own.
    pub fn name(&self) -> &OsStr {
        self.host
            .file_name()
            .unwrap_or_else(|| OsStr::new("document"))
    }

    /// The names a [`Kind::Save`] may write besides its own: its save
    /// sibling.
    pub fn save_sibling(&self) -> Option<OsString> {
        (self.kind == Kind::Save).then(|| {
            let mut sibling = OsString::from(".");
            sibling.push(self.name());
            sibling.push(".tilo-save");
            sibling
        })
    }
}

/// Why a choice could not be given to the app.
#[derive(Debug)]
pub enum GrantError {
    /// What was chosen is not what the chooser asked for (a folder for a
    /// file, a missing file for an Open, a symbolic link for a Save).
    NotThatKind,
    /// What was chosen could not be resolved on the host.
    Io(std::io::Error),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotThatKind => f.write_str("the chosen item is not what was asked for"),
            Self::Io(error) => write!(f, "the chosen item cannot be reached: {error}"),
        }
    }
}

impl std::error::Error for GrantError {}

/// What one app in one room was given, and the order it was chosen in.
#[derive(Default)]
struct View {
    documents: BTreeMap<String, Document>,
    /// The tokens of `documents`, most recently chosen first.
    recent: Vec<String>,
}

impl View {
    /// Add `document` as the latest choice. Answers whether the view
    /// changed.
    fn choose(&mut self, token: String, document: Document) -> bool {
        if self.recent.first() == Some(&token) {
            return false;
        }
        self.recent.retain(|kept| *kept != token);
        self.recent.insert(0, token.clone());
        self.documents.insert(token, document);
        true
    }

    /// Add `document` behind those already in the view, as reading a kept
    /// table (newest first) does.
    fn keep_older(&mut self, token: String, document: Document) {
        if self.documents.insert(token.clone(), document).is_none() {
            self.recent.push(token);
        }
    }

    fn by_recency(&self) -> impl Iterator<Item = &Document> {
        self.recent
            .iter()
            .filter_map(|token| self.documents.get(token))
    }
}

type Views = HashMap<AppKey, View>;

/// Every app's documents. Cheap to clone: clones share the table.
#[derive(Clone, Default)]
pub struct Documents {
    views: Arc<RwLock<Views>>,
    /// Where the table is kept between sessions, when it is.
    record: Option<Arc<PathBuf>>,
    /// Open while the kept table is still being read: every use of the
    /// table waits for it, so none sees, or writes over, half a table.
    loading: Arc<Loading>,
}

/// Whether the kept table is still being read.
#[derive(Default)]
struct Loading {
    pending: Mutex<bool>,
    done: Condvar,
}

impl Loading {
    fn pending() -> Self {
        Self {
            pending: Mutex::new(true),
            done: Condvar::new(),
        }
    }

    fn wait(&self) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        while *pending {
            match self.done.wait(pending) {
                Ok(next) => pending = next,
                Err(_) => return,
            }
        }
    }

    fn finish(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            *pending = false;
        }
        self.done.notify_all();
    }
}

/// The kept table: what each app in each room was given, each app's
/// documents newest first.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Record {
    documents: Vec<Kept>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
    room: String,
    app: String,
    kind: String,
    host: PathBuf,
}

impl Documents {
    /// A table for this session only.
    pub fn new() -> Self {
        Self::default()
    }

    /// A table kept in `record`, a private file on this device, so a file
    /// chosen in an earlier session reopens where it was. Like the
    /// person's other portal answers it stays on this device: it is never
    /// written into a room, which would replicate a private choice to every
    /// peer and widen what the app is given at launch. A kept item that is
    /// gone or no longer what it was chosen as is dropped when read.
    ///
    /// The record is read on a thread of its own, so a session starts
    /// without waiting on the disk; anything that uses the table before it
    /// is in waits for it.
    pub fn kept_in(record: PathBuf) -> Self {
        let documents = Self {
            views: Arc::default(),
            record: Some(Arc::new(record)),
            loading: Arc::new(Loading::pending()),
        };
        let loader = documents.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-documents-load".into())
            .spawn(move || loader.load());
        if spawned.is_err() {
            // No thread to read it on: read it here rather than lose it.
            documents.load();
        }
        documents
    }

    /// Read the kept record into the table, behind anything chosen since.
    fn load(&self) {
        let Some(record) = self.record.as_deref() else {
            self.loading.finish();
            return;
        };
        let mut loaded = Vec::new();
        let kept = std::fs::read(record)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
            .unwrap_or_default();
        for kept in kept.documents {
            let (Some(key), Some(kind)) =
                (AppKey::new(&kept.room, &kept.app), Kind::of_tag(&kept.kind))
            else {
                continue;
            };
            let Ok(host) = resolve_choice(&kept.host, kind) else {
                continue;
            };
            loaded.push((key, Document { host, kind }));
        }
        if let Ok(mut views) = self.views.write() {
            for (key, document) in loaded {
                let token = token(&key, &document);
                views.entry(key).or_default().keep_older(token, document);
            }
        }
        self.loading.finish();
    }

    /// Write the table to its record, when it is kept.
    fn keep(&self) {
        let Some(record) = self.record.as_deref() else {
            return;
        };
        self.loading.wait();
        let Some(snapshot) = self.views.read().ok().map(|views| Record {
            documents: views
                .iter()
                .flat_map(|(key, view)| {
                    view.by_recency().map(|document| Kept {
                        room: key.room.clone(),
                        app: key.app.clone(),
                        kind: document.kind.tag().to_owned(),
                        host: document.host.clone(),
                    })
                })
                .collect(),
        }) else {
            return;
        };
        // A record that cannot be written leaves this session's table as
        // it is; only the next session forgets.
        let _ = write_private(record, &snapshot);
    }

    /// Give `key` the item the person chose at `chosen`, as `kind`, and
    /// answer the path the app reaches it by inside its sandbox. Choosing
    /// the same item again answers the same path, so a recent file reopens
    /// where it was, and makes it the most recent.
    ///
    /// # Errors
    ///
    /// [`GrantError::NotThatKind`] when `chosen` is not what `kind` needs;
    /// [`GrantError::Io`] when it cannot be resolved.
    pub fn grant(&self, key: &AppKey, chosen: &Path, kind: Kind) -> Result<PathBuf, GrantError> {
        let host = resolve_choice(chosen, kind)?;
        let document = Document { host, kind };
        let token = token(key, &document);
        let path = Path::new(SANDBOX_DIR).join(&token).join(document.name());
        self.loading.wait();
        let changed = self.views.write().is_ok_and(|mut views| {
            views
                .entry(key.clone())
                .or_default()
                .choose(token, document)
        });
        if changed {
            self.keep();
        }
        Ok(path)
    }

    /// The document `token` of `key`'s view, while it is given.
    pub fn document(&self, key: &AppKey, token: &str) -> Option<Document> {
        self.loading.wait();
        self.views
            .read()
            .ok()?
            .get(key)
            .and_then(|view| view.documents.get(token))
            .cloned()
    }

    /// What was chosen for `key`, most recent first: the chooser's Recent.
    pub fn chosen(&self, key: &AppKey) -> Vec<PathBuf> {
        self.loading.wait();
        self.views
            .read()
            .ok()
            .and_then(|views| {
                views
                    .get(key)
                    .map(|view| view.by_recency().map(|d| d.host.clone()).collect())
            })
            .unwrap_or_default()
    }

    pub fn tokens(&self, key: &AppKey) -> Vec<String> {
        self.loading.wait();
        self.views
            .read()
            .ok()
            .and_then(|views| {
                views
                    .get(key)
                    .map(|view| view.documents.keys().cloned().collect())
            })
            .unwrap_or_default()
    }

    /// Take back everything given to `key`.
    pub fn revoke(&self, key: &AppKey) {
        self.loading.wait();
        let removed = self
            .views
            .write()
            .is_ok_and(|mut views| views.remove(key).is_some());
        if removed {
            self.keep();
        }
    }
}

/// Write `record` to `path` readable by this user only, whole or not at
/// all: a sibling written, flushed and renamed over it.
fn write_private(path: &Path, record: &Record) -> std::io::Result<()> {
    use std::io::Write as _;
    let folder = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the record names no folder"))?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(folder)?;
    let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    let mut sibling = path.as_os_str().to_owned();
    sibling.push(".new");
    let sibling = PathBuf::from(sibling);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&sibling)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(&sibling, path)
}

/// What a choice resolves to on the host, checked against what it is for.
fn resolve_choice(chosen: &Path, kind: Kind) -> Result<PathBuf, GrantError> {
    match kind {
        Kind::Read => {
            let host = chosen.canonicalize().map_err(GrantError::Io)?;
            let meta = std::fs::metadata(&host).map_err(GrantError::Io)?;
            if meta.is_file() {
                Ok(host)
            } else {
                Err(GrantError::NotThatKind)
            }
        }
        Kind::Folder { .. } => {
            let host = chosen.canonicalize().map_err(GrantError::Io)?;
            if std::fs::metadata(&host).map_err(GrantError::Io)?.is_dir() {
                Ok(host)
            } else {
                Err(GrantError::NotThatKind)
            }
        }
        Kind::Save => {
            let name = plain_name(chosen).ok_or(GrantError::NotThatKind)?;
            let parent = chosen
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or(GrantError::NotThatKind)?
                .canonicalize()
                .map_err(GrantError::Io)?;
            if !std::fs::metadata(&parent).map_err(GrantError::Io)?.is_dir() {
                return Err(GrantError::NotThatKind);
            }
            let host = parent.join(name);
            match std::fs::symlink_metadata(&host) {
                Ok(meta) if meta.is_file() => Ok(host),
                Ok(_) => Err(GrantError::NotThatKind),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(host),
                Err(error) => Err(GrantError::Io(error)),
            }
        }
    }
}

/// The last component of `path` when it is an ordinary name.
fn plain_name(path: &Path) -> Option<&OsStr> {
    match path.components().next_back()? {
        Component::Normal(name) => Some(name),
        _ => None,
    }
}

/// A document's token: the same for the same choice by the same app in the
/// same room, and unguessable from outside without knowing all three.
fn token(key: &AppKey, document: &Document) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-documents/1\0");
    hasher.update(key.room.as_bytes());
    hasher.update(b"\0");
    hasher.update(key.app.as_bytes());
    hasher.update(b"\0");
    hasher.update(document.kind.tag().as_bytes());
    hasher.update(b"\0");
    hasher.update(document.host.as_os_str().as_encoded_bytes());
    let hex = hasher.finalize().to_hex();
    hex.chars().take(24).collect()
}

/// The token and the path inside the document of a path an app was given
/// (`/run/tilo/documents/<token>/<name>/…`): `None` for any other path.
pub fn split_sandbox_path(path: &Path) -> Option<(String, PathBuf)> {
    let rest = path.strip_prefix(SANDBOX_DIR).ok()?;
    let mut components = rest.components();
    let Component::Normal(token) = components.next()? else {
        return None;
    };
    let inner: PathBuf = components.collect();
    let token = token.to_str()?.to_owned();
    (!inner.as_os_str().is_empty()).then_some((token, inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AppKey {
        AppKey::new("5ba25f73-87c5-447f-a5a6-880dc4249a81", "so.tilo.totp").unwrap()
    }

    #[test]
    fn keys_carry_only_names_one_path_component_can_hold() {
        assert!(AppKey::new("room-1", "so.tilo.files").is_some());
        for (room, app) in [
            ("", "so.tilo.files"),
            ("../x", "so.tilo.files"),
            ("room", ".."),
            ("room", "a/b"),
            ("room", ".hidden"),
        ] {
            assert!(AppKey::new(room, app).is_none(), "{room:?} {app:?}");
        }
    }

    #[test]
    fn a_choice_answers_a_stable_path_in_the_apps_view() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.pdf");
        std::fs::write(&file, b"pdf").unwrap();
        let documents = Documents::new();
        let path = documents.grant(&key(), &file, Kind::Read).unwrap();
        assert_eq!(documents.grant(&key(), &file, Kind::Read).unwrap(), path);
        let (token, inner) = split_sandbox_path(&path).unwrap();
        assert_eq!(inner, Path::new("report.pdf"));
        assert_eq!(
            documents.document(&key(), &token).unwrap().host,
            file.canonicalize().unwrap()
        );
        let other = AppKey::new("other-room", "so.tilo.totp").unwrap();
        assert!(documents.document(&other, &token).is_none());
        assert_ne!(documents.grant(&other, &file, Kind::Read).unwrap(), path);
    }

    #[test]
    fn a_kept_table_reopens_a_chosen_file_where_it_was_and_forgets_what_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("state/documents.json");
        let kept = dir.path().join("kept.pdf");
        let gone = dir.path().join("gone.pdf");
        std::fs::write(&kept, b"kept").unwrap();
        std::fs::write(&gone, b"gone").unwrap();
        let documents = Documents::kept_in(record.clone());
        let path = documents.grant(&key(), &kept, Kind::Read).unwrap();
        documents.grant(&key(), &gone, Kind::Read).unwrap();
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&record).unwrap().permissions()
            ) & 0o777,
            0o600,
            "the record is this user's only"
        );
        std::fs::remove_file(&gone).unwrap();

        let next = Documents::kept_in(record);
        let (token, _) = split_sandbox_path(&path).unwrap();
        assert_eq!(
            next.document(&key(), &token).unwrap().host,
            kept.canonicalize().unwrap()
        );
        assert_eq!(next.tokens(&key()).len(), 1, "what is gone is forgotten");
        assert_eq!(next.grant(&key(), &kept, Kind::Read).unwrap(), path);
    }

    #[cfg(unix)]
    #[test]
    fn a_session_starts_before_its_kept_table_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("documents.json");
        let kept = dir.path().join("kept.pdf");
        std::fs::write(&kept, b"kept").unwrap();
        let path = Documents::kept_in(record.clone())
            .grant(&key(), &kept, Kind::Read)
            .unwrap();
        let bytes = std::fs::read(&record).unwrap();
        // The record on a disk that has not answered yet: a pipe nothing
        // has written to, which blocks whoever reads it.
        let slow = dir.path().join("slow.json");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&slow)
                .status()
                .unwrap()
                .success()
        );
        let (started, table) = std::sync::mpsc::channel();
        std::thread::spawn({
            let slow = slow.clone();
            move || {
                let _ = started.send(Documents::kept_in(slow));
            }
        });
        let documents = table
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the table is there before its record is read");
        std::fs::write(&slow, &bytes).unwrap();
        let (token, _) = split_sandbox_path(&path).unwrap();
        assert_eq!(
            documents.document(&key(), &token).unwrap().host,
            kept.canonicalize().unwrap(),
            "what was kept is served once the record is read"
        );
    }

    #[test]
    fn recent_lists_choices_newest_first_and_keeps_the_order() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("documents.json");
        let [a, b, c] = ["a.txt", "b.txt", "c.txt"].map(|name| {
            let path = dir.path().join(name);
            std::fs::write(&path, name).unwrap();
            path.canonicalize().unwrap()
        });
        let documents = Documents::kept_in(record.clone());
        for path in [&a, &b, &c, &a] {
            documents.grant(&key(), path, Kind::Read).unwrap();
        }
        assert_eq!(
            documents.chosen(&key()),
            [a.clone(), c.clone(), b.clone()],
            "choosing again makes it the newest"
        );
        let other = AppKey::new("other-room", "so.tilo.totp").unwrap();
        assert!(documents.chosen(&other).is_empty(), "each app has its own");
        assert_eq!(
            Documents::kept_in(record).chosen(&key()),
            [a, c, b],
            "the next session sees the same order"
        );
    }

    #[test]
    fn a_choice_must_be_what_the_chooser_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let documents = Documents::new();
        assert!(matches!(
            documents.grant(&key(), dir.path(), Kind::Read),
            Err(GrantError::NotThatKind)
        ));
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"a").unwrap();
        assert!(matches!(
            documents.grant(&key(), &file, Kind::Folder { write: false }),
            Err(GrantError::NotThatKind)
        ));
        assert!(
            documents
                .grant(&key(), &dir.path().join("new.txt"), Kind::Save)
                .is_ok()
        );
        assert!(matches!(
            documents.grant(&key(), &dir.path().join("missing.txt"), Kind::Read),
            Err(GrantError::Io(_))
        ));
    }
}
