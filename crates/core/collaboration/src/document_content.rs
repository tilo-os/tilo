//! Content an app's record references, travelling on the record's route.
//!
//! A record holds a reference to its binaries (a picture's content hash),
//! never the bytes. An app reads and writes those bytes through the content
//! store its host lent it; [`DocumentContent`] wraps that store so that
//!
//! - content the app stores is also offered to its host, which keeps it
//!   where the record's other readers are served from
//!   ([`CollabRequest::OfferContent`](super::CollabRequest::OfferContent));
//! - content the record references and this device lacks is asked of the
//!   host, which answers from this device or the record's route
//!   ([`CollabRequest::WantContent`](super::CollabRequest::WantContent)), and
//!   reads as where the host says the want stands until it is here
//!   ([`CollabEvent::DocumentContentState`](super::CollabEvent::DocumentContentState)):
//!   [`ContentState::Pending`] while a connected device is asked for it,
//!   [`ContentState::Waiting`] while none that could hand it over is
//!   connected, [`ContentState::Unavailable`] once every connected one said
//!   it keeps none;
//! - content that arrives ([`CollabEvent::DocumentContent`](super::CollabEvent::DocumentContent))
//!   is verified and kept in the store, where the app's next read finds it.
//!   The event syncs the window, and [`arrivals`] moves: an app waiting for
//!   content looks again then, never on a timer.
//!
//! The app's window and the store meet in a [`ContentLink`], one per window
//! ([`ContentLink::of_window`]): the store queues its requests there, and
//! the window's collaboration loop sends them on the record's route and
//! hands back what arrives. Two windows showing one record each queue on
//! their own content link, so what one asks for is never what the other sends.
//!
//! Content an app stores is sealed under a key derived from the content
//! secret of the members who read the record, once its route holds their
//! keys ([`ContentLink::hold_secret`]): the same bytes stored twice, by any
//! window on a record of the same members, are one item under one
//! reference. Before the keys arrive, each item is sealed under a fresh key.
//! A record is named by its route, whose cluster is its workspace: one app's
//! records in two workspaces are two content links, each sealed under the secret of
//! its own workspace's members.
//!
//! A record other than the window's own — one of the person's records,
//! kept in their own workspace — has a content link of its own that names its route
//! ([`ContentLink::resource_store`]): its content lives where that record
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

// The content link `window`, the wake scope a window's code runs in, keeps for the
// record it shows: one per window, so two windows of one app in a process are
// never the one content link of the record they share. The window's own threads — the
// ones that entered its scope — read the content link from here.
thread_local! {
    static OWN_LINK: RefCell<Option<(crate::WakeHandle, Arc<ContentLink>)>> =
        const { RefCell::new(None) };
}

/// Let `window`'s record content travel on `link`: the window keeps it, and
/// its own record content reads it back ([`ContentLink::of_window`]).
pub fn keep_link(window: &crate::WakeHandle, link: Arc<ContentLink>) {
    OWN_LINK.with(|own| *own.borrow_mut() = Some((window.clone(), link)));
}

/// Largest content one record references.
pub const MAX_DOCUMENT_CONTENT_BYTES: usize = 64 * 1024 * 1024;

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
struct LinkState {
    /// Content asked for and not yet here.
    wanted: BTreeSet<String>,
    /// Where the host said a want stands, for those it said so of.
    known: HashMap<String, ContentState>,
    /// Requests the window has yet to send.
    asks: Vec<String>,
    offers: Vec<(String, Vec<u8>)>,
    /// Content that arrived and the store has yet to keep.
    arrived: Vec<(String, Vec<u8>)>,
}

/// Where one record's content store and its window's collaboration meet.
#[derive(Default)]
pub struct ContentLink {
    /// The record's route, for a record that is not the window's own.
    route: OnceLock<super::RouteSpec>,
    /// The content store of such a record.
    store: OnceLock<ContentStore>,
    state: Mutex<LinkState>,
    wake: Mutex<Option<crate::WakeHandle>>,
    /// The record's host carries its content to the record's other
    /// readers.
    served: std::sync::atomic::AtomicBool,
    /// The content secret of the members who read the record, while its
    /// route holds their keys.
    secret: Mutex<Option<tilo_seal::SymmetricKey>>,
    /// Keys held only for routes that require their own group. The key history opens stored
    /// field values written before a membership change.
    document_keys: Mutex<Option<key_group::ContentKeys>>,
    /// The record's own content link ([`Self::of`]), for a window's content link: the
    /// secret is the record's, whichever window stores.
    record: OnceLock<Arc<ContentLink>>,
}

/// What the window sends on the record's route.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ContentRequests {
    pub wants: Vec<String>,
    pub offers: Vec<(String, Vec<u8>)>,
}

/// Which record a content link carries: its workspace (the route's cluster), its
/// app and its wire name. One app's records in two workspaces are two
/// content links, each sealed under its own members' secret.
type LinkKey = (uuid::Uuid, String, String);

fn link_key(route: &super::RouteSpec) -> LinkKey {
    (
        route.cluster_id,
        route.app_id.clone(),
        route.document.clone().unwrap_or_default(),
    )
}

type Links = Mutex<HashMap<LinkKey, Arc<ContentLink>>>;

fn links() -> std::sync::MutexGuard<'static, HashMap<LinkKey, Arc<ContentLink>>> {
    static LINKS: OnceLock<Links> = OnceLock::new();
    LINKS.get_or_init(Mutex::default).lock_recover()
}

/// The content links windows keep for the records they show, by record
/// ([`ContentLink::show_on_route`]); a content link goes once its window let it go.
type WindowLinks = Mutex<HashMap<LinkKey, Vec<std::sync::Weak<ContentLink>>>>;

fn window_links()
-> std::sync::MutexGuard<'static, HashMap<LinkKey, Vec<std::sync::Weak<ContentLink>>>> {
    static WINDOW_LINKS: OnceLock<WindowLinks> = OnceLock::new();
    WINDOW_LINKS.get_or_init(Mutex::default).lock_recover()
}

impl ContentLink {
    /// `link`, a window's own ([`Self::of_window`]), carries the content of
    /// the record `route` names: whatever moves that record's content for
    /// the process reaches it through [`Self::on_route`] for as long as the
    /// window keeps it, and what it stores is sealed under the secret of
    /// that record's members.
    pub fn show_on_route(route: &super::RouteSpec, link: &Arc<Self>) {
        let _ = link.record.set(Self::of(route));
        let mut links = window_links();
        let kept = links.entry(link_key(route)).or_default();
        kept.retain(|kept| {
            kept.strong_count() > 0 && !std::ptr::eq(kept.as_ptr(), Arc::as_ptr(link))
        });
        kept.push(Arc::downgrade(link));
    }

    /// Every content link carrying the content of the record `route` names in this
    /// process: the record's own ([`Self::of`]) and those of the windows
    /// showing it ([`Self::show_on_route`]). What their windows ask for is
    /// asked of the record's route, and what arrives on it is handed to
    /// each.
    pub fn on_route(route: &super::RouteSpec) -> Vec<Arc<Self>> {
        let mut found = vec![Self::of(route)];
        let mut links = window_links();
        if let Some(kept) = links.get_mut(&link_key(route)) {
            kept.retain(|kept| kept.strong_count() > 0);
            found.extend(kept.iter().filter_map(std::sync::Weak::upgrade));
        }
        found
    }

    /// The content link of the record `route` names in this process.
    pub fn of(route: &super::RouteSpec) -> Arc<Self> {
        Arc::clone(links().entry(link_key(route)).or_default())
    }

    /// The content link of the window whose code is running: the one it keeps
    /// ([`keep_link`]), made for it when it keeps none yet.
    ///
    /// A content link belongs to a window, not to a record: two windows of one app
    /// showing the same record queue their wants and their offers apart, so
    /// what one asks for is never what the other thinks it asked. The record
    /// travels on its route either way; its route is the same for both
    /// windows, its content link is not.
    pub fn of_window() -> Arc<Self> {
        let window = crate::WakeHandle::new();
        OWN_LINK.with(|own| {
            let mut own = own.borrow_mut();
            match own.as_ref() {
                Some((kept, link)) if kept.same_open_scope(&window) => return Arc::clone(link),
                // The window that kept a content link before this one closed; the
                // window here now keeps a content link of its own.
                Some(_) => *own = None,
                None => {}
            }
            let link = Arc::new(Self::default());
            *own = Some((window, Arc::clone(&link)));
            link
        })
    }

    /// The content store of the record `route` names, when it is not the
    /// window's own: what it references is read from and kept by the host
    /// under that record's scope, and travels on its route alone. This
    /// device holds the bytes for the app only while it runs.
    pub fn resource_store(route: &super::RouteSpec) -> ContentStore {
        let link = Self::of(route);
        let _ = link.route.set(route.clone());
        link.store
            .get_or_init(|| DocumentContent::store(ContentStore::memory(), Arc::clone(&link)))
            .clone()
    }

    /// The content links of app `app`'s records that are not a window's own, with
    /// the routes they send on.
    pub fn routed(app: &str) -> Vec<(super::RouteSpec, Arc<Self>)> {
        links()
            .iter()
            .filter(|((_, link_app, _), _)| link_app == app)
            .filter_map(|(_, link)| Some((link.route.get()?.clone(), Arc::clone(link))))
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

    #[cfg(any(feature = "broker-transport", feature = "test-support", test))]
    pub(crate) fn hold_document_keys(
        &self,
        spec: &crate::RouteSpec,
        keys: Option<key_group::ContentKeys>,
    ) {
        let document = spec.document.as_deref().unwrap_or_default();
        let workspace = collab_types::WorkspaceId::from_u128(spec.cluster_id.as_u128());
        let expected = workspace.document_key_group(&spec.app_id, document);
        *self.document_keys.lock_recover() = keys.filter(|keys| keys.workspace() == expected);
    }

    /// Seal a sensitive field before putting it in a locally persisted document.
    ///
    /// # Errors
    /// The document's own keys are unavailable or encryption fails.
    pub fn seal_private_field(&self, field: &str, bytes: &[u8]) -> Result<String, String> {
        self.with_document_keys(|keys| {
            keys.seal_private_field(field, bytes)
                .map_err(|error| error.to_string())
        })
    }

    /// Open a sensitive field sealed for this document and field address.
    ///
    /// # Errors
    /// The relevant epoch key is unavailable or the field does not authenticate.
    pub fn open_private_field(
        &self,
        field: &str,
        encoded: &str,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
        self.with_document_keys(|keys| {
            keys.open_private_field(field, encoded)
                .map_err(|error| error.to_string())
        })
    }

    /// The current document key epoch, while this endpoint can open private fields.
    #[must_use]
    pub fn private_field_epoch(&self) -> Option<u64> {
        self.with_document_keys(|keys| Ok(keys.epoch())).ok()
    }

    fn with_document_keys<T>(
        &self,
        use_keys: impl FnOnce(&key_group::ContentKeys) -> Result<T, String>,
    ) -> Result<T, String> {
        let link = self.record.get().map_or(self, Arc::as_ref);
        let held = link.document_keys.lock_recover();
        use_keys(held.as_ref().ok_or("the document keys are unavailable")?)
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
    fn known(&self, hash: &str) -> Option<ContentState> {
        let state = self.state.lock_recover();
        state.wanted.contains(hash).then(|| {
            state
                .known
                .get(hash)
                .copied()
                .unwrap_or(ContentState::Pending)
        })
    }

    /// The requests to send on the record's route now.
    pub fn take_requests(&self) -> ContentRequests {
        let mut state = self.state.lock_recover();
        ContentRequests {
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
    pub fn stands(&self, hash: String, found: ContentState) {
        {
            let mut state = self.state.lock_recover();
            if !state.wanted.contains(&hash) || found == ContentState::Resolved {
                return;
            }
            state.known.insert(hash, found);
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
            state.known.remove(hash);
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
pub struct DocumentContent {
    local: ContentStore,
    link: Arc<ContentLink>,
}

impl DocumentContent {
    /// `local`, joined to the route `link` reaches.
    pub fn store(local: ContentStore, link: Arc<ContentLink>) -> ContentStore {
        // What reaches this backend is already as it is kept (sealed, or
        // public): `local` stores it under its own address, never sealing it
        // again, as a lent store that seals would.
        let local = local.unsealed();
        ContentStore::from_backend(Arc::new(Self { local, link })).sealing()
    }

    /// Keep what arrived, verified against the address it arrived under.
    fn keep_arrived(&self) {
        for (hash, bytes) in self.link.take_arrived() {
            if !tilo_content::verify(&hash, &bytes) {
                tracing::warn!(%hash, "record content did not match its address");
                continue;
            }
            match self.local.put(&bytes) {
                Ok(_) => self.link.settled(&hash),
                Err(error) => tracing::warn!(%error, %hash, "record content was not kept"),
            }
        }
    }
}

impl ContentBackend for DocumentContent {
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_DOCUMENT_CONTENT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_DOCUMENT_CONTENT_BYTES {
            return Err(ContentError::ItemTooLarge {
                actual: bytes.len(),
                limit: MAX_DOCUMENT_CONTENT_BYTES,
            });
        }
        let stored = self.local.put_reader(&mut bytes.as_slice(), mime)?;
        self.link.offer(&stored.hash, bytes);
        arrived_somewhere();
        Ok(stored)
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        self.keep_arrived();
        let found = self.local.open(hash);
        if found.is_none() && tilo_content::valid_hash(hash) {
            self.link.want(&hash.to_ascii_lowercase());
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
            local => self.link.known(&hash.to_ascii_lowercase()).unwrap_or(local),
        }
    }

    fn shared(&self) -> bool {
        self.local.shared() || self.link.is_served()
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
        self.link.content_secret()
    }

    fn max_item_bytes(&self) -> usize {
        MAX_DOCUMENT_CONTENT_BYTES
    }

    fn spill(&self) -> std::io::Result<Option<Box<dyn tilo_content::SpillFile>>> {
        self.local.spill()
    }
}

#[cfg(test)]
mod tests;
