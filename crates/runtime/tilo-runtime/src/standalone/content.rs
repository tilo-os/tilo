//! The record content a workspace's documents reference, on the workspace
//! service's links.
//!
//! This device keeps what it has of a record's content in the profile's
//! content store. A want goes to the device serving a member copy, or, for the
//! workspace this device governs, to the workspace host, which fans it out to
//! the workspace's members; the answers come back the same way, taken into the
//! runtime's record content so an app waiting on a want hears of it.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use neighbour_wire::NeighbourMsg;
use neighbour_wire::content::{PIECE_PATIENCE_MS, Span};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::peer_handle;
use tilo_workspace_service::apps::{
    document_content::{
        ContentPeer, ContentPeers, ContentPiece, ContentScope, ContentShelf, ContentSources,
        RouteContent,
    },
    route::CrdtRoute,
};
use tilo_workspace_service::workspaces::{ContentKeeper, WorkspaceContent};

use super::service::ServiceWorkspaces;

/// The profile's content store, read and written as the desktop session
/// reaches its own: through the scope a record's content lives under.
struct ProfileShelf(PathBuf);

impl ContentShelf for ProfileShelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        use runtime_fs::blobs::{BlobClient, ReadOutcome};

        let scoped = BlobClient::connect_session(&self.0)
            .and_then(|session| session.scope(&scope.workspace, &scope.app))
            .ok()?;
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

/// The profile's content store as a workspace host answers its members'
/// wants from.
struct ShelfContent(Arc<dyn ContentShelf>);

impl WorkspaceContent for ShelfContent {
    fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        self.0.read(
            &ContentScope::of(&CrdtRoute::app(app.to_owned(), workspace.to_owned())),
            hash,
        )
    }
}

/// The workspaces' links as the route of their records' content.
struct MemberPeers(Arc<ServiceWorkspaces>);

impl MemberPeers {
    fn workspace(route: &CrdtRoute) -> Option<WorkspaceId> {
        WorkspaceId::parse(&route.cluster_id)
    }
}

impl ContentPeers for MemberPeers {
    fn want(&self, route: &CrdtRoute, hash: &str, have: &[Span]) {
        let Some(workspace) = Self::workspace(route) else {
            return;
        };
        let message = NeighbourMsg::DocumentContentWant {
            requester: peer_handle(&self.0.device_cert().device_public_key),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.to_owned(),
            have: have.to_vec(),
        };
        if self.0.holds(workspace) {
            let _ = self.0.copies().send_member(workspace, message);
            return;
        }
        // The workspace this device governs: its host asks the members.
        let Ok(body) = postcard::to_stdvec(&message) else {
            return;
        };
        let copies = self.0.copies().clone();
        let cluster = route.cluster_id.clone();
        tilo_rt::app().spawn(async move {
            if let Err(error) = copies.ask_members(&cluster, body).await {
                tracing::debug!(%cluster, %error, "a workspace's host did not ask its members for content");
            }
        });
    }

    /// The service answers a want on whichever link it arrived on, from the
    /// content it is given ([`ShelfContent`]).
    fn answer(&self, _route: &CrdtRoute, _peer: ContentPeer, _piece: ContentPiece) {}

    /// The service answers what it keeps none of itself.
    fn lacks(&self, _route: &CrdtRoute, _peer: ContentPeer, _hash: &str) {}

    fn sources(&self, route: &CrdtRoute) -> ContentSources {
        let Some(workspace) = Self::workspace(route) else {
            return ContentSources::default();
        };
        let hosts = self
            .0
            .copies()
            .member_material(workspace)
            .map(|material| peer_handle(&material.hint.host.device_public_key))
            .into_iter()
            .collect::<BTreeSet<_>>();
        ContentSources {
            peers: BTreeSet::new(),
            hosts,
        }
    }
}

/// The pieces the device serving a member copy answers this device's wants
/// with, taken into the runtime's record content.
struct MemberContent(Arc<RouteContent>);

impl ContentKeeper for MemberContent {
    fn piece(&self, _workspace: &str, _from: [u8; 32], frame: &[u8]) {
        match NeighbourMsg::decode(frame) {
            Ok(NeighbourMsg::DocumentContentPiece {
                app_id,
                cluster_id,
                hash,
                total,
                offset,
                bytes,
                ..
            }) => self.0.peer_piece(
                &CrdtRoute::app(app_id, cluster_id),
                &ContentPiece {
                    hash,
                    total,
                    offset,
                    bytes,
                },
            ),
            Ok(NeighbourMsg::DocumentContentLacks {
                responder,
                app_id,
                cluster_id,
                hash,
                ..
            }) => self
                .0
                .peer_lacks(&CrdtRoute::app(app_id, cluster_id), responder, &hash),
            Ok(NeighbourMsg::DocumentContentRefused {
                responder,
                app_id,
                cluster_id,
                hash,
                ..
            }) => self
                .0
                .peer_refused(&CrdtRoute::app(app_id, cluster_id), responder, &hash),
            _ => {}
        }
    }

    fn expects(&self, _workspace: &str, frame: &[u8]) -> bool {
        match NeighbourMsg::decode(frame) {
            Ok(
                NeighbourMsg::DocumentContentPiece {
                    app_id,
                    cluster_id,
                    hash,
                    ..
                }
                | NeighbourMsg::DocumentContentLacks {
                    app_id,
                    cluster_id,
                    hash,
                    ..
                }
                | NeighbourMsg::DocumentContentRefused {
                    app_id,
                    cluster_id,
                    hash,
                    ..
                },
            ) => self.0.expecting(&CrdtRoute::app(app_id, cluster_id), &hash),
            _ => false,
        }
    }
}

/// The profile's record-content service: its content store, served on
/// `blob_socket`, and the workspaces the workspace service holds.
pub(in crate::standalone) fn document_content(
    workspaces: &Arc<ServiceWorkspaces>,
    blob_socket: &Path,
) -> Arc<RouteContent> {
    let shelf: Arc<dyn ContentShelf> = Arc::new(ProfileShelf(blob_socket.to_path_buf()));
    record_content_on(workspaces, shelf)
}

/// The record-content service of `workspaces`' workspaces over `shelf`.
pub(super) fn record_content_on(
    workspaces: &Arc<ServiceWorkspaces>,
    shelf: Arc<dyn ContentShelf>,
) -> Arc<RouteContent> {
    let content = RouteContent::new(
        Arc::clone(&shelf),
        Arc::new(MemberPeers(Arc::clone(workspaces))),
    );
    workspaces.install_content(&content);
    // The service answers wants from the same store, and hands what a member
    // answers this device to the record content.
    workspaces
        .service()
        .workspaces()
        .serve_content(Arc::new(ShelfContent(shelf)));
    let keeper: Arc<dyn ContentKeeper> = Arc::new(MemberContent(Arc::clone(&content)));
    workspaces.service().workspaces().keep_content(&keeper);
    workspaces.install_keeper(keeper);
    // A want that got no content is asked again once the patience of its
    // last answer passed: a holder that said it keeps none may keep it a
    // moment later, and a large content crosses in windows of one answer.
    let ticking = Arc::clone(&content);
    tilo_rt::app().spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(PIECE_PATIENCE_MS)).await;
            ticking.retry(tilo_clock::unix_now_ms());
        }
    });
    content
}
