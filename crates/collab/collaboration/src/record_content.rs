//! Content an app's record references, travelling on the record's route.
//!
//! A record holds a reference to its binaries (a picture's content hash),
//! never the bytes. An app reads and writes those bytes through the content
//! store its host lent it; [`RecordContent`] wraps that store so that
//!
//! - content the app stores is also offered to its host, which keeps it
//!   where the record's other readers are served from
//!   ([`CollabRequest::OfferContent`](super::CollabRequest::OfferContent));
//! - content the record references and this device lacks is asked of the
//!   host, which answers from this device or the record's route
//!   ([`CollabRequest::WantContent`](super::CollabRequest::WantContent)), and
//!   reads as where the host says the want stands until it is here
//!   ([`CollabEvent::RecordContentState`](super::CollabEvent::RecordContentState)):
//!   [`ContentState::Pending`] while a connected device is asked for it,
//!   [`ContentState::Waiting`] while none that could hand it over is
//!   connected, [`ContentState::Unavailable`] once every connected one said
//!   it keeps none;
//! - content that arrives ([`CollabEvent::RecordContent`](super::CollabEvent::RecordContent))
//!   is verified and kept in the store, where the app's next read finds it.
//!   The event syncs the window, and [`arrivals`] moves: an app waiting for
//!   content looks again then, never on a timer.
//!
//! The app's window and the store meet in a [`ContentLane`], one per window
//! ([`ContentLane::of_window`]): the store queues its requests there, and
//! the window's collaboration loop sends them on the record's route and
//! hands back what arrives. Two windows showing one record each queue on
//! their own lane, so what one asks for is never what the other sends.
//!
//! Content an app stores is sealed under a key derived from the content
//! secret of the members who read the record, once its route holds their
//! keys ([`ContentLane::hold_secret`]): the same bytes stored twice, by any
//! window on a record of the same members, are one item under one
//! reference. Before the keys arrive, each item is sealed under a fresh key.
//! A record is named by its route, whose cluster is its workspace: one app's
//! records in two workspaces are two lanes, each sealed under the secret of
//! its own workspace's members.
//!
//! A record other than the window's own — one of the person's records,
//! kept in their own workspace — has a lane of its own that names its route
//! ([`ContentLane::record_store`]): its content lives where that record
//! does, on the person's devices, never in the window's workspace.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tilo_content::{
    Collected, ContentBackend, ContentError, ContentReader, ContentState, ContentStore, Stored,
};
use tilo_lock::MutexRecover as _;

// The lane `window`, the wake scope a window's code runs in, keeps for the
// record it shows: one per window, so two windows of one app in a process are
// never the one lane of the record they share. The window's own threads — the
// ones that entered its scope — read the lane from here.
thread_local! {
    static OWN_LANE: RefCell<Option<(crate::WakeHandle, Arc<ContentLane>)>> =
        const { RefCell::new(None) };
}

/// Let `window`'s record content travel on `lane`: the window keeps it, and
/// its own record content reads it back ([`ContentLane::of_window`]).
pub fn keep_lane(window: &crate::WakeHandle, lane: Arc<ContentLane>) {
    OWN_LANE.with(|own| *own.borrow_mut() = Some((window.clone(), lane)));
}

/// Largest content one record references.
pub const MAX_RECORD_CONTENT_BYTES: usize = 64 * 1024 * 1024;

static ARRIVALS: AtomicU64 = AtomicU64::new(0);

/// How many times content has reached a record's store in this process:
/// arrived on a record's route, or stored by an app. A window that lacks
/// content its record references reads again when this has moved since it
/// last looked; it never looks on a timer.
#[must_use]
pub fn arrivals() -> u64 {
    ARRIVALS.load(Ordering::Acquire)
}

fn arrived_somewhere() {
    ARRIVALS.fetch_add(1, Ordering::AcqRel);
}

#[derive(Default)]
struct LaneState {
    /// Content asked for and not yet here.
    wanted: BTreeSet<String>,
    /// Where the host said a want stands, for those it said so of.
    standing: HashMap<String, ContentState>,
    /// Requests the window has yet to send.
    asks: Vec<String>,
    offers: Vec<(String, Vec<u8>)>,
    /// Content that arrived and the store has yet to keep.
    arrived: Vec<(String, Vec<u8>)>,
}

/// Where one record's content store and its window's collaboration meet.
#[derive(Default)]
pub struct ContentLane {
    /// The record's route, for a record that is not the window's own.
    route: OnceLock<super::RouteSpec>,
    /// The content store of such a record.
    store: OnceLock<ContentStore>,
    state: Mutex<LaneState>,
    wake: Mutex<Option<crate::WakeHandle>>,
    /// The record's host carries its content to the record's other
    /// readers.
    served: std::sync::atomic::AtomicBool,
    /// The content secret of the members who read the record, while its
    /// route holds their keys.
    secret: Mutex<Option<tilo_seal::SymmetricKey>>,
    /// The record's own lane ([`Self::of`]), for a window's lane: the
    /// secret is the record's, whichever window stores.
    record: OnceLock<Arc<ContentLane>>,
}

/// What the window sends on the record's route.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LaneRequests {
    pub wants: Vec<String>,
    pub offers: Vec<(String, Vec<u8>)>,
}

/// Which record a lane carries: its workspace (the route's cluster), its
/// app and its wire name. One app's records in two workspaces are two
/// lanes, each sealed under its own members' secret.
type LaneKey = (uuid::Uuid, String, String);

fn lane_key(route: &super::RouteSpec) -> LaneKey {
    (
        route.cluster_id,
        route.app_id.clone(),
        route.record.clone().unwrap_or_default(),
    )
}

type Lanes = Mutex<HashMap<LaneKey, Arc<ContentLane>>>;

fn lanes() -> std::sync::MutexGuard<'static, HashMap<LaneKey, Arc<ContentLane>>> {
    static LANES: OnceLock<Lanes> = OnceLock::new();
    LANES.get_or_init(Mutex::default).lock_recover()
}

/// The lanes windows keep for the records they show, by record
/// ([`ContentLane::show_on_route`]); a lane goes once its window let it go.
type WindowLanes = Mutex<HashMap<LaneKey, Vec<std::sync::Weak<ContentLane>>>>;

fn window_lanes()
-> std::sync::MutexGuard<'static, HashMap<LaneKey, Vec<std::sync::Weak<ContentLane>>>> {
    static WINDOW_LANES: OnceLock<WindowLanes> = OnceLock::new();
    WINDOW_LANES.get_or_init(Mutex::default).lock_recover()
}

impl ContentLane {
    /// `lane`, a window's own ([`Self::of_window`]), carries the content of
    /// the record `route` names: whatever moves that record's content for
    /// the process reaches it through [`Self::on_route`] for as long as the
    /// window keeps it, and what it stores is sealed under the secret of
    /// that record's members.
    pub fn show_on_route(route: &super::RouteSpec, lane: &Arc<Self>) {
        let _ = lane.record.set(Self::of(route));
        let mut lanes = window_lanes();
        let kept = lanes.entry(lane_key(route)).or_default();
        kept.retain(|kept| {
            kept.strong_count() > 0 && !std::ptr::eq(kept.as_ptr(), Arc::as_ptr(lane))
        });
        kept.push(Arc::downgrade(lane));
    }

    /// Every lane carrying the content of the record `route` names in this
    /// process: the record's own ([`Self::of`]) and those of the windows
    /// showing it ([`Self::show_on_route`]). What their windows ask for is
    /// asked of the record's route, and what arrives on it is handed to
    /// each.
    pub fn on_route(route: &super::RouteSpec) -> Vec<Arc<Self>> {
        let mut found = vec![Self::of(route)];
        let mut lanes = window_lanes();
        if let Some(kept) = lanes.get_mut(&lane_key(route)) {
            kept.retain(|kept| kept.strong_count() > 0);
            found.extend(kept.iter().filter_map(std::sync::Weak::upgrade));
        }
        found
    }

    /// The lane of the record `route` names in this process.
    pub fn of(route: &super::RouteSpec) -> Arc<Self> {
        Arc::clone(lanes().entry(lane_key(route)).or_default())
    }

    /// The lane of the window whose code is running: the one it keeps
    /// ([`keep_lane`]), made for it when it keeps none yet.
    ///
    /// A lane belongs to a window, not to a record: two windows of one app
    /// showing the same record queue their wants and their offers apart, so
    /// what one asks for is never what the other thinks it asked. The record
    /// travels on its route either way; its route is the same for both
    /// windows, its lane is not.
    pub fn of_window() -> Arc<Self> {
        let window = crate::WakeHandle::new();
        OWN_LANE.with(|own| {
            let mut own = own.borrow_mut();
            match own.as_ref() {
                Some((kept, lane)) if kept.same_open_scope(&window) => return Arc::clone(lane),
                // The window that kept a lane before this one closed; the
                // window here now keeps a lane of its own.
                Some(_) => *own = None,
                None => {}
            }
            let lane = Arc::new(Self::default());
            *own = Some((window, Arc::clone(&lane)));
            lane
        })
    }

    /// The content store of the record `route` names, when it is not the
    /// window's own: what it references is read from and kept by the host
    /// under that record's scope, and travels on its route alone. This
    /// device holds the bytes for the app only while it runs.
    pub fn record_store(route: &super::RouteSpec) -> ContentStore {
        let lane = Self::of(route);
        let _ = lane.route.set(route.clone());
        lane.store
            .get_or_init(|| RecordContent::store(ContentStore::memory(), Arc::clone(&lane)))
            .clone()
    }

    /// The lanes of app `app`'s records that are not a window's own, with
    /// the routes they send on.
    pub fn routed(app: &str) -> Vec<(super::RouteSpec, Arc<Self>)> {
        lanes()
            .iter()
            .filter(|((_, lane_app, _), _)| lane_app == app)
            .filter_map(|(_, lane)| Some((lane.route.get()?.clone(), Arc::clone(lane))))
            .collect()
    }

    /// The record's host answered the route's content subscription: what
    /// the app stores reaches the record's other readers by its address.
    pub fn serve(&self) {
        self.served
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// The record's route holds the keys of the members who read it, whose
    /// content secret is `secret`, or no longer holds them (`None`). What the
    /// record's windows store from now on is sealed under keys derived from
    /// it.
    pub fn hold_secret(&self, secret: Option<tilo_seal::SymmetricKey>) {
        *self.secret.lock_recover() = secret;
    }

    fn content_secret(&self) -> Option<tilo_seal::SymmetricKey> {
        self.secret
            .lock_recover()
            .clone()
            .or_else(|| self.record.get()?.content_secret())
    }

    fn is_served(&self) -> bool {
        self.served.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Wake `wake`'s owner when the store has something for the route.
    pub fn attach(&self, wake: crate::WakeHandle) {
        *self.wake.lock_recover() = Some(wake);
    }

    fn wake(&self) {
        if let Some(wake) = self.wake.lock_recover().as_ref() {
            wake.wake();
        }
    }

    fn want(&self, hash: &str) {
        let asked = {
            let mut state = self.state.lock_recover();
            if state.wanted.insert(hash.to_owned()) {
                state.asks.push(hash.to_owned());
                true
            } else {
                false
            }
        };
        if asked {
            self.wake();
        }
    }

    fn offer(&self, hash: &str, bytes: Vec<u8>) {
        self.state
            .lock_recover()
            .offers
            .push((hash.to_owned(), bytes));
        self.wake();
    }

    /// Where a want of `hash` stands, while it is wanted.
    fn standing(&self, hash: &str) -> Option<ContentState> {
        let state = self.state.lock_recover();
        state.wanted.contains(hash).then(|| {
            state
                .standing
                .get(hash)
                .copied()
                .unwrap_or(ContentState::Pending)
        })
    }

    /// The requests to send on the record's route now.
    pub fn take_requests(&self) -> LaneRequests {
        let mut state = self.state.lock_recover();
        LaneRequests {
            wants: std::mem::take(&mut state.asks),
            offers: std::mem::take(&mut state.offers),
        }
    }

    /// Put back a request the route could not take yet.
    pub fn retry_want(&self, hash: String) {
        self.state.lock_recover().asks.push(hash);
    }

    /// Put back an offer the route could not take yet.
    pub fn retry_offer(&self, hash: String, bytes: Vec<u8>) {
        self.state.lock_recover().offers.push((hash, bytes));
    }

    /// Content arrived on the record's route. The store keeps it on its
    /// next read.
    pub fn arrive(&self, hash: String, bytes: Vec<u8>) {
        self.state.lock_recover().arrived.push((hash, bytes));
        arrived_somewhere();
    }

    /// The record's host said where a want of `hash` stands. A window
    /// waiting for the content looks again: what it shows follows.
    pub fn stands(&self, hash: String, standing: ContentState) {
        {
            let mut state = self.state.lock_recover();
            if !state.wanted.contains(&hash) || standing == ContentState::Resolved {
                return;
            }
            state.standing.insert(hash, standing);
        }
        arrived_somewhere();
    }

    fn take_arrived(&self) -> Vec<(String, Vec<u8>)> {
        std::mem::take(&mut self.state.lock_recover().arrived)
    }

    fn settled(&self, hash: &str) {
        {
            let mut state = self.state.lock_recover();
            state.wanted.remove(hash);
            state.standing.remove(hash);
        }
    }
}

/// A record's content store: the store a host lent the app, with content
/// the record references travelling on the record's route.
///
/// This is where it is decided that what a record references is sealed
/// ([`ContentStore::sealing`]): every app keeps its record's content
/// through it, and only the record's readers hold the keys its references
/// carry. Below it, this device's store, the route and the record's host
/// hold and carry ciphertext alone.
pub struct RecordContent {
    local: ContentStore,
    lane: Arc<ContentLane>,
}

impl RecordContent {
    /// `local`, joined to the route `lane` reaches.
    pub fn store(local: ContentStore, lane: Arc<ContentLane>) -> ContentStore {
        // What reaches this backend is already as it is kept (sealed, or
        // public): `local` stores it under its own address, never sealing it
        // again, as a lent store that seals would.
        let local = local.unsealed();
        ContentStore::from_backend(Arc::new(Self { local, lane })).sealing()
    }

    /// Keep what arrived, verified against the address it arrived under.
    fn keep_arrived(&self) {
        for (hash, bytes) in self.lane.take_arrived() {
            if !tilo_content::verify(&hash, &bytes) {
                tracing::warn!(%hash, "record content did not match its address");
                continue;
            }
            match self.local.put(&bytes) {
                Ok(_) => self.lane.settled(&hash),
                Err(error) => tracing::warn!(%error, %hash, "record content was not kept"),
            }
        }
    }
}

impl ContentBackend for RecordContent {
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_RECORD_CONTENT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RECORD_CONTENT_BYTES {
            return Err(ContentError::ItemTooLarge {
                actual: bytes.len(),
                limit: MAX_RECORD_CONTENT_BYTES,
            });
        }
        let stored = self.local.put_reader(&mut bytes.as_slice(), mime)?;
        self.lane.offer(&stored.hash, bytes);
        arrived_somewhere();
        Ok(stored)
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        self.keep_arrived();
        let found = self.local.open(hash);
        if found.is_none() && tilo_content::valid_hash(hash) {
            self.lane.want(&hash.to_ascii_lowercase());
        }
        Ok(found)
    }

    fn has(&self, hash: &str) -> bool {
        self.keep_arrived();
        self.local.has(hash)
    }

    fn state(&self, hash: &str) -> ContentState {
        self.keep_arrived();
        match self.local.state(hash) {
            ContentState::Resolved => ContentState::Resolved,
            local => self
                .lane
                .standing(&hash.to_ascii_lowercase())
                .unwrap_or(local),
        }
    }

    fn shared(&self) -> bool {
        self.local.shared() || self.lane.is_served()
    }

    /// The route only brings what the record's other readers hold for it, so
    /// what this store reads is as scoped as the window's own store.
    fn workspace_scoped(&self) -> bool {
        self.local.workspace_scoped()
    }

    fn collect_unreferenced(
        &self,
        live: &std::collections::BTreeSet<String>,
    ) -> Result<Collected, ContentError> {
        self.local.collect_unreferenced(live)
    }

    fn content_secret(&self) -> Option<tilo_seal::SymmetricKey> {
        self.lane.content_secret()
    }

    fn max_item_bytes(&self) -> usize {
        MAX_RECORD_CONTENT_BYTES
    }

    fn scratch_dir(&self) -> Option<std::path::PathBuf> {
        self.local.scratch_dir()
    }
}

#[cfg(test)]
mod tests;
