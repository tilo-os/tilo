//! A certified room's documents in the page: every record a mount has open
//! and the room's desktop, replicated through the relay session in host
//! order.
//!
//! One [`Room`] exists per started peer and joined room. It owns an event
//! queue created when the peer started, so documents the host replays at
//! admission are kept even before any mount exists. Mounts attach their
//! record stores (and desktop, if they present one):
//!
//! - **Records** go out as Automerge deltas on the route `(app, room,
//!   record)`, the native shell's own `AppDocument` wire; a record's first
//!   publication in a session is a snapshot, which the host retains as the
//!   route's checkpoint for peers that join later. Inbound mutations merge
//!   into the open record (`RecordStore::merge_replica`, Automerge
//!   semantics, local undo untouched) or, when no mount has it open, into
//!   the record's stored document so opening it later shows them.
//! - **The desktop** (tree plus each tab's record) is one small Automerge
//!   document on its own route whose `desktop` value is the shared JSON.
//!   Concurrent layouts resolve last-writer-wins, identically everywhere;
//!   an inbound value reaches the surface through `replace_tree`.
//!
//! Nothing is published before the host answered `Ready` for the room, so a
//! page never announces a stale snapshot over what the room already holds.
//!
//! A package's room-primary document travels with an empty record, as the
//! native broker sends it; in the page's stores it is
//! [`RecordId::package_primary`], kept at the room index.
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collab_types::DocumentMutation;
use durable_store::{DurableStore as _, MemoryStore, StorageKey, WriteOp};
use tilo_automerge::ApplyIncoming as _;
use tilo_browser_peer::{DocumentRoute, PeerEvent, PeerEvents, PeerHandle};
use ui::automerge::{
    AutoCommit, ROOT, ReadDoc as _, ScalarValue, Value, transaction::Transactable as _,
};
use ui::record::{RecordCheckout, RecordId, RecordStore};
use ui::state::{DocumentFrontier, DocumentReplica};

/// The desktop document's route. Changing it splits rooms by build.
const DESKTOP_APP: &str = "so.tilo.browser-desktop";
pub(crate) const DESKTOP_RECORD: &str = "desktop";
const DESKTOP_KEY: &str = "desktop";
/// How often a page asks again whether the room lets its person moderate
/// the shared desktop: a role the room grants while they are in it reaches
/// their page within this.
const MODERATION_ASK_EVERY_MS: u64 = 30_000;
/// How soon a page asks again when the room could not answer (the page was
/// between connections): what it knew stands meanwhile.
const MODERATION_RETRY_MS: u64 = 3_000;

thread_local! {
    static ROOMS: RefCell<HashMap<u32, Rc<RefCell<Room>>>> = RefCell::new(HashMap::new());
}

/// What a mount shares with its room.
pub(crate) struct Attachment {
    pub stores: Rc<RefCell<Vec<RecordStore>>>,
    pub desktop: Option<(tilo_desktop_surface::DesktopHandle, crate::desktop::Shared)>,
    /// Set when a peer's change reached one of this mount's records.
    pub changed: Rc<Cell<bool>>,
    /// Schedules a frame for the mount.
    pub notify: Arc<dyn Fn() + Send + Sync>,
    /// Who the mount's person is: the desktop changes it publishes name
    /// them, so every page's Activity says whose they were.
    pub author: String,
}

struct Replica {
    frontier: DocumentFrontier,
    snapshotted: bool,
}

struct Mounted {
    attachment: Attachment,
    records: HashMap<(String, String), Replica>,
}

pub(crate) struct Room {
    peer: PeerHandle,
    /// The certified device key, hex.
    device: String,
    events: PeerEvents,
    uuid: String,
    workspace: u64,
    store: Rc<MemoryStore>,
    ready: bool,
    mounts: HashMap<u64, Mounted>,
    desktop: DocumentReplica,
    desktop_snapshotted: bool,
    /// The desktop value last published or applied.
    desktop_value: Option<serde_json::Value>,
    /// Records the room delivered that no mount had open, as wire
    /// `(app, record)`.
    stored: BTreeSet<(String, String)>,
    /// Routes a delivered document failed to apply to: this page never
    /// answers a compaction request for them.
    unapplied: HashSet<DocumentRoute>,
    /// Changes this page made to the room: to its records, sent, and to its
    /// desktop's windows (opened, closed, moved), not the desktop it joined.
    authored: u64,
    /// Whether the room lets this page's person close and move what others
    /// made on the shared desktop: the attached desktop's flag, set from the
    /// room's answer.
    moderates: Arc<AtomicBool>,
    /// When the page next asks the room that (Unix milliseconds; 0: now).
    moderation_due_ms: Arc<std::sync::atomic::AtomicU64>,
}

/// The record a checkout is on the collaboration wire.
fn wire_record(id: &RecordId) -> &str {
    if id.is_package_primary() {
        ""
    } else {
        id.as_str()
    }
}

/// The page's storage key of wire record `record` of `app` in `room`.
fn storage_key(app: &str, room: &str, record: &str) -> Result<StorageKey, &'static str> {
    if record.is_empty() {
        return Ok(StorageKey::room_index(app, room));
    }
    let id = RecordId::parse(record).ok_or("not a record id")?;
    Ok(StorageKey::record(app, room, id.storage_key()))
}

/// Start replicating `room` for peer `id`. `events` must have been created
/// when the peer started.
///
/// # Errors
/// The room is not a uuid.
pub(crate) fn open(
    id: u32,
    device: String,
    peer: PeerHandle,
    events: PeerEvents,
    room: &str,
    store: Rc<MemoryStore>,
) -> Result<(), String> {
    let workspace =
        tilo_relay_protocol::room_workspace_id(room).map_err(|_| "room must be a uuid")?;
    let room = Room {
        peer,
        device,
        events,
        uuid: room.to_owned(),
        workspace,
        store,
        ready: false,
        mounts: HashMap::new(),
        desktop: DocumentReplica::new(AutoCommit::new()),
        desktop_snapshotted: false,
        desktop_value: None,
        stored: BTreeSet::new(),
        unapplied: HashSet::new(),
        authored: 0,
        moderates: Arc::default(),
        moderation_due_ms: Arc::default(),
    };
    ROOMS.with_borrow_mut(|rooms| rooms.insert(id, Rc::new(RefCell::new(room))));
    pump(id);
    Ok(())
}

/// How many changes to the room peer `id` made: records it sent, and
/// windows the person opened, closed or moved.
pub(crate) fn authored(id: u32) -> u64 {
    room(id).map_or(0, |room| room.borrow().authored)
}

/// The certified device key (hex) of peer `id`, if it replicates a room.
pub(crate) fn device_of(id: u32) -> Option<String> {
    room(id).map(|room| room.borrow().device.clone())
}

/// Whether peer `id`'s room holds record `record` (wire form) of `app`:
/// open in a mount, or delivered and stored for whoever opens it.
pub(crate) fn has_record(id: u32, app: &str, record: &str) -> bool {
    room(id).is_some_and(|room| {
        let room = room.borrow();
        room.stored.contains(&(app.to_owned(), record.to_owned()))
            || room.open_records().any(|(a, r)| a == app && r == record)
            || storage_key(app, &room.uuid, record)
                .ok()
                .and_then(|key| room.store.read(&key).ok().flatten())
                .is_some()
    })
}

/// The room's records peer `id` holds, open in a mount or delivered, as
/// wire `(app, record)`, sorted.
pub(crate) fn records(id: u32) -> Vec<(String, String)> {
    let Some(room) = room(id) else {
        return Vec::new();
    };
    let room = room.borrow();
    let mut records = room.stored.clone();
    records.extend(room.open_records());
    records.into_iter().collect()
}

/// The room's desktop document with every peer's changes, for Activity.
/// `None` until the room delivered or published one.
pub(crate) fn desktop_history(id: u32) -> Option<AutoCommit> {
    let room = room(id)?;
    let room = room.borrow();
    let doc = room.desktop.document();
    (!doc.base_heads().is_empty()).then(|| doc.clone())
}

/// Read the retained desktop's frontier without cloning its whole history.
pub(crate) fn desktop_history_heads(id: u32) -> Option<Vec<ui::automerge::ChangeHash>> {
    let room = room(id)?;
    let room = room.borrow();
    let heads = room.desktop.document().base_heads();
    (!heads.is_empty()).then_some(heads)
}

pub(crate) fn close(id: u32) {
    crate::browser::forget_feedback_startup(id);
    crate::feedback_delivery::forget(id);
    ROOMS.with_borrow_mut(|rooms| rooms.remove(&id));
}

fn room(id: u32) -> Option<Rc<RefCell<Room>>> {
    ROOMS.with_borrow(|rooms| rooms.get(&id).cloned())
}

/// Attach mount `mount` to peer `id`'s room.
pub(crate) fn attach(id: u32, mount: u64, attachment: Attachment) -> bool {
    let Some(room) = room(id) else {
        return false;
    };
    {
        let mut room = room.borrow_mut();
        if let (Some(value), Some((handle, shared))) = (&room.desktop_value, &attachment.desktop)
            && let Err(error) = crate::desktop::apply_shared(handle, shared, value)
        {
            warn(&format!("shared desktop not applied: {error}"));
        }
        if let Some((_, shared)) = &attachment.desktop {
            room.moderates = Arc::clone(&shared.moderates);
            room.moderation_due_ms.store(0, Ordering::Relaxed);
        }
        room.mounts.insert(
            mount,
            Mounted {
                attachment,
                records: HashMap::new(),
            },
        );
    }
    publish(id);
    true
}

pub(crate) fn detach(id: u32, mount: u64) {
    if let Some(room) = room(id) {
        publish(id);
        room.borrow_mut().mounts.remove(&mount);
    }
}

/// Apply everything the host delivered since the last call.
pub(crate) fn pump(id: u32) {
    let Some(room) = room(id) else {
        return;
    };
    let notify = room.borrow_mut().receive();
    for notify in notify {
        notify();
    }
    publish(id);
}

/// Send what attached mounts changed since the last call.
pub(crate) fn publish(id: u32) {
    if let Some(room) = room(id) {
        let mut room = room.borrow_mut();
        room.send();
        // Every frame passes here, so the room is asked again on time
        // whether or not anything arrived from it.
        room.ask_moderation_when_due();
    }
}

fn warn(message: &str) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
        "Browser runtime replication: {message}"
    )));
}

impl Room {
    /// Ask the room, at most every [`MODERATION_ASK_EVERY_MS`], whether it
    /// lets this page's person close and move what others made on its
    /// shared desktop; the answer sets the desktop's flag and wakes it.
    fn ask_moderation_when_due(&mut self) {
        if !self.ready
            || !self
                .mounts
                .values()
                .any(|mounted| mounted.attachment.desktop.is_some())
        {
            return;
        }
        let now = crate::shell::now_ms();
        if now < self.moderation_due_ms.load(Ordering::Relaxed) {
            return;
        }
        self.moderation_due_ms.store(
            now.saturating_add(MODERATION_ASK_EVERY_MS),
            Ordering::Relaxed,
        );
        let moderates = Arc::clone(&self.moderates);
        let due = Arc::clone(&self.moderation_due_ms);
        let wake: Vec<_> = self
            .mounts
            .values()
            .map(|mounted| Arc::clone(&mounted.attachment.notify))
            .collect();
        let request = tilo_relay_protocol::rooms::RoomsRequest::Permits {
            room: self.uuid.clone(),
            node: tilo_desktop_model::ownership::moderation_node(&self.uuid).to_string(),
        };
        if let Err(error) = self.peer.rooms_then(
            request,
            Box::new(move |answer| {
                let now = match &answer {
                    Ok(tilo_relay_protocol::rooms::RoomsReply::Permits(may)) => *may,
                    Ok(other) => {
                        warn(&format!(
                            "the room did not say what this page may moderate: {other:?}"
                        ));
                        false
                    }
                    Err(_) => {
                        due.store(
                            crate::shell::now_ms().saturating_add(MODERATION_RETRY_MS),
                            Ordering::Relaxed,
                        );
                        return;
                    }
                };
                if moderates.swap(now, Ordering::Relaxed) != now {
                    for wake in &wake {
                        wake();
                    }
                }
            }),
        ) {
            warn(&format!(
                "could not ask the room what this page may moderate: {error}"
            ));
            self.moderation_due_ms
                .store(now.saturating_add(MODERATION_RETRY_MS), Ordering::Relaxed);
        }
    }

    /// Whether `value`, this page's desktop, closes, moves or renames
    /// something on the room's desktop that its person did not make and may
    /// not moderate: the room refuses such a write, so it is never sent.
    fn rearranges_others(&self, value: &serde_json::Value, author: &str) -> bool {
        if self.moderates.load(Ordering::Relaxed) {
            return false;
        }
        let Some(room) = &self.desktop_value else {
            return false;
        };
        let parse = |value: &serde_json::Value| {
            serde_json::from_value::<tilo_desktop_model::ownership::SharedDesktop>(value.clone())
                .ok()
        };
        let (Some(before), Some(after)) = (parse(room), parse(value)) else {
            return false;
        };
        let acts = tilo_desktop_model::ownership::foreign_acts(&before, &after, author);
        if !acts.is_empty() {
            warn(&format!("undid a change to what others made: {acts:?}"));
        }
        !acts.is_empty()
    }

    /// Every record a mount has open in this room, as wire `(app, record)`.
    fn open_records(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.mounts.values().flat_map(|mounted| {
            let stores = mounted.attachment.stores.borrow();
            stores
                .iter()
                .filter(|store| store.package_and_home().1 == self.uuid)
                .flat_map(|store| {
                    let app = store.package_and_home().0;
                    store
                        .open_records()
                        .into_iter()
                        .map(move |checkout| (app.clone(), wire_record(&checkout.id).to_owned()))
                })
                .collect::<Vec<_>>()
        })
    }

    fn route(&self, app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: self.uuid.clone(),
            record: record.to_owned(),
        }
    }

    /// Apply inbound events; returns the mounts to wake.
    ///
    /// A late join or a reconnect delivers a route's retained checkpoint and
    /// its whole tail at once. Each merge into an open record copies the
    /// document, so the mutations of one route are coalesced first (see
    /// [`tilo_browser_peer::coalesce`]) and merged once per run instead of once per change.
    fn receive(&mut self) -> Vec<Arc<dyn Fn() + Send + Sync>> {
        let mut woken: HashSet<u64> = HashSet::new();
        let mut batches: Vec<(DocumentRoute, Vec<DocumentMutation>)> = Vec::new();
        let mut compactions: Vec<(DocumentRoute, [u8; 16])> = Vec::new();
        for event in self.events.drain() {
            match event {
                PeerEvent::Ready { workspace_id } if workspace_id == self.workspace => {
                    self.ready = true;
                    self.moderation_due_ms.store(0, Ordering::Relaxed);
                }
                PeerEvent::CompactionRequested { route, request }
                    if route.cluster_id == self.uuid =>
                {
                    compactions.push((route, request));
                }
                PeerEvent::Document(document) if document.route.cluster_id == self.uuid => {
                    let Some(mutation) = document.mutation() else {
                        self.unapplied.insert(document.route.clone());
                        warn(&format!(
                            "{}/{}: not a complete document mutation",
                            document.route.app_id, document.route.record
                        ));
                        continue;
                    };
                    match batches
                        .iter_mut()
                        .find(|(route, _)| *route == document.route)
                    {
                        Some((_, mutations)) => mutations.push(mutation),
                        None => batches.push((document.route.clone(), vec![mutation])),
                    }
                }
                PeerEvent::DocumentRejected(reason) => {
                    warn(&format!("dropped a document: {reason}"));
                }
                _ => {}
            }
        }
        // Records first: a desktop naming a new tab's record finds it.
        batches.sort_by_key(|(route, _)| route.app_id == DESKTOP_APP);
        for (route, mutations) in batches {
            if let Err(error) =
                self.apply(&route, tilo_browser_peer::coalesce(mutations), &mut woken)
            {
                self.unapplied.insert(route.clone());
                warn(&format!("{}/{}: {error}", route.app_id, route.record));
            }
        }
        // Every document delivered before a request is applied by now.
        for (route, request) in compactions {
            match self.snapshot_of(&route) {
                Ok(Some(snapshot)) => {
                    let _ = self.peer.compact(request, snapshot);
                }
                Ok(None) => {}
                Err(error) => warn(&format!(
                    "{}/{}: no snapshot to compact: {error}",
                    route.app_id, route.record
                )),
            }
        }
        woken
            .into_iter()
            .filter_map(|mount| self.mounts.get(&mount))
            .map(|mounted| {
                mounted.attachment.changed.set(true);
                Arc::clone(&mounted.attachment.notify)
            })
            .collect()
    }

    /// The route's document as this page holds it, for a host compaction
    /// request: an open record's replica, the desktop, or a record's stored
    /// document. `None` when the page cannot vouch for every change it was
    /// delivered for the route.
    fn snapshot_of(&mut self, route: &DocumentRoute) -> Result<Option<Vec<u8>>, String> {
        if self.unapplied.contains(route) {
            return Ok(None);
        }
        if route.app_id == DESKTOP_APP {
            return Ok(Some(self.desktop.document_mut().save()));
        }
        for mounted in self.mounts.values() {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.package_and_home().0 != route.app_id {
                    continue;
                }
                if let Some(checkout) = store
                    .open_records()
                    .into_iter()
                    .find(|checkout| wire_record(&checkout.id) == route.record)
                {
                    return store
                        .replication_snapshot(&checkout.id)
                        .map(|(bytes, _)| Some(bytes))
                        .map_err(|error| error.to_string());
                }
            }
        }
        let Ok(key) = storage_key(&route.app_id, &self.uuid, &route.record) else {
            return Ok(None);
        };
        Ok(self
            .store
            .read(&key)
            .map_err(|error| error.to_string())?
            .map(|stored| stored.bytes)
            .filter(|bytes| !bytes.is_empty()))
    }

    fn apply(
        &mut self,
        route: &DocumentRoute,
        mutations: Vec<DocumentMutation>,
        woken: &mut HashSet<u64>,
    ) -> Result<(), String> {
        if route.app_id == DESKTOP_APP {
            for mutation in mutations {
                self.apply_desktop(mutation, woken)?;
            }
            return Ok(());
        }
        let app = route.app_id.clone();
        let record = route.record.clone();
        let mut merged = false;
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.package_and_home().0 != app {
                    continue;
                }
                let Some(checkout) = store
                    .open_records()
                    .into_iter()
                    .find(|checkout| wire_record(&checkout.id) == record)
                else {
                    continue;
                };
                let replica = mounted
                    .records
                    .entry((app.clone(), record.clone()))
                    .or_insert_with(|| Replica {
                        frontier: DocumentFrontier::default(),
                        snapshotted: false,
                    });
                for mutation in &mutations {
                    let (changed, observed) = store.merge_replica(&checkout, mutation.clone())?;
                    replica.frontier.observe(observed);
                    if changed {
                        woken.insert(id);
                    }
                }
                merged = true;
            }
        }
        if merged {
            return Ok(());
        }
        self.store_unopened(&app, &record, mutations)?;
        self.stored.insert((app, record));
        for (&id, mounted) in &self.mounts {
            if let Some((handle, shared)) = &mounted.attachment.desktop {
                shared.retry_waiting(handle);
                woken.insert(id);
            }
        }
        Ok(())
    }

    /// Merge into the record's stored document for whoever opens it next.
    /// A snapshot into an empty store is kept as it came: opening the record
    /// loads and checks it once, instead of loading, merging and saving it
    /// here first.
    fn store_unopened(
        &self,
        app: &str,
        record: &str,
        mutations: Vec<DocumentMutation>,
    ) -> Result<(), String> {
        let key = storage_key(app, &self.uuid, record)?;
        let stored = self.store.read(&key).map_err(|error| error.to_string())?;
        let writer = stored.as_ref().and_then(|stored| stored.writer.clone());
        let stored = stored
            .map(|stored| stored.bytes)
            .filter(|bytes| !bytes.is_empty());
        let mut mutations = mutations.into_iter().peekable();
        let fresh =
            stored.is_none() && matches!(mutations.peek(), Some(DocumentMutation::Snapshot(_)));
        let bytes = match (stored, mutations.peek()) {
            (None, Some(DocumentMutation::Snapshot(_))) => {
                let Some(DocumentMutation::Snapshot(snapshot)) = mutations.next() else {
                    return Ok(());
                };
                if mutations.peek().is_none() {
                    return self
                        .store
                        .write(&key, WriteOp::Replace(snapshot), writer.as_deref())
                        .map(drop)
                        .map_err(|error| error.to_string());
                }
                snapshot
            }
            (stored, _) => stored.unwrap_or_default(),
        };
        let mut doc = if bytes.is_empty() {
            AutoCommit::new()
        } else {
            AutoCommit::load(&bytes).map_err(|error| error.to_string())?
        };
        let before = doc.get_heads();
        for mutation in mutations {
            match mutation {
                DocumentMutation::Snapshot(bytes) => {
                    let mut incoming =
                        AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
                    // Merge the side with fewer unshared changes into the
                    // other, as `RecordStore::merge_replica` does.
                    let shared: Vec<_> = incoming
                        .get_heads()
                        .into_iter()
                        .filter(|head| doc.get_change_by_hash(head).is_some())
                        .collect();
                    if shared.len() < incoming.get_heads().len() {
                        incoming
                            .load_incoming(&doc.save_after(&shared))
                            .map_err(|error| error.to_string())?;
                        doc = incoming;
                    }
                }
                DocumentMutation::Incremental(bytes) => {
                    doc.load_incoming(&bytes)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        if doc.get_heads() == before && !fresh {
            return Ok(());
        }
        self.store
            .write(&key, WriteOp::Replace(doc.save()), writer.as_deref())
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn apply_desktop(
        &mut self,
        mutation: DocumentMutation,
        woken: &mut HashSet<u64>,
    ) -> Result<(), String> {
        match mutation {
            DocumentMutation::Snapshot(bytes) => self.desktop.merge_snapshot(&bytes),
            DocumentMutation::Incremental(bytes) => self.desktop.load_incremental(&bytes).map(drop),
        }
        .map_err(|error| error.to_string())?;
        let Some(value) = desktop_value(self.desktop.document()) else {
            return Ok(());
        };
        if self.desktop_value.as_ref() == Some(&value) {
            return Ok(());
        }
        for (&id, mounted) in &self.mounts {
            if let Some((handle, shared)) = &mounted.attachment.desktop {
                crate::desktop::apply_shared(handle, shared, &value)?;
                woken.insert(id);
            }
        }
        self.desktop_value = Some(value);
        Ok(())
    }

    fn send(&mut self) {
        if !self.ready {
            return;
        }
        let mut outgoing: Vec<Pending> = Vec::new();
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                let (app, home) = store.package_and_home();
                if home != self.uuid {
                    continue;
                }
                for checkout in store.open_records() {
                    let key = (app.clone(), wire_record(&checkout.id).to_owned());
                    let replica = mounted
                        .records
                        .entry(key.clone())
                        .or_insert_with(|| Replica {
                            frontier: DocumentFrontier::default(),
                            snapshotted: false,
                        });
                    let route = DocumentRoute {
                        app_id: app.clone(),
                        cluster_id: self.uuid.clone(),
                        record: key.1.clone(),
                    };
                    match outbound(store, &checkout, replica) {
                        Ok(Some((mutation, heads))) => {
                            outgoing.push((route, mutation, (id, key, heads)));
                        }
                        Ok(None) => {}
                        Err(error) => warn(&format!("{}/{}: {error}", key.0, key.1)),
                    }
                }
            }
        }
        // Records first: a desktop naming a new tab's record must not reach
        // a peer before the record does.
        for (route, mutation, advance) in outgoing {
            // A first snapshot carries what was there already; an
            // increment is what this page changed.
            let authored = matches!(mutation, DocumentMutation::Incremental(_));
            if self.peer.publish(route, &mutation).is_err() {
                return;
            }
            if authored {
                self.authored = self.authored.saturating_add(1);
            }
            let (id, key, heads) = advance;
            if let Some(replica) = self
                .mounts
                .get_mut(&id)
                .and_then(|m| m.records.get_mut(&key))
            {
                replica.frontier.advance(heads);
                replica.snapshotted = true;
            }
        }
        self.send_desktop();
    }

    fn send_desktop(&mut self) {
        let Some((value, author)) = self.mounts.values().find_map(|mounted| {
            mounted.attachment.desktop.as_ref().map(|(handle, shared)| {
                (
                    crate::desktop::shared_value(handle, shared),
                    mounted.attachment.author.clone(),
                )
            })
        }) else {
            return;
        };
        if self.desktop_value.as_ref() == Some(&value) {
            return;
        }
        if self.rearranges_others(&value, &author) {
            // Whatever moved someone else's window (a command, a race with
            // their own move) is undone here: it springs back to where the
            // room has it, and nothing is sent.
            if let Some(room) = &self.desktop_value {
                for mounted in self.mounts.values() {
                    if let Some((handle, shared)) = &mounted.attachment.desktop {
                        if let Err(error) = crate::desktop::return_to(handle, shared, room) {
                            warn(&format!("shared desktop not restored: {error}"));
                        }
                        (mounted.attachment.notify)();
                    }
                }
            }
            return;
        }
        // The desktop this page joined with is not something the person did.
        let joined = self.desktop_value.is_some();
        let doc = self.desktop.document_mut();
        // The same change the desktop's own history log would make: its
        // time, and who made it.
        if tilo_desktop_surface::history_log::record(doc, &value, &author, ui::time::unix_now_i64())
        {
            // Windows opened, closed, moved or resized: the person's own
            // work on the desktop, as the history pane tells it.
            if joined {
                self.authored = self.authored.saturating_add(1);
            }
        } else if doc.put(ROOT, DESKTOP_KEY, value.to_string()).is_ok() {
            doc.commit();
        }
        let mutation = if self.desktop_snapshotted {
            let (delta, heads) = self.desktop.pending_outbound_delta();
            if delta.is_empty() {
                return;
            }
            (DocumentMutation::Incremental(delta), heads)
        } else {
            let doc = self.desktop.document_mut();
            (DocumentMutation::Snapshot(doc.save()), doc.get_heads())
        };
        let route = self.route(DESKTOP_APP, DESKTOP_RECORD);
        if self.peer.publish(route, &mutation.0).is_ok() {
            self.desktop.advance_outbound(mutation.1);
            self.desktop_snapshotted = true;
            self.desktop_value = Some(value);
        }
    }
}

type Outbound = (DocumentMutation, Vec<ui::automerge::ChangeHash>);
/// A publication and the replica frontier it advances once accepted.
type Pending = (
    DocumentRoute,
    DocumentMutation,
    (u64, (String, String), Vec<ui::automerge::ChangeHash>),
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

fn desktop_value(doc: &AutoCommit) -> Option<serde_json::Value> {
    match doc.get(ROOT, DESKTOP_KEY).ok()?? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Str(text) => serde_json::from_str(text).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Publish a reviewed report as this peer, using the existing action and record.
/// This neither creates a destination nor answers its canonical receipt.
pub(crate) fn submit_feedback(
    id: u32,
    admission: &tilo_desktop::feedback::transport::Enrollment,
    submission: &tilo_app::feedback::Submission,
) -> Result<(), String> {
    use ui::record::RecordOwners;
    let room = room(id).ok_or("feedback peer closed")?;
    let (records, store, peer, uuid) = {
        let room = room.borrow();
        if !room.ready
            || room.uuid != admission.destination.room
            || room.device
                != hex::encode(
                    admission
                        .reporter
                        .device_public_key()
                        .ok_or("reporter device absent")?,
                )
        {
            return Err("feedback room not ready".into());
        }
        let store = room.store.clone();
        let records = room.mounts.values().find_map(|mount| {
            mount
                .attachment
                .stores
                .borrow()
                .iter()
                .find(|records| {
                    records.package_and_home() == ("so.tilo.chat".into(), room.uuid.clone())
                })
                .cloned()
        });
        let records = match records {
            Some(records) => records,
            None => RecordOwners::with_store(store.clone())
                .open("so.tilo.chat", &room.uuid)
                .map_err(|error| error.to_string())?,
        };
        (records, store, room.peer.clone(), room.uuid.clone())
    };
    let mut frame = feedback_publisher(admission, &records, store)?;
    frame.relayout();
    frame
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: 1,
                address: tilo_actions::ActionAddress::new(
                    tilo_app::feedback::destination::item(
                        tilo_app::feedback::destination::CHANNELS,
                        &admission.destination.channel,
                    ),
                    tilo_app::feedback::destination::SUBMIT,
                ),
            },
            serde_json::json!({"submission": submission})
                .as_object()
                .ok_or("invalid submission arguments")?,
        )
        .map_err(|error| error.to_string())?;
    let bytes = records
        .read(&RecordId::package_primary(), |document| {
            document.clone().save()
        })
        .ok_or("feedback source disappeared")?;
    peer.publish(
        DocumentRoute {
            app_id: "so.tilo.chat".into(),
            cluster_id: uuid,
            record: String::new(),
        },
        &DocumentMutation::Snapshot(bytes),
    )
}

fn feedback_publisher(
    admission: &tilo_desktop::feedback::transport::Enrollment,
    records: &RecordStore,
    store: Rc<MemoryStore>,
) -> Result<ui::headless::FrameHarness, String> {
    use ui::application::{
        ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
    };
    use ui::application_view::InstanceViewStore;
    let uuid = &admission.destination.room;
    let definition =
        crate::desktop::application("so.tilo.chat").ok_or("Chat component unavailable")?;
    let checkout = RecordCheckout::package_primary(definition.record_kind.clone());
    records
        .try_open_record(&checkout)
        .map_err(|error| error.to_string())?;
    let target = LaunchTarget::Record {
        checkout,
        reveal: None,
    };
    let views = Rc::new(
        InstanceViewStore::with_store(
            store,
            &hex::encode(
                admission
                    .reporter
                    .device_public_key()
                    .ok_or("reporter device absent")?,
            ),
            uuid,
        )
        .map_err(|error| error.to_string())?
        .without_persistence(),
    );
    let context = HostContext::prepare(
        definition,
        HostContextInputs {
            records: records.clone(),
            views,
            device_id: hex::encode(
                admission
                    .reporter
                    .device_public_key()
                    .ok_or("reporter device absent")?,
            ),
            instance_id: 1,
            permissions: Rc::new(RefCell::new(admission.permissions.clone())),
            author: ui::permissions::UserId(hex::encode(
                admission
                    .reporter
                    .user_public_key()
                    .ok_or("reporter root absent")?,
            )),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )
    .map_err(|error| error.to_string())?;
    let component = definition
        .instantiate(context, target, RestoreInput::None)
        .map_err(|error| error.to_string())?;
    ui::headless::FrameHarness::from_component(
        component,
        (1000, 700),
        ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::new(),
        },
    )
    .map_err(|error| error.to_string())
}
