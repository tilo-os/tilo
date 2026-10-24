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
//! The app's window and the store meet in a [`ContentLane`], one per
//! record of a process: the store queues its requests there, and the
//! window's collaboration loop sends them on the record's route and hands
//! back what arrives.
//!
//! A record other than the window's own — one of the person's records,
//! kept in their own room — has a lane of its own that names its route
//! ([`ContentLane::record_store`]): its content lives where that record
//! does, on the person's devices, never in the window's room.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tilo_content::{
    Collected, ContentBackend, ContentError, ContentReader, ContentState, ContentStore, Stored,
};
use tilo_lock::MutexRecover as _;

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
}

/// What the window sends on the record's route.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LaneRequests {
    pub wants: Vec<String>,
    pub offers: Vec<(String, Vec<u8>)>,
}

type Lanes = Mutex<HashMap<(String, String), Arc<ContentLane>>>;

fn lanes() -> std::sync::MutexGuard<'static, HashMap<(String, String), Arc<ContentLane>>> {
    static LANES: OnceLock<Lanes> = OnceLock::new();
    LANES.get_or_init(Mutex::default).lock_recover()
}

impl ContentLane {
    /// The lane of record `record` of app `app` in this process.
    pub fn of(app: &str, record: &str) -> Arc<Self> {
        Arc::clone(
            lanes()
                .entry((app.to_owned(), record.to_owned()))
                .or_default(),
        )
    }

    /// The lane of a window's record `record` of app `app`: the one its
    /// route carries. A package's primary record travels as the route
    /// without a record, so its lane is that route's.
    pub fn of_record(app: &str, record: &crate::record::RecordId) -> Arc<Self> {
        let wire = if record.is_package_primary() {
            ""
        } else {
            record.as_str()
        };
        Self::of(app, wire)
    }

    /// The content store of the record `route` names, when it is not the
    /// window's own: what it references is read from and kept by the host
    /// under that record's scope, and travels on its route alone. This
    /// device holds the bytes for the app only while it runs.
    pub fn record_store(route: &super::RouteSpec) -> ContentStore {
        let lane = Self::of(&route.app_id, route.record.as_deref().unwrap_or(""));
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
            .filter(|((lane_app, _), _)| lane_app == app)
            .filter_map(|(_, lane)| Some((lane.route.get()?.clone(), Arc::clone(lane))))
            .collect()
    }

    /// The record's host answered the route's content subscription: what
    /// the app stores reaches the record's other readers by its address.
    pub fn serve(&self) {
        self.served
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn is_served(&self) -> bool {
        self.served.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Wake `wake`'s owner when the store has something for the route.
    pub fn attach(&self, wake: crate::WakeHandle) {
        if let Ok(mut slot) = self.wake.lock() {
            *slot = Some(wake);
        }
    }

    fn wake(&self) {
        if let Ok(slot) = self.wake.lock()
            && let Some(wake) = slot.as_ref()
        {
            wake.wake();
        }
    }

    fn want(&self, hash: &str) {
        let asked = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
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
        if let Ok(mut state) = self.state.lock() {
            state.offers.push((hash.to_owned(), bytes));
        }
        self.wake();
    }

    /// Where a want of `hash` stands, while it is wanted.
    fn standing(&self, hash: &str) -> Option<ContentState> {
        let state = self.state.lock().ok()?;
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
        let Ok(mut state) = self.state.lock() else {
            return LaneRequests::default();
        };
        LaneRequests {
            wants: std::mem::take(&mut state.asks),
            offers: std::mem::take(&mut state.offers),
        }
    }

    /// Put back a request the route could not take yet.
    pub fn retry_want(&self, hash: String) {
        if let Ok(mut state) = self.state.lock() {
            state.asks.push(hash);
        }
    }

    /// Put back an offer the route could not take yet.
    pub fn retry_offer(&self, hash: String, bytes: Vec<u8>) {
        if let Ok(mut state) = self.state.lock() {
            state.offers.push((hash, bytes));
        }
    }

    /// Content arrived on the record's route. The store keeps it on its
    /// next read.
    pub fn arrive(&self, hash: String, bytes: Vec<u8>) {
        if let Ok(mut state) = self.state.lock() {
            state.arrived.push((hash, bytes));
        }
        arrived_somewhere();
    }

    /// The record's host said where a want of `hash` stands. A window
    /// waiting for the content looks again: what it shows follows.
    pub fn stands(&self, hash: String, standing: ContentState) {
        if let Ok(mut state) = self.state.lock() {
            if !state.wanted.contains(&hash) || standing == ContentState::Resolved {
                return;
            }
            state.standing.insert(hash, standing);
        }
        arrived_somewhere();
    }

    fn take_arrived(&self) -> Vec<(String, Vec<u8>)> {
        self.state
            .lock()
            .map(|mut state| std::mem::take(&mut state.arrived))
            .unwrap_or_default()
    }

    fn settled(&self, hash: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.wanted.remove(hash);
            state.standing.remove(hash);
        }
    }
}

/// A record's content store: the store a host lent the app, with content
/// the record references travelling on the record's route.
pub struct RecordContent {
    local: ContentStore,
    lane: Arc<ContentLane>,
}

impl RecordContent {
    /// `local`, joined to the route `lane` reaches.
    pub fn store(local: ContentStore, lane: Arc<ContentLane>) -> ContentStore {
        ContentStore::from_backend(Arc::new(Self { local, lane }))
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
    fn room_scoped(&self) -> bool {
        self.local.room_scoped()
    }

    fn collect_unreferenced(
        &self,
        live: &std::collections::BTreeSet<String>,
    ) -> Result<Collected, ContentError> {
        self.local.collect_unreferenced(live)
    }
}

#[cfg(test)]
mod tests;
