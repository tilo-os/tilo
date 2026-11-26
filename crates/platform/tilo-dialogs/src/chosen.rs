//! The files a person chose for an app in the system file chooser, outside any workspace.
//!
//! Choosing a file in the session shell's chooser is the person's consent, and it gives the app
//! that asked exactly what was chosen: the file to read for an Open, the file to write (created if
//! it is new) for a Save, the folder and everything under it for a folder. Nothing else.
//!
//! A sandboxed app is handed the descriptor the chooser opened with the person's rights, and a
//! reference that names the choice again (`/run/tilo/chosen/records/<token>/<name>`): an opaque
//! path, never a host path the app could use itself. Reopening resolves the reference through this
//! table, and answers nothing for a choice that is gone.
//!
//! The table is the session's one for everything it keeps of a choice, and it is kept as a record
//! of the device it runs on ([`Store`], bound by the session to the device's own records): read
//! and written through the same settings action as the device's other records, which only its
//! person reaches, rather than living in a private file of this session. A choice inside a
//! workspace is not here: it is a grant on the file's tree node, written through the workspace's
//! own policy.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// Where a chosen item is named from on every device: the app reaches nothing of it but the
/// descriptor the chooser hands, and this path names the choice again later.
pub const CHOSEN_ROOT: &str = "/run/tilo/chosen";

/// The path segment device-record choices live under, below [`CHOSEN_ROOT`].
const RECORDS: &str = "records";

/// The device record the table is kept as, through [`Store`].
pub const RECORD: &str = "chosen.files";

/// One choice, as the device's record keeps it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Kept {
    pub workspace: String,
    pub app: String,
    pub kind: String,
    pub host: PathBuf,
}

/// Where the device keeps the table: its own records, read when the session starts and written as
/// each choice is made.
pub trait Store: Send + Sync {
    /// What the record held when this session started.
    fn load(&self) -> Vec<Kept>;
    /// Keep `kept` as the record from now on.
    fn keep(&self, kept: Vec<Kept>);
    /// Follow the record from now on, handing `changed` what it holds each time the record changes
    /// elsewhere: what another of the person's tools writes reaches the table as it lands. Whether
    /// the record can be followed.
    fn follow(&self, _changed: Arc<dyn Fn(Vec<Kept>) + Send + Sync>) -> bool {
        false
    }
}

/// The key a choice is made under: the workspace the app runs in and its package id, so the same
/// app in another workspace sees none of it and no app reaches another's choices.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppKey {
    workspace: String,
    app: String,
}

impl AppKey {
    /// The key for `app` running in `workspace`, or `None` when either is not a name the entries
    /// can carry as one path component (a workspace id is letters, digits and `-`; a package id
    /// adds `.` and `_`, and does not start with `.`).
    #[must_use]
    pub fn new(workspace: &str, app: &str) -> Option<Self> {
        (is_workspace(workspace) && is_app(app)).then(|| Self {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
        })
    }

    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }
}

pub(crate) fn is_workspace(name: &str) -> bool {
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

/// What a choice allows the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Read,
    /// One file to write, created if it does not exist yet: a Save. Its save sibling
    /// (`.<name>.tilo-save`) may be written and renamed over it, so a save replaces the file whole
    /// or not at all.
    Save,
    Folder {
        write: bool,
    },
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

    /// Whether the app may change what the choice names.
    #[must_use]
    pub fn writes(self) -> bool {
        matches!(self, Self::Save | Self::Folder { write: true })
    }
}

/// One chosen file or folder of the host, behind a choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    /// The chosen file or folder on the host, resolved when it was chosen. For a [`Kind::Save`] of
    /// a new file, the resolved folder joined with the chosen name.
    pub host: PathBuf,
    pub kind: Kind,
}

impl Document {
    #[must_use]
    pub fn name(&self) -> &OsStr {
        self.host
            .file_name()
            .unwrap_or_else(|| OsStr::new("document"))
    }

    /// Where a save writes before its bytes replace the file.
    #[must_use]
    pub fn save_sibling(&self) -> Option<OsString> {
        (self.kind == Kind::Save).then(|| {
            let mut sibling = OsString::from(".");
            sibling.push(self.name());
            sibling.push(".tilo-save");
            sibling
        })
    }
}

/// One choice, as a host hands it to the chooser and to the app.
#[derive(Clone, Debug)]
pub struct Chosen {
    /// The path the app reaches it by: what names it again, never a host path.
    pub reference: PathBuf,
    /// Its name there.
    pub name: String,
    /// The host file or folder it is.
    pub host: PathBuf,
    /// What the person's choice allows.
    pub kind: Kind,
    /// Where a save writes before its bytes replace the file; `None` for anything but a save.
    pub save_sibling: Option<OsString>,
}

#[derive(Debug)]
pub enum GrantError {
    /// What was chosen is not what the chooser asked for (a folder for a file, a missing file for
    /// an Open, a symbolic link for a Save).
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

/// What one app in one workspace was given, and the order it was chosen in.
#[derive(Default, PartialEq)]
struct View {
    documents: BTreeMap<String, Document>,
    recent: Vec<String>,
}

impl View {
    fn choose(&mut self, token: String, document: Document) -> bool {
        if self.recent.first() == Some(&token) {
            return false;
        }
        self.recent.retain(|kept| *kept != token);
        self.recent.insert(0, token.clone());
        self.documents.insert(token, document);
        true
    }

    /// Add `document` behind those already in the view, as reading a kept table (newest first)
    /// does.
    fn keep_older(&mut self, token: String, document: Document) {
        if let Some(held) = self.documents.get_mut(&token) {
            *held = document;
            return;
        }
        self.documents.insert(token.clone(), document);
        self.recent.push(token);
    }

    fn by_recency(&self) -> impl Iterator<Item = (&String, &Document)> {
        self.recent
            .iter()
            .filter_map(|token| self.documents.get_key_value(token))
    }
}

type Views = HashMap<AppKey, View>;

/// The files people chose for their apps outside any workspace, as this session reaches them.
#[derive(Clone, Default)]
pub struct ChosenTable {
    views: Arc<RwLock<Views>>,
    store: Arc<OnceLock<Arc<dyn Store>>>,
    /// How often what the table holds changed: what a reader compares to tell whether any choice
    /// moved since it last read.
    revision: Arc<AtomicU64>,
}

impl ChosenTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How often what the table holds changed. A number that moves each time a choice is made,
    /// taken back, or read from the device's record.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    /// Keep the table as the device's record `store` holds, reading what it held now and following
    /// it from then on, so a choice another of the person's tools makes reaches the table as it
    /// lands.
    ///
    /// The read runs on a thread of its own, so a session starts without waiting on its records;
    /// what was chosen earlier is there before any choice needs to know it.
    pub fn bind(&self, store: Arc<dyn Store>) {
        if self.store.set(store).is_err() {
            return;
        }
        let table = self.clone();
        let loaded = std::thread::Builder::new()
            .name("tilo-chosen-load".into())
            .spawn(move || {
                table.load();
                table.follow();
            });
        if let Err(error) = loaded {
            tracing::warn!(%error, "the files people chose earlier are not read");
        }
    }

    /// Follow the record the table is kept as from now on: each reading of it replaces what the
    /// table holds.
    fn follow(&self) {
        let Some(store) = self.store.get().cloned() else {
            return;
        };
        let table = self.clone();
        // A store that cannot follow its record reads it when the table is bound only.
        store.follow(Arc::new(move |kept| table.replace(kept)));
    }

    fn load(&self) {
        let Some(store) = self.store.get().cloned() else {
            return;
        };
        self.replace(store.load());
    }

    /// Hold `kept` as the table from now on, as the device's record keeps it, and say the table
    /// changed where it did.
    fn replace(&self, kept: Vec<Kept>) {
        let mut views: Views = HashMap::new();
        for kept in kept {
            let (Some(key), Some(kind)) = (
                AppKey::new(&kept.workspace, &kept.app),
                Kind::of_tag(&kept.kind),
            ) else {
                continue;
            };
            let Ok(host) = resolve_choice(&kept.host, kind) else {
                continue;
            };
            let document = Document { host, kind };
            let token = token(&key, &document);
            views.entry(key).or_default().keep_older(token, document);
        }
        let changed = self.views.write().is_ok_and(|mut held| {
            if *held == views {
                return false;
            }
            *held = views;
            true
        });
        if changed {
            self.changed();
        }
    }

    fn keep(&self) {
        let Some(store) = self.store.get() else {
            return;
        };
        let Some(snapshot) = self.views.read().ok().map(|views| {
            views
                .iter()
                .flat_map(|(key, view)| {
                    view.by_recency().map(|(_, document)| Kept {
                        workspace: key.workspace.clone(),
                        app: key.app.clone(),
                        kind: document.kind.tag().to_owned(),
                        host: document.host.clone(),
                    })
                })
                .collect::<Vec<_>>()
        }) else {
            return;
        };
        store.keep(snapshot);
    }

    /// Give `key` the item the person chose at `chosen`, as `kind`, and answer the choice the app
    /// is handed. Choosing the same item again answers the same reference, so a recent file
    /// reopens where it was, and makes it the most recent.
    ///
    /// # Errors
    ///
    /// [`GrantError::NotThatKind`] when `chosen` is not what `kind` needs; [`GrantError::Io`] when
    /// it cannot be resolved.
    pub fn grant(&self, key: &AppKey, chosen: &Path, kind: Kind) -> Result<Chosen, GrantError> {
        let host = resolve_choice(chosen, kind)?;
        let document = Document { host, kind };
        let token = token(key, &document);
        let changed = self.views.write().is_ok_and(|mut views| {
            views
                .entry(key.clone())
                .or_default()
                .choose(token.clone(), document.clone())
        });
        if changed {
            self.keep();
            self.changed();
        }
        Ok(entry(&token, document))
    }

    /// The document `token` names for `key`.
    #[must_use]
    pub fn document(&self, key: &AppKey, token: &str) -> Option<Document> {
        self.views
            .read()
            .ok()?
            .get(key)
            .and_then(|view| view.documents.get(token).cloned())
    }

    /// The choice `reference` names for `key`, if it is one the person gave that app.
    ///
    /// The answer names the choice's own reference whatever `reference` asked for: a caller
    /// checking what it may reach strips the reference from this one, and so refuses anything
    /// that is not a plain name beneath a chosen folder.
    #[must_use]
    pub fn looked_up(&self, key: &AppKey, reference: &Path) -> Option<Chosen> {
        let (token, _) = split_reference(reference)?;
        let document = self.document(key, &token)?;
        Some(entry(&token, document))
    }

    /// What was chosen for `key`, most recent first.
    #[must_use]
    pub fn entries(&self, key: &AppKey) -> Vec<Chosen> {
        self.views
            .read()
            .ok()
            .and_then(|views| {
                views.get(key).map(|view| {
                    view.by_recency()
                        .map(|(token, document)| entry(token, document.clone()))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// What was chosen for `key` on the host, most recent first.
    #[must_use]
    pub fn chosen(&self, key: &AppKey) -> Vec<PathBuf> {
        self.views
            .read()
            .ok()
            .and_then(|views| {
                views
                    .get(key)
                    .map(|view| view.by_recency().map(|(_, d)| d.host.clone()).collect())
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn tokens(&self, key: &AppKey) -> Vec<String> {
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

    /// Forget everything chosen for `key`.
    pub fn revoke(&self, key: &AppKey) {
        let removed = self
            .views
            .write()
            .is_ok_and(|mut views| views.remove(key).is_some());
        if removed {
            self.keep();
            self.changed();
        }
    }

    /// Every choice the table holds, by the app it was made for, most recent first within an app:
    /// what a view lists of the files people chose outside any workspace.
    #[must_use]
    pub fn all(&self) -> Vec<(AppKey, String, Chosen)> {
        self.views
            .read()
            .ok()
            .map(|views| {
                views
                    .iter()
                    .flat_map(|(key, view)| {
                        view.by_recency().map(move |(token, document)| {
                            (key.clone(), token.clone(), entry(token, document.clone()))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Forget the one choice `token` of `key`, keeping the rest. Whether it was there: the device's
    /// record is written again without it.
    pub fn forget(&self, key: &AppKey, token: &str) -> bool {
        let removed = self.views.write().is_ok_and(|mut views| {
            let Some(view) = views.get_mut(key) else {
                return false;
            };
            let removed = view.documents.remove(token).is_some();
            if removed {
                view.recent.retain(|held| held != token);
                if view.documents.is_empty() {
                    views.remove(key);
                }
            }
            removed
        });
        if removed {
            self.keep();
            self.changed();
        }
        removed
    }
}

/// One choice, from the token the table keeps it under.
fn entry(token: &str, document: Document) -> Chosen {
    let name = document.name().to_string_lossy().into_owned();
    let reference = Path::new(CHOSEN_ROOT).join(RECORDS).join(token).join(&name);
    Chosen {
        reference,
        name,
        save_sibling: document.save_sibling(),
        host: document.host,
        kind: document.kind,
    }
}

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

fn plain_name(path: &Path) -> Option<&OsStr> {
    match path.components().next_back()? {
        Component::Normal(name) => Some(name),
        _ => None,
    }
}

/// A choice's token: the same for the same choice by the same app in the same workspace, and
/// unguessable from outside without knowing all three.
fn token(key: &AppKey, document: &Document) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-chosen/1\0");
    hasher.update(key.workspace.as_bytes());
    hasher.update(b"\0");
    hasher.update(key.app.as_bytes());
    hasher.update(b"\0");
    hasher.update(document.kind.tag().as_bytes());
    hasher.update(b"\0");
    hasher.update(document.host.as_os_str().as_encoded_bytes());
    let hex = hasher.finalize().to_hex();
    hex.chars().take(24).collect()
}

/// The token a reference names and the path beneath it.
#[must_use]
pub fn split_reference(path: &Path) -> Option<(String, PathBuf)> {
    let rest = path.strip_prefix(CHOSEN_ROOT).ok()?;
    let mut components = rest.components();
    let Component::Normal(first) = components.next()? else {
        return None;
    };
    if first != RECORDS {
        return None;
    }
    let Component::Normal(token) = components.next()? else {
        return None;
    };
    let inner: PathBuf = components.collect();
    let token = token.to_str()?.to_owned();
    Some((token, inner))
}

/// The table the session and its portal share: one for every app this session serves, bound to the
/// device's records once the session has them.
#[must_use]
pub fn chosen_table() -> Arc<ChosenTable> {
    static TABLE: OnceLock<Arc<ChosenTable>> = OnceLock::new();
    Arc::clone(TABLE.get_or_init(|| Arc::new(ChosenTable::new())))
}

/// Keep the shared table as the device's record `store` holds.
pub fn bind_store(store: Arc<dyn Store>) {
    chosen_table().bind(store);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn key() -> AppKey {
        AppKey::new("5ba25f73-87c5-447f-a5a6-880dc4249a81", "so.tilo.totp").unwrap()
    }

    /// A store kept in this process, as the device's record keeps it.
    #[derive(Default)]
    struct Held(Mutex<Vec<Kept>>);

    impl Store for Held {
        fn load(&self) -> Vec<Kept> {
            self.0.lock().unwrap().clone()
        }

        fn keep(&self, kept: Vec<Kept>) {
            *self.0.lock().unwrap() = kept;
        }
    }

    fn wait_for(table: &ChosenTable, key: &AppKey, tokens: usize) {
        tilo_test_env::wait_until("the kept table is read", || {
            table.tokens(key).len() == tokens
        });
    }

    #[test]
    fn keys_carry_only_names_one_path_component_can_hold() {
        assert!(AppKey::new("workspace-1", "so.tilo.files").is_some());
        for (workspace, app) in [
            ("", "so.tilo.files"),
            ("../x", "so.tilo.files"),
            ("workspace", ".."),
            ("workspace", "a/b"),
            ("workspace", ".hidden"),
        ] {
            assert!(
                AppKey::new(workspace, app).is_none(),
                "{workspace:?} {app:?}"
            );
        }
    }

    #[test]
    fn a_choice_answers_a_stable_reference_in_the_chosen_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.pdf");
        std::fs::write(&file, b"pdf").unwrap();
        let table = ChosenTable::new();
        let chosen = table.grant(&key(), &file, Kind::Read).unwrap();
        assert_eq!(
            table.grant(&key(), &file, Kind::Read).unwrap().reference,
            chosen.reference
        );
        assert_eq!(table.entries(&key())[0].reference, chosen.reference);
        assert!(chosen.reference.starts_with(CHOSEN_ROOT));
        assert_eq!(chosen.name, "report.pdf");
        assert_eq!(
            table.looked_up(&key(), &chosen.reference).unwrap().host,
            file.canonicalize().unwrap()
        );
        let other = AppKey::new("other-workspace", "so.tilo.totp").unwrap();
        assert!(table.looked_up(&other, &chosen.reference).is_none());
        assert_ne!(
            table.grant(&other, &file, Kind::Read).unwrap().reference,
            chosen.reference
        );
        assert!(
            table
                .looked_up(
                    &key(),
                    Path::new("/run/tilo/chosen/records/nope/report.pdf")
                )
                .is_none()
        );
    }

    #[test]
    fn a_kept_table_reopens_a_choice_where_it_was_and_forgets_what_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept.pdf");
        let gone = dir.path().join("gone.pdf");
        std::fs::write(&kept, b"kept").unwrap();
        std::fs::write(&gone, b"gone").unwrap();
        let store = Arc::new(Held::default());
        let table = ChosenTable::new();
        table.bind(store.clone());
        let reference = table.grant(&key(), &kept, Kind::Read).unwrap().reference;
        table.grant(&key(), &gone, Kind::Read).unwrap();
        std::fs::remove_file(&gone).unwrap();

        let next = ChosenTable::new();
        next.bind(store);
        wait_for(&next, &key(), 1);
        assert!(
            next.looked_up(&key(), &reference).is_some(),
            "what was kept is served once the record is read"
        );
    }

    #[test]
    fn recent_lists_choices_newest_first_and_keeps_the_order() {
        let dir = tempfile::tempdir().unwrap();
        let [a, b, c] = ["a.txt", "b.txt", "c.txt"].map(|name| {
            let path = dir.path().join(name);
            std::fs::write(&path, name).unwrap();
            path.canonicalize().unwrap()
        });
        let table = ChosenTable::new();
        for path in [&a, &b, &c, &a] {
            table.grant(&key(), path, Kind::Read).unwrap();
        }
        assert_eq!(
            table.chosen(&key()),
            [a, c, b],
            "choosing again makes it the newest"
        );
        let other = AppKey::new("other-workspace", "so.tilo.totp").unwrap();
        assert!(table.chosen(&other).is_empty(), "each app has its own");
    }

    #[test]
    fn a_choice_must_be_what_the_chooser_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let table = ChosenTable::new();
        assert!(matches!(
            table.grant(&key(), dir.path(), Kind::Read),
            Err(GrantError::NotThatKind)
        ));
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"a").unwrap();
        assert!(matches!(
            table.grant(&key(), &file, Kind::Folder { write: false }),
            Err(GrantError::NotThatKind)
        ));
        assert!(
            table
                .grant(&key(), &dir.path().join("new.txt"), Kind::Save)
                .is_ok()
        );
        assert!(matches!(
            table.grant(&key(), &dir.path().join("missing.txt"), Kind::Read),
            Err(GrantError::Io(_))
        ));
    }
}
