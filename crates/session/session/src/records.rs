//! The records the workspaces on this device hold, and the reference that
//! brings one of them into the workspace in front.
//!
//! A workspace holds a record by naming it: each of its apps keeps a room
//! index ([`ui::record::room`]) saying which records the workspace holds and
//! what kind each one is, one document per app and workspace. Every app of
//! the workspace writes that index as a shared writer, so this session writes
//! it too: naming a record there is the same act as the app naming it, and
//! what it names is what the workspace's apps read back as the records they
//! may open.
//!
//! Search offers every index on the device, as the worker last read them, so
//! a record made elsewhere — another workspace's note, a file-backed document,
//! one a peer brought into a room — is found by its name. Bringing it here is
//! then one write, a reference into the workspace in front: no bytes move
//! between the two workspaces' documents and no id is minted, because the id
//! is the record. [`ui::record::room::duplicate`] is the deliberate act of
//! copying one instead.
//!
//! Reading happens away from the session's loop: the indexes are small files
//! in the stores of apps this process does not own, and one of them being
//! slow to read must not hold up a frame. A worker reads them once and again
//! whenever an index directory changes, and the loop takes what it last read.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use app_state::DocStore;
use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use durable_store::{DurableStore, StorageKey};
use tilo_desktop_search::providers::records::HeldRecord;
use tilo_files::native::Watched;
use ui::record::RecordCheckout;

/// Ceiling on an index document this session reads. Automerge cannot read a
/// document in part, and a room index is a list of names rather than the
/// records' own content: a file past this is not an index a workspace wrote,
/// and skipping it costs a search row instead of the worker's time on every
/// change.
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

/// A record one or more workspaces on this device hold: what search offers,
/// without the names of the workspaces, which only the desk knows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    id: String,
    kind: String,
    /// The app whose index named it first: the app that opens it, and whose
    /// index the reference goes into when the record is brought here.
    package: String,
    /// Every workspace whose index names it.
    rooms: Vec<String>,
}

/// The device's record indexes, as search offers them.
pub struct Records {
    /// Where the indexes are, and where this session's own references are
    /// written.
    root: PathBuf,
    /// The indexes as last read.
    reading: Reading,
    /// The name the desk gives each room, as of its last frame: a record
    /// held elsewhere is offered under the name of the workspace that holds
    /// it, and a room id alone names nothing a person knows.
    names: RefCell<Vec<(String, String)>>,
}

/// How the indexes are read: by a worker following their directories, or —
/// where they cannot be followed — once, when the session starts.
enum Reading {
    Followed(Watched<Vec<Held>>),
    Once(Arc<Vec<Held>>),
}

impl Records {
    /// The records this device keeps, read now and followed from then on:
    /// the same documents the apps of its workspaces write, in the platform
    /// data directory.
    #[must_use]
    pub fn of_device() -> Self {
        Self::of(cli::data_home())
    }

    /// [`Self::of_device`] over the store rooted at `root`, for a session
    /// whose device keeps its state somewhere else.
    #[must_use]
    pub fn of(root: PathBuf) -> Self {
        let store: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(&root));
        let indexes = store.room_indexes().unwrap_or_else(|error| {
            tracing::warn!(%error, "the room indexes of this device are not listed");
            Vec::new()
        });
        // Each index directory, and the directory of rooms holding it: a
        // workspace made after this session started keeps its index in a
        // directory nothing watches yet, so the room appearing next to the
        // others is what tells the worker to read the indexes again.
        let mut directories: BTreeSet<PathBuf> = BTreeSet::new();
        for (package, room) in &indexes {
            match app_state::room_index_directory(&root, package, room) {
                Ok(directory) => {
                    if let Some(rooms) = directory.parent() {
                        directories.insert(rooms.to_path_buf());
                    }
                    directories.insert(directory);
                }
                Err(error) => tracing::warn!(%error, package, room, "a room index is not watched"),
            }
        }
        let reading = match Watched::start(
            "tilo-search-records",
            directories.into_iter().collect(),
            reader(root.clone()),
        ) {
            Ok(watched) => Reading::Followed(watched),
            Err(error) => {
                tracing::warn!(%error, "the record indexes are not followed");
                Reading::Once(Arc::new(read_indexes(&root)))
            }
        };
        Self {
            root,
            reading,
            names: RefCell::default(),
        }
    }

    /// The workspaces this device shows, by room and the name the desk gives
    /// it: what a row says of a record no workspace in front holds.
    pub(crate) fn set_workspaces(&self, workspaces: Vec<(String, String)>) {
        *self.names.borrow_mut() = workspaces;
    }

    /// The records the workspaces on this device hold, as search offers them
    /// in the workspace of `room`: one row per record, saying whether this
    /// workspace holds it and, when another one does, which.
    #[must_use]
    pub(crate) fn held(&self, room: &str) -> Vec<HeldRecord> {
        let held = self.latest();
        let names = self.names.borrow();
        held.iter()
            .map(|record| HeldRecord {
                id: record.id.clone(),
                kind: record.kind.clone(),
                package: record.package.clone(),
                here: record.rooms.iter().any(|held_in| held_in == room),
                workspace: record
                    .rooms
                    .iter()
                    .find(|held_in| held_in.as_str() != room)
                    .and_then(|held_in| {
                        names
                            .iter()
                            .find(|(named, _)| named == held_in)
                            .map(|(_, name)| name.clone())
                    }),
            })
            .collect()
    }

    /// Name `record` in the workspace of `room`, in the index of `package`:
    /// the reference it holds from then on, and the whole of what bringing a
    /// record here does. Written away from the loop, since the workspace may
    /// be one whose index no app of this process has opened yet; the index
    /// directory changing is what makes search read the indexes again.
    pub(crate) fn bring_here(&self, room: &str, package: &str, record: RecordCheckout) {
        let data_home = self.root.clone();
        let (room, package) = (room.to_owned(), package.to_owned());
        let named = std::thread::Builder::new()
            .name("tilo-search-bring".into())
            .spawn(move || {
                let store: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(&data_home));
                let key = StorageKey::room_index(&package, &room);
                let mut index = match DocStore::shared(store, key) {
                    Ok(index) => index,
                    Err(error) => {
                        tracing::warn!(%error, package, room, "a record is not brought here");
                        return;
                    }
                };
                let mut doc = index.load_or_new();
                if ui::record::room::reference(&mut doc, &record.id, &record.kind)
                    && let Err(error) = index.append(&mut doc)
                {
                    tracing::warn!(%error, package, room, "a record's reference is not kept");
                }
            });
        if let Err(error) = named {
            tracing::warn!(%error, "a record is not brought here");
        }
    }

    /// The indexes as last read.
    fn latest(&self) -> Arc<Vec<Held>> {
        match &self.reading {
            Reading::Followed(watched) => watched.get(),
            Reading::Once(held) => Arc::clone(held),
        }
    }
}

/// `read` over `root`: the indexes the worker reads.
fn reader(root: PathBuf) -> impl Fn() -> Vec<Held> + Send + Sync + 'static {
    move || read_indexes(&root)
}

/// Every record the indexes under `root` name, by id, with the workspaces
/// that name it. A record two workspaces hold is one entry here: it is one
/// record.
fn read_indexes(root: &Path) -> Vec<Held> {
    let store: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(root));
    let indexes = match store.room_indexes() {
        Ok(indexes) => indexes,
        Err(error) => {
            tracing::warn!(%error, "the room indexes of this device are not listed");
            return Vec::new();
        }
    };
    let mut held: BTreeMap<String, Held> = BTreeMap::new();
    for (package, room) in indexes {
        for record in named_in(store.as_ref(), &package, &room) {
            let entry = held
                .entry(record.id.as_str().to_owned())
                .or_insert_with(|| Held {
                    id: record.id.as_str().to_owned(),
                    kind: record.kind.as_str().to_owned(),
                    package: package.clone(),
                    rooms: Vec::new(),
                });
            if !entry.rooms.contains(&room) {
                entry.rooms.push(room.clone());
            }
        }
    }
    held.into_values().collect()
}

/// The records `package`'s index for `room` names, as the store holds it.
/// Read without taking a writer: an app of the workspace owns the index, and
/// a lane of search opening must not wait for it.
fn named_in(store: &dyn DurableStore, package: &str, room: &str) -> Vec<RecordCheckout> {
    let key = StorageKey::room_index(package, room);
    let stored = match store.read(&key) {
        Ok(Some(stored))
            if !stored.bytes.is_empty()
                && u64::try_from(stored.bytes.len()).unwrap_or(u64::MAX) <= MAX_INDEX_BYTES =>
        {
            stored
        }
        Ok(_) => return Vec::new(),
        Err(error) => {
            tracing::warn!(%error, package, room, "a room's records are not read");
            return Vec::new();
        }
    };
    // An app keeping the index appends to it, and this read may land on a tail
    // it has not finished writing: the changes before it are the index as of
    // the last write that completed, which is what the app itself loads.
    let options = LoadOptions::new().on_partial_load(OnPartialLoad::Ignore);
    match AutoCommit::load_with_options(&stored.bytes, options) {
        Ok(doc) => ui::record::room::discoverable_references(&doc),
        Err(error) => {
            tracing::warn!(%error, package, room, "a room's index does not load");
            Vec::new()
        }
    }
}
