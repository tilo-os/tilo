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
//! whenever the change stream says a record of this device moved — this
//! session naming one in an index writes the store that feeds the stream, and
//! so does the app that names it in its own process, whose write the store's
//! files report to it — and the loop takes what it last read.

use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::platform::Store;
use crate::platform::records::{Reading, Watch};
use durable_store::DurableStore;
use tilo_desktop_search::providers::records::HeldRecord;
use ui::record::RecordCheckout;

/// A record one or more workspaces on this device hold: what search offers,
/// without the names of the workspaces, which only the desk knows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    id: String,
    kind: String,
    /// What the index that named it first says it is called.
    name: Option<String>,
    /// The app whose index named it first: the app that opens it, and whose index the reference
    /// goes into when the record is brought here.
    package: String,
    /// Every workspace whose index names it.
    workspaces: Vec<String>,
}

/// The device's record indexes, as search offers them.
pub struct Records {
    /// Where the indexes are, and where this session's own references are written; `None` for a
    /// host whose records reach it from one workspace, which brings none of another here. A page
    /// is such a host.
    #[cfg(target_os = "linux")]
    root: Option<PathBuf>,
    /// The store the records are in, noting every write of one in the change stream: the session's
    /// one store for the workspaces' records, read by the activity and the person's mentions.
    store: Store,
    /// The one stream of record changes this device's store feeds: what this session writes, and
    /// what the apps of the workspaces write in their own processes.
    changes: crate::platform::records::Changes,
    /// The watch telling the change stream what the apps of the workspaces write from their own
    /// processes; dropping it stops the news.
    #[cfg(target_os = "linux")]
    _followed: Option<tilo_files::native::ChangeWatch>,
    /// The indexes as last read, read again whenever the change stream says a record of this
    /// device moved: the one watcher there is.
    reading: Reading<Vec<Held>>,
    /// The name the desk gives each workspace, as of its last frame: a record held elsewhere is
    /// offered under the name of the workspace that holds it, and a workspace id alone names
    /// nothing a person knows.
    names: RefCell<Vec<(String, String)>>,
}

impl Records {
    /// The records this device keeps, in the store rooted at `root`: the same documents the apps
    /// of its workspaces write, read now and followed from then on, in this process or theirs.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn of(root: PathBuf) -> Self {
        let changes = crate::platform::records::Changes::default();
        let plain: Store = Arc::new(app_state::FsStore::new(&root));
        let store = crate::platform::records::noting(plain, changes.clone());
        Self {
            _followed: crate::platform::records::follow(&root, &changes),
            root: Some(root),
            ..Self::read(store, changes)
        }
    }

    /// The records of the workspace whose records `store` keeps as they reach this host, read
    /// again whenever `changes` says one moved: a page's, whose replication writes the store and
    /// notes each record it takes in. What it holds is the one workspace's, so nothing of another
    /// is brought here.
    #[must_use]
    pub fn over(store: Store, changes: crate::platform::records::Changes) -> Self {
        Self::read(store, changes)
    }

    fn read(store: Store, changes: crate::platform::records::Changes) -> Self {
        // What the indexes name is read again whenever a record of this device moves: an app of a
        // workspace naming one writes the store this stream hears, in this process or its own, and
        // so does this session.
        let reading = reading(store.clone(), &changes);
        Self {
            #[cfg(target_os = "linux")]
            root: None,
            store,
            changes,
            #[cfg(target_os = "linux")]
            _followed: None,
            reading,
            names: RefCell::default(),
        }
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
                name: record.name.clone(),
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
    /// the activity and the mentions follow. Whether this host brings records here at all.
    pub fn bring_here(&self, workspace: &str, package: &str, record: RecordCheckout) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(root) = self.root.clone() {
            let (workspace, package) = (workspace.to_owned(), package.to_owned());
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
            return true;
        }
        let _ = (workspace, package, record);
        false
    }

    /// The indexes as last read.
    fn latest(&self) -> Arc<Vec<Held>> {
        self.reading.following(&self.changes, &Watch::Any)
    }
}

/// Name `record` in the index of `package` in `workspace` , as its app would. Whether it is named.
#[cfg(target_os = "linux")]
fn name_in(root: &Path, workspace: &str, package: &str, record: RecordCheckout) -> bool {
    let RecordCheckout { id, kind, .. } = record;
    let store: std::rc::Rc<dyn DurableStore> =
        std::rc::Rc::new(app_state::FsStore::new(root.to_path_buf()));
    let key = durable_store::StorageKey::workspace_index(package, workspace);
    let mut index = match app_state::DocStore::shared(store, key) {
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

/// The indexes in `store` , read now and again whenever the records of this device move.
#[cfg(target_os = "linux")]
fn reading(store: Store, changes: &crate::platform::records::Changes) -> Reading<Vec<Held>> {
    Reading::start(
        "tilo-search-records",
        changes.clone(),
        Watch::Any,
        move || read_indexes(store.as_ref()),
        || {},
    )
}

/// [`reading`] where this host has one thread: read now, and again as the stream says they moved.
#[cfg(not(target_os = "linux"))]
fn reading(store: Store, changes: &crate::platform::records::Changes) -> Reading<Vec<Held>> {
    Reading::read(changes, &Watch::Any, move || read_indexes(store.as_ref()))
}

/// Every record the indexes in `store` name, by id, with the workspaces that name it. A record
/// two workspaces hold is one entry here: it is one record.
fn read_indexes(store: &dyn DurableStore) -> Vec<Held> {
    let indexes = match store.workspace_indexes() {
        Ok(indexes) => indexes,
        Err(error) => {
            tracing::warn!(%error, "the workspace indexes of this device are not listed");
            return Vec::new();
        }
    };
    let mut held: BTreeMap<String, Held> = BTreeMap::new();
    for (package, workspace) in indexes {
        for (record, name) in crate::platform::records::named_checkouts(store, &package, &workspace)
            .unwrap_or_default()
        {
            let entry = held
                .entry(record.id.as_str().to_owned())
                .or_insert_with(|| Held {
                    id: record.id.as_str().to_owned(),
                    kind: record.kind.as_str().to_owned(),
                    name: None,
                    package: package.clone(),
                    workspaces: Vec::new(),
                });
            if entry.name.is_none() {
                entry.name = name;
            }
            if !entry.workspaces.contains(&workspace) {
                entry.workspaces.push(workspace.clone());
            }
        }
    }
    held.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use durable_store::{StorageKey, WriteOp};
    use ui::record::{RecordId, RecordKind};

    const NOTES: &str = "so.tilo.notes";
    const WORKSPACE: &str = "6d1c1f36-0000-4000-8000-000000000001";
    const NOTE: &str = "tab:00000000000000a1";

    /// A record an app names in its workspace index and writes is one search offers: the list
    /// follows the change stream every write of the records lands in, not a directory of its own.
    #[test]
    fn a_record_an_app_writes_through_the_store_is_one_search_finds() {
        let dir = tempfile::tempdir().expect("a store");
        let records = Records::of(dir.path().to_path_buf());
        let store = records.store();
        assert!(
            records.held(WORKSPACE).is_empty(),
            "the list starts as the store holds it"
        );

        // The app names its record in the workspace's index, and writes the record.
        let (id, kind) = (
            RecordId::parse(NOTE).expect("a record id"),
            RecordKind::new("notes.notebook"),
        );
        let mut index = AutoCommit::new();
        assert!(ui::record::index::reference(&mut index, &id, &kind));
        assert!(ui::record::index::set_name(
            &mut index,
            &id,
            Some("Groceries")
        ));
        index.commit();
        write(
            &store,
            &StorageKey::workspace_index(NOTES, WORKSPACE),
            &mut index,
        );
        let mut note = AutoCommit::new();
        ui::record::init(&mut note, &id, &kind, WORKSPACE);
        write(
            &store,
            &StorageKey::document(NOTES, WORKSPACE, id.storage_key()),
            &mut note,
        );

        // The list is read again on its worker, off the change the writes made: search finds the
        // record by the name its index gave it.
        let found = wait_for(&records, |records| !records.held(WORKSPACE).is_empty());
        let row = found.first().expect("search offers the record");
        assert_eq!(row.id, NOTE, "{found:?}");
        assert_eq!(row.kind, "notes.notebook");
        assert_eq!(row.package, NOTES);
        assert_eq!(
            row.name.as_deref(),
            Some("Groceries"),
            "named as its index names it"
        );
        assert!(row.here, "the workspace in front holds it: {found:?}");
    }

    /// The list `read` answers once it shows what the test waits for, or what it last showed.
    fn wait_for(
        records: &Records,
        done: impl Fn(&Records) -> bool,
    ) -> Vec<tilo_desktop_search::providers::records::HeldRecord> {
        let until = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(2))
            .expect("the wait's bound fits in an Instant");
        loop {
            let held = records.held(WORKSPACE);
            if done(records) || std::time::Instant::now() > until {
                return held;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Write `doc` at `key` through `store`, as an app of the workspace does.
    fn write(store: &Store, key: &StorageKey, doc: &mut AutoCommit) {
        let lease = store.lease(key).expect("the key is this test's");
        let bytes = doc.save();
        store
            .write(key, WriteOp::Replace(bytes), None)
            .expect("the document is written");
        store.flush(Some(key)).expect("the document is kept");
        drop(lease);
    }
}
