//! The records the workspaces on this device hold, and the reference that
//! brings one of them into the workspace in front.
//!
//! A workspace holds a record by naming it: each of its apps keeps a
//! workspace index ([`ui::record::index`]) saying which records the workspace
//! holds and what kind each one is, one document per app and workspace.
//! Every app of the workspace writes that index as a shared writer, so this
//! session writes it too: naming a record there is the same act as the app
//! naming it, and what it names is what the workspace's apps read back as the
//! records they may open.
//!
//! Search offers every index on the device, as the worker last read them, so
//! a record made elsewhere — another workspace's note, a file-backed document,
//! one a peer brought into a workspace — is found by its name. Bringing it
//! here is then one write, a reference into the workspace in front: no bytes
//! move between the two workspaces' documents and no id is minted, because
//! the id is the record. [`ui::record::index::duplicate`] is the deliberate
//! act of copying one instead.
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

use crate::platform::Store;
use crate::platform::watched::Watched;
use app_state::DocStore;
use durable_store::{DurableStore, StorageKey};
use tilo_desktop_search::providers::records::HeldRecord;
use ui::record::RecordCheckout;

/// A record one or more workspaces on this device hold: what search offers,
/// without the names of the workspaces, which only the desk knows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    id: String,
    kind: String,
    /// The app whose index named it first: the app that opens it, and whose index the reference
    /// goes into when the record is brought here.
    package: String,
    /// Every workspace whose index names it.
    workspaces: Vec<String>,
}

/// The device's record indexes, as search offers them.
pub struct Records {
    /// Where the indexes are, and where this session's own references are written.
    root: PathBuf,
    /// The store the records are in, noting every write of one in the change stream: the session's
    /// one store for the workspaces' records, read by the activity and the person's mentions.
    store: Store,
    /// The one stream of record changes this device's store feeds.
    changes: crate::platform::records::Changes,
    /// The indexes as last read.
    reading: Reading,
    /// The name the desk gives each workspace, as of its last frame: a record held elsewhere is
    /// offered under the name of the workspace that holds it, and a workspace id alone names
    /// nothing a person knows.
    names: RefCell<Vec<(String, String)>>,
}

/// The indexes as last read, read again whenever the change stream says a workspace's records
/// moved: the store every writer of the records lands in is the one watcher there is.
enum Reading {
    Followed(Watched<Vec<Held>>),
    Once(Arc<Vec<Held>>),
}

impl Records {
    /// The records this device keeps, read now and followed from then on: the same documents the
    /// apps of its workspaces write, in the platform data directory.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn of_device() -> Self {
        Self::of(cli::data_home())
    }

    /// [`Self::of_device`] over the store rooted at `root` , for a session whose device keeps its
    /// state somewhere else.
    #[must_use]
    pub fn of(root: PathBuf) -> Self {
        let changes = crate::platform::records::Changes::default();
        #[cfg(target_os = "linux")]
        let plain: Store = std::sync::Arc::new(app_state::FsStore::new(&root));
        #[cfg(not(target_os = "linux"))]
        let plain: Store = std::rc::Rc::new(app_state::FsStore::new(&root));
        let store = crate::platform::records::noting(plain, changes.clone());
        let indexes = store.workspace_indexes().unwrap_or_else(|error| {
            tracing::warn!(%error, "the workspace indexes of this device are not listed");
            Vec::new()
        });
        // The index directories the reader follows: a record written into one of them reads the
        // list again. A workspace made after this session started keeps its index in a directory
        // nothing watches yet, so the directory of workspaces holding them is followed too.
        let mut directories: BTreeSet<PathBuf> = BTreeSet::new();
        for (package, workspace) in &indexes {
            match app_state::workspace_index_directory(&root, package, workspace) {
                Ok(directory) => {
                    if let Some(holding) = directory.parent() {
                        directories.insert(holding.to_path_buf());
                    }
                    directories.insert(directory);
                }
                Err(error) => tracing::warn!(
                    %error,
                    package,
                    %workspace,
                    "a workspace's index is not watched"
                ),
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
            store,
            changes,
            reading,
            names: RefCell::default(),
        }
    }

    /// Where this device keeps its record indexes, and where a record's document is.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    /// The store the workspaces' records are kept in, as this session reads them.
    #[must_use]
    pub fn store(&self) -> Store {
        Store::clone(&self.store)
    }

    /// The one stream of record changes this device's store feeds.
    #[must_use]
    pub fn changes(&self) -> crate::platform::records::Changes {
        self.changes.clone()
    }

    /// The workspaces this device shows, by id and the name the desk gives it: what a row says of a
    /// record no workspace in front holds.
    pub fn set_workspaces(&self, workspaces: Vec<(String, String)>) {
        *self.names.borrow_mut() = workspaces;
    }

    /// The records the workspaces on this device hold, as search offers them in the workspace
    /// `workspace` : one row per record, saying whether this workspace holds it and, when another
    /// one does, which.
    #[must_use]
    pub fn held(&self, workspace: &str) -> Vec<HeldRecord> {
        let held = self.latest();
        let names = self.names.borrow();
        held.iter()
            .map(|record| HeldRecord {
                id: record.id.clone(),
                kind: record.kind.clone(),
                package: record.package.clone(),
                here: record.workspaces.iter().any(|held_in| held_in == workspace),
                workspace: record
                    .workspaces
                    .iter()
                    .find(|held_in| held_in.as_str() != workspace)
                    .and_then(|held_in| {
                        names
                            .iter()
                            .find(|(named, _)| named == held_in)
                            .map(|(_, name)| name.clone())
                    }),
            })
            .collect()
    }

    /// Whether any workspace on this device still names `record` . A row is collected from one
    /// reading of the indexes and chosen later, and an app may have stopped naming the record in
    /// between: naming it here would hold a record no workspace has, so such a row is refused
    /// instead.
    #[must_use]
    pub fn holds(&self, record: &str) -> bool {
        self.latest().iter().any(|held| held.id == record)
    }

    /// Name `record` in the workspace `workspace` , in the index of `package` : the reference it
    /// holds from then on, and the whole of what bringing a record here does. Written away from the
    /// loop, through the one store the records are in, whose write notes the change in the stream
    /// the activity and the mentions follow.
    pub fn bring_here(&self, workspace: &str, package: &str, record: RecordCheckout) {
        let (workspace, package) = (workspace.to_owned(), package.to_owned());
        #[cfg(target_os = "linux")]
        {
            let root = self.root.clone();
            let changes = self.changes.clone();
            let (keeping, held) = (workspace.clone(), package.clone());
            let named = std::thread::Builder::new()
                .name("tilo-search-bring".into())
                .spawn(move || {
                    if name_in(&root, &workspace, &package, record) {
                        changes.note(&keeping, &held, "", false);
                    }
                });
            if let Err(error) = named {
                tracing::warn!(%error, "a record is not brought here");
            }
        }
        #[cfg(not(target_os = "linux"))]
        if name_in(&self.root, &workspace, &package, record) {
            self.changes.note(&workspace, &package, "", false);
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

/// Name `record` in the index of `package` in `workspace` , as its app would. Whether it is named.
fn name_in(root: &Path, workspace: &str, package: &str, record: RecordCheckout) -> bool {
    let RecordCheckout { id, kind, .. } = record;
    let store: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(root.to_path_buf()));
    let key = StorageKey::workspace_index(package, workspace);
    let mut index = match DocStore::shared(store, key) {
        Ok(index) => index,
        Err(error) => {
            tracing::warn!(%error, package, %workspace, "a record is not brought here");
            return false;
        }
    };
    let mut doc = index.load_or_new();
    if !ui::record::index::reference(&mut doc, &id, &kind) {
        return true;
    }
    match index.append(&mut doc) {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(%error, package, %workspace, "a record's reference is not kept");
            false
        }
    }
}

/// `read` over `root` : the indexes the worker reads.
fn reader(root: PathBuf) -> impl Fn() -> Vec<Held> + Send + Sync + 'static {
    move || read_indexes(&root)
}

/// Every record the indexes under `root` name, by id, with the workspaces that name it. A record
/// two workspaces hold is one entry here: it is one record.
fn read_indexes(root: &Path) -> Vec<Held> {
    let store: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(root));
    let indexes = match store.workspace_indexes() {
        Ok(indexes) => indexes,
        Err(error) => {
            tracing::warn!(%error, "the workspace indexes of this device are not listed");
            return Vec::new();
        }
    };
    let mut held: BTreeMap<String, Held> = BTreeMap::new();
    for (package, workspace) in indexes {
        for record in named_in(store.as_ref(), &package, &workspace) {
            let entry = held
                .entry(record.id.as_str().to_owned())
                .or_insert_with(|| Held {
                    id: record.id.as_str().to_owned(),
                    kind: record.kind.as_str().to_owned(),
                    package: package.clone(),
                    workspaces: Vec::new(),
                });
            if !entry.workspaces.contains(&workspace) {
                entry.workspaces.push(workspace.clone());
            }
        }
    }
    held.into_values().collect()
}

/// The records `package` 's index for `workspace` names, as the store holds it: read without taking
/// a writer, since an app of the workspace owns the index.
fn named_in(store: &dyn DurableStore, package: &str, workspace: &str) -> Vec<RecordCheckout> {
    crate::platform::records::checkouts(store, package, workspace).unwrap_or_default()
}
