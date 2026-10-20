//! The session's persistent search index of what apps hold.
//!
//! A running app publishes its search entries over its certified broker
//! route ([`ui::search::index_publication`]): per app, workspace room and
//! record, keyed by the stable resource id a hit opens. This index applies
//! each change as it arrives and keeps the entries after the app closes, so
//! the app's content is findable whether or not it runs
//! ([`super::providers::app_content`] answers from it).
//!
//! It is derived state: every entry is the app's own to publish again, and
//! the files here may be deleted and rebuilt. Nothing here ever changes an
//! app's records or ids.
//!
//! Secret-bearing entries (a vault's logins) are kept as metadata only (a
//! name, a username, a site) and are found only while their app last said
//! its secrets are unlocked. A locked app cannot list what it holds, so a
//! publication made while locked never drops its secret entries: being
//! locked is not deleting.
//!
//! On disk, under `$XDG_STATE_HOME/tilo/search/app-content/`, each slot
//! (app, room, record) is one private file rewritten when that slot
//! changes, and the apps' last lock reports are another.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_lock::MutexRecover as _;
use ui::search::index_publication::{IndexDelta, IndexEntry, MAX_ENTRIES};

/// Where one app's entries were published from: its app id, the workspace
/// room and the record, a window's record standing for every window of the
/// app playing the same roles ([`record_slot`]).
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SlotKey {
    pub app: String,
    pub room: String,
    pub record: String,
}

impl SlotKey {
    /// The slot an app's publication from `record` in `room` fills.
    pub fn new(app: &str, room: &str, record: &str) -> Self {
        Self {
            app: app.to_owned(),
            room: room.to_owned(),
            record: record_slot(record),
        }
    }
}

/// The slot a record's entries are kept under. A window's own record
/// (`tab:<id>`, or one derived from it) names the window, not anything the
/// person keeps: every window of an app in a room publishes the same
/// entries, so all of them fill one slot (`tab:*`) instead of one per
/// window ever opened.
pub fn record_slot(record_id: &str) -> String {
    let mut rest = record_id;
    while let Some(derived) = rest.strip_prefix("derived:") {
        match derived.split_once('@') {
            Some((_, after)) => rest = after,
            None => return record_id.to_owned(),
        }
    }
    match rest.strip_prefix("tab:") {
        Some(tab) if u64::from_str_radix(tab, 16).is_ok_and(|tab| tab != 0) => {
            let roles = record_id.strip_suffix(rest).unwrap_or_default();
            format!("{roles}tab:*")
        }
        _ => record_id.to_owned(),
    }
}

/// One entry and where it was published from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedEntry {
    pub slot: SlotKey,
    pub entry: IndexEntry,
}

/// What the index holds at one generation, split by whether a person may
/// find it now.
#[derive(Clone, Debug, Default)]
pub struct Contents {
    /// Entries a search finds.
    pub visible: Vec<IndexedEntry>,
    /// Secret-bearing entries of apps whose secrets are locked (or that
    /// never said they are unlocked): never shown, only matched to offer
    /// unlocking the app.
    pub locked: Vec<IndexedEntry>,
}

#[derive(Default)]
struct Inner {
    slots: BTreeMap<SlotKey, BTreeMap<String, IndexEntry>>,
    /// Each app's last word on whether its secrets are unlocked.
    secrets: BTreeMap<String, bool>,
}

impl Inner {
    fn unlocked(&self, app: &str) -> bool {
        self.secrets.get(app).copied() == Some(true)
    }
}

/// The session's persistent index of apps' search entries. Shared between
/// the broker, which hands it publications, and search, which reads it.
pub struct AppContentIndex {
    inner: Mutex<Inner>,
    /// Moves on every change a search could see.
    generation: AtomicU64,
    store: Option<Store>,
    /// Wakes the compositor loop when the index changes, so a showing
    /// search takes the change in.
    wake: Mutex<Option<std::task::Waker>>,
}

impl AppContentIndex {
    /// An index kept in memory only: tests, and hosts with no state
    /// directory.
    pub fn in_memory() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            generation: AtomicU64::new(0),
            store: None,
            wake: Mutex::new(None),
        }
    }

    /// An index kept in `dir`, reading what an earlier session kept there
    /// on a worker thread: entries an app publishes before that read lands
    /// are newer than the files, and win.
    pub fn persistent(dir: PathBuf) -> Arc<Self> {
        let index = Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            generation: AtomicU64::new(0),
            store: Store::open(dir.clone()),
            wake: Mutex::new(None),
        });
        let loading = Arc::clone(&index);
        let spawned = std::thread::Builder::new()
            .name("tilo-app-content-load".into())
            .spawn(move || loading.load(&dir));
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not read the app content search index");
        }
        index
    }

    /// Which build of the contents a search reads.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Wake `waker` whenever the index changes.
    pub fn set_waker(&self, waker: std::task::Waker) {
        *self.wake.lock_recover() = Some(waker);
    }

    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(waker) = self.wake.lock_recover().as_ref() {
            waker.wake_by_ref();
        }
    }

    /// Apply one change `app` published in `room` from `record`.
    ///
    /// # Errors
    ///
    /// The delta is malformed.
    pub fn publish(&self, room: &str, app: &str, record: &str, delta: &[u8]) -> Result<(), String> {
        let delta = IndexDelta::decode(delta)?;
        self.apply(&SlotKey::new(app, room, record), delta);
        Ok(())
    }

    /// Apply `delta` to `key`'s slot, and keep the slot.
    pub fn apply(&self, key: &SlotKey, delta: IndexDelta) {
        let writes = {
            let mut inner = self.inner.lock_recover();
            let mut secrets_moved = false;
            if let Some(unlocked) = delta.secrets_unlocked {
                secrets_moved = inner.secrets.insert(key.app.clone(), unlocked) != Some(unlocked);
            }
            // A locked app lists none of its secrets; that is not their
            // removal.
            let keeps_secrets = !inner.unlocked(&key.app);
            let slot = inner.slots.entry(key.clone()).or_default();
            if delta.reset {
                slot.retain(|_, entry| keeps_secrets && entry.secret);
            }
            for gone in &delta.remove {
                if keeps_secrets && slot.get(gone).is_some_and(|entry| entry.secret) {
                    continue;
                }
                slot.remove(gone);
            }
            for entry in delta.upsert.into_iter().filter_map(IndexEntry::bounded) {
                if slot.len() >= MAX_ENTRIES && !slot.contains_key(&entry.key) {
                    continue;
                }
                slot.insert(entry.key.clone(), entry);
            }
            let entries: Vec<IndexEntry> = slot.values().cloned().collect();
            if entries.is_empty() {
                inner.slots.remove(key);
            }
            let mut writes = vec![Write::slot(key, &entries)];
            if secrets_moved {
                writes.push(Write::secrets(&inner.secrets));
            }
            writes
        };
        if let Some(store) = &self.store {
            store.write(writes);
        }
        self.changed();
    }

    /// Everything the index holds now, split by what a person may find.
    pub fn contents(&self) -> Contents {
        let inner = self.inner.lock_recover();
        let mut contents = Contents::default();
        for (slot, entries) in &inner.slots {
            let unlocked = inner.unlocked(&slot.app);
            for entry in entries.values() {
                let indexed = IndexedEntry {
                    slot: slot.clone(),
                    entry: entry.clone(),
                };
                if entry.secret && !unlocked {
                    contents.locked.push(indexed);
                } else {
                    contents.visible.push(indexed);
                }
            }
        }
        drop(inner);
        contents
    }

    /// Whether `app` last said its secrets are unlocked.
    pub fn secrets_unlocked(&self, app: &str) -> bool {
        self.inner.lock_recover().unlocked(app)
    }

    /// Take in what an earlier session kept in `dir`, under what this one
    /// already holds.
    fn load(&self, dir: &Path) {
        let Ok(files) = std::fs::read_dir(dir) else {
            return;
        };
        let mut slots = Vec::new();
        let mut secrets = BTreeMap::new();
        for file in files.flatten() {
            let path = file.path();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if path.file_name().and_then(|name| name.to_str()) == Some(SECRETS_FILE) {
                match serde_json::from_slice::<BTreeMap<String, bool>>(&bytes) {
                    Ok(read) => secrets = read,
                    Err(error) => tracing::warn!(%error, "unreadable app lock reports"),
                }
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            match serde_json::from_slice::<StoredSlot>(&bytes) {
                Ok(stored) => slots.push(stored),
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "unreadable app content slot");
                }
            }
        }
        {
            let mut inner = self.inner.lock_recover();
            for (app, unlocked) in secrets {
                inner.secrets.entry(app).or_insert(unlocked);
            }
            for stored in slots {
                let entries: BTreeMap<String, IndexEntry> = stored
                    .entries
                    .into_iter()
                    .filter_map(IndexEntry::bounded)
                    .take(MAX_ENTRIES)
                    .map(|entry| (entry.key.clone(), entry))
                    .collect();
                if !entries.is_empty() {
                    inner.slots.entry(stored.key).or_insert(entries);
                }
            }
        }
        self.changed();
    }
}

/// The session's index, kept in the state directory; in memory where there
/// is none, and in unit tests.
pub fn session() -> Arc<AppContentIndex> {
    static SESSION: OnceLock<Arc<AppContentIndex>> = OnceLock::new();
    Arc::clone(SESSION.get_or_init(|| {
        let dir = if cfg!(test) {
            None
        } else {
            tilo_paths::state_dir().map(|root| root.join("tilo/search/app-content"))
        };
        dir.map_or_else(
            || Arc::new(AppContentIndex::in_memory()),
            AppContentIndex::persistent,
        )
    }))
}

/// The host service the broker hands publications to.
pub struct SearchIndexService(pub Arc<AppContentIndex>);

impl tilo_action_broker::services::SearchIndexServices for SearchIndexService {
    fn publish(
        &self,
        room: tilo_desktop_model::RoomId,
        app: &str,
        record: &str,
        delta: &[u8],
    ) -> Result<(), String> {
        self.0.publish(&room.to_string(), app, record, delta)
    }
}

/// The file the apps' lock reports are kept in.
const SECRETS_FILE: &str = "secrets.json";

/// One slot as kept on disk.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredSlot {
    key: SlotKey,
    entries: Vec<IndexEntry>,
}

/// One file to replace, or to remove (`None`).
struct Write {
    file: String,
    bytes: Option<Vec<u8>>,
}

impl Write {
    fn slot(key: &SlotKey, entries: &[IndexEntry]) -> Self {
        let file = format!("{:016x}.json", fnv1a(key));
        let bytes = (!entries.is_empty())
            .then(|| {
                serde_json::to_vec(&StoredSlot {
                    key: key.clone(),
                    entries: entries.to_vec(),
                })
                .ok()
            })
            .flatten();
        Self { file, bytes }
    }

    fn secrets(secrets: &BTreeMap<String, bool>) -> Self {
        Self {
            file: SECRETS_FILE.into(),
            bytes: serde_json::to_vec(secrets).ok(),
        }
    }
}

/// A stable name for a slot's file: FNV-1a over its key, the same in every
/// build (the standard hasher promises no such thing).
fn fnv1a(key: &SlotKey) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [&key.app, &key.room, &key.record] {
        for byte in part.bytes().chain(std::iter::once(0x1f)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// The files, written on a thread of their own: a publication never waits
/// on the disk. Consecutive writes of one file keep only the newest.
struct Store {
    sender: mpsc::Sender<Vec<Write>>,
}

impl Store {
    fn open(dir: PathBuf) -> Option<Self> {
        let (sender, receiver) = mpsc::channel::<Vec<Write>>();
        let spawned = std::thread::Builder::new()
            .name("tilo-app-content-store".into())
            .spawn(move || {
                while let Ok(first) = receiver.recv() {
                    let mut pending: HashMap<String, Option<Vec<u8>>> = HashMap::new();
                    for write in first.into_iter().chain(receiver.try_iter().flatten()) {
                        pending.insert(write.file, write.bytes);
                    }
                    for (file, bytes) in pending {
                        if let Err(error) = store_file(&dir, &file, bytes.as_deref()) {
                            tracing::warn!(%error, file, "could not keep the app content search index");
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => Some(Self { sender }),
            Err(error) => {
                tracing::warn!(%error, "app content search index kept in memory only");
                None
            }
        }
    }

    fn write(&self, writes: Vec<Write>) {
        let _ = self.sender.send(writes);
    }
}

/// Replace `file` in `dir` with `bytes` in one step, private to the person;
/// remove it for `None`.
fn store_file(dir: &Path, file: &str, bytes: Option<&[u8]>) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = dir.join(file);
    let Some(bytes) = bytes else {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    };
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // A temporary file is private to its owner from the start.
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::index_publication::{IndexAction, IndexWeight};

    const ROOM: &str = "6f1c3c43-2a59-4d36-9d0e-0b6a3b1f7e21";

    fn entry(key: &str, title: &str) -> IndexEntry {
        IndexEntry {
            key: key.into(),
            resource: key.into(),
            action: None,
            title: title.into(),
            subtitle: None,
            group: None,
            icon: None,
            boost: 0,
            fields: Vec::new(),
            secret: false,
        }
    }

    fn titles(entries: &[IndexedEntry]) -> Vec<&str> {
        entries
            .iter()
            .map(|indexed| indexed.entry.title.as_str())
            .collect()
    }

    #[test]
    fn a_window_record_stands_for_every_window_of_its_roles() {
        assert_eq!(record_slot("tab:002a"), "tab:*");
        assert_eq!(
            record_slot("derived:default@tab:002a"),
            "derived:default@tab:*"
        );
        assert_eq!(record_slot("tab:0"), "tab:0");
        assert_eq!(record_slot("notes:7f3e"), "notes:7f3e");
        assert_eq!(
            SlotKey::new("so.tilo.notes", ROOM, "tab:0001"),
            SlotKey::new("so.tilo.notes", ROOM, "tab:00ff")
        );
    }

    #[test]
    fn publications_update_and_remove_entries_by_key() {
        let index = AppContentIndex::in_memory();
        let key = SlotKey::new("so.tilo.notes", ROOM, "tab:0001");
        index.apply(
            &key,
            IndexDelta {
                reset: true,
                upsert: vec![entry("n1", "Groceries"), entry("n2", "Trip plan")],
                ..IndexDelta::default()
            },
        );
        assert_eq!(
            titles(&index.contents().visible),
            ["Groceries", "Trip plan"]
        );

        let before = index.generation();
        index.apply(
            &key,
            IndexDelta {
                upsert: vec![entry("n1", "Groceries for Sunday")],
                remove: vec!["n2".into()],
                ..IndexDelta::default()
            },
        );
        assert!(index.generation() > before, "a change moves the generation");
        assert_eq!(titles(&index.contents().visible), ["Groceries for Sunday"]);

        index.apply(
            &key,
            IndexDelta {
                reset: true,
                ..IndexDelta::default()
            },
        );
        assert!(
            index.contents().visible.is_empty(),
            "a reset to nothing empties the slot"
        );
    }

    #[test]
    fn secrets_are_found_only_while_unlocked_and_a_locked_publication_keeps_them() {
        let index = AppContentIndex::in_memory();
        let key = SlotKey::new("so.tilo.passwords", ROOM, "tab:0001");
        let login = IndexEntry {
            secret: true,
            fields: vec![("github.com".into(), IndexWeight::Identity)],
            ..entry("passwords.item:gh", "Work account")
        };
        index.apply(
            &key,
            IndexDelta {
                reset: true,
                upsert: vec![login],
                secrets_unlocked: Some(true),
                ..IndexDelta::default()
            },
        );
        assert_eq!(titles(&index.contents().visible), ["Work account"]);
        assert!(index.contents().locked.is_empty());

        // Locked, the vault lists nothing: the entry is hidden, not gone.
        index.apply(
            &key,
            IndexDelta {
                reset: true,
                secrets_unlocked: Some(false),
                ..IndexDelta::default()
            },
        );
        assert!(index.contents().visible.is_empty());
        assert_eq!(titles(&index.contents().locked), ["Work account"]);

        index.apply(
            &key,
            IndexDelta {
                secrets_unlocked: Some(true),
                ..IndexDelta::default()
            },
        );
        assert_eq!(titles(&index.contents().visible), ["Work account"]);
    }

    #[test]
    fn an_app_that_never_said_unlocked_keeps_its_secrets_hidden() {
        let index = AppContentIndex::in_memory();
        index.apply(
            &SlotKey::new("so.tilo.passwords", ROOM, "tab:0001"),
            IndexDelta {
                reset: true,
                upsert: vec![IndexEntry {
                    secret: true,
                    ..entry("passwords.item:gh", "GitHub")
                }],
                ..IndexDelta::default()
            },
        );
        assert!(index.contents().visible.is_empty());
        assert_eq!(titles(&index.contents().locked), ["GitHub"]);
    }

    #[test]
    fn the_index_survives_a_restart_from_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let key = SlotKey::new("so.tilo.notes", ROOM, "notes:main");
        {
            let index = AppContentIndex::persistent(dir.path().to_path_buf());
            index.apply(
                &key,
                IndexDelta {
                    reset: true,
                    upsert: vec![IndexEntry {
                        action: Some(IndexAction {
                            resource: "n1".into(),
                            action: "open".into(),
                        }),
                        ..entry("n1", "Groceries")
                    }],
                    ..IndexDelta::default()
                },
            );
        }
        // The writer thread keeps the file; wait for it the way a restart
        // would find it.
        let file = dir.path().join(format!("{:016x}.json", fnv1a(&key)));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !file.exists() {
            assert!(std::time::Instant::now() < deadline, "the slot is kept");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let reopened = AppContentIndex::in_memory();
        reopened.load(dir.path());
        assert_eq!(titles(&reopened.contents().visible), ["Groceries"]);
    }
}
