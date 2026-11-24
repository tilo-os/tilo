//! The content a record references: the bytes an app's record names by hash, kept
//! on this device beside the workspace's records, and carried to the workspace's
//! other devices on the topic the records travel.
//! A record holds the address of its binaries, never the bytes. The
//! [broker's service](tilo_workspace_service::content) keeps the rules: content
//! is verified against its address, possessing an address never grants access to
//! it, and nothing arrives unasked. This module supplies what a session adds:
//! [`Shelf`], a store under the session's state, a directory per workspace and app,
//! so content outlives the session that stored it; [`Peers`], the workspace's topic
//! as the record's route, where a want is published and an answer goes to the one
//! device that asked; and the queue, which reaches [`RouteContent`] from a worker
//! thread, never from the session's loop.

use std::collections::{BTreeSet, HashMap};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::EndpointId;
use neighbour_wire::content::{MAX_CONTENT_BYTES, PIECE_PATIENCE_MS, Span};
use neighbour_wire::{NeighbourMsg, peer_handle};
use tilo_broker_core::route::CrdtRoute;
use tilo_broker_core::services::ContentStoreServices;
use tilo_content::{ContentStore, hash_bytes};
use tilo_desktop_model::WorkspaceId as ModelWorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_network_actor::{RecordContentDelivery, WorkspaceId};
use tilo_runtime::offload::{BlobScope, MediaFuture, WorkspaceMedia};
use tilo_workspace_service::content::{
    ContentPeer, ContentPeers, ContentPiece, ContentScope, ContentShelf, ContentSources,
    ContentState, RouteContent,
};

use crate::sync::{Cmd, HostOutbox};

/// Where record content is kept under the session's state: beside the engine's records, apart from
/// the blocks their history keeps.
const CONTENT_DIR: &str = "record-content";
/// Record scopes whose store this device keeps open at once. Content itself stays on disk; this is
/// only what one pass over many workspaces holds.
const MAX_OPEN_SCOPES: usize = 32;
/// Devices whose address this device remembers to answer them at.
const MAX_REMEMBERED_REQUESTERS: usize = 256;
/// Members' messages waiting for the content service. Past it, an ask is dropped and asked again
/// when the devices that could answer change.
const MAX_QUEUED: usize = 512;

/// The session's side of the records' content: the broker's service, the workspace's topic as its
/// route, and the worker that takes members' asks and answers off the session's loop.
pub(crate) struct ContentService {
    route: Arc<RouteContent>,
    shelf: Arc<Shelf>,
    peers: Arc<Peers>,
    inbound: SyncSender<Inbound>,
    /// The store of each record the session keeps itself, by workspace and app
    /// ([`ContentService::store`]).
    kept: Mutex<HashMap<(String, String), Arc<Kept>>>,
}

impl ContentService {
    /// The record-content service of a session whose state is `state` : content under it outlives the
    /// session, as the records do. A session that keeps no state keeps content for as long as it
    /// runs.
    pub(crate) fn open(state: Option<&Path>) -> Arc<Self> {
        let shelf = Arc::new(Shelf {
            root: state.map(|state| state.join("tilo").join(CONTENT_DIR)),
            stores: Mutex::default(),
        });
        let peers = Arc::new(Peers::default());
        let route = RouteContent::new(
            Arc::clone(&shelf) as Arc<dyn ContentShelf>,
            Arc::clone(&peers) as Arc<dyn ContentPeers>,
        );
        let (inbound, arriving) = sync_channel(MAX_QUEUED);
        let served = Arc::clone(&route);
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-content".into())
            .spawn(move || serve(&served, &arriving))
        {
            tracing::warn!(%error, "record content arrives unwatched");
        }
        Arc::new(Self {
            route,
            shelf,
            peers,
            inbound,
            kept: Mutex::default(),
        })
    }

    /// What the broker's `offerBlob` , `wantBlob` and `subscribeBlobs` answer through.
    pub(crate) fn route(&self) -> Arc<RouteContent> {
        Arc::clone(&self.route)
    }

    /// The store an app uploads content into through the content put API,
    /// addressed by the hash its record then references.
    pub(crate) fn puts(&self) -> Arc<dyn ContentStoreServices> {
        tilo_workspace_service::content::shelf_puts(Arc::clone(&self.shelf) as Arc<dyn ContentShelf>)
    }

    /// Keep `bytes` as content of `workspace`, under the same store the
    /// content put API serves its apps: what the session itself makes of a
    /// workspace — the picture a window it does not run shows — is read by
    /// the same addresses as what the workspace's apps store.
    ///
    /// # Errors
    /// The workspace names no content, or the store refused the bytes.
    pub(crate) fn keep(&self, workspace: &str, bytes: &[u8]) -> Result<String, String> {
        let puts = self.puts();
        let mut sink = puts.begin(workspace)?;
        sink.append(bytes)?;
        let (hash, _) = sink.finish()?;
        Ok(hash)
    }

    /// This device's content as an offload route reads and keeps it: the invoking app's scope in
    /// its workspace, through the same store the app's own `offerBlob` and `wantBlob` reach.
    pub(crate) fn media(self: &Arc<Self>) -> Arc<dyn WorkspaceMedia> {
        Arc::new(Media(Arc::clone(&self.shelf)))
    }

    /// The content store of the record of `app` in `workspace`, as the session keeps it itself:
    /// what its actions keep is offered where the workspace's devices fetch the record's content
    /// from, and what a record names and this device lacks is asked of the route.
    ///
    /// The store is the same one for the workspace and app every time: a session lends it to the
    /// services whose records it keeps (`tilo_service::Service::lend_content`) on every pass.
    pub(crate) fn store(&self, workspace: &str, app: &str) -> ContentStore {
        let kept = self.kept_document(workspace, app);
        ContentStore::from_backend(kept as Arc<dyn tilo_content::ContentBackend>).sealing()
    }

    /// Seal what the record of `app` in `workspace` keeps under `secret`, the content secret its
    /// members hold: the same bytes stored twice are then one item under one reference, as a
    /// window's record content is, and whoever keeps the ciphertext holds no key.
    pub(crate) fn seal_under(
        &self,
        workspace: &str,
        app: &str,
        secret: Option<tilo_seal::SymmetricKey>,
    ) {
        let Some(secret) = secret else {
            return;
        };
        *self.kept_document(workspace, app).secret.lock_recover() = Some(secret);
    }

    /// The store of the record of `app` in `workspace`, as the session keeps it: the same one for
    /// the workspace and app every time.
    fn kept_document(&self, workspace: &str, app: &str) -> Arc<Kept> {
        let scope = (workspace.to_owned(), app.to_owned());
        if let Some(kept) = self.kept.lock_recover().get(&scope) {
            return Arc::clone(kept);
        }
        let kept = Arc::new(Kept {
            route: Arc::clone(&self.route),
            on: CrdtRoute::app(app, workspace),
            secret: Mutex::default(),
        });
        self.kept.lock_recover().insert(scope, Arc::clone(&kept));
        kept
    }

    /// The bytes this device keeps of `app` 's content in `workspace` under `hash` (lower-case
    /// hex): never a fetch of what it lacks.
    pub(crate) fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        let scope = ContentScope {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
        };
        ContentShelf::read(self.shelf.as_ref(), &scope, hash)
    }

    /// `digest` of `app` 's content in `workspace` , once it is here: what this device keeps
    /// already, else what the workspace's other devices hand over within `within` . `None` when it
    /// does not arrive in time.
    pub(crate) async fn fetch(
        &self,
        workspace: &str,
        app: &str,
        digest: [u8; 32],
        within: Duration,
    ) -> Option<Vec<u8>> {
        let hash = hex::encode(digest);
        let route = CrdtRoute::app(app, workspace);
        // Subscribed before it is asked for, so content that arrives at once is not missed.
        let mut news = self.route.subscribe(&route);
        self.route.want(&route, &hash);
        let started = tokio::time::Instant::now();
        loop {
            let left = within.saturating_sub(started.elapsed());
            match tokio::time::timeout(left, news.recv()).await {
                Ok(Some(arrived))
                    if arrived.hash == hash && arrived.state == ContentState::Resolved =>
                {
                    return Some(arrived.bytes);
                }
                // Where another want of this scope got to, or what the devices connected could
                // answer: this one is still coming.
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// Where this device's content service says and hears on the network: the workspace topics it publishes
    /// on, and this device's handle there.
    pub(crate) fn bind(&self, outgoing: HostOutbox, publisher: EndpointId) {
        self.peers.bind(outgoing, publisher);
    }

    /// The devices this session reaches in the workspace on `ws` changed: what is wanted there is
    /// asked of those that connected, and where each want stands follows.
    pub(crate) fn sources_changed(&self, ws: WorkspaceId, sources: ContentSources) {
        self.peers.know(ws, sources);
        self.route.sources_changed();
    }

    /// A message `from` published on the topic of `ws` , which is the workspace `workspace` :
    /// whether it was about a record's content, which this content service takes and the session's loop then
    /// leaves to it.
    pub(crate) fn heard(
        &self,
        ws: WorkspaceId,
        workspace: &str,
        from: EndpointId,
        msg: &NeighbourMsg,
        may_read: &dyn Fn(&str) -> bool,
    ) -> bool {
        let Some(entry) = inbound(ws, workspace, from, msg, &self.peers, may_read) else {
            return false;
        };
        // A full queue drops what a member said here; the devices that could answer are told again
        // when they change, and ask again from there.
        let _ = self.inbound.try_send(entry);
        true
    }
}

/// What this session's content service holds of a record's content, as an offload round that ran its
/// invocation on another device reads what that device produced: [`ContentService::fetch`] from the devices
/// that have it.
impl tilo_runtime::offload::OutputContent for ContentService {
    fn content<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
        digest: [u8; 32],
        within: Duration,
    ) -> MediaFuture<'a, Option<Vec<u8>>> {
        Box::pin(self.fetch(workspace, app, digest, within))
    }
}

/// What a message from a workspace's member says about content, when it says anything about it: the
/// record's route, and the ask or answer it carries.
fn inbound(
    ws: WorkspaceId,
    workspace: &str,
    from: EndpointId,
    msg: &NeighbourMsg,
    peers: &Peers,
    may_read: &dyn Fn(&str) -> bool,
) -> Option<Inbound> {
    let entry = match msg {
        NeighbourMsg::DocumentContentWant {
            requester,
            app_id,
            cluster_id,
            hash,
            have,
        } => {
            if *cluster_id != workspace || *requester == peers.own() {
                return None;
            }
            // The route is the member's own device, as the transport authenticated it: an ask
            // naming another device is nobody's.
            if *requester != peer_handle(from.as_bytes()) {
                return None;
            }
            peers.remember(ws, *requester, from);
            Inbound::Want {
                on: CrdtRoute::app(app_id.clone(), cluster_id.clone()),
                peer: *requester,
                may_read: may_read(app_id),
                hash: hash.clone(),
                have: have.clone(),
            }
        }
        NeighbourMsg::DocumentContentPiece {
            responder,
            requester,
            app_id,
            cluster_id,
            hash,
            total,
            offset,
            bytes,
        } => {
            if *cluster_id != workspace
                || *requester != peers.own()
                || *responder != peer_handle(from.as_bytes())
            {
                return None;
            }
            Inbound::Piece {
                on: CrdtRoute::app(app_id.clone(), cluster_id.clone()),
                piece: ContentPiece {
                    hash: hash.clone(),
                    total: *total,
                    offset: *offset,
                    bytes: bytes.clone(),
                },
            }
        }
        NeighbourMsg::DocumentContentLacks {
            responder,
            requester,
            app_id,
            cluster_id,
            hash,
        }
        | NeighbourMsg::DocumentContentRefused {
            responder,
            requester,
            app_id,
            cluster_id,
            hash,
        } => {
            if *cluster_id != workspace
                || *requester != peers.own()
                || *responder != peer_handle(from.as_bytes())
            {
                return None;
            }
            Inbound::NoContent {
                on: CrdtRoute::app(app_id.clone(), cluster_id.clone()),
                peer: *responder,
                hash: hash.clone(),
                refused: matches!(msg, NeighbourMsg::DocumentContentRefused { .. }),
            }
        }
        _ => return None,
    };
    Some(entry)
}

/// A message about a record's content, for [`RouteContent`]'s worker.
enum Inbound {
    /// A member asked the workspace for content it lacks, naming what it
    /// holds already.
    Want {
        on: CrdtRoute,
        peer: ContentPeer,
        /// Whether the asking device's person may read the record the content belongs to.
        may_read: bool,
        hash: String,
        have: Vec<Span>,
    },
    /// A piece of content this device asked for.
    Piece { on: CrdtRoute, piece: ContentPiece },
    /// A device keeps none of the content this device asked it for, or
    /// refused an ask misaddressed to it.
    NoContent {
        on: CrdtRoute,
        peer: ContentPeer,
        hash: String,
        refused: bool,
    },
}

/// Hand what the workspace's members said about content to `content` , off the session's loop: the
/// service reads and writes this device's store, which the loop, which walks no path, must not do
/// itself. A pass of the patience of a want asks again for what is still missing, so content that
/// reached a holder after a first failed ask arrives without a restart. Ends when the content
/// service drops.
fn serve(content: &RouteContent, arriving: &Receiver<Inbound>) {
    loop {
        match arriving.recv_timeout(Duration::from_millis(PIECE_PATIENCE_MS)) {
            Ok(entry) => match entry {
                Inbound::Want {
                    on,
                    peer,
                    may_read,
                    hash,
                    have,
                } => content.peer_wants(&on, peer, may_read, &hash, &have),
                Inbound::Piece { on, piece } => content.peer_piece(&on, &piece),
                Inbound::NoContent {
                    on,
                    peer,
                    hash,
                    refused: false,
                } => content.peer_lacks(&on, peer, &hash),
                Inbound::NoContent {
                    on,
                    peer,
                    hash,
                    refused: true,
                } => content.peer_refused(&on, peer, &hash),
            },
            Err(RecvTimeoutError::Timeout) => content.retry(tilo_clock::unix_now_ms()),
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// This device's record content: the store of each record scope under the session's state.
struct Shelf {
    /// Where the stores live, or `None` on a session that keeps no state.
    root: Option<PathBuf>,
    stores: Mutex<HashMap<ContentScope, ContentStore>>,
}

impl Shelf {
    /// The store of `scope` , when the scope names a directory this device keeps content in.
    fn store(&self, scope: &ContentScope) -> Option<ContentStore> {
        if !scope.names_a_store() {
            return None;
        }
        if let Some(store) = self.stores.lock_recover().get(scope) {
            return Some(store.clone());
        }
        let store = match &self.root {
            Some(root) => ContentStore::in_dir(root.join(&scope.workspace).join(&scope.app)),
            None => ContentStore::memory(),
        };
        {
            let mut stores = self.stores.lock_recover();
            if stores.len() >= MAX_OPEN_SCOPES {
                stores.clear();
            }
            stores.insert(scope.clone(), store.clone());
        }
        Some(store)
    }
}

impl ContentShelf for Shelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        // What this device keeps, never a fetch of what it lacks: a peer's ask must not set this
        // device looking for bytes nowhere has yet.
        self.store(scope)?.read(hash)
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let store = self
            .store(scope)
            .ok_or("this workspace and app name no content of theirs")?;
        let kept = store.put(bytes).map_err(|error| error.to_string())?;
        if kept == hash {
            Ok(())
        } else {
            Err("the content store keeps these bytes under another address".into())
        }
    }

    fn has(&self, scope: &ContentScope, hash: &str) -> bool {
        self.store(scope).is_some_and(|store| store.has(hash))
    }
}

/// This device's content as an offload route measures, reads and keeps it: the store of the scope
/// the route names, and nothing else.
struct Media(Arc<Shelf>);

/// The record scope a route names, when it names one this device keeps content in.
fn scoped(scope: &BlobScope) -> ContentScope {
    ContentScope {
        workspace: scope.workspace().to_owned(),
        app: scope.app().to_owned(),
    }
}

impl WorkspaceMedia for Media {
    fn keep<'a>(
        &'a self,
        scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>> {
        let scope = scoped(scope);
        Box::pin(async move {
            let hash = hash_bytes(&bytes);
            self.0.keep(&scope, &hash, &bytes)?;
            let mut digest = [0; 32];
            hex::decode_to_slice(&hash, &mut digest)
                .map_err(|_| "the content store keeps these bytes under no address".to_owned())?;
            Ok(digest)
        })
    }
}

/// The store of a record the session keeps itself, on the route the workspace's devices fetch its
/// content from: what is kept is offered there, and what the record names and this device lacks is
/// asked of it ([`RouteContent`]).
struct Kept {
    route: Arc<RouteContent>,
    on: CrdtRoute,
    /// The content secret of the members who read the record, once the engine holds it
    /// ([`ContentService::seal_under`]).
    secret: Mutex<Option<tilo_seal::SymmetricKey>>,
}

impl tilo_content::ContentBackend for Kept {
    fn put(
        &self,
        reader: &mut dyn std::io::Read,
        _mime: &str,
    ) -> Result<tilo_content::Stored, tilo_content::ContentError> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_CONTENT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_CONTENT_BYTES {
            return Err(tilo_content::ContentError::ItemTooLarge {
                actual: bytes.len(),
                limit: usize::try_from(MAX_CONTENT_BYTES).unwrap_or(usize::MAX),
            });
        }
        let hash = hash_bytes(&bytes);
        self.route
            .offer(&self.on, &hash, &bytes)
            .map_err(tilo_content::ContentError::Io)?;
        Ok(tilo_content::Stored {
            hash,
            len: bytes.len() as u64,
        })
    }

    fn open(
        &self,
        hash: &str,
    ) -> Result<Option<tilo_content::ContentReader>, tilo_content::ContentError> {
        if let Some(bytes) = self.route.kept(&self.on, hash) {
            return Ok(Some(Box::new(std::io::Cursor::new(bytes))));
        }
        // Not here: the route's other devices are asked, and what one hands over is kept where
        // this store reads it the next time.
        self.route.want(&self.on, hash);
        Ok(None)
    }

    fn has(&self, hash: &str) -> bool {
        self.route.kept(&self.on, hash).is_some()
    }

    fn state(&self, hash: &str) -> ContentState {
        if self.has(hash) {
            ContentState::Resolved
        } else {
            // A want started by [`Self::open`]: where it stands reaches the route's subscribers.
            ContentState::Pending
        }
    }

    fn shared(&self) -> bool {
        true
    }

    /// The route serves the workspace's record, so what this store reads is as scoped as the
    /// workspace the record is in.
    fn workspace_scoped(&self) -> bool {
        true
    }

    fn content_secret(&self) -> Option<tilo_seal::SymmetricKey> {
        self.secret.lock_recover().clone()
    }

    fn max_item_bytes(&self) -> usize {
        usize::try_from(MAX_CONTENT_BYTES).unwrap_or(usize::MAX)
    }
}

/// How this device reaches the other devices of each workspace: what it publishes on their topics,
/// and the address of each device that asked it for content through them.
#[derive(Default)]
struct Peers {
    /// Where this device's content service is reached on the network, once it runs: its handle there and the
    /// channel it says a message on.
    link: Mutex<Option<Link>>,
    /// The devices this device reaches in each workspace, as the workspaces reported them.
    sources: Mutex<HashMap<WorkspaceId, ContentSources>>,
    /// The device behind each handle that asked this one, per workspace: an answer goes to that
    /// device alone.
    requesters: Mutex<HashMap<(WorkspaceId, ContentPeer), EndpointId>>,
}

struct Link {
    outgoing: HostOutbox,
    /// This device's handle here, as the wire names it.
    own: ContentPeer,
}

impl Peers {
    fn bind(&self, outgoing: HostOutbox, publisher: EndpointId) {
        *self.link.lock_recover() = Some(Link {
            outgoing,
            own: peer_handle(publisher.as_bytes()),
        });
    }

    /// This device's handle here, or zero while the content service runs nowhere: no message names it then.
    fn own(&self) -> ContentPeer {
        self.link.lock_recover().as_ref().map_or(0, |link| link.own)
    }

    fn know(&self, ws: WorkspaceId, sources: ContentSources) {
        self.sources.lock_recover().insert(ws, sources);
    }

    fn remember(&self, ws: WorkspaceId, peer: ContentPeer, device: EndpointId) {
        let mut requesters = self.requesters.lock_recover();
        if requesters.len() >= MAX_REMEMBERED_REQUESTERS && !requesters.contains_key(&(ws, peer)) {
            requesters.clear();
        }
        requesters.insert((ws, peer), device);
    }

    fn target(&self, ws: WorkspaceId, peer: ContentPeer) -> Option<EndpointId> {
        self.requesters.lock_recover().get(&(ws, peer)).copied()
    }

    /// Say `msg` on the topic of `ws` , when this device's content service runs.
    fn say(&self, ws: WorkspaceId, msg: NeighbourMsg) {
        if let Some(link) = self.link.lock_recover().as_ref() {
            let _ = link.outgoing.network(Cmd::Publish {
                ws,
                msg: Box::new(msg),
            });
        }
    }

    /// Send `msg` to `target` on `ws` : directly when the workspace admits that device, else
    /// through the workspace's hosts, which hand it to that device alone.
    fn send(&self, ws: WorkspaceId, target: Option<EndpointId>, msg: NeighbourMsg) {
        if let Some(link) = self.link.lock_recover().as_ref() {
            link.outgoing
                .content(RecordContentDelivery { ws, target, msg });
        }
    }
}

impl ContentPeers for Peers {
    fn want(&self, route: &CrdtRoute, hash: &str, have: &[Span]) {
        let Some(ws) = topic(route) else {
            return;
        };
        let msg = NeighbourMsg::DocumentContentWant {
            requester: self.own(),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.to_owned(),
            have: have.to_vec(),
        };
        self.say(ws, msg);
    }

    fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
        let Some(ws) = topic(route) else {
            return;
        };
        let msg = NeighbourMsg::DocumentContentPiece {
            responder: self.own(),
            requester: peer,
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: piece.hash,
            total: piece.total,
            offset: piece.offset,
            bytes: piece.bytes,
        };
        self.send(ws, self.target(ws, peer), msg);
    }

    fn lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str) {
        let Some(ws) = topic(route) else {
            return;
        };
        let msg = NeighbourMsg::DocumentContentLacks {
            responder: self.own(),
            requester: peer,
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.to_owned(),
        };
        self.send(ws, self.target(ws, peer), msg);
    }

    fn sources(&self, route: &CrdtRoute) -> ContentSources {
        let Some(ws) = topic(route) else {
            return ContentSources::default();
        };
        self.sources
            .lock_recover()
            .get(&ws)
            .cloned()
            .unwrap_or_default()
    }
}

/// The topic a record's content travels on: its workspace's, and none for a record of no workspace.
fn topic(route: &CrdtRoute) -> Option<WorkspaceId> {
    let workspace = ModelWorkspaceId::parse(&route.cluster_id)?;
    (!workspace.is_nil()).then_some(workspace)
}

/// The devices a workspace's topic carries to this session, from what the content service heard: those that
/// published there, and the workspace's hosts, which the members it reaches are behind.
#[derive(Debug, Default)]
pub(crate) struct Topology {
    peers: BTreeSet<EndpointId>,
    hosts: BTreeSet<EndpointId>,
}

impl Topology {
    /// `device` said something on the workspace's topic.
    pub(crate) fn heard(&mut self, device: EndpointId) -> bool {
        self.peers.insert(device)
    }

    /// `host` serves the workspace: what its topic publishes, and the members it reports reaching.
    pub(crate) fn host(&mut self, host: EndpointId) -> bool {
        self.hosts.insert(host)
    }

    /// `device` left the workspace's topic, or the host that reached it did.
    pub(crate) fn left(&mut self, device: EndpointId) -> bool {
        let peer = self.peers.remove(&device);
        let host = self.hosts.remove(&device);
        peer || host
    }

    /// The devices of the workspace this session reaches now: those heard on its topic and the
    /// hosts that serve it, never `own` , this device.
    pub(crate) fn devices(&self, own: EndpointId) -> BTreeSet<EndpointId> {
        self.peers
            .iter()
            .chain(self.hosts.iter())
            .filter(|device| **device != own)
            .copied()
            .collect()
    }

    /// The devices that could hand the workspace's content over, as the content service names them:
    /// never `own` , this device.
    pub(crate) fn sources(&self, own: EndpointId) -> ContentSources {
        ContentSources {
            peers: self
                .peers
                .iter()
                .filter(|device| **device != own)
                .map(|device| peer_handle(device.as_bytes()))
                .collect(),
            hosts: self
                .hosts
                .iter()
                .filter(|host| **host != own)
                .map(|host| peer_handle(host.as_bytes()))
                .collect(),
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
