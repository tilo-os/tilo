//! The content of the workspaces' records, in the device's search index, without an app of theirs
//! running.
//!
//! A resource is what the workspace keeps; a window is one reading of it. What search finds is
//! therefore the resource's to answer: this reads every record a write lands in, asks the app whose
//! record it is what it holds ([`RecordReader`], the app's own binary in its record mode, which
//! links the model types the daemon does not), and publishes the answer to the same
//! [`AppContentIndex`] a running app publishes to. A device that never opened Chat still finds its
//! messages, a conversation or an agent, and one that deletes a message has it gone from search
//! with it.
//!
//! What is read is what this device's store holds ([`StoreReader`]): a workspace it does not
//! keep, a record whose keys are not here, and a record it may not read are all nothing to index
//! — the device's appetite decides, not a list of apps.
//!
//! A tilolang app keeps its resources in the workspace's resource store, which places them in
//! whichever of the app's documents it chooses. A write to one of them has that document's
//! resources read from the store, through the app's package on this device and as the device's
//! person reads them ([`StoreReader`]), and publishes the whole of the document's set in a slot
//! of its own: a document that changed replaces what search held of it rather than adding to
//! it, so a resource or a paragraph it no longer holds goes from search with the write. Notes,
//! Tasks and Chat are found on a device that never opened them, and what is deleted goes from
//! search with no window open either. A slot is a store document, which a caller who may read
//! that document passes, so an entry of a resource the store sealed to its readers (a direct
//! conversation, an unsent draft) names them, and search answers it to them alone
//! ([`IndexEntry::readers`]).
//!
//! Writes are batched per record, and the worker reads each record once per pass on its own
//! thread, so a burst of changes to one conversation costs one reading of it.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, Weak};

use tilo_lock::MutexRecover as _;
use tilo_resource::search_index::{IndexEntry, IndexState, Publisher};

use super::{AppContentIndex, IndexKey};

/// The argument an app's binary is started with to read one of its records for search: it reads
/// the record on its input and answers its entries on its output.
pub const INDEX_FLAG: &str = "--index";

/// What an app answers search about one of its records: the app's own model types, read from the
/// document, as its window reads them.
pub trait RecordReader: Send + Sync + 'static {
    /// What `app`'s record `doc` holds for search.
    ///
    /// # Errors
    /// [`Unread::Stopped`] for a reader that was stopped before it answered, and
    /// [`Unread::Refused`] for a record the app cannot read.
    fn entries(&self, app: &str, doc: &[u8]) -> Result<Vec<IndexEntry>, Unread>;
}

/// What the workspaces' stores hold, as search reads them: a tilolang app's resources, read
/// through its package as a window of the app publishes them, and an app's own records, whose
/// documents its own reader reads ([`RecordReader`]).
pub trait StoreReader: Send + Sync + 'static {
    /// The workspaces this device holds.
    fn workspaces(&self) -> Vec<String>;

    /// Every record of an app's own kind `workspace`'s store holds, by its app and id.
    ///
    /// # Errors
    /// The workspace is not held here.
    fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String>;

    /// The document `app`'s record `record` of `workspace` is, `None` while the store holds
    /// nothing of it.
    ///
    /// # Errors
    /// The record cannot be read now.
    fn record(&self, workspace: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String>;

    /// Whether `app` keeps its resources in the workspace's store and is read here through a
    /// package of it this device installed or this build carries: a tilolang app.
    fn reads(&self, app: &str) -> bool;

    /// Every entry the store document `record` of `app` in `workspace` publishes now.
    ///
    /// # Errors
    /// The app's package does not load, or the store does not read now.
    fn entries(&self, workspace: &str, app: &str, record: &str) -> Result<Vec<IndexEntry>, String>;
}

/// Why an app's reader did not say what a record of its holds.
#[derive(Debug)]
pub enum Unread {
    /// Whoever read it was stopped before it answered (the daemon stopping, the app closed for
    /// memory), so nothing is known of the record's content: not a record that holds nothing.
    Stopped(String),
    /// The app cannot read the record: it says why.
    Refused(String),
}

/// A record by its workspace, app and route.
type Record = (String, String, String);

/// The records whose content search offers, read from the records themselves.
pub struct RecordIndex {
    reader: Arc<dyn RecordReader>,
    /// What the tilolang apps' resources say, read from the workspace's store.
    stores: Arc<dyn StoreReader>,
    content: Arc<AppContentIndex>,
    /// The records to read, each once however often it changed.
    queued: Mutex<BTreeSet<Record>>,
    /// The records whose keys were not here when they were read: read again on the next write
    /// to their workspace, which may be the keys arriving.
    sealed: Mutex<BTreeSet<Record>>,
    /// The apps whose entries this index published in one slot per app before it published one
    /// per document: their app-wide slot is emptied once, so search holds nothing twice.
    cleared: Mutex<BTreeSet<(String, String)>>,
    /// The workspaces this device held when they were last read whole ([`RecordIndex::sweep`]).
    swept: Mutex<BTreeSet<String>>,
    /// Wakes the worker: a record is waiting.
    wake: std::sync::mpsc::Sender<()>,
}

impl RecordIndex {
    /// Index what `stores` hold, reading each app's record through `reader` (a tilolang app's
    /// resources through `stores`) and publishing to `content`, from a worker of its own: what
    /// this device holds is read at once, and each record [`Self::changed`] names after.
    pub fn start(
        reader: Arc<dyn RecordReader>,
        stores: Arc<dyn StoreReader>,
        content: Arc<AppContentIndex>,
    ) -> Arc<Self> {
        let (wake, woken) = std::sync::mpsc::channel();
        let index = Arc::new(Self {
            reader,
            stores,
            content,
            queued: Mutex::new(BTreeSet::new()),
            sealed: Mutex::new(BTreeSet::new()),
            cleared: Mutex::new(BTreeSet::new()),
            swept: Mutex::new(BTreeSet::new()),
            wake,
        });
        // The worker reads the documents and the app's reader, neither of which may hold up the
        // daemon's runtime: it wakes only when a record is waiting, and reads each one once.
        let worker = Arc::downgrade(&index);
        let started = std::thread::Builder::new()
            .name("tilo-record-index".into())
            .spawn(move || {
                while woken.recv().is_ok() {
                    let Some(index) = Weak::upgrade(&worker) else {
                        break;
                    };
                    index.sweep();
                    index.pass();
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "the records are not indexed");
        }
        let _ = index.wake.send(());
        index
    }

    /// Queue every record of each workspace this device holds that was not read whole yet, so
    /// search finds it before anything changes.
    fn sweep(&self) {
        for workspace in self.stores.workspaces() {
            if self.swept.lock_recover().contains(&workspace) {
                continue;
            }
            match self.stores.records(&workspace) {
                Ok(records) => {
                    self.swept.lock_recover().insert(workspace.clone());
                    let mut queued = self.queued.lock_recover();
                    for (app, record) in records {
                        queued.insert((workspace.clone(), app, record));
                    }
                }
                Err(error) => tracing::warn!(
                    %error,
                    %workspace,
                    "the records search holds are not listed"
                ),
            }
        }
    }

    /// `record` of `app` changed in `workspace`: read it on the next pass, once however often it
    /// changed, and what of the workspace had no keys here again with it. A tilolang app's
    /// write names the store document it landed in, which is read from the store.
    pub fn changed(&self, workspace: &str, app: &str, record: &str) {
        let mut queued = self.queued.lock_recover();
        let mut sealed = self.sealed.lock_recover();
        let unsealed: Vec<Record> = sealed
            .iter()
            .filter(|(held, _, _)| held == workspace)
            .cloned()
            .collect();
        for record in unsealed {
            sealed.remove(&record);
            queued.insert(record);
        }
        queued.insert((workspace.to_owned(), app.to_owned(), record.to_owned()));
        drop((queued, sealed));
        let _ = self.wake.send(());
    }

    /// Read every record queued since the last pass, each once, and publish what it now holds.
    fn pass(&self) {
        let queued: Vec<Record> = std::mem::take(&mut *self.queued.lock_recover())
            .into_iter()
            .collect();
        // A tilolang app's resources are read from the store document the change names: the
        // store placed them, and reads that document whole.
        let mut stored = BTreeSet::new();
        for (workspace, app, record) in queued {
            if self.stores.reads(&app) {
                stored.insert((workspace, app, record));
                continue;
            }
            // A write of the store names the document it wrote: an app's record is the content
            // document of its resource, and a document of resources holds none.
            let record = match tilo_resource_store::content_resource(&record) {
                Some((_, id)) => id.to_owned(),
                None if collab_types::is_store_document(&record) => continue,
                None => record,
            };
            if record.is_empty() {
                continue;
            }
            let doc = match self.stores.record(&workspace, &app, &record) {
                Ok(Some(doc)) => doc,
                // Its keys are not here yet: it is read again with the next write there.
                Ok(None) => {
                    self.sealed.lock_recover().insert((workspace, app, record));
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, %workspace, %app, %record, "a record is not indexed");
                    continue;
                }
            };
            let entries = match self.reader.entries(&app, &doc) {
                Ok(entries) => entries,
                // Stopped before it answered: nothing is known of what the record holds, so it is
                // read again on its next change rather than kept as a record holding nothing.
                Err(Unread::Stopped(reason)) => {
                    tracing::debug!(%reason, %app, %record, "a record's reading was stopped");
                    continue;
                }
                Err(Unread::Refused(error)) => {
                    tracing::warn!(%error, %app, %record, "a record's app reads none of it");
                    continue;
                }
            };
            self.publish(&IndexKey::new(&app, &workspace, &record), entries);
        }
        for (workspace, app, record) in stored {
            self.clear_app_slot(&workspace, &app);
            match self.stores.entries(&workspace, &app, &record) {
                // Under the key of the document the change named: each document's set is
                // published whole, so what it no longer holds goes with it.
                Ok(entries) => self.publish(&IndexKey::new(&app, &workspace, &record), entries),
                // Read again with the next write there, which may be the keys or the policy
                // arriving.
                Err(error) => {
                    tracing::warn!(
                        %error,
                        %workspace,
                        %app,
                        %record,
                        "an app's resources are not indexed"
                    );
                    self.sealed.lock_recover().insert((workspace, app, record));
                }
            }
        }
    }

    /// Empty the one slot per app this index published before it read documents one at a time,
    /// the first time `app`'s resources of `workspace` are published: what search would
    /// otherwise hold twice, once for the app and once for each of its documents.
    fn clear_app_slot(&self, workspace: &str, app: &str) {
        if !self
            .cleared
            .lock_recover()
            .insert((workspace.to_owned(), app.to_owned()))
        {
            return;
        }
        self.publish(&IndexKey::new(app, workspace, ""), Vec::new());
    }

    /// Publish `entries` as the whole of what `slot` holds: what it no longer holds, deleted
    /// messages included, goes with it.
    fn publish(&self, slot: &IndexKey, entries: Vec<IndexEntry>) {
        let deltas = Publisher::default().publish(
            IndexState {
                generation: 0,
                secrets_unlocked: None,
            },
            entries,
        );
        for delta in deltas {
            self.content.apply(slot, delta);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};

    use tilo_actions::remote::{Caller, Within};
    use tilo_lock::MutexRecover as _;
    use tilo_resource::search_index::IndexEntry;

    use super::{RecordIndex, RecordReader, StoreReader, Unread};
    use crate::search::{AppContentIndex, IndexSearch};

    const WORKSPACE: &str = "w-team";
    const NOTES: &str = "so.tilo.notes";
    /// The documents the device holds, and how many notes each holds.
    const DOCUMENTS: usize = 100;
    const PER_DOCUMENT: usize = 100;

    /// The store document `n`.
    fn document(n: usize) -> String {
        format!("notes/{n}")
    }

    /// The entry of the note `notes/<n>/<i>`, found by the one word `word<n>x<i>`: its numbers
    /// padded, so a word finds its own note alone and no note whose number goes on from it.
    fn note(n: usize, i: usize) -> IndexEntry {
        IndexEntry {
            key: format!("notes/{n}/{i}"),
            resource: format!("notes/{n}/{i}"),
            title: format!("word{n:02}x{i:02}"),
            ..IndexEntry::default()
        }
    }

    /// The app read in its record mode: nothing here is read that way.
    struct Records;

    impl RecordReader for Records {
        fn entries(&self, app: &str, _: &[u8]) -> Result<Vec<IndexEntry>, Unread> {
            Err(Unread::Refused(format!("{app} is read from its store")))
        }
    }

    /// The workspace's store, read a document at a time: what each document holds now, and the
    /// documents every read was for.
    struct Store {
        held: Mutex<BTreeMap<String, Vec<IndexEntry>>>,
        read: Mutex<Vec<String>>,
    }

    impl Store {
        fn of(held: BTreeMap<String, Vec<IndexEntry>>) -> Self {
            Self {
                held: Mutex::new(held),
                read: Mutex::new(Vec::new()),
            }
        }

        /// What `document` holds now.
        fn held(&self, document: &str) -> Vec<IndexEntry> {
            self.held
                .lock_recover()
                .get(document)
                .cloned()
                .unwrap_or_default()
        }

        /// What `document` holds from now on.
        fn edit(&self, document: &str, entries: Vec<IndexEntry>) {
            self.held
                .lock_recover()
                .insert(document.to_owned(), entries);
        }

        /// The documents read since the last ask, each once.
        fn count(&self) -> Vec<String> {
            std::mem::take(&mut *self.read.lock_recover())
        }
    }

    /// A device whose workspace holds no record of an app's own: what it holds is its store's
    /// documents.
    impl StoreReader for Store {
        fn workspaces(&self) -> Vec<String> {
            vec![WORKSPACE.to_owned()]
        }

        fn records(&self, _: &str) -> Result<Vec<(String, String)>, String> {
            Ok(Vec::new())
        }

        fn record(&self, _: &str, _: &str, _: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }

        fn reads(&self, app: &str) -> bool {
            app == NOTES
        }

        fn entries(
            &self,
            _workspace: &str,
            _app: &str,
            record: &str,
        ) -> Result<Vec<IndexEntry>, String> {
            self.read.lock_recover().push(record.to_owned());
            Ok(self.held(record))
        }
    }

    /// One note of one store document edited of ten thousand in a hundred documents: the
    /// document it landed in alone is read, published whole, and search holds the note's new
    /// words in place of the ones it no longer holds.
    #[test]
    fn an_edit_reads_the_document_it_landed_in_and_publishes_it_whole() {
        let mut held = BTreeMap::new();
        for n in 0..DOCUMENTS {
            held.insert(document(n), (0..PER_DOCUMENT).map(|i| note(n, i)).collect());
        }
        let store = Arc::new(Store::of(held));
        let content = Arc::new(AppContentIndex::in_memory());
        // An index no worker drives: the test reads a pass at a time, and counts them.
        let index = RecordIndex {
            reader: Arc::new(Records),
            stores: Arc::clone(&store) as Arc<dyn StoreReader>,
            content: Arc::clone(&content),
            queued: Mutex::new(BTreeSet::new()),
            sealed: Mutex::new(BTreeSet::new()),
            cleared: Mutex::new(BTreeSet::new()),
            swept: Mutex::new(BTreeSet::new()),
            wake: std::sync::mpsc::channel().0,
        };
        // What the device holds, published first: each document read once.
        for n in 0..DOCUMENTS {
            index.changed(WORKSPACE, NOTES, &document(n));
        }
        index.pass();
        assert_eq!(
            store.count().len(),
            DOCUMENTS,
            "every document the device holds was read once"
        );
        let search = IndexSearch::new(Arc::clone(&content), crate::search::deciding(|_, _| true));
        let ada = Caller::local_person("ada");
        let found = search.found(&ada, "word07x03", &Within::default());
        assert_eq!(found.len(), 1, "one note holds the word: {found:?}");
        assert_eq!(found[0].workspace.as_deref(), Some(WORKSPACE));
        assert_eq!(found[0].app.as_deref(), Some(NOTES));

        // The note `notes/7/3` is edited: the document it is in alone is read.
        let edited = document(7);
        let mut entries = store.held(&edited);
        entries[3] = IndexEntry {
            title: "groceries for the weekend".to_owned(),
            ..entries[3].clone()
        };
        store.edit(&edited, entries);
        index.changed(WORKSPACE, NOTES, &edited);
        index.pass();
        assert_eq!(
            store.count(),
            [edited],
            "one edit reads the one document it landed in"
        );

        let found = search.found(&ada, "groceries", &Within::default());
        assert_eq!(found.len(), 1, "the edited note is found: {found:?}");
        assert_eq!(found[0].resource, "notes/7/3");
        let found = search.found(&ada, "word07x03", &Within::default());
        assert!(
            found.is_empty(),
            "the words it no longer holds are gone: {found:?}"
        );
        let found = search.found(&ada, "word08x03", &Within::default());
        assert_eq!(found.len(), 1, "another document's notes stay: {found:?}");
    }
}
