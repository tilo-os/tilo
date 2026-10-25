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
use std::sync::{Mutex, PoisonError};
use tilo_sort::CompactSort as _;

use collab_types::DocumentMutation;
use durable_store::{DurableStore as _, MemoryStore, StorageKey, WriteOp};
use tilo_automerge::ApplyIncoming as _;
use tilo_browser_peer::content::ContentWants;
use tilo_browser_peer::{DocumentRoute, PeerEvent, PeerEvents, PeerHandle};
use ui::automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
use ui::collaboration::record_content::ContentLane;
use ui::record::{RecordCheckout, RecordId, RecordStore};
use ui::state::{DocumentFrontier, DocumentReplica};

/// The desktop document's route. Changing it splits rooms by build.
pub(crate) const DESKTOP_APP: &str = "so.tilo.browser-desktop";
pub(crate) const DESKTOP_RECORD: &str = "desktop";
/// How often a page asks again what the room lets its person do: a role the
/// room grants or withdraws while they are in it reaches their page within
/// this.
const MODERATION_ASK_EVERY_MS: u64 = 15_000;
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
    /// What the mount's apps may offer their person: set to the role the
    /// room answers they hold, so an app does not offer what the room
    /// would refuse.
    pub permissions: Rc<RefCell<ui::permissions::PermissionContext>>,
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
    /// Where `store` lives.
    kept: Kept,
    ready: bool,
    mounts: HashMap<u64, Mounted>,
    desktop: DocumentReplica,
    desktop_snapshotted: bool,
    /// The room's desktop as this page last published or applied it.
    desktop_value: Option<serde_json::Value>,
    /// This page's own desktop as it last wrote it into the record: a
    /// frame that changed nothing shared writes nothing.
    local_value: Option<serde_json::Value>,
    /// Records the room delivered that no mount had open, as wire
    /// `(app, record)`.
    stored: BTreeSet<(String, String)>,
    /// The heads of each such record as the room delivered it, until a mount
    /// opens it. The room has everything up to them, so what the mount sends
    /// first is what it changed after them, not the whole document: a
    /// checkpoint reaches only devices that lack the record, and the changes
    /// a window makes as it opens (taking a seat) would never reach the
    /// others.
    delivered: HashMap<(String, String), Vec<ui::automerge::ChangeHash>>,
    /// Changes the room delivered for a record before the changes they
    /// build on, as the incremental runs they came in, by wire
    /// `(app, record)`. The room replays a record by author, each author's
    /// checkpoint and then their deltas, so a late joiner can be sent the
    /// second player's seat before the table it sits at, and the dealer's
    /// deal before that seat. A saved document keeps only the changes it can
    /// apply, so these wait here, applied again with whatever arrives next,
    /// until nothing they build on is missing.
    waiting: HashMap<(String, String), Vec<Vec<u8>>>,
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
    /// The role the room last answered this page's person holds: the
    /// attached desktop's, so its policy offers what the room allows.
    standing: crate::desktop::Standing,
    /// The standing the mounts' permissions were last set to.
    followed: u8,
    /// The room keeps no member's layout: it refused this page's desktop as
    /// a public room does. The attached desktops are their people's own
    /// from then on: sent to nobody, and not replaced by the room's.
    own_layout: bool,
    /// The room's members as its governance lists them, once heard: what
    /// someone no longer among them made on the desktop is released.
    members: Option<std::collections::BTreeSet<String>>,
    /// How many commits of each record route this page published the room
    /// has not answered yet: once none is left, what the record's windows
    /// said they did is said (`RecordStore::room_settled`).
    in_flight: HashMap<DocumentRoute, usize>,
    /// Content the open records' windows lacked and the page asked the
    /// room for, by the wire `(app, record)` that references it.
    content: ContentWants<(String, String)>,
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
pub(crate) fn open(
    id: u32,
    device: String,
    peer: PeerHandle,
    events: PeerEvents,
    room: &str,
    (store, kept): (Rc<MemoryStore>, Kept),
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
        kept,
        ready: false,
        mounts: HashMap::new(),
        desktop: DocumentReplica::new(AutoCommit::new()),
        desktop_snapshotted: false,
        desktop_value: None,
        local_value: None,
        stored: BTreeSet::new(),
        delivered: HashMap::new(),
        waiting: HashMap::new(),
        unapplied: HashSet::new(),
        authored: 0,
        moderates: Arc::default(),
        moderation_due_ms: Arc::default(),
        standing: crate::desktop::Standing::unknown(),
        followed: 0,
        own_layout: false,
        members: None,
        in_flight: HashMap::new(),
        content: ContentWants::default(),
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

/// `mutations` with every snapshot ahead of the incremental runs, each kind
/// in the order it came. A snapshot holds its whole history, so a run that
/// builds on it applies after it; a run applied before the changes it
/// builds on only waits in memory, and merging a snapshot into a document
/// copies the document's saved changes, never the ones still waiting.
fn snapshots_first(mut mutations: Vec<DocumentMutation>) -> Vec<DocumentMutation> {
    mutations.sort_by_key(|mutation| matches!(mutation, DocumentMutation::Incremental(_)));
    mutations
}

/// The heads of the saved document `bytes`, when it holds any change: the
/// host's empty document for a record a member is creating holds none.
fn heads_of(bytes: &[u8]) -> Option<Vec<ui::automerge::ChangeHash>> {
    let heads = AutoCommit::load(bytes).ok()?.get_heads();
    (!heads.is_empty()).then_some(heads)
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
                .is_some_and(|stored| heads_of(&stored.bytes).is_some())
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

/// The name of the workspace in front on peer `id`'s room desktop, once the
/// room delivered or the page published one.
pub(crate) fn workspace_name(id: u32) -> Option<String> {
    let room = room(id)?;
    let room = room.borrow();
    room.desktop_value
        .as_ref()
        .and_then(tilo_desktop_surface::history_log::workspace_name)
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
    crate::feedback_delivery::forget_feedback_startup(id);
    crate::feedback_delivery::forget(id);
    ROOMS.with_borrow_mut(|rooms| rooms.remove(&id));
}

/// The store peer `id` keeps the room's documents in when it is kept in
/// no profile: a mount through it opens its records from there, where
/// what the room delivers is kept, instead of from a profile of its own.
pub(crate) fn page_store(id: u32) -> Option<Rc<MemoryStore>> {
    let room = room(id)?;
    let room = room.borrow();
    (room.kept == Kept::InPage).then(|| Rc::clone(&room.store))
}

/// Where a peer keeps the room's documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    /// In the person's profile, with everything else of theirs.
    InProfile,
    /// Only in the page, for as long as it is open: a peer kept in no
    /// profile (a watching page's). Its mounts keep their records there too.
    InPage,
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
            shared.own_layout.set(room.own_layout);
            shared.watching.set(room.peer.watching());
            shared.members.replace(room.members.clone());
            room.moderates = Arc::clone(&shared.moderates);
            room.standing.share(&shared.standing);
        }
        room.moderation_due_ms.store(0, Ordering::Relaxed);
        // The new mount's apps learn what the room answered.
        room.followed = 0;
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
        room.ask_content();
        // Every frame passes here, so the room is asked again on time
        // whether or not anything arrived from it.
        room.ask_standing_when_due();
        room.follow_standing();
    }
}

/// Ask the room now for content the open records' windows lack.
pub(crate) fn ask_content(id: u32) {
    if let Some(room) = room(id) {
        room.borrow_mut().ask_content();
    }
}

/// What a person reads when the room refused a change `app` made: that it
/// was not kept, and the room's reason in plain words.
fn refused_change(app: &str, plain: &str) -> String {
    let title = crate::desktop::applications()
        .into_iter()
        .find(|definition| definition.app_id == app)
        .map_or("The app", |definition| definition.title);
    format!("{title} didn't keep that. {plain}")
}

fn debug(message: &str) {
    web_sys::console::debug_1(&wasm_bindgen::JsValue::from_str(&format!(
        "Browser runtime replication: {message}"
    )));
}

fn warn(message: &str) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
        "Browser runtime replication: {message}"
    )));
}

impl Room {
    /// Ask the room, at most every [`MODERATION_ASK_EVERY_MS`], what it
    /// lets this page's person do: whether they write, moderate (and so
    /// close and move what others made on its shared desktop) and govern
    /// it. The answers set the desktop's flag and the role the mounts' apps
    /// follow, and wake them.
    fn ask_standing_when_due(&self) {
        if !self.ready || self.mounts.is_empty() {
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
        let wake: Vec<_> = self
            .mounts
            .values()
            .map(|mounted| Arc::clone(&mounted.attachment.notify))
            .collect();
        let asked = tilo_action_policy::roles::telling_nodes(&self.uuid);
        let answers: Arc<Mutex<[Option<bool>; 3]>> = Arc::default();
        for (index, node) in asked.into_iter().enumerate() {
            let answers = Arc::clone(&answers);
            let moderates = Arc::clone(&self.moderates);
            let standing = self.standing.clone();
            let due = Arc::clone(&self.moderation_due_ms);
            let wake = wake.clone();
            let request = tilo_relay_protocol::rooms::RoomsRequest::Permits {
                room: self.uuid.clone(),
                node: node.to_string(),
            };
            if let Err(error) = self.peer.rooms_then(
                request,
                Box::new(move |answer| {
                    let may = match &answer {
                        Ok(tilo_relay_protocol::rooms::RoomsReply::Permits(may)) => *may,
                        Ok(other) => {
                            warn(&format!(
                                "the room did not say what this page may do: {other:?}"
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
                    let answered = {
                        let mut answers = answers.lock().unwrap_or_else(PoisonError::into_inner);
                        if let Some(slot) = answers.get_mut(index) {
                            *slot = Some(may);
                        }
                        *answers
                    };
                    let [Some(writes), Some(moderating), Some(governs)] = answered else {
                        return;
                    };
                    let role =
                        tilo_action_policy::roles::role_answered([writes, moderating, governs]);
                    let moved = moderates.swap(moderating, Ordering::Relaxed) != moderating;
                    if standing.set(Some(role)) || moved {
                        for wake in &wake {
                            wake();
                        }
                    }
                }),
            ) {
                warn(&format!(
                    "could not ask the room what this page may do: {error}"
                ));
                self.moderation_due_ms
                    .store(now.saturating_add(MODERATION_RETRY_MS), Ordering::Relaxed);
                return;
            }
        }
    }

    /// Give every mount's apps the role the room last answered, once per
    /// answer that changed it.
    fn follow_standing(&mut self) {
        let standing = self.standing.code();
        if standing == self.followed {
            return;
        }
        let Some(role) = self.standing.role() else {
            return;
        };
        self.followed = standing;
        for mounted in self.mounts.values() {
            let attachment = &mounted.attachment;
            *attachment.permissions.borrow_mut() =
                ui::permissions::PermissionContext::member(attachment.author.clone(), role);
            // A window waiting to learn whether its person starts the
            // room's record now knows.
            if let Some((handle, shared)) = &attachment.desktop {
                shared.retry_waiting(handle);
            }
        }
    }

    /// What `value`, this page's desktop, closes, moves or renames on the
    /// room's desktop that its person did not make and may not moderate:
    /// the room refuses such a write, so it is never sent. Said in words
    /// the person reads; `None` when it touches nothing of anyone else's.
    fn rearranges_others(&self, value: &serde_json::Value, author: &str) -> Option<String> {
        if self.moderates.load(Ordering::Relaxed) {
            return None;
        }
        let room = self.desktop_value.as_ref()?;
        let parse = |value: &serde_json::Value| {
            serde_json::from_value::<tilo_desktop_model::ownership::SharedDesktop>(value.clone())
                .ok()
        };
        let (Some(before), Some(after)) = (parse(room), parse(value)) else {
            return None;
        };
        let acts = tilo_desktop_model::ownership::foreign_acts(
            &before,
            &after,
            author,
            self.members.as_ref(),
        );
        let first = acts.first()?;
        warn(&format!("undid a change to what others made: {acts:?}"));
        Some(
            tilo_desktop_model::commands::Refusal::Others {
                id: first.id,
                owner: first.owner.clone(),
            }
            .to_string(),
        )
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
        let mut refusals: Vec<(DocumentRoute, Vec<[u8; 32]>, String)> = Vec::new();
        let mut answered: Vec<DocumentRoute> = Vec::new();
        let mut content_arrived = false;
        for event in self.events.drain() {
            match event {
                PeerEvent::Ready { workspace_id } if workspace_id == self.workspace => {
                    self.ready = true;
                    self.moderation_due_ms.store(0, Ordering::Relaxed);
                    self.follow_layout(&mut woken);
                }
                PeerEvent::RoomLayout { .. } => self.follow_layout(&mut woken),
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
                PeerEvent::RecordContent {
                    workspace_id,
                    frame,
                    ..
                } if workspace_id == self.workspace => {
                    content_arrived |= self.content_piece(&frame);
                }
                PeerEvent::ContentWanted {
                    workspace_id,
                    from,
                    request,
                } if workspace_id == self.workspace => self.content_wanted(from, &request),
                PeerEvent::CommitRefused {
                    route,
                    changes,
                    reason,
                } if route.cluster_id == self.uuid => {
                    answered.push(route.clone());
                    refusals.push((route, changes, reason));
                }
                PeerEvent::Committed { route, .. } if route.cluster_id == self.uuid => {
                    answered.push(route);
                }
                _ => {}
            }
        }
        self.follow_members(&mut woken);
        // Withdrawn before what arrived since is merged: a peer's change can
        // never build on one the room refused.
        for (route, changes, reason) in refusals {
            self.withdraw(&route, &changes, &reason, &mut woken);
        }
        for route in answered {
            self.answered(&route, &mut woken);
        }
        // Records first: a desktop naming a new tab's record finds it.
        batches.compact_sort_by_key(|(route, _)| route.app_id == DESKTOP_APP);
        for (route, mutations) in batches {
            if let Err(error) =
                self.apply(&route, tilo_browser_peer::coalesce(mutations), &mut woken)
            {
                self.unapplied.insert(route.clone());
                warn(&format!("{}/{}: {error}", route.app_id, route.record));
            }
        }
        // Every document delivered before a request is applied by now.
        self.compact(compactions);
        let mut notify: Vec<Arc<dyn Fn() + Send + Sync>> = woken
            .into_iter()
            .filter_map(|mount| self.mounts.get(&mount))
            .map(|mounted| {
                mounted.attachment.changed.set(true);
                Arc::clone(&mounted.attachment.notify)
            })
            .collect();
        // Content changes no record: its windows draw what they waited for.
        if content_arrived {
            notify.extend(
                self.mounts
                    .values()
                    .map(|mounted| Arc::clone(&mounted.attachment.notify)),
            );
        }
        notify
    }

    /// Ask the room for the content open records' windows lack: what they
    /// queued on the records' lanes ([`ContentLane`]). Content a window
    /// stored for others is not offered from a page.
    fn ask_content(&mut self) {
        if !self.ready {
            return;
        }
        let device = self.peer.status().device;
        let now = crate::shell::now_ms();
        let records: BTreeSet<(String, String)> = self.open_records().collect();
        for (app, record) in records {
            let lane = ContentLane::of(&app, &record);
            for hash in lane.take_requests().wants {
                let Some(request) = self.content.ask(
                    &device,
                    (app.clone(), record.clone()),
                    (&app, &self.uuid),
                    &hash,
                    now,
                ) else {
                    continue;
                };
                if self.peer.want_content(self.workspace, request).is_err() {
                    lane.retry_want(hash);
                }
            }
        }
    }

    /// Answer device `from`'s request for content a record of an app this
    /// page has open references, from what the page keeps: a file a visitor
    /// posted here, which the room's host asks for to keep its own copy.
    fn content_wanted(&self, from: [u8; 32], request: &[u8]) {
        let device = self.peer.status().device;
        let apps: BTreeSet<String> = self.open_records().map(|(app, _)| app).collect();
        let pieces =
            tilo_browser_peer::content::answer(&device, &from, request, |app, room, hash| {
                if room != self.uuid || !apps.contains(app) {
                    return None;
                }
                crate::page_content::page_content().read(hash)
            });
        for piece in pieces {
            if self.peer.send_content(self.workspace, from, piece).is_err() {
                return;
            }
        }
    }

    /// Keep a piece of content a member sent; whether it completed content
    /// a record's window asked for, now on the record's lane.
    fn content_piece(&mut self, frame: &[u8]) -> bool {
        let device = self.peer.status().device;
        let Some(arrived) = self.content.piece(&device, frame) else {
            return false;
        };
        let (app, record) = arrived.tag;
        ContentLane::of(&app, &record).arrive(arrived.hash, arrived.bytes);
        true
    }

    /// Withdraw what the host refused of `route` from the open record that
    /// sent it, which tells its app, and move the record's replication
    /// frontier back to what it keeps. The raw `reason` is for the log; the
    /// app shows a person its plain form.
    fn withdraw(
        &mut self,
        route: &DocumentRoute,
        changes: &[[u8; 32]],
        reason: &str,
        woken: &mut HashSet<u64>,
    ) {
        let personal = route.app_id == DESKTOP_APP
            && reason.trim() == tilo_relay_protocol::refusal::PERSONAL_LAYOUT;
        if personal {
            // Expected in a public room, not a fault: said once, quietly.
            if !self.own_layout {
                debug("this room keeps each member's layout to themselves");
                self.keep_own_layout(woken);
            }
            return;
        }
        warn(&format!(
            "{}/{}: the room refused a change: {reason}",
            route.app_id, route.record
        ));
        if route.app_id == DESKTOP_APP {
            return;
        }
        if changes.is_empty() {
            return;
        }
        let refused: Vec<ui::automerge::ChangeHash> = changes
            .iter()
            .map(|hash| ui::automerge::ChangeHash(*hash))
            .collect();
        let plain = tilo_relay_protocol::refusal::plain_reason(reason);
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.package_and_home().0 != route.app_id {
                    continue;
                }
                let Some(checkout) = store
                    .open_records()
                    .into_iter()
                    .find(|checkout| wire_record(&checkout.id) == route.record)
                else {
                    continue;
                };
                match store.withdraw_refused(&checkout.id, &refused, plain) {
                    Ok(withdrawn) => {
                        if let Some(heads) = withdrawn {
                            // What the person just did is taken back: say so
                            // over the desktop, whatever the app shows, so a
                            // change the room refused never looks kept.
                            if let Some((handle, _)) = &mounted.attachment.desktop {
                                handle.acknowledge(refused_change(&route.app_id, plain));
                            }
                            if let Some(replica) = mounted
                                .records
                                .get_mut(&(route.app_id.clone(), route.record.clone()))
                            {
                                replica.frontier.advance(heads);
                            }
                        }
                        woken.insert(id);
                    }
                    Err(error) => warn(&format!(
                        "{}/{}: a refused change stays: {error}",
                        route.app_id, route.record
                    )),
                }
            }
        }
    }

    /// Answer the host's compaction `requests` with this page's snapshot of
    /// each route, where it can vouch for one.
    fn compact(&mut self, requests: Vec<(DocumentRoute, [u8; 16])>) {
        for (route, request) in requests {
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
    }

    /// The room answered one commit of `route` this page published. Once it
    /// answered all of them, what the record's windows said they did is
    /// theirs to say: a refused change was withdrawn first, with what was
    /// said of it.
    fn answered(&mut self, route: &DocumentRoute, woken: &mut HashSet<u64>) {
        let Some(left) = self.in_flight.get_mut(route) else {
            return;
        };
        *left = left.saturating_sub(1);
        if *left > 0 {
            return;
        }
        self.in_flight.remove(route);
        for (&id, mounted) in &self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.package_and_home().0 != route.app_id {
                    continue;
                }
                if let Some(checkout) = store
                    .open_records()
                    .into_iter()
                    .find(|checkout| wire_record(&checkout.id) == route.record)
                {
                    store.room_settled(&checkout.id);
                    woken.insert(id);
                }
            }
        }
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
        let key = (app.clone(), record.clone());
        // What waited for the changes it builds on goes with what arrived:
        // after it, so a snapshot that may hold those changes comes first.
        let mutations = snapshots_first(
            mutations
                .into_iter()
                .chain(
                    self.waiting
                        .get(&key)
                        .into_iter()
                        .flatten()
                        .cloned()
                        .map(DocumentMutation::Incremental),
                )
                .collect(),
        );
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
            // An open record keeps what still waits in its own document,
            // which applies it once the changes it builds on arrive.
            self.waiting.remove(&key);
            return Ok(());
        }
        // The host takes up a record a member created with an empty
        // document of its own before the member's content arrives: no
        // record yet, which a page opening it would refuse.
        let Some(heads) = self.store_unopened(&app, &record, mutations)? else {
            return Ok(());
        };
        self.delivered.insert((app.clone(), record.clone()), heads);
        // A window of the app that follows what its store keeps (a chain's
        // newer head) takes it up at its next frame.
        for mounted in self.mounts.values() {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.package_and_home().0 == app {
                    store.note_kept();
                }
            }
        }
        self.stored.insert((app, record));
        for (&id, mounted) in &self.mounts {
            if let Some((handle, shared)) = &mounted.attachment.desktop {
                shared.retry_waiting(handle);
                woken.insert(id);
            }
        }
        Ok(())
    }

    /// Merge into the record's stored document for whoever opens it next,
    /// and answer its heads once it holds any change. A snapshot into an empty
    /// store is kept as it came: opening the record loads and checks it once,
    /// instead of loading, merging and saving it here first. A document with
    /// no change in it is no record yet and is not kept.
    fn store_unopened(
        &mut self,
        app: &str,
        record: &str,
        mutations: Vec<DocumentMutation>,
    ) -> Result<Option<Vec<ui::automerge::ChangeHash>>, String> {
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
                    return Ok(None);
                };
                if mutations.peek().is_none() {
                    let Some(heads) = heads_of(&snapshot) else {
                        return Ok(None);
                    };
                    return self
                        .store
                        .write(&key, WriteOp::Replace(snapshot), writer.as_deref())
                        .map(|_| Some(heads))
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
        let mut runs = Vec::new();
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
                    if !doc.get_missing_deps(&[]).is_empty() {
                        // What waits in the stored document for the changes
                        // it builds on stays only if the snapshot comes to
                        // it, not it to the snapshot.
                        doc.load_incoming(&incoming.save_after(&[]))
                            .map_err(|error| error.to_string())?;
                    } else if shared.len() < incoming.get_heads().len() {
                        incoming
                            .load_incoming(&doc.save_after(&shared))
                            .map_err(|error| error.to_string())?;
                        doc = incoming;
                    }
                }
                DocumentMutation::Incremental(bytes) => {
                    doc.load_incoming(&bytes)
                        .map_err(|error| error.to_string())?;
                    runs.push(bytes);
                }
            }
        }
        // Changes whose dependencies have not arrived wait for the next
        // delivery, or for the mount that opens the record: a document that
        // holds nothing else is not stored, and a document merged into a
        // snapshot brings only the changes it applied.
        let waiting = (app.to_owned(), record.to_owned());
        if doc.get_missing_deps(&[]).is_empty() {
            self.waiting.remove(&waiting);
        } else {
            self.waiting.insert(waiting, runs);
        }
        let heads = doc.get_heads();
        if heads.is_empty() {
            return Ok(None);
        }
        if heads == before && !fresh {
            return Ok(Some(heads));
        }
        self.store
            .write(&key, WriteOp::Replace(doc.save()), writer.as_deref())
            .map(|_| Some(heads))
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
        // A desktop of the person's own keeps what they arranged.
        if self.own_layout {
            self.desktop_value = Some(value);
            return Ok(());
        }
        // A peer renaming the workspace is news to everyone else here; the
        // first desktop this page receives is not.
        let renamed = tilo_desktop_surface::history_log::WorkspaceRename::merged(
            self.desktop.document(),
            self.desktop_value.as_ref(),
            &value,
        );
        for (&id, mounted) in &self.mounts {
            if let Some((handle, shared)) = &mounted.attachment.desktop {
                crate::desktop::apply_shared(handle, shared, &value)?;
                if let Some(rename) = &renamed {
                    shared.renames.borrow_mut().push(rename.clone());
                }
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
        let mut opening: Vec<DocumentRoute> = Vec::new();
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                let (app, home) = store.package_and_home();
                if home != self.uuid {
                    continue;
                }
                for checkout in store.open_records() {
                    let key = (app.clone(), wire_record(&checkout.id).to_owned());
                    // A record a join does not replay is asked for when a
                    // mount first opens it.
                    if !mounted.records.contains_key(&key)
                        && neighbour_wire::record_sync(&key.1) != neighbour_wire::RecordSync::Eager
                    {
                        opening.push(DocumentRoute {
                            app_id: app.clone(),
                            cluster_id: self.uuid.clone(),
                            record: key.1.clone(),
                        });
                    }
                    let delivered = &mut self.delivered;
                    let replica = mounted.records.entry(key.clone()).or_insert_with(|| {
                        match delivered.remove(&key) {
                            // The room has the record as it delivered it.
                            Some(heads) => {
                                let mut frontier = DocumentFrontier::default();
                                frontier.advance(heads);
                                Replica {
                                    frontier,
                                    snapshotted: true,
                                }
                            }
                            None => Replica {
                                frontier: DocumentFrontier::default(),
                                snapshotted: false,
                            },
                        }
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
        for route in opening {
            if self.peer.open_record(self.workspace, route).is_err() {
                return;
            }
        }
        // Records first: a desktop naming a new tab's record must not reach
        // a peer before the record does.
        for (route, mutation, advance) in outgoing {
            // A first snapshot carries what was there already; an
            // increment is what this page changed.
            let authored = matches!(mutation, DocumentMutation::Incremental(_));
            // The room answers each frame the publication is split into.
            let commits = mutation.encode_frames().map_or(1, |frames| frames.len());
            if self.peer.publish(route.clone(), &mutation).is_err() {
                return;
            }
            let left = self.in_flight.entry(route).or_default();
            *left = left.saturating_add(commits);
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

    /// A room whose layout is personal, as a public room's is, shares no
    /// member's windows or layout: what this person arranges is theirs
    /// alone, never sent, and nothing of it is undone. Known from the room's
    /// governance as soon as the page hears it, or from the room refusing
    /// this page's desktop as personal.
    fn keep_own_layout(&mut self, woken: &mut HashSet<u64>) {
        self.own_layout = true;
        for (&id, mounted) in &self.mounts {
            if let Some((_, shared)) = &mounted.attachment.desktop {
                shared.own_layout.set(true);
                woken.insert(id);
            }
        }
    }

    /// [`Self::keep_own_layout`] once the room's governance says its layout
    /// is personal, as the peer heard it (`PeerStatus::personal_rooms`):
    /// asked on joining, for a scope heard before this room listened, and
    /// whenever a room's scope changes.
    fn follow_layout(&mut self, woken: &mut HashSet<u64>) {
        if !self.own_layout && self.peer.status().personal_rooms.contains(&self.uuid) {
            self.keep_own_layout(woken);
        }
    }

    /// The room's members as the peer last heard its governance list them:
    /// the attached desktops release what someone who left made.
    fn follow_members(&mut self, woken: &mut HashSet<u64>) {
        let members = self.peer.status().room_members.get(&self.uuid).cloned();
        if members == self.members {
            return;
        }
        self.members = members;
        for (&id, mounted) in &self.mounts {
            if let Some((_, shared)) = &mounted.attachment.desktop {
                shared.members.replace(self.members.clone());
                woken.insert(id);
            }
        }
    }

    fn send_desktop(&mut self) {
        if self.own_layout {
            return;
        }
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
        if self.local_value.as_ref() == Some(&value) {
            return;
        }
        if let Some(refusal) = self.rearranges_others(&value, &author) {
            // Whatever moved someone else's window that the desktop did not
            // refuse first (a race with their own move) is undone here: it
            // springs back to where the room has it, nothing is sent, and
            // the person is told why.
            if let Some(room) = &self.desktop_value {
                for mounted in self.mounts.values() {
                    if let Some((handle, shared)) = &mounted.attachment.desktop {
                        if let Err(error) = crate::desktop::return_to(handle, shared, room) {
                            warn(&format!("shared desktop not restored: {error}"));
                        }
                        handle.acknowledge(refusal.clone());
                        (mounted.attachment.notify)();
                    }
                }
            }
            return;
        }
        // The desktop this page joined with is not something the person did.
        let joined = self.desktop_value.is_some();
        let doc = self.desktop.document_mut();
        // Only the entries that differ are written, stamped with the time
        // and who made the change, as the history pane tells it.
        let wrote = tilo_desktop_surface::history_log::record(
            doc,
            &value,
            &author,
            ui::time::unix_now_i64(),
        );
        if wrote && joined {
            // Windows opened, closed, moved or renamed: the person's own
            // work on the desktop.
            self.authored = self.authored.saturating_add(1);
        }
        if !wrote && self.desktop_snapshotted {
            self.local_value = Some(value);
            return;
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
            self.desktop_value = desktop_value(self.desktop.document());
            self.local_value = Some(value);
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

/// Who wrote the newest change of the room's desktop, as its message names
/// them (`history_log::record`).
fn desktop_value(doc: &AutoCommit) -> Option<serde_json::Value> {
    tilo_desktop_model::shared_record::read(doc, None)
        .and_then(|desktop| serde_json::to_value(desktop).ok())
}
