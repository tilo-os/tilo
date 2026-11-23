//! The workspace records the session itself reads and writes — Chat's record of a workspace's
//! agents, its emoji and its conversations — held as the workspace's own copy of them: the one
//! its documents live in, which every app of the workspace, the person's other devices and the
//! index read. The session opens them as any app's window opens a workspace document
//! ([`WorkspaceResources`]), over stores kept in memory and replicated with that copy the way a
//! page replicates its documents with its workspace: what the workspace holds is merged into the
//! stores, and what the session changes in them is written into the workspace through this
//! device's service — sealed where the workspace's documents are sealed for its members — and
//! handed to this device's apps as a member's write is. No copy of its own is kept beside the
//! workspace's.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::{Rc, Weak};
use std::sync::{Arc, mpsc};

use collab_types::DocumentMutation;
use durable_store::{DurableStore as _, MemoryStore, StorageKey, WriteOp};
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::WorkspaceId;
use ui::permissions::MemberId;
use ui::resource::workspace::WorkspaceResources;
use ui::resource::{DocumentSync, ResourceOwners, ResourceStore};

use crate::engine_write::{Writing, Written};
use crate::workspace_view::WorkspaceView;

/// The session's copies of the workspaces' records, replicated with the device's daemon. Cloning shares
/// them.
#[derive(Clone)]
pub(crate) struct EngineRecords(Rc<Inner>);

struct Inner {
    store: Rc<MemoryStore>,
    owners: ResourceOwners,
    /// How the person's workspaces stand on the device's daemon, which holds them, once the
    /// session follows them.
    view: RefCell<Option<Arc<WorkspaceView>>>,
    /// Where this computer's own workspace service keeps what it keeps of its workspaces.
    service_root: RefCell<Option<std::path::PathBuf>>,
    /// The documents of the workspaces this device's service holds, once the session opened it:
    /// what the workspace holds of a record reads from there.
    reads: RefCell<Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>>>,
    /// What this device's service writes into a workspace's documents: where a write of the
    /// session's reaches the workspace, signed, sealed and kept as this device's.
    writes: RefCell<Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>>>,
    /// Where a write reaches this device's apps, as a member's write does.
    apps: RefCell<Option<crate::sync::PeerDocuments>>,
    /// The store of each app's records in each workspace, by workspace and app, as handed out.
    stores: RefCell<BTreeMap<(String, String), ResourceStore>>,
    /// Each record's replication with the device's daemon, by workspace, app and wire record.
    replicas: RefCell<HashMap<DocumentKey, Replica>>,
    /// The workspaces whose records moved in the device's daemon, told as each one does.
    moved: RefCell<Option<mpsc::Receiver<crate::platform::records::Changed>>>,
    /// The stream of record changes, told what the session wrote once the workspace took it.
    changes: RefCell<Option<crate::platform::records::Changes>>,
    /// The stores, by workspace and app, the session wrote to since their changes were last
    /// written into the workspaces.
    written: RefCell<BTreeSet<(String, String)>>,
    /// The stores, by workspace and app, holding a record the device's daemon has that this device cannot
    /// read yet (its channel's keys are on their way): read again on the next pass.
    unread: RefCell<BTreeSet<(String, String)>>,
    /// Set while the daemon's copy of a record is kept in the store: no write of the session's.
    keeping: Cell<bool>,
    /// The writes under way, by workspace, app and wire record, with the heads each one writes up
    /// to: a record is written again once its write is done.
    writing: RefCell<HashMap<DocumentKey, (Writing, Vec<automerge::ChangeHash>)>>,
    /// The records the session's services keep as an app's window keeps them — a tilolang app's
    /// record, a document of the app's own — by workspace, app and wire record: each the one
    /// document the service runs over, replicated with the device's daemon here.
    documents: RefCell<HashMap<DocumentKey, Document>>,
    /// Wakes the session's loop when the session wrote to a record, so the write reaches the
    /// engine on the next pass.
    wake: tilo_service::Wake,
}

/// A record, by workspace, app and wire record.
type DocumentKey = (String, String, String);

/// A document a service of the session keeps in a workspace: one other threads write too.
#[derive(Clone)]
pub(crate) struct KeptDoc(Arc<std::sync::Mutex<automerge::AutoCommit>>);

impl KeptDoc {
    /// `doc` kept for a service that writes it from another thread.
    pub(crate) fn shared(doc: Arc<std::sync::Mutex<automerge::AutoCommit>>) -> Self {
        Self(doc)
    }

    fn borrow_mut(&self) -> std::sync::MutexGuard<'_, automerge::AutoCommit> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// An app's record a service of the session runs over, as the device's daemon and the service hold it.
struct Document {
    /// The document itself, the service's: what it runs over and changes.
    doc: KeptDoc,
    /// The heads of `doc` the device's daemon holds, as far as this copy knows: `None` until it was read
    /// from the device's daemon or founded there.
    synced: Option<Vec<automerge::ChangeHash>>,
    /// The revision of the daemon's copy last taken in here.
    revision: Option<[u8; 32]>,
    /// The heads `doc` stood at when it was handed over: while it still stands there, it holds
    /// only what the service started it with (a development session's scenario data), which a
    /// record the workspace already holds takes the place of.
    handed: Vec<automerge::ChangeHash>,
}

/// One record as the device's daemon and this copy hold it.
struct Replica {
    sync: DocumentSync,
    /// The revision of the daemon's copy last merged here.
    revision: Option<[u8; 32]>,
}

impl EngineRecords {
    /// Empty copies, waking the session's loop through `wake` when the session wrote to one.
    pub(crate) fn new(wake: tilo_service::Wake) -> Self {
        Self(Rc::new_cyclic(|inner: &Weak<Inner>| {
            // Every write the records' store takes is noted as it lands, and is the store's at once:
            // what the session wrote reaches the device's daemon on the pass it wakes.
            let store = Rc::new(MemoryStore::queued());
            let wrote = Weak::clone(inner);
            store.set_write_hook(Some(Rc::new(move || {
                if let Some(inner) = wrote.upgrade() {
                    inner.wrote();
                }
            })));
            Inner {
                owners: ResourceOwners::with_store(
                    Rc::clone(&store) as Rc<dyn durable_store::DurableStore>
                ),
                store,
                view: RefCell::default(),
                service_root: RefCell::default(),
                reads: RefCell::default(),
                writes: RefCell::default(),
                apps: RefCell::default(),
                stores: RefCell::default(),
                replicas: RefCell::default(),
                moved: RefCell::default(),
                changes: RefCell::default(),
                written: RefCell::default(),
                unread: RefCell::default(),
                keeping: Cell::default(),
                writing: RefCell::default(),
                documents: RefCell::default(),
                wake,
            }
        }))
    }

    /// Keep the records of the workspaces `view` says the daemon holds, read through `reads` and
    /// written through `writes` (the daemon seals them), reading them again whenever `changes`
    /// says a workspace's records moved, and handing what the session writes to this device's
    /// apps through `apps` . `service_root` is where this computer's own workspace service keeps
    /// what it keeps of them.
    pub(crate) fn attach(
        &self,
        view: Arc<WorkspaceView>,
        service_root: Option<std::path::PathBuf>,
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        writes: Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
        changes: &crate::platform::records::Changes,
        apps: Option<crate::sync::PeerDocuments>,
    ) {
        *self.0.view.borrow_mut() = Some(view);
        *self.0.service_root.borrow_mut() = service_root;
        *self.0.reads.borrow_mut() = Some(reads);
        *self.0.writes.borrow_mut() = Some(writes);
        *self.0.apps.borrow_mut() = apps;
        *self.0.moved.borrow_mut() = Some(changes.reader());
        *self.0.changes.borrow_mut() = Some(changes.clone());
        self.0.write(true);
        let opened: Vec<(String, String)> = self.0.stores.borrow().keys().cloned().collect();
        for (workspace, app) in opened {
            self.0.read(&workspace, &app);
        }
        self.0.read_documents(None);
    }

    /// Ask `workspace` for `change` of its record of its agents, as this device's person, over
    /// the daemon's dispatch (`tilo-actions/1`), off the loop: the answer is the agent's id for a
    /// made one. The session's copy of the record takes the change in once the service wrote it
    /// ([`Self::took_agents`]).
    pub(crate) fn change_agents(
        &self,
        workspace: &str,
        change: AgentsChange,
    ) -> tilo_actions::remote::Pending<Result<String, String>> {
        let (operation, arguments) = change.call();
        let asked = self.0.engine.borrow().clone().and_then(|engine| {
            let workspace = tilo_desktop_model::WorkspaceId::parse(workspace)?;
            engine.ask_holder(workspace, operation, arguments)
        });
        let Some(asked) = asked else {
            let (done, answer) = tilo_actions::remote::pending();
            done.complete(Err(
                "this device reaches no workspace service that keeps this workspace".to_owned(),
            ));
            return answer;
        };
        crate::landing::off_loop(async move {
            let answer = asked.await?;
            Ok(answer
                .get("agent")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned())
        })
    }

    /// Take in what the service holds of `workspace`'s record of its agents now: what a change
    /// the session asked for wrote, before any later pass reads it.
    pub(crate) fn took_agents(&self, workspace: &str) {
        self.0
            .read(workspace, tilo_app::tilo_workspace_agent::OWNER);
    }

    /// Whether the workspaces' copy is here and holds `workspace`: where a record kept in it is
    /// written and read.
    pub(crate) fn holds(&self, workspace: &str) -> bool {
        let reads = self.0.reads.borrow();
        reads
            .as_ref()
            .is_some_and(|reads| reads.workspaces().iter().any(|held| held == workspace))
    }

    /// Every record the workspaces' copy holds of `workspace`, by app and route: none while the
    /// copy is not here, or holds no such workspace.
    pub(crate) fn listing(&self, workspace: &str) -> Vec<(String, String)> {
        self.0
            .reads
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, |reads| {
                reads.listing(workspace).unwrap_or_else(|error| {
                    tracing::debug!(%error, %workspace, "the workspace's records were not listed");
                    Vec::new()
                })
            })
    }

    /// Keep `doc`, `app`'s `record` in `workspace` as a service of the session runs over it, in
    /// the workspace: what the workspace holds of the record is taken into it, and what the
    /// service writes in it is written into the workspace and handed to this device's apps, the
    /// app's windows among them, as a member's write is. A record the workspace holds no copy of
    /// is founded with what `doc` holds; one it holds takes the place of what `doc` was started
    /// with, and is merged with anything written in it since.
    pub(crate) fn keep_document(&self, workspace: &str, (app, record): (&str, &str), doc: KeptDoc) {
        if self
            .0
            .documents
            .borrow()
            .values()
            .any(|kept| kept.doc.same(&doc))
        {
            return;
        }
        let handed = doc.borrow_mut().get_heads();
        self.0.documents.borrow_mut().insert(
            (workspace.to_owned(), app.to_owned(), record.to_owned()),
            Document {
                doc,
                synced: None,
                revision: None,
                handed,
            },
        );
        self.0.read_documents(Some(workspace));
        (self.0.wake)();
    }

    /// What wakes the session's loop to write what changed in a kept document into the workspace.
    pub(crate) fn wake(&self) -> tilo_service::Wake {
        std::sync::Arc::clone(&self.0.wake)
    }

    /// Whether the document kept as `app`'s `record` in `workspace` ([`Self::keep_document`]) took
    /// in what the workspace holds of it, or founded it there: before then it holds only what it
    /// was handed.
    pub(crate) fn kept_read(&self, workspace: &str, (app, record): (&str, &str)) -> bool {
        self.0
            .documents
            .borrow()
            .get(&(workspace.to_owned(), app.to_owned(), record.to_owned()))
            .is_some_and(|kept| kept.synced.is_some())
    }

    /// Name `record` in `app`'s index of `workspace` , as the app's windows name the records they
    /// open there, written as `person` : the workspace holds it from then on. Written into the
    /// workspace on the next pass. Whether it is named.
    pub(crate) fn reference(
        &self,
        workspace: &str,
        app: &str,
        record: &ui::resource::DocumentCheckout,
        person: &str,
    ) -> bool {
        if !self.holds(workspace) {
            return false;
        }
        // The index is the app's own record of the workspace, which names no kind of its own.
        let named = self
            .in_workspace(workspace, MemberId::person(person.to_owned()))
            .open_primary(app, record.kind.as_str(), &|_| Ok(()))
            .map_err(|error| error.to_string())
            .and_then(|index| {
                index.edit(false, |doc| {
                    ui::resource::index::reference(doc, &record.id, &record.kind);
                    Ok(())
                })?;
                index.flush();
                Ok(())
            });
        if let Err(error) = named {
            tracing::warn!(%error, %workspace, %app, "a record is not brought here");
            return false;
        }
        true
    }

    /// `app`'s `resource` of `workspace` whole, as this device's service holds it: nothing while
    /// the service holds nothing of it or it is on its way.
    ///
    /// # Errors
    ///
    /// This session reaches no service, the service holds no such workspace, or this device may
    /// not read it.
    pub(crate) fn whole(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        use tilo_peer_state::workspace_documents::DocumentRead;
        let reads = self
            .0
            .reads
            .borrow()
            .clone()
            .ok_or_else(|| "this session holds no workspaces".to_owned())?;
        match reads.read(workspace, app, resource, None)? {
            DocumentRead::Whole { bytes, .. } => Ok(Some(bytes)),
            _ => Ok(None),
        }
    }

    /// What the session's copy of `record` of `app` in `workspace` holds as stored: the record a
    /// test reads back, as the session's own readers do.
    pub(crate) fn stored(&self, app: &str, workspace: &str, record: &str) -> Option<Vec<u8>> {
        self.0
            .store
            .read(&crate::platform::records::storage_key(
                app, workspace, record,
            )?)
            .ok()
            .flatten()
            .map(|stored| stored.bytes)
    }

    /// The records of `workspace` , opened as `author` .
    /// Where this device's own workspace service keeps its workspaces, once the session keeps
    /// its records.
    pub(crate) fn service_root(&self) -> Option<std::path::PathBuf> {
        self.0.service_root.borrow().clone()
    }

    /// How `workspace` stands on the device's daemon, once it holds it: a number that moves
    /// whenever a workspace's people, policy or keys change there.
    pub(crate) fn standing(&self, workspace: WorkspaceId) -> Option<u64> {
        let view = self.0.view.borrow().clone()?;
        view.holds(workspace).then(|| view.revision())
    }

    pub(crate) fn in_workspace(&self, workspace: &str, author: MemberId) -> WorkspaceResources {
        let inner = Rc::clone(&self.0);
        let workspace = workspace.to_owned();
        WorkspaceResources::new(author, move |app| inner.store_of(&workspace, app))
    }

    /// Take in what moved in the device's daemon in the workspaces the session's copies hold, and what it
    /// could not read yet, then write into it what the session wrote since: nothing to do while
    /// nothing moved.
    pub(crate) fn replicate(&self) {
        let moved: BTreeSet<String> = self
            .0
            .moved
            .borrow()
            .as_ref()
            .map(|moved| moved.try_iter().map(|changed| changed.workspace).collect())
            .unwrap_or_default();
        let mut due = std::mem::take(&mut *self.0.unread.borrow_mut());
        due.extend(
            self.0
                .stores
                .borrow()
                .keys()
                .filter(|(workspace, _)| moved.contains(workspace))
                .cloned(),
        );
        for (workspace, app) in due {
            self.0.read(&workspace, &app);
        }
        for workspace in &moved {
            self.0.read_documents(Some(workspace));
        }
        // A document the workspace had none of on the last look, or could not be read then, is
        // looked for again.
        self.0.read_documents_unsynced();
        self.write();
    }

    /// Write into the workspaces what the session wrote since the last write.
    pub(crate) fn write(&self) {
        self.0.write(false);
    }
}

impl Drop for Inner {
    /// What the session changed and no pass wrote yet still reaches the workspace, as far as this
    /// device's ownership signs it at once.
    fn drop(&mut self) {
        self.write(true);
    }
}

impl Inner {
    /// The session wrote to the records' store: note which, and wake the loop to write it on.
    fn wrote(&self) {
        let mut noted = false;
        for write in self.store.drain() {
            let _ = self.store.acknowledge(write.revision, Ok(()));
            if self.keeping.get() {
                continue;
            }
            if let StorageKey::Document { app, workspace, .. }
            | StorageKey::WorkspaceIndex { app, workspace } = write.key
            {
                noted |= self
                    .written
                    .borrow_mut()
                    .insert((workspace, app.to_string()));
            }
        }
        if noted {
            (self.wake)();
        }
    }

    /// What this device's service writes into a workspace's documents, once the session opened it.
    fn writes(&self) -> Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>> {
        self.writes.borrow().clone()
    }

    /// Whether this device's service holds the document `key` names.
    fn holds_resource(&self, key: &DocumentKey) -> bool {
        self.reads
            .borrow()
            .as_ref()
            .is_some_and(|reads| reads.holds(&key.0, &key.1, &key.2))
    }

    /// The store `app` keeps its records of `workspace` in, holding what the device's daemon does from the
    /// first time it is asked for.
    fn store_of(
        &self,
        workspace: &str,
        app: &str,
    ) -> Result<ResourceStore, ui::resource::ResourceOpenError> {
        let key = (workspace.to_owned(), app.to_owned());
        if let Some(store) = self.stores.borrow().get(&key) {
            return Ok(store.clone());
        }
        let store = self.owners.clone().open_beside(app, workspace)?;
        self.stores.borrow_mut().insert(key, store.clone());
        self.read(workspace, app);
        Ok(store)
    }

    /// Take in what the device's daemon holds of `app`'s records in `workspace` : merged into a record a
    /// copy has open, and kept for whoever opens one that is not.
    fn read(&self, workspace: &str, app: &str) {
        let Some(reads) = self.reads.borrow().clone() else {
            return;
        };
        let Some(store) = self
            .stores
            .borrow()
            .get(&(workspace.to_owned(), app.to_owned()))
            .cloned()
        else {
            return;
        };
        let Ok(records) = reads.listing(workspace) else {
            return;
        };
        let mut kept = false;
        for (held, record) in records {
            if held != app {
                continue;
            }
            let key = (workspace.to_owned(), app.to_owned(), record.clone());
            let known = self
                .replicas
                .borrow()
                .get(&key)
                .and_then(|replica| replica.revision);
            let bytes = match reads.read(workspace, app, &record, None) {
                Ok(tilo_peer_state::workspace_documents::DocumentRead::Whole {
                    revision,
                    bytes,
                    ..
                }) if known != Some(revision) => {
                    let mut replicas = self.replicas.borrow_mut();
                    let replica = replicas.entry(key.clone()).or_insert_with(|| Replica {
                        sync: DocumentSync::delivered(heads_of(&bytes)),
                        revision: None,
                    });
                    replica.revision = Some(revision);
                    bytes
                }
                Ok(_) => {
                    // Never read and still nothing to read: the keys to open it are on their way.
                    if known.is_none() {
                        self.unread
                            .borrow_mut()
                            .insert((workspace.to_owned(), app.to_owned()));
                    }
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, %workspace, %app, %record, "a record is not read");
                    continue;
                }
            };
            let open = store
                .open_resources()
                .into_iter()
                .find(|checkout| checkout.id.route() == record);
            let taken = if let Some(checkout) = open {
                self.replicas
                    .borrow_mut()
                    .get_mut(&key)
                    .map_or(Ok(()), |replica| {
                        replica
                            .sync
                            .merge(&store, &checkout, DocumentMutation::Snapshot(bytes))
                            .map(|_| ())
                    })
            } else {
                kept = true;
                self.keep(workspace, app, &record, bytes)
            };
            if let Err(error) = taken {
                tracing::warn!(%error, %workspace, %app, %record, "a record is not taken in");
            }
        }
        if kept {
            store.note_kept();
        }
    }

    /// Take what the device's daemon holds of the documents kept in `workspace` (every workspace's, `None`)
    /// into them.
    fn read_documents(&self, workspace: Option<&str>) {
        let keys: Vec<DocumentKey> = self
            .documents
            .borrow()
            .keys()
            .filter(|(held, _, _)| workspace.is_none_or(|workspace| held == workspace))
            .cloned()
            .collect();
        for key in keys {
            self.read_document(&key);
        }
    }

    /// [`Self::read_documents`] of the documents the device's daemon was never read for yet.
    fn read_documents_unsynced(&self) {
        let keys: Vec<DocumentKey> = self
            .documents
            .borrow()
            .iter()
            .filter(|(_, document)| document.synced.is_none())
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.read_document(&key);
        }
    }

    /// Take what the device's daemon holds of the document `key` into it: the first time, the record the
    /// workspace holds takes the place of what the document was handed over with, while nothing
    /// was written in it since; anything else is merged.
    fn read_document(&self, key: &DocumentKey) {
        let Some(reads) = self.reads.borrow().clone() else {
            return;
        };
        let (workspace, app, record) = key;
        if !reads.holds(workspace, app, record) {
            return;
        }
        let known = self
            .documents
            .borrow()
            .get(key)
            .and_then(|document| document.revision);
        let (bytes, revision) = match reads.read(workspace, app, record, None) {
            Ok(tilo_peer_state::workspace_documents::DocumentRead::Whole {
                revision,
                bytes,
                ..
            }) => (bytes, revision),
            // Unchanged, or not to be read yet (its keys are on their way): read again on a
            // later pass.
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(%error, %workspace, %app, %record, "a record is not read");
                return;
            }
        };
        if known == Some(revision) {
            // The service stands where this copy last read it.
            return;
        }
        let mut documents = self.documents.borrow_mut();
        let Some(document) = documents.get_mut(key) else {
            return;
        };
        let theirs = match automerge::AutoCommit::load(&bytes) {
            Ok(theirs) => theirs,
            Err(error) => {
                tracing::warn!(%error, %workspace, %app, %record, "a record does not read");
                return;
            }
        };
        let mut doc = document.doc.borrow_mut();
        let first = document.synced.is_none();
        if first && doc.get_heads() == document.handed {
            // What the service was started with yields to the record the workspace holds.
            let mut theirs = theirs;
            theirs.set_actor(doc.get_actor().clone());
            *doc = theirs;
        } else {
            let mut theirs = theirs;
            if let Err(error) = doc.merge_incoming(&mut theirs) {
                tracing::warn!(%error, %workspace, %app, %record, "a record is not taken in");
                return;
            }
        }
        document.synced = Some(heads_of(&bytes));
        document.revision = Some(revision);
        tracing::debug!(%workspace, %app, %record, first, "a kept record took in the workspace's copy");
    }

    /// Write what the services changed in the documents kept in the workspaces into the workspaces,
    /// and hand each write to this device's apps. Answers the workspaces and apps whose writes
    /// wait for a later pass.
    fn write_documents(&self, view: &WorkspaceView) -> BTreeSet<(String, String)> {
        let mut unwritten = BTreeSet::new();
        let Some(writes) = self.writes() else {
            return unwritten;
        };
        let keys: Vec<DocumentKey> = self.documents.borrow().keys().cloned().collect();
        for key in keys {
            let (workspace, app, _) = &key;
            let Some(room) = WorkspaceId::parse(workspace) else {
                continue;
            };
            if !view.holds(room) || self.writing.borrow().contains_key(&key) {
                continue;
            }
            let (changes, founding, heads) = {
                let documents = self.documents.borrow();
                let Some(document) = documents.get(&key) else {
                    continue;
                };
                let mut doc = document.doc.borrow_mut();
                let heads = doc.get_heads();
                match &document.synced {
                    Some(synced) if *synced == heads => continue,
                    Some(synced) => (doc.save_after(synced), false, heads),
                    // A record the workspace holds is written once it was read here: one never
                    // read is founded by this write, never over what the workspace holds.
                    None if self.holds_resource(&key) => {
                        unwritten.insert((workspace.clone(), app.clone()));
                        continue;
                    }
                    None => (doc.save(), true, heads),
                }
            };
            let (writes, writing_key) = (Arc::clone(&writes), key.clone());
            let mut writing = Writing::write(Arc::clone(&self.wake), async move {
                writes
                    .write(
                        &writing_key.0,
                        &changes,
                        tilo_peer_state::workspace_documents::DocumentWriting {
                            app: &writing_key.1,
                            document: &writing_key.2,
                            readers: None,
                            founding,
                            snapshot: founding,
                            sealed: None,
                        },
                    )
                    .await
            });
            match writing.done() {
                Some(written) => {
                    if !self.written(&key, written, heads) {
                        unwritten.insert((workspace.clone(), app.clone()));
                    }
                }
                None => {
                    self.writing.borrow_mut().insert(key, (writing, heads));
                }
            }
        }
        unwritten
    }

    /// Keep the daemon's copy of a record no copy has open, for whoever opens it next.
    fn keep(&self, workspace: &str, app: &str, record: &str, bytes: Vec<u8>) -> Result<(), String> {
        let key = crate::platform::records::storage_key(app, workspace, record)
            .ok_or("not a record id")?;
        let writer = self
            .store
            .read(&key)
            .map_err(|error| error.to_string())?
            .and_then(|stored| stored.writer);
        // What the device's daemon gave is no write of the session's to give back.
        self.keeping.set(true);
        let kept = self
            .store
            .write(&key, WriteOp::Replace(bytes), writer.as_deref());
        self.keeping.set(false);
        kept.map(|_| ()).map_err(|error| error.to_string())
    }

    /// Write what the session changed in the open records of the stores it wrote to — of every
    /// store, `all` — into the workspaces, and hand each write to this device's apps.
    fn write(&self, all: bool) {
        let Some(view) = self.view.borrow().clone() else {
            return;
        };
        let Some(writes) = self.writes() else {
            return;
        };
        // What a write that is done leaves to write is written on this pass.
        let mut written = std::mem::take(&mut *self.written.borrow_mut());
        written.extend(self.settle_writes());
        let mut unwritten = BTreeSet::new();
        let stores: Vec<((String, String), ResourceStore)> = self
            .stores
            .borrow()
            .iter()
            .filter(|(key, _)| all || written.contains(*key))
            .map(|(key, store)| (key.clone(), store.clone()))
            .collect();
        for ((workspace, app), store) in stores {
            let Some(room) = WorkspaceId::parse(&workspace) else {
                continue;
            };
            // A workspace the daemon does not hold yet (one still being founded) takes its records
            // on a later pass.
            if !view.holds(room) {
                unwritten.insert((workspace.clone(), app.clone()));
                continue;
            }
            for checkout in store.open_resources() {
                let record = checkout.id.route().to_owned();
                let key = (workspace.clone(), app.clone(), record.clone());
                // One write of a record at a time: the next takes what it leaves.
                if self.writing.borrow().contains_key(&key) {
                    unwritten.insert((workspace.clone(), app.clone()));
                    continue;
                }
                let mut replicas = self.replicas.borrow_mut();
                // A record the device's daemon holds is written once its copy was read here: one this copy
                // never read is founded by this write, never over what the workspace holds.
                if !replicas.contains_key(&key) && self.holds_resource(&key) {
                    unwritten.insert((workspace.clone(), app.clone()));
                    continue;
                }
                let replica = replicas.entry(key.clone()).or_insert_with(|| Replica {
                    sync: DocumentSync::new(),
                    revision: None,
                });
                let next = match replica.sync.take_outbound(&store, &checkout.id) {
                    Ok(Some(next)) => next,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(%error, %workspace, %app, %record, "a record is not written");
                        continue;
                    }
                };
                drop(replicas);
                let (changes, snapshot, founding) = match next.mutation {
                    DocumentMutation::Snapshot(bytes) => {
                        // The record whole is a founding only where the workspace holds nothing
                        // of it yet: over one it holds, it is a checkpoint.
                        let founding = !self.holds_resource(&key);
                        (bytes, true, founding)
                    }
                    DocumentMutation::Incremental(bytes) => (bytes, false, false),
                };
                let (writes, writing_key) = (Arc::clone(&writes), key.clone());
                let mut writing = Writing::write(Arc::clone(&self.wake), async move {
                    writes
                        .write(
                            &writing_key.0,
                            &changes,
                            tilo_peer_state::workspace_documents::DocumentWriting {
                                app: &writing_key.1,
                                document: &writing_key.2,
                                readers: None,
                                founding,
                                snapshot,
                                sealed: None,
                            },
                        )
                        .await
                });
                match writing.done() {
                    Some(written) => {
                        if !self.written(&key, written, next.heads) {
                            unwritten.insert((workspace.clone(), app.clone()));
                        }
                    }
                    None => {
                        self.writing.borrow_mut().insert(key, (writing, next.heads));
                    }
                }
            }
        }
        // The documents the services keep are written too: each one's heads say whether anything
        // is left to write, so every pass looks.
        unwritten.extend(self.write_documents(&view));
        // What the workspace did not take yet is written again on the next pass.
        self.written.borrow_mut().extend(unwritten);
    }

    /// Take what the writes under way came to: the stores, by workspace and app, whose writes are
    /// done, to write what the session wrote since.
    fn settle_writes(&self) -> BTreeSet<(String, String)> {
        let done: Vec<_> = self
            .writing
            .borrow_mut()
            .iter_mut()
            .filter_map(|(key, (writing, _))| Some((key.clone(), writing.done()?)))
            .collect();
        let mut again = BTreeSet::new();
        for (key, written) in done {
            let Some((_, heads)) = self.writing.borrow_mut().remove(&key) else {
                continue;
            };
            // Taken or not, the record is written again from where its copy stands.
            self.written(&key, written, heads);
            again.insert((key.0, key.1));
        }
        again
    }

    /// Take what the write of `key` up to `heads` came to: the record's copy here stands at what
    /// the workspace took, and this device's apps hear it. Answers whether it took it.
    fn written(
        &self,
        (workspace, app, record): &DocumentKey,
        written: Written,
        heads: Vec<automerge::ChangeHash>,
    ) -> bool {
        match written {
            Ok(write) => {
                let key = (workspace.clone(), app.clone(), record.clone());
                if let Some(document) = self.documents.borrow_mut().get_mut(&key) {
                    document.synced = Some(heads);
                } else if let Some(replica) = self.replicas.borrow_mut().get_mut(&key) {
                    replica.sync.advance(heads);
                }
                if let Some(apps) = self.apps.borrow().as_ref() {
                    apps.deliver_write(write);
                }
                if let Some(changes) = self.changes.borrow().as_ref() {
                    changes.note(workspace, app, record, false);
                }
                true
            }
            // A change the workspace's rules refuse is told to whoever wrote it (the write
            // answered with the typed refusal) and filed where the person's activity reads it;
            // here the copy stays where it is, so no speaker holds a change nobody else has.
            Err(error) => {
                tracing::warn!(%error, %workspace, %app, %record, "the workspace took no write");
                false
            }
        }
    }
}

impl crate::Shared {
    /// Keep the apps' resources the services run over in the workspace the person is in, once the
    /// session holds it: the store the app's windows open, so the service, the windows, an agent's
    /// programs and the person's other devices read and change one copy.
    pub(crate) fn keep_app_stores(&self, runtime: &tokio::runtime::Handle) {
        if *self.app_stores.borrow() == AppStores::Kept {
            return;
        }
        let waits = |on: &'static str| *self.app_stores.borrow_mut() = AppStores::Waiting(on);
        let Some(workspace) = self.handle.read(|tree| {
            tree.active_workspace()
                .map(tilo_desktop_model::Workspace::id)
        }) else {
            return waits("the session shows no workspace");
        };
        // A workspace the daemon does not hold yet (one still being founded) would refuse every
        // record moved there.
        let Some(standing) = self.records.standing(workspace) else {
            return waits("the device's daemon does not hold the workspace the session shows yet");
        };
        // A move the workspace refused waits for how the workspace stands to move on, rather
        // than being asked again on every pass.
        if matches!(&*self.app_stores.borrow(), AppStores::Refused(held, _) if *held == standing) {
            return;
        }
        let workspace = workspace.to_string();
        if !self.records.holds(&workspace) {
            return waits("the session's records do not hold the workspace yet");
        }
        let Some(store) = (self.held_stores)(workspace, runtime) else {
            return waits("this device's workspace service holds no store of the workspace");
        };
        let kept = match self.services.borrow_mut().keep_in_workspace(&store) {
            Ok(_) => AppStores::Kept,
            Err(error) => {
                tracing::warn!(%error, "the apps' resources are not kept in the workspace");
                AppStores::Refused(standing, error)
            }
        };
        *self.app_stores.borrow_mut() = kept;
    }
}

/// Where moving the apps' resources into the workspace the person is in stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppStores {
    /// Not moved yet, and what the move waits on.
    Waiting(&'static str),
    /// The workspace refused them, saying why, while it stood here on the device's daemon.
    Refused(u64, String),
    /// Every app's resources are kept there.
    Kept,
}

impl Default for AppStores {
    fn default() -> Self {
        Self::Waiting("the session has not yet offered them")
    }
}

impl std::fmt::Display for AppStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Waiting(on) => write!(f, "waiting: {on}"),
            Self::Refused(_, why) => write!(f, "refused: {why}"),
            Self::Kept => f.write_str("kept"),
        }
    }
}

/// The store of a workspace this device's own service holds, none while the service is not open.
pub(crate) type HeldStores = std::rc::Rc<
    dyn Fn(
        String,
        &tokio::runtime::Handle,
    ) -> Option<std::rc::Rc<dyn tilo_service::WorkspaceStore>>,
>;

/// The stores of the workspaces `service` holds, once it is open.
pub(crate) fn held_stores(service: tilo_workspace_service::service::ServiceSlot) -> HeldStores {
    std::rc::Rc::new(move |workspace, runtime| {
        service.service()?;
        Some(std::rc::Rc::new(HeldStore {
            service: service.clone(),
            workspace,
            runtime: runtime.clone(),
        }))
    })
}

/// The store of one workspace this device's own service holds, as the apps the session runs
/// itself write and read their resources in it.
struct HeldStore {
    service: tilo_workspace_service::service::ServiceSlot,
    workspace: String,
    runtime: tokio::runtime::Handle,
}

impl tilo_service::WorkspaceStore for HeldStore {
    fn command(&self, app: &str, command: &[u8]) -> Result<Vec<u8>, String> {
        let command: tilo_resource_store::wire::Command =
            serde_json::from_slice(command).map_err(|error| error.to_string())?;
        // A first write can wait for this device's group keys on the reactor.
        let reply = self.runtime.block_on(
            self.service
                .service()
                .ok_or("this device's workspace service is not open")?
                .workspaces()
                .own_resource_command(&self.workspace, app, command),
        )?;
        serde_json::to_vec(&reply).map_err(|error| error.to_string())
    }
}

/// The heads of a saved document, none for bytes that are not one.
fn heads_of(bytes: &[u8]) -> Vec<automerge::ChangeHash> {
    automerge::AutoCommit::load(bytes)
        .map(|mut doc| doc.get_heads())
        .unwrap_or_default()
}
