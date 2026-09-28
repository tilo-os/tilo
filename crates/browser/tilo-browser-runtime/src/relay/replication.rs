//! A relay room's documents in the page, replicated through the certified
//! session in host order (after #1432's `replication.rs`).
//!
//! One [`Room`] exists per started peer and joined room. Its event queue was
//! created when the peer started, so documents the host replays at admission
//! are kept before any mount exists; until a mount hands the room its
//! profile cache ([`attach_cache`]) they are merged in memory.
//!
//! - **Outbound**, every record the page's mounts have open in the room goes
//!   out as Automerge deltas on the route `(app, room, record)`, the native
//!   shell's own `AppDocument` wire; a record's first publication in a
//!   session is a snapshot, which the host retains as the route's checkpoint
//!   for peers that join later. The stores are the cache's live record
//!   owners of the room, the very ones the mounts edit.
//! - **Inbound** mutations merge into the open record
//!   (`RecordStore::merge_replica`, Automerge semantics, local undo
//!   untouched) or, when no mount has it open, into the record's cached
//!   document so opening it later shows them.
//!
//! Nothing is published before the host answered `Ready` for the room, so a
//! page never announces a stale snapshot over what the room already holds.
//! Each page lays out its own desktop; the room shares documents.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;
use tilo_browser_peer::{DocumentRoute, OpenedDocument, PeerEvent, PeerEvents, PeerHandle};
use ui::automerge::AutoCommit;
use ui::portable_storage::{PortableStorage, StorageKey};
use ui::record::{RecordCheckout, RecordStore};
use ui::state::DocumentFrontier;

thread_local! {
    static ROOMS: RefCell<HashMap<u32, Rc<RefCell<Room>>>> = RefCell::new(HashMap::new());
}

struct Replica {
    frontier: DocumentFrontier,
    snapshotted: bool,
}

pub(crate) struct Room {
    peer: PeerHandle,
    events: PeerEvents,
    uuid: String,
    workspace: u64,
    ready: bool,
    /// The profile cache of the page's mounts in this room, once one exists.
    cache: Option<PortableStorage>,
    /// Documents that arrived before the cache, by `(app, record)`.
    early: HashMap<(String, String), AutoCommit>,
    replicas: HashMap<(String, String), Replica>,
}

/// Start replicating `room` for peer `id`. `events` must have been created
/// when the peer started.
///
/// # Errors
/// The room is not a uuid.
pub(crate) fn open(id: u32, peer: PeerHandle, events: PeerEvents, room: &str) -> Result<(), String> {
    let workspace =
        tilo_relay_protocol::room_workspace_id(room).map_err(|_| "room must be a uuid")?;
    let room = Room {
        peer,
        events,
        uuid: room.to_owned(),
        workspace,
        ready: false,
        cache: None,
        early: HashMap::new(),
        replicas: HashMap::new(),
    };
    ROOMS.with_borrow_mut(|rooms| rooms.insert(id, Rc::new(RefCell::new(room))));
    pump(id);
    Ok(())
}

pub(crate) fn close(id: u32) {
    ROOMS.with_borrow_mut(|rooms| rooms.remove(&id));
}

fn room(id: u32) -> Option<Rc<RefCell<Room>>> {
    ROOMS.with_borrow(|rooms| rooms.get(&id).cloned())
}

/// Hand peer `id`'s room the profile cache a mount in it uses, before the
/// mount opens its records: documents that arrived earlier are stored in it
/// first, so the mount opens what the room holds.
///
/// # Errors
/// Peer `id` replicates no room, or the room is not `room`.
pub(crate) fn attach_cache(id: u32, room_id: &str, cache: &PortableStorage) -> Result<(), String> {
    let room = room(id).ok_or("the relay peer has not joined a room")?;
    {
        let mut room = room.borrow_mut();
        if room.uuid != room_id {
            return Err("a relay mount must use its peer's room".into());
        }
        if room.cache.is_none() {
            room.cache = Some(cache.clone());
            for ((app, record), mut doc) in std::mem::take(&mut room.early) {
                if let Err(error) = room.store_unopened(&app, &record, DocumentMutation::Snapshot(doc.save())) {
                    warn(&format!("{app}/{record}: {error}"));
                }
            }
        }
    }
    pump(id);
    Ok(())
}

/// Whether peer `id`'s room holds record `record` of `app` in this page.
pub(crate) fn has_record(id: u32, app: &str, record: &str) -> bool {
    room(id).is_some_and(|room| {
        let room = room.borrow();
        room.early.contains_key(&(app.to_owned(), record.to_owned()))
            || room.cache.as_ref().is_some_and(|cache| cache.get(&record_key(app, &room.uuid, record)).is_some())
    })
}

/// Apply everything the host delivered since the last call, then send what
/// the mounts changed.
pub(crate) fn pump(id: u32) {
    let Some(room) = room(id) else {
        return;
    };
    let woken = room.borrow_mut().receive();
    if woken {
        crate::browser::wake_relay_mounts(id);
    }
    publish_changes(id);
}

/// Send what the mounts changed in peer `id`'s room since the last call.
pub(crate) fn publish_changes(id: u32) {
    if let Some(room) = room(id) {
        room.borrow_mut().send();
    }
}

fn warn(message: &str) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
        "Browser runtime relay replication: {message}"
    )));
}

/// The cache key of a record; the package-primary record has its own slot.
fn record_key(app: &str, room: &str, record: &str) -> StorageKey {
    if record.is_empty() {
        StorageKey::Primary {
            package: app.to_owned(),
            workspace: room.to_owned(),
        }
    } else {
        StorageKey::Record {
            package: app.to_owned(),
            workspace: room.to_owned(),
            record: record.to_owned(),
        }
    }
}

fn merge_into(doc: &mut AutoCommit, mutation: DocumentMutation) -> Result<(), String> {
    match mutation {
        DocumentMutation::Snapshot(bytes) => {
            let mut incoming = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
            doc.merge_incoming(&mut incoming)
                .map(drop)
                .map_err(|error| error.to_string())
        }
        DocumentMutation::Incremental(bytes) => doc
            .load_incoming(&bytes)
            .map(drop)
            .map_err(|error| error.to_string()),
    }
}

impl Room {
    fn stores(&self) -> Vec<RecordStore> {
        self.cache
            .as_ref()
            .map(|cache| cache.record_stores(&self.uuid))
            .unwrap_or_default()
    }

    /// Apply inbound events; whether an open record changed.
    fn receive(&mut self) -> bool {
        let mut woken = false;
        for event in self.events.drain() {
            match event {
                PeerEvent::Ready { workspace_id } if workspace_id == self.workspace => {
                    self.ready = true;
                }
                PeerEvent::Document(document) if document.route.cluster_id == self.uuid => {
                    match self.apply(&document) {
                        Ok(changed) => woken |= changed,
                        Err(error) => warn(&format!(
                            "{}/{}: {error}",
                            document.route.app_id, document.route.record
                        )),
                    }
                }
                _ => {}
            }
        }
        woken
    }

    fn apply(&mut self, document: &OpenedDocument) -> Result<bool, String> {
        let mutation = document
            .mutation()
            .ok_or("a segmented transfer is not reassembled yet")?;
        let app = document.route.app_id.clone();
        let record = document.route.record.clone();
        if self.cache.is_none() {
            let doc = self.early.entry((app, record)).or_insert_with(AutoCommit::new);
            merge_into(doc, mutation)?;
            return Ok(false);
        }
        for store in self.stores() {
            if store.package_and_home().0 != app {
                continue;
            }
            let Some(checkout) = store
                .open_records()
                .into_iter()
                .find(|checkout| checkout.id.as_str() == record)
            else {
                continue;
            };
            let (changed, observed) = store.merge_replica(&checkout, mutation)?;
            self.replicas
                .entry((app, record))
                .or_insert_with(|| Replica {
                    frontier: DocumentFrontier::default(),
                    snapshotted: false,
                })
                .frontier
                .observe(observed);
            return Ok(changed);
        }
        self.store_unopened(&app, &record, mutation)?;
        Ok(false)
    }

    /// Merge into the record's cached document for whoever opens it next.
    fn store_unopened(&self, app: &str, record: &str, mutation: DocumentMutation) -> Result<(), String> {
        let cache = self.cache.as_ref().ok_or("no cache")?;
        let key = record_key(app, &self.uuid, record);
        let stored = cache.get(&key);
        let mut doc = match &stored {
            Some(stored) if !stored.bytes.is_empty() => {
                AutoCommit::load(&stored.bytes).map_err(|error| error.to_string())?
            }
            _ => AutoCommit::new(),
        };
        let before = doc.get_heads();
        merge_into(&mut doc, mutation)?;
        if doc.get_heads() == before {
            return Ok(());
        }
        let actor = stored.and_then(|stored| stored.actor);
        cache.put_document(key, &mut doc, actor)
    }

    fn send(&mut self) {
        if !self.ready || self.cache.is_none() {
            return;
        }
        let mut outgoing: Vec<Pending> = Vec::new();
        for store in self.stores() {
            let (app, home) = store.package_and_home();
            if home != self.uuid {
                continue;
            }
            for checkout in store.open_records() {
                let key = (app.clone(), checkout.id.as_str().to_owned());
                let replica = self.replicas.entry(key.clone()).or_insert_with(|| Replica {
                    frontier: DocumentFrontier::default(),
                    snapshotted: false,
                });
                match outbound(&store, &checkout, replica) {
                    Ok(Some((mutation, heads))) => outgoing.push((
                        DocumentRoute {
                            app_id: app.clone(),
                            cluster_id: self.uuid.clone(),
                            record: key.1.clone(),
                        },
                        mutation,
                        (key, heads),
                    )),
                    Ok(None) => {}
                    Err(error) => warn(&format!("{}/{}: {error}", key.0, key.1)),
                }
            }
        }
        for (route, mutation, (key, heads)) in outgoing {
            if self.peer.publish(route, &mutation).is_err() {
                return;
            }
            if let Some(replica) = self.replicas.get_mut(&key) {
                replica.frontier.advance(heads);
                replica.snapshotted = true;
            }
        }
    }
}

type Outbound = (DocumentMutation, Vec<ui::automerge::ChangeHash>);
/// A publication and the replica frontier it advances once accepted.
type Pending = (
    DocumentRoute,
    DocumentMutation,
    ((String, String), Vec<ui::automerge::ChangeHash>),
);

fn outbound(
    store: &RecordStore,
    checkout: &RecordCheckout,
    replica: &mut Replica,
) -> Result<Option<Outbound>, String> {
    if !replica.snapshotted {
        let (bytes, heads) = store
            .replication_snapshot(&checkout.id)
            .map_err(|error| error.to_string())?;
        return Ok(Some((DocumentMutation::Snapshot(bytes), heads)));
    }
    let (delta, heads) = store
        .replication_delta(&checkout.id, &mut replica.frontier)
        .map_err(|error| error.to_string())?;
    Ok((!delta.is_empty()).then_some((DocumentMutation::Incremental(delta), heads)))
}
