//! The content a record references: the bytes an app's record names by hash, kept
//! on this device beside the workspace's records, and carried to the workspace's
//! other devices on the topic the records travel.
//! A record holds the address of its binaries, never the bytes. The
//! [broker's service](tilo_action_broker::record_content) keeps the rules: content
//! is verified against its address, possessing an address never grants access to
//! it, and nothing arrives unasked. This module supplies what a session adds:
//! [`Shelf`], a store under the session's state, a directory per workspace and app,
//! so content outlives the session that stored it; [`Peers`], the workspace's topic
//! as the record's route, where a want is published and an answer goes to the one
//! device that asked; and the queue, which reaches [`RouteContent`] from a worker
//! thread, never from the session's loop.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::EndpointId;
use neighbour_wire::{NeighbourMsg, peer_handle};
use tilo_action_broker::record_content::{
    ContentPeer, ContentPeers, ContentPiece, ContentScope, ContentShelf, ContentSources,
    ContentState, MAX_CONTENT_BYTES, RouteContent,
};
use tilo_action_broker::route::CrdtRoute;
use tilo_action_broker::services::{CONTENT_PUT_SCOPE, ContentSink, ContentStoreServices};
use tilo_content::{ContentStore, hash_bytes};
use tilo_desktop_model::WorkspaceId as ModelWorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_network_actor::{RecordContentDelivery, WorkspaceId, workspace_topic_id};
use tilo_runtime::offload::{BindError, BlobScope, MediaFuture, WorkspaceMedia};

use crate::sync::Outgoing;

/// Where record content is kept under the session's state: beside the engine's records, apart from
/// the blocks their history keeps.
const CONTENT_DIR: &str = "record-content";
/// The longest a workspace or app name may be, as a content store names one.
const MAX_SCOPE_COMPONENT: usize = 128;
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
pub(crate) struct Plane {
    route: Arc<RouteContent>,
    shelf: Arc<Shelf>,
    peers: Arc<Peers>,
    inbound: SyncSender<Inbound>,
}

impl Plane {
    /// The record-content plane of a session whose state is `state` : content under it outlives the
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
        })
    }

    /// What the broker's `offerBlob` , `wantBlob` and `subscribeBlobs` answer through.
    pub(crate) fn route(&self) -> Arc<RouteContent> {
        Arc::clone(&self.route)
    }

    /// The store an app uploads content into through the content put door,
    /// addressed by the hash its record then references.
    pub(crate) fn puts(&self) -> Arc<dyn ContentStoreServices> {
        Arc::new(Puts(Arc::clone(&self.shelf)))
    }

    /// Keep `bytes` as content of `workspace`, under the same store the
    /// content put door serves its apps: what the session itself makes of a
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

    /// Whether this device keeps `digest` of `app` 's content in `workspace` : what this session
    /// says of itself when another of the person's devices offers it work reading that content.
    pub(crate) fn holds(&self, workspace: &str, app: &str, digest: [u8; 32]) -> bool {
        let scope = ContentScope {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
        };
        self.shelf
            .store(&scope)
            .is_some_and(|store| store.has(&hex::encode(digest)))
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

    /// Where this device's plane says and hears on the network: the workspace topics it publishes
    /// on, and this device's handle there.
    pub(crate) fn bind(
        &self,
        outgoing: tokio::sync::mpsc::UnboundedSender<Outgoing>,
        publisher: EndpointId,
    ) {
        self.peers.bind(outgoing, publisher);
    }

    /// The devices this session reaches in the workspace on `ws` changed: what is wanted there is
    /// asked of those that connected, and where each want stands follows.
    pub(crate) fn sources_changed(&self, ws: WorkspaceId, sources: ContentSources) {
        self.peers.know(ws, sources);
        self.route.sources_changed();
    }

    /// A message `from` published on the topic of `ws` , which is the workspace `workspace` :
    /// whether it was about a record's content, which this plane takes and the session's loop then
    /// leaves to it.
    pub(crate) fn heard(
        &self,
        ws: WorkspaceId,
        workspace: &str,
        from: EndpointId,
        msg: &NeighbourMsg,
    ) -> bool {
        let Some(entry) = inbound(ws, workspace, from, msg, &self.peers) else {
            return false;
        };
        // A full queue drops what a member said here; the devices that could answer are told again
        // when they change, and ask again from there.
        let _ = self.inbound.try_send(entry);
        true
    }
}

/// What this session's plane holds of a record's content, as an offload round that ran its
/// invocation on another device reads what that device produced: [`Plane::fetch`] from the devices
/// that have it.
impl tilo_runtime::offload::OutputContent for Plane {
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
) -> Option<Inbound> {
    let entry = match msg {
        NeighbourMsg::RecordContentWant {
            requester,
            app_id,
            cluster_id,
            hash,
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
                hash: hash.clone(),
            }
        }
        NeighbourMsg::RecordContentPiece {
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
        NeighbourMsg::RecordContentLacks {
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
            Inbound::Lacks {
                on: CrdtRoute::app(app_id.clone(), cluster_id.clone()),
                peer: *responder,
                hash: hash.clone(),
            }
        }
        _ => return None,
    };
    Some(entry)
}

/// A message about a record's content, for [`RouteContent`]'s worker.
enum Inbound {
    /// A member asked the workspace for content it lacks.
    Want {
        on: CrdtRoute,
        peer: ContentPeer,
        hash: String,
    },
    /// A piece of content this device asked for.
    Piece { on: CrdtRoute, piece: ContentPiece },
    /// A member keeps none of the content this device asked it for.
    Lacks {
        on: CrdtRoute,
        peer: ContentPeer,
        hash: String,
    },
}

/// Hand what the workspace's members said about content to `content` , off the session's loop: the
/// service reads and writes this device's store, which the loop, which walks no path, must not do
/// itself. Ends when the plane drops.
fn serve(content: &RouteContent, arriving: &Receiver<Inbound>) {
    while let Ok(entry) = arriving.recv() {
        match entry {
            // A member that reached here was admitted to the workspace by its policy, which lets it
            // read the records the content belongs to.
            Inbound::Want { on, peer, hash } => content.peer_wants(&on, peer, true, &hash),
            Inbound::Piece { on, piece } => content.peer_piece(&on, &piece),
            Inbound::Lacks { on, peer, hash } => content.peer_lacks(&on, peer, &hash),
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
        if !component(&scope.workspace) || !component(&scope.app) {
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
    fn len<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<u64, BindError>> {
        let scope = scoped(scope);
        Box::pin(async move {
            let bytes = self
                .0
                .read(&scope, &hex::encode(digest))
                .ok_or(BindError::NotLocal)?;
            Ok(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        })
    }

    fn read<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<Vec<u8>, BindError>> {
        let scope = scoped(scope);
        Box::pin(async move {
            let bytes = self
                .0
                .read(&scope, &hex::encode(digest))
                .ok_or(BindError::NotLocal)?;
            if hash_bytes(&bytes) == hex::encode(digest) {
                Ok(bytes)
            } else {
                Err(BindError::Integrity)
            }
        })
    }

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

fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SCOPE_COMPONENT
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !value.bytes().all(|byte| byte == b'.')
}

/// The content put door's store: bytes an app uploads into its workspace,
/// kept beside the content its records reference.
struct Puts(Arc<Shelf>);

impl ContentStoreServices for Puts {
    fn max_bytes(&self) -> u64 {
        MAX_CONTENT_BYTES
    }

    fn begin(&self, workspace: &str) -> Result<Box<dyn ContentSink>, String> {
        if !component(workspace) {
            return Err("this workspace name keeps no content".into());
        }
        Ok(Box::new(Upload {
            shelf: Arc::clone(&self.0),
            scope: ContentScope {
                workspace: workspace.to_owned(),
                app: CONTENT_PUT_SCOPE.to_owned(),
            },
            bytes: Vec::new(),
        }))
    }

    fn holds(&self, workspace: &str, hash: &str) -> bool {
        let scope = ContentScope {
            workspace: workspace.to_owned(),
            app: CONTENT_PUT_SCOPE.to_owned(),
        };
        self.0.store(&scope).is_none_or(|store| store.has(hash))
    }
}

/// One upload an app is still sending.
struct Upload {
    shelf: Arc<Shelf>,
    scope: ContentScope,
    bytes: Vec<u8>,
}

impl ContentSink for Upload {
    fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.bytes.len().saturating_add(bytes.len()) as u64 > MAX_CONTENT_BYTES {
            return Err("content is larger than a record may reference".into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(String, u64), String> {
        let Self {
            shelf,
            scope,
            bytes,
        } = *self;
        let hash = hash_bytes(&bytes);
        shelf.keep(&scope, &hash, &bytes)?;
        let len = bytes.len() as u64;
        Ok((hash, len))
    }
}

/// How this device reaches the other devices of each workspace: what it publishes on their topics,
/// and the address of each device that asked it for content through them.
#[derive(Default)]
struct Peers {
    /// Where this device's plane is reached on the network, once it runs: its handle there and the
    /// channel it says a message on.
    link: Mutex<Option<Link>>,
    /// The devices this device reaches in each workspace, as the workspaces reported them.
    sources: Mutex<HashMap<WorkspaceId, ContentSources>>,
    /// The device behind each handle that asked this one, per workspace: an answer goes to that
    /// device alone.
    requesters: Mutex<HashMap<(WorkspaceId, ContentPeer), EndpointId>>,
}

struct Link {
    outgoing: tokio::sync::mpsc::UnboundedSender<Outgoing>,
    /// This device's handle here, as the wire names it.
    own: ContentPeer,
}

impl Peers {
    fn bind(&self, outgoing: tokio::sync::mpsc::UnboundedSender<Outgoing>, publisher: EndpointId) {
        *self.link.lock_recover() = Some(Link {
            outgoing,
            own: peer_handle(publisher.as_bytes()),
        });
    }

    /// This device's handle here, or zero while the plane runs nowhere: no message names it then.
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

    /// Say `msg` on the topic of `ws` , when this device's plane runs.
    fn say(&self, ws: WorkspaceId, msg: NeighbourMsg) {
        if let Some(link) = self.link.lock_recover().as_ref() {
            let _ = link.outgoing.send(Outgoing::Publish {
                ws,
                msg: Box::new(msg),
            });
        }
    }

    /// Send `msg` to `target` on `ws` : directly when the workspace admits that device, else
    /// through the workspace's hosts, which hand it to that device alone.
    fn send(&self, ws: WorkspaceId, target: Option<EndpointId>, msg: NeighbourMsg) {
        if let Some(link) = self.link.lock_recover().as_ref() {
            let _ =
                link.outgoing
                    .send(Outgoing::Content(RecordContentDelivery { ws, target, msg }));
        }
    }
}

impl ContentPeers for Peers {
    fn want(&self, route: &CrdtRoute, hash: &str) {
        let Some(ws) = topic(route) else {
            return;
        };
        let msg = NeighbourMsg::RecordContentWant {
            requester: self.own(),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.to_owned(),
        };
        self.say(ws, msg);
    }

    fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
        let Some(ws) = topic(route) else {
            return;
        };
        let msg = NeighbourMsg::RecordContentPiece {
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
        let msg = NeighbourMsg::RecordContentLacks {
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
    (!workspace.is_nil()).then(|| workspace_topic_id(workspace))
}

/// The devices a workspace's topic carries to this session, from what the plane heard: those that
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
