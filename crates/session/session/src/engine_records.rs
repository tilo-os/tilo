//! The workspace records the session itself reads and writes — Chat's record of a workspace's
//! agents, its emoji and its conversations — held as the workspace's own copy of them: the
//! engine's, which every app of the workspace, the person's other devices and the index read.
//! The session opens them as any app's window opens a workspace record ([`WorkspaceRecords`]),
//! over stores kept in memory and replicated with the engine the way a page replicates its
//! records with its workspace: what the engine holds is merged into the stores, and what the
//! session changes in them is written into the engine — sealed where the workspace's records are
//! its members' sealed channels — and handed to this device's apps as a member's write is. No
//! copy of its own is kept beside the workspace's.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use collab_types::DocumentMutation;
use durable_store::{DurableStore as _, MemoryStore, StorageKey, WriteOp};
use tilo_broker_core::custody::OwnerCustody as _;
use tilo_desktop_model::WorkspaceId;
use ui::permissions::UserId;
use ui::record::workspace::WorkspaceRecords;
use ui::record::{RecordId, RecordOwners, RecordStore, RecordSync};

use crate::engine::{Engine, SealKeys};

/// The session's copies of the workspaces' records, replicated with the engine. Cloning shares
/// them.
#[derive(Clone)]
pub(crate) struct EngineRecords(Rc<Inner>);

struct Inner {
    store: Rc<MemoryStore>,
    owners: RecordOwners,
    /// The workspaces' copy, once the session holds one.
    engine: RefCell<Option<Arc<Engine>>>,
    /// Where a write reaches this device's apps, as a member's write does.
    apps: RefCell<Option<crate::sync::PeerDocuments>>,
    /// The store of each app's records in each workspace, by workspace and app, as handed out.
    stores: RefCell<BTreeMap<(String, String), RecordStore>>,
    /// Each record's replication with the engine, by workspace, app and wire record.
    replicas: RefCell<HashMap<(String, String, String), Replica>>,
    /// The engine's revision at the last read: a pass that finds it unmoved reads nothing.
    read_at: Cell<Option<u64>>,
}

/// One record as the engine and this copy hold it.
struct Replica {
    sync: RecordSync,
    /// The revision of the engine's copy last merged here.
    revision: Option<[u8; 32]>,
}

impl EngineRecords {
    pub(crate) fn new() -> Self {
        let store = Rc::new(MemoryStore::new());
        Self(Rc::new(Inner {
            owners: RecordOwners::with_store(
                Rc::clone(&store) as Rc<dyn durable_store::DurableStore>
            ),
            store,
            engine: RefCell::default(),
            apps: RefCell::default(),
            stores: RefCell::default(),
            replicas: RefCell::default(),
            read_at: Cell::default(),
        }))
    }

    /// Keep the records in `engine` , handing what the session writes to this device's apps
    /// through `apps` .
    pub(crate) fn attach(&self, engine: Arc<Engine>, apps: Option<crate::sync::PeerDocuments>) {
        *self.0.engine.borrow_mut() = Some(engine);
        *self.0.apps.borrow_mut() = apps;
        self.0.read_at.set(None);
        let opened: Vec<(String, String)> = self.0.stores.borrow().keys().cloned().collect();
        for (workspace, app) in opened {
            self.0.read(&workspace, &app);
        }
    }

    /// The records of `workspace` , opened as `author` .
    pub(crate) fn in_workspace(&self, workspace: &str, author: UserId) -> WorkspaceRecords {
        let inner = Rc::clone(&self.0);
        let workspace = workspace.to_owned();
        WorkspaceRecords::new(author, move |app| inner.store_of(&workspace, app))
    }

    /// Write what the session changed into the engine, then take in what the engine holds that
    /// the session's copies lack.
    pub(crate) fn replicate(&self) {
        self.0.write();
        let Some(engine) = self.0.engine.borrow().clone() else {
            return;
        };
        let revision = engine.owner().revision();
        if self.0.read_at.get() == Some(revision) {
            return;
        }
        self.0.read_at.set(Some(revision));
        let opened: Vec<(String, String)> = self.0.stores.borrow().keys().cloned().collect();
        for (workspace, app) in opened {
            self.0.read(&workspace, &app);
        }
    }
}

impl Inner {
    /// The store `app` keeps its records of `workspace` in, holding what the engine does from the
    /// first time it is asked for.
    fn store_of(
        &self,
        workspace: &str,
        app: &str,
    ) -> Result<RecordStore, ui::record::RecordOpenError> {
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
        let Some(engine) = self.engine.borrow().clone() else {
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
        let Ok(records) = engine.records(workspace) else {
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
            let bytes = match engine.record_since(workspace, app, &record, known) {
                Ok(Some((bytes, revision))) => {
                    let mut replicas = self.replicas.borrow_mut();
                    let replica = replicas.entry(key.clone()).or_insert_with(|| Replica {
                        sync: RecordSync::delivered(heads_of(&bytes)),
                        revision: None,
                    });
                    replica.revision = Some(revision);
                    bytes
                }
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, %workspace, %app, %record, "a record is not read");
                    continue;
                }
            };
            let open = store
                .open_records()
                .into_iter()
                .find(|checkout| checkout.id.route() == record);
            let taken = match open {
                Some(checkout) => self
                    .replicas
                    .borrow_mut()
                    .get_mut(&key)
                    .map(|replica| {
                        replica
                            .sync
                            .merge(&store, &checkout, DocumentMutation::Snapshot(bytes))
                            .map(|_| ())
                    })
                    .unwrap_or(Ok(())),
                None => {
                    kept = true;
                    self.keep(workspace, app, &record, bytes)
                }
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
        let key = if record.is_empty() {
            StorageKey::workspace_index(app, workspace)
        } else {
            let id = RecordId::parse(record).ok_or("not a record id")?;
            StorageKey::record(app, workspace, id.storage_key())
        };
        let writer = self
            .store
            .read(&key)
            .map_err(|error| error.to_string())?
            .and_then(|stored| stored.writer);
        self.store
            .write(&key, WriteOp::Replace(bytes), writer.as_deref())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Write what the session changed in each open record into the engine, and hand each write to
    /// this device's apps.
    fn write(&self) {
        let Some(engine) = self.engine.borrow().clone() else {
            return;
        };
        let stores: Vec<((String, String), RecordStore)> = self
            .stores
            .borrow()
            .iter()
            .map(|(key, store)| (key.clone(), store.clone()))
            .collect();
        for ((workspace, app), store) in stores {
            let Some(room) = WorkspaceId::parse(&workspace) else {
                continue;
            };
            for checkout in store.open_records() {
                let record = checkout.id.route().to_owned();
                let key = (workspace.clone(), app.clone(), record.clone());
                let mut replicas = self.replicas.borrow_mut();
                // A record the engine holds is written once its copy was read here: one this copy
                // never read is founded by this write, never over what the workspace holds.
                if !replicas.contains_key(&key) && engine.holds_record(&workspace, &app, &record) {
                    continue;
                }
                let replica = replicas.entry(key).or_insert_with(|| Replica {
                    sync: RecordSync::new(),
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
                let Some(sealed) = sealing(&engine, room, &app, &record) else {
                    continue;
                };
                match engine.write_record(
                    room,
                    (&app, &record),
                    &changes,
                    founding,
                    sealed.as_ref(),
                ) {
                    Ok(write) => {
                        if let Some(replica) = self.replicas.borrow_mut().get_mut(&(
                            workspace.clone(),
                            app.clone(),
                            record.clone(),
                        )) {
                            replica.sync.advance(next.heads);
                        }
                        if let Some(apps) = self.apps.borrow().as_ref() {
                            apps.deliver_write(&workspace, write);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, %workspace, %app, %record, "the workspace took no write");
                    }
                }
            }
        }
    }
}

/// How this device seals a write of `record` : `Some(None)` in the clear, where the workspace's
/// device commits its documents, `Some(Some(keys))` under the record channel's keys, and `None`
/// while it holds no keys to seal with.
fn sealing(
    engine: &Engine,
    room: WorkspaceId,
    app: &str,
    record: &str,
) -> Option<Option<channel_crypto::ChannelContentKeys>> {
    if engine.commits_documents(room) {
        return Some(None);
    }
    match engine.seal_keys(room, app, record) {
        Ok(SealKeys::Here(keys)) => Some(Some(keys)),
        Ok(SealKeys::Waiting | SealKeys::NoSecretStore) => None,
        Err(error) => {
            tracing::warn!(%error, %room, %app, %record, "a record is not written");
            None
        }
    }
}

/// The heads of a saved document, none for bytes that are not one.
fn heads_of(bytes: &[u8]) -> Vec<automerge::ChangeHash> {
    automerge::AutoCommit::load(bytes)
        .map(|mut doc| doc.get_heads())
        .unwrap_or_default()
}
