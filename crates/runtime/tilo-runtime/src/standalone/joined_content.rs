//! The content a joined workspace's records reference, on the workspace's
//! direct link. This device asks the workspace for a hash its record lacks on
//! its link; the host fans the request out to every member, and a member
//! keeping the content answers on its own link, from what the profile's
//! content store keeps under the record's workspace and app.

use super::Joined;
use neighbour_wire::NeighbourMsg;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
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

/// The joined workspaces' links as the route of their records' content.
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
        let _ = live.send(message);
    }

    fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
        let Some(workspace) = WorkspaceId::parse(&route.cluster_id) else {
            return;
        };
        let Some(live) = self.0.live(workspace) else {
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
        let _ = live.send(message);
    }

    fn lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str) {
        let Some(workspace) = WorkspaceId::parse(&route.cluster_id) else {
            return;
        };
        let Some(live) = self.0.live(workspace) else {
            return;
        };
        let message = NeighbourMsg::RecordContentLacks {
            responder: self.0.own_handle(),
            requester: peer,
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.to_owned(),
        };
        let _ = live.send(message);
    }

    /// The host the workspace's link is connected to, and the members whose
    /// frames it heard: this device reaches them only through it.
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
    /// The profile's record-content service, once the broker runs.
    pub(in crate::standalone) fn record_content_service(&self) -> Option<Arc<RouteContent>> {
        self.content.get().cloned()
    }

    fn own_handle(&self) -> ContentPeer {
        peer_handle(&self.custody.device_public_key())
    }

    /// A member of `workspace` asked it for content. Returns whether `message`
    /// was such a request.
    pub(super) fn content_wanted(&self, workspace: WorkspaceId, message: &NeighbourMsg) -> bool {
        let NeighbourMsg::RecordContentWant {
            requester,
            app_id,
            cluster_id,
            hash,
        } = message
        else {
            return false;
        };
        if *requester == self.own_handle() || WorkspaceId::parse(cluster_id) != Some(workspace) {
            return true;
        }
        let Some(content) = self.content.get().cloned() else {
            return true;
        };
        let route = CrdtRoute::app(app_id.clone(), cluster_id.clone());
        let (peer, hash) = (*requester, hash.clone());
        // The host admitted the member to the workspace, which lets it read.
        tilo_rt::app().spawn_blocking(move || content.peer_wants(&route, peer, true, &hash));
        true
    }

    /// A piece of content another member sent this device through the
    /// host, or word that a device keeps none of what this one asked for.
    pub(super) fn content_arrived(
        &self,
        workspace: WorkspaceId,
        from: [u8; 32],
        message: &NeighbourMsg,
    ) {
        if let NeighbourMsg::RecordContentLacks {
            responder,
            requester,
            app_id,
            cluster_id,
            hash,
        } = message
        {
            if *responder == peer_handle(&from)
                && *requester == self.own_handle()
                && WorkspaceId::parse(cluster_id) == Some(workspace)
                && let Some(content) = self.content.get().cloned()
            {
                let (route, responder, hash) = (
                    CrdtRoute::app(app_id.clone(), cluster_id.clone()),
                    *responder,
                    hash.clone(),
                );
                tilo_rt::app().spawn_blocking(move || content.peer_lacks(&route, responder, &hash));
            }
            return;
        }
        let NeighbourMsg::RecordContentPiece {
            responder,
            requester,
            app_id,
            cluster_id,
            hash,
            total,
            offset,
            bytes,
        } = message
        else {
            return;
        };
        if *responder != peer_handle(&from)
            || *requester != self.own_handle()
            || WorkspaceId::parse(cluster_id) != Some(workspace)
        {
            return;
        }
        let Some(content) = self.content.get().cloned() else {
            return;
        };
        let route = CrdtRoute::app(app_id.clone(), cluster_id.clone());
        let piece = ContentPiece {
            hash: hash.clone(),
            total: *total,
            offset: *offset,
            bytes: bytes.clone(),
        };
        tilo_rt::app().spawn_blocking(move || content.peer_piece(&route, &piece));
    }
}
