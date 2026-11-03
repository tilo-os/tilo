//! The content a joined workspace's records reference, on the workspace's relay
//! session. This device asks the workspace for a hash its record lacks with the
//! workspace's presence, which the host fans out to every member; a member
//! keeping it answers this device alone through the host. Requests from
//! other members are answered the same way, from what the profile's content
//! store keeps under the record's workspace and app.

use super::Joined;
use neighbour_wire::NeighbourMsg;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tilo_action_broker::{
    record_content::{
        ContentPeer, ContentPeers, ContentPiece, ContentScope, ContentShelf, ContentSources,
        RouteContent,
    },
    route::CrdtRoute,
};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::peer_handle;

/// Devices whose key this device remembers for an answer.
const MAX_REMEMBERED_REQUESTERS: usize = 256;

/// Who asked a joined workspace for content, by the handle their requests name.
#[derive(Default)]
pub(super) struct Requesters(Mutex<HashMap<(WorkspaceId, ContentPeer), [u8; 32]>>);

impl Requesters {
    fn remember(&self, workspace: WorkspaceId, peer: ContentPeer, device: [u8; 32]) {
        if let Ok(mut requesters) = self.0.lock() {
            if requesters.len() >= MAX_REMEMBERED_REQUESTERS
                && !requesters.contains_key(&(workspace, peer))
            {
                requesters.clear();
            }
            requesters.insert((workspace, peer), device);
        }
    }

    fn device(&self, workspace: WorkspaceId, peer: ContentPeer) -> Option<[u8; 32]> {
        self.0.lock().ok()?.get(&(workspace, peer)).copied()
    }
}

/// The profile's content store, read and written as the desktop session
/// reaches its own: through the scope a record's content lives under.
struct ProfileShelf(PathBuf);

impl ContentShelf for ProfileShelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        use runtime_fs::blobs::{BlobClient, ReadOutcome};

        let scoped = BlobClient::connect_session(&self.0)
            .and_then(|session| session.scope(&scope.workspace, &scope.app))
            .ok()?;
        // Only what this device keeps.
        if !scoped.has(hash).ok()? {
            return None;
        }
        match scoped.read_all(hash).ok()? {
            ReadOutcome::Ready { bytes, .. } => Some(bytes),
            ReadOutcome::Pending | ReadOutcome::Unavailable => None,
        }
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let kept = runtime_fs::blobs::BlobClient::connect_session(&self.0)
            .and_then(|session| session.scope(&scope.workspace, &scope.app))
            .and_then(|scoped| scoped.put(bytes, "application/octet-stream"))
            .map_err(|error| error.to_string())?;
        if kept.hash == hash {
            Ok(())
        } else {
            Err("the content store keeps these bytes under another address".into())
        }
    }
}

/// The joined workspaces' relay sessions as the route of their records' content.
struct JoinedPeers(Arc<Joined>);

impl ContentPeers for JoinedPeers {
    fn want(&self, route: &CrdtRoute, hash: &str) {
        let Some(workspace) = WorkspaceId::parse(&route.cluster_id) else {
            return;
        };
        let Some(live) = self.0.live(workspace) else {
            return;
        };
        let message = NeighbourMsg::RecordContentWant {
            requester: self.0.own_handle(),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.to_owned(),
        };
        if let Ok(bytes) = postcard::to_allocvec(&message) {
            let _ = live.publish_presence(bytes);
        }
    }

    fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
        let Some(workspace) = WorkspaceId::parse(&route.cluster_id) else {
            return;
        };
        let (Some(live), Some(device)) = (
            self.0.live(workspace),
            self.0.requesters.device(workspace, peer),
        ) else {
            return;
        };
        let message = NeighbourMsg::RecordContentPiece {
            responder: self.0.own_handle(),
            requester: peer,
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: piece.hash,
            total: piece.total,
            offset: piece.offset,
            bytes: piece.bytes,
        };
        if let Ok(frame) = postcard::to_allocvec(&message) {
            let _ = live.publish_record_content(device, frame);
        }
    }

    fn lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str) {
        let Some(workspace) = WorkspaceId::parse(&route.cluster_id) else {
            return;
        };
        let (Some(live), Some(device)) = (
            self.0.live(workspace),
            self.0.requesters.device(workspace, peer),
        ) else {
            return;
        };
        let message = NeighbourMsg::RecordContentLacks {
            responder: self.0.own_handle(),
            requester: peer,
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.to_owned(),
        };
        if let Ok(frame) = postcard::to_allocvec(&message) {
            let _ = live.publish_record_content(device, frame);
        }
    }

    /// The host the workspace's session is connected to, and the members it
    /// reports connected: this device reaches them only through it.
    fn sources(&self, route: &CrdtRoute) -> ContentSources {
        let Some(workspace) = WorkspaceId::parse(&route.cluster_id) else {
            return ContentSources::default();
        };
        ContentSources {
            peers: self
                .0
                .live_members(workspace)
                .iter()
                .map(peer_handle)
                .collect(),
            hosts: self
                .0
                .live_host(workspace)
                .map(|host| peer_handle(&host))
                .into_iter()
                .collect::<BTreeSet<_>>(),
        }
    }
}

/// The profile's record-content service: its content store, served on
/// `blob_socket`, and the joined workspaces' sessions.
pub(in crate::standalone) fn record_content(
    joined: &Arc<Joined>,
    blob_socket: &Path,
) -> Arc<RouteContent> {
    record_content_on(joined, Arc::new(ProfileShelf(blob_socket.to_path_buf())))
}

/// The record-content service of `joined`'s workspaces over `shelf`.
pub(super) fn record_content_on(
    joined: &Arc<Joined>,
    shelf: Arc<dyn ContentShelf>,
) -> Arc<RouteContent> {
    let content = RouteContent::new(shelf, Arc::new(JoinedPeers(Arc::clone(joined))));
    let _ = joined.content.set(Arc::clone(&content));
    content
}

impl Joined {
    fn own_handle(&self) -> ContentPeer {
        peer_handle(&self.custody.device_signing_key.verifying_key().to_bytes())
    }

    /// A member of `workspace` asked it for content, with presence `author`
    /// published. Returns whether `message` was such a request.
    pub(super) fn content_wanted(
        &self,
        workspace: WorkspaceId,
        author: [u8; 32],
        message: &NeighbourMsg,
    ) -> bool {
        let NeighbourMsg::RecordContentWant {
            requester,
            app_id,
            cluster_id,
            hash,
        } = message
        else {
            return false;
        };
        if *requester != peer_handle(&author)
            || *requester == self.own_handle()
            || WorkspaceId::parse(cluster_id) != Some(workspace)
        {
            return true;
        }
        let Some(content) = self.content.get().cloned() else {
            return true;
        };
        self.requesters.remember(workspace, *requester, author);
        let route = CrdtRoute::app(app_id.clone(), cluster_id.clone());
        let (peer, hash) = (*requester, hash.clone());
        // The host admitted the member to the workspace, which lets it read.
        tilo_rt::app().spawn_blocking(move || content.peer_wants(&route, peer, true, &hash));
        true
    }

    /// A piece of content another member sent this device through the
    /// host, or word that a device keeps none of what this one asked for.
    pub(super) fn content_arrived(&self, workspace: WorkspaceId, from: [u8; 32], frame: &[u8]) {
        if let Ok(NeighbourMsg::RecordContentLacks {
            responder,
            requester,
            app_id,
            cluster_id,
            hash,
        }) = postcard::from_bytes(frame)
        {
            if responder == peer_handle(&from)
                && requester == self.own_handle()
                && WorkspaceId::parse(&cluster_id) == Some(workspace)
                && let Some(content) = self.content.get().cloned()
            {
                let route = CrdtRoute::app(app_id, cluster_id);
                tilo_rt::app().spawn_blocking(move || content.peer_lacks(&route, responder, &hash));
            }
            return;
        }
        let Ok(NeighbourMsg::RecordContentPiece {
            responder,
            requester,
            app_id,
            cluster_id,
            hash,
            total,
            offset,
            bytes,
        }) = postcard::from_bytes(frame)
        else {
            return;
        };
        if responder != peer_handle(&from)
            || requester != self.own_handle()
            || WorkspaceId::parse(&cluster_id) != Some(workspace)
        {
            return;
        }
        let Some(content) = self.content.get().cloned() else {
            return;
        };
        let route = CrdtRoute::app(app_id, cluster_id);
        let piece = ContentPiece {
            hash,
            total,
            offset,
            bytes,
        };
        tilo_rt::app().spawn_blocking(move || content.peer_piece(&route, &piece));
    }
}
