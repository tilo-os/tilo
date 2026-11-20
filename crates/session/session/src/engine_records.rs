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
use tilo_desktop_model::WorkspaceId;
use ui::permissions::MemberId;
use ui::resource::workspace::WorkspaceResources;
use ui::resource::{DocumentSync, ResourceOwners, ResourceStore};

use crate::engine::{Engine, SealKeys};
use crate::engine_write::{Writing, Written};

/// The session's copies of the workspaces' records, replicated with the engine. Cloning shares
/// them.
#[derive(Clone)]
pub(crate) struct EngineRecords(Rc<Inner>);

struct Inner {
    store: Rc<MemoryStore>,
    owners: ResourceOwners,
    /// The workspaces' copy, once the session holds one.
    engine: RefCell<Option<Arc<Engine>>>,
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
    /// Each record's replication with the engine, by workspace, app and wire record.
    replicas: RefCell<HashMap<DocumentKey, Replica>>,
    /// The workspaces whose records moved in the engine, told as each one does.
    moved: RefCell<Option<mpsc::Receiver<crate::platform::records::Changed>>>,
    /// The stream of record changes, told what the session wrote once the workspace took it.
    changes: RefCell<Option<crate::platform::records::Changes>>,
    /// The stores, by workspace and app, the session wrote to since their changes were last
    /// written into the engine.
    written: RefCell<BTreeSet<(String, String)>>,
    /// The stores, by workspace and app, holding a record the engine has that this device cannot
    /// read yet (its channel's keys are on their way): read again on the next pass.
    unread: RefCell<BTreeSet<(String, String)>>,
    /// Set while the engine's copy of a record is kept in the store: no write of the session's.
    keeping: Cell<bool>,
    /// The writes under way, by workspace, app and wire record, with the heads each one writes up
    /// to: a record is written again once its write is done.
    writing: RefCell<HashMap<DocumentKey, (Writing, Vec<automerge::ChangeHash>)>>,
    /// Wakes the session's loop when the session wrote to a record, so the write reaches the
    /// engine on the next pass.
    wake: tilo_service::Wake,
}

/// A record, by workspace, app and wire record.
type DocumentKey = (String, String, String);

/// One record as the engine and this copy hold it.
struct Replica {
    sync: DocumentSync,
    /// The revision of the engine's copy last merged here.
    revision: Option<[u8; 32]>,
}

impl EngineRecords {
    /// Empty copies, waking the session's loop through `wake` when the session wrote to one.
    pub(crate) fn new(wake: tilo_service::Wake) -> Self {
        Self(Rc::new_cyclic(|inner: &Weak<Inner>| {
            // Every write the records' store takes is noted as it lands, and is the store's at once:
            // what the session wrote reaches the engine on the pass it wakes.
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
                engine: RefCell::default(),
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
                wake,
            }
        }))
    }

    /// Keep the records in `engine` , reading them again whenever `changes` says a workspace's
    /// records moved, and handing what the session writes to this device's apps through `apps` .
    pub(crate) fn attach(
        &self,
        engine: Arc<Engine>,
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        writes: Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
        changes: &crate::platform::records::Changes,
        apps: Option<crate::sync::PeerDocuments>,
    ) {
        *self.0.engine.borrow_mut() = Some(engine);
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

    /// The members' own records the workspaces' copy holds of `workspace`, each with whose it is.
    pub(crate) fn own_records(
        &self,
        workspace: &str,
    ) -> std::collections::BTreeMap<(String, String), [u8; 32]> {
        self.0
            .reads
            .borrow()
            .as_ref()
            .map(|reads| reads.own_records(workspace))
            .unwrap_or_default()
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
    pub(crate) fn in_workspace(&self, workspace: &str, author: MemberId) -> WorkspaceResources {
        let inner = Rc::clone(&self.0);
        let workspace = workspace.to_owned();
        WorkspaceResources::new(author, move |app| inner.store_of(&workspace, app))
    }

    /// Take in what moved in the engine in the workspaces the session's copies hold, and what it
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
        self.write();
    }

    /// Write into the engine what the session wrote since the last write.
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

    /// The store `app` keeps its records of `workspace` in, holding what the engine does from the
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

    /// Take in what the engine holds of `app`'s records in `workspace` : merged into a record a
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

    /// Keep the engine's copy of a record no copy has open, for whoever opens it next.
    fn keep(&self, workspace: &str, app: &str, record: &str, bytes: Vec<u8>) -> Result<(), String> {
        let key = crate::platform::records::storage_key(app, workspace, record)
            .ok_or("not a record id")?;
        let writer = self
            .store
            .read(&key)
            .map_err(|error| error.to_string())?
            .and_then(|stored| stored.writer);
        // What the engine gave is no write of the session's to give back.
        self.keeping.set(true);
        let kept = self
            .store
            .write(&key, WriteOp::Replace(bytes), writer.as_deref());
        self.keeping.set(false);
        kept.map(|_| ()).map_err(|error| error.to_string())
    }

    /// Write what the session changed in the open records of the stores it wrote to — of every
    /// store, `all` — into the engine, and hand each write to this device's apps.
    fn write(&self, all: bool) {
        let Some(engine) = self.engine.borrow().clone() else {
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
            for checkout in store.open_resources() {
                let record = checkout.id.route().to_owned();
                let key = (workspace.clone(), app.clone(), record.clone());
                // One write of a record at a time: the next takes what it leaves.
                if self.writing.borrow().contains_key(&key) {
                    unwritten.insert((workspace.clone(), app.clone()));
                    continue;
                }
                let mut replicas = self.replicas.borrow_mut();
                // A record the engine holds is written once its copy was read here: one this copy
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
                let (changes, founding) = match next.mutation {
                    DocumentMutation::Snapshot(bytes) => (bytes, true),
                    DocumentMutation::Incremental(bytes) => (bytes, false),
                };
                // Seal under the keys selected for this record.
                let sealed = match engine.resource_write_keys(room, (&app, &record)) {
                    Ok(SealKeys::Here(keys)) => Some(keys),
                    Ok(SealKeys::Waiting | SealKeys::NoSecretStore) => {
                        unwritten.insert((workspace.clone(), app.clone()));
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(%error, %workspace, %app, %record, "a record is not written");
                        unwritten.insert((workspace.clone(), app.clone()));
                        continue;
                    }
                };
                let (writes, writing_key) = (Arc::clone(&writes), key.clone());
                let mut writing = Writing::write(Arc::clone(&self.wake), async move {
                    writes
                        .write(
                            &writing_key.0,
                            &writing_key.1,
                            &writing_key.2,
                            &changes,
                            founding,
                            sealed.as_ref(),
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
                if let Some(replica) = self.replicas.borrow_mut().get_mut(&key) {
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
    pub(crate) fn keep_app_stores(&self) {
        let Some(workspace) = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        }) else {
            return;
        };
        if !self.records.holds(&workspace) || self.workspace_service.service().is_none() {
            return;
        }
        let store: std::rc::Rc<dyn tilo_service::WorkspaceStore> = std::rc::Rc::new(HeldStore {
            service: self.workspace_service.clone(),
            workspace,
        });
        if let Err(error) = self.services.borrow_mut().keep_in_workspace(&store) {
            tracing::warn!(%error, "the apps' resources are not kept in the workspace");
        }
    }
}

/// The store of one workspace this device's own service holds, as the apps the session runs
/// itself write and read their resources in it.
struct HeldStore {
    service: tilo_workspace_service::service::ServiceSlot,
    workspace: String,
}

impl tilo_service::WorkspaceStore for HeldStore {
    fn command(&self, app: &str, command: &[u8]) -> Result<Vec<u8>, String> {
        let command: tilo_resource_store::wire::Command =
            serde_json::from_slice(command).map_err(|error| error.to_string())?;
        let workspaces = self
            .service
            .service()
            .ok_or("this device's workspace service is not open")?
            .workspaces()
            .clone();
        // The store answers under the workspace's lock, which no reactor is needed to take.
        let reply = futures::executor::block_on(workspaces.own_resource_command(
            &self.workspace,
            app,
            command,
        ))?;
        serde_json::to_vec(&reply).map_err(|error| error.to_string())
    }
}

/// The heads of a saved document, none for bytes that are not one.
fn heads_of(bytes: &[u8]) -> Vec<automerge::ChangeHash> {
    automerge::AutoCommit::load(bytes)
        .map(|mut doc| doc.get_heads())
        .unwrap_or_default()
}
