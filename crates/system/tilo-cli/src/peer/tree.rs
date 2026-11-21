//! A workspace's file tree on this computer: the Files app's records, held through the
//! device's session as every device of the workspace holds them.
//!
//! The records of a workspace's tree ([`fs_protocol::tree`]) and this computer's
//! [`tilo_fs::Namespace`] of it are kept converged by [`tilo_fs::tree_sync`], which knows the
//! order a tree goes out in, its pacing and what it does when the host refuses a change. This
//! module wires that sync to this device's session with the workspace's host — the one a
//! browser tab keeps — by the calls this device reads and writes a record through: the
//! session's events, its replica's reads and its outbox's writes.

use std::path::Path;
use std::sync::Arc;

use collab_types::DocumentMutation;
use fs_protocol::TreeChunk;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DocumentRead};
use tilo_server_protocol::{DocumentAnswer, WorkspaceId};
use tilo_session_client::{Cmd, DocumentRoute, Event, Events, Link, Outbox as _};

use super::content::SessionContent;

/// The app whose record a workspace's tree is.
pub(crate) use fs_protocol::tree::FILES_APP;

/// What `tilo status` says of one workspace's tree.
pub(crate) use tilo_fs::tree_sync::Status as TreeStatus;
/// One workspace's tree, held while this value lives.
pub(crate) type TreeReplica = tilo_fs::tree_sync::TreeSync;

/// The route of record `chunk` of `workspace`'s tree: the tree's own record for 0.
fn route(workspace: WorkspaceId, chunk: TreeChunk) -> DocumentRoute {
    DocumentRoute {
        app_id: FILES_APP.to_owned(),
        cluster_id: workspace.to_string(),
        document: tilo_fs::tree_sync::document_for(chunk),
    }
}

/// Hold `workspace`'s tree over `link`, kept in `state` between runs, as the device whose key
/// is `device`; its file bytes come and go through `content`.
///
/// # Errors
/// The kept namespace cannot be read.
pub(crate) async fn start(
    link: Link,
    workspace: WorkspaceId,
    state: &Path,
    device: [u8; 32],
    person: [u8; 32],
    content: Arc<SessionContent>,
) -> std::io::Result<TreeReplica> {
    let name = workspace.to_string();
    let documents = Arc::new(SessionRecords(link.engine().documents()));
    let peer = Arc::new(SessionPeer {
        link: link.clone(),
        workspace,
        content: Arc::clone(&content),
    });
    let (wakes, wake_events) = tokio::sync::mpsc::unbounded_channel();
    let replica = TreeReplica::start(tilo_fs::tree_sync::Config {
        workspace: name,
        device,
        person,
        kept_at: state.join("tree"),
        documents,
        writer: Box::new(SessionWriter {
            link: link.clone(),
            workspace,
        }),
        peer,
        content: content.store(),
        wakes: wake_events,
    })
    .await?;
    // The session's news reaches the sync as wakes: what landed, what the host answered, what
    // it refused, and the content it sent this device.
    let wake = replica.wake();
    let notify = Arc::clone(&wake);
    link.on_change(Arc::new(move || notify.notify_waiters()));
    tokio::spawn(forward(
        link.events_where(takes),
        wake,
        wakes,
        Arc::clone(&content),
        workspace,
    ));
    Ok(replica)
}

/// This device's session as the reader of a workspace's tree records: the engine's replica,
/// which holds them as every device of a workspace that keeps no service of its own does.
struct SessionRecords(Arc<dyn DeviceDocuments>);

impl tilo_fs::tree_sync::Records for SessionRecords {
    fn read(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<tilo_fs::tree_sync::Reading, String> {
        self.0
            .read(workspace, app, document, since)
            .map(|read| match read {
                DocumentRead::Whole { heads, bytes, .. } => {
                    tilo_fs::tree_sync::Reading::Whole { heads, bytes }
                }
                DocumentRead::Changes { heads, bytes, .. } => {
                    tilo_fs::tree_sync::Reading::Changes { heads, bytes }
                }
                DocumentRead::Unchanged(_) => tilo_fs::tree_sync::Reading::Unchanged,
                DocumentRead::Absent => tilo_fs::tree_sync::Reading::Absent,
                DocumentRead::Pending => tilo_fs::tree_sync::Reading::Pending,
            })
    }
}

/// This device's session as the one a workspace's tree is read and written through: the
/// replica reads its records ([`DeviceDocuments`]), and its outbox writes them, as this device.
struct SessionPeer {
    link: Link,
    workspace: WorkspaceId,
    content: Arc<SessionContent>,
}

impl tilo_fs::tree_sync::TreePeer for SessionPeer {
    fn open(&self, chunk: TreeChunk) {
        let route = route(self.workspace, chunk);
        let _ = self.link.send(Cmd::OpenDocumentAt(self.workspace, route.clone()));
        let _ = self.link.send(Cmd::OpenRoute(route.clone()));
        // And on the workspace's live session, whose host answers what it holds.
        let _ = self.link.engine().open_resource(
            &self.workspace.to_string(),
            &route.app_id,
            &route.document,
        );
    }

    fn digest(&self, chunk: TreeChunk) -> Option<[u8; 32]> {
        self.link.engine().admitted_digest(
            &self.workspace.to_string(),
            FILES_APP,
            &tilo_fs::tree_sync::document_for(chunk),
        )
    }

    fn answer(&self, chunk: TreeChunk) -> Option<DocumentAnswer> {
        self.link.engine().resource_answer(
            &self.workspace.to_string(),
            FILES_APP,
            &tilo_fs::tree_sync::document_for(chunk),
        )
    }

    fn holds(&self, chunk: TreeChunk) -> bool {
        self.link.engine().holds_resource(
            &self.workspace.to_string(),
            FILES_APP,
            &tilo_fs::tree_sync::document_for(chunk),
        )
    }

    fn holding(&self, chunk: TreeChunk) -> bool {
        self.link.holding(&route(self.workspace, chunk))
    }

    fn pending(&self) -> usize {
        self.link.outgoing_pending()
    }

    fn reask_content(&self) {
        self.content.reask(&self.link);
    }
}

/// This device's session as the writer of a workspace's tree: a record goes out as this
/// device's own write, and is durable once the workspace's host answered it.
struct SessionWriter {
    link: Link,
    workspace: WorkspaceId,
}

impl tilo_fs::tree_sync::TreeWriter for SessionWriter {
    fn write<'a>(
        &'a mut self,
        chunk: TreeChunk,
        changes: &'a [u8],
        _founding: bool,
        snapshot: bool,
    ) -> tilo_fs::tree_sync::WriteFuture<'a> {
        let mutation = if snapshot {
            DocumentMutation::Snapshot(changes.to_vec())
        } else {
            DocumentMutation::Incremental(changes.to_vec())
        };
        let written = self.link.write(route(self.workspace, chunk), &mutation);
        Box::pin(std::future::ready(written))
    }

    fn served(&self, chunks: &[TreeChunk]) -> tilo_fs::tree_sync::Served {
        let routes: Vec<DocumentRoute> = chunks
            .iter()
            .map(|chunk| route(self.workspace, *chunk))
            .collect();
        match self.link.barrier_for(&routes) {
            Ok(receipt) => Box::pin(async move {
                match receipt.await {
                    Ok(answer) => answer,
                    Err(_) => Err("the session closed before the workspace answered".to_owned()),
                }
            }),
            Err(closed) => Box::pin(std::future::ready(Err(closed.to_string()))),
        }
    }
}

/// What the sync waits on: the same events a session client follows.
fn takes(event: &Event) -> bool {
    matches!(
        event,
        Event::Landed { .. }
            | Event::DocumentOpened { .. }
            | Event::Refused { .. }
            | Event::DocumentContent { .. }
            | Event::HostReady { .. }
    )
}

/// Carry the session's events to the sync as wakes, and the content it sent this device to the
/// store the tree's files are read from.
async fn forward(
    events: Events,
    wake: Arc<tokio::sync::Notify>,
    wakes: tokio::sync::mpsc::UnboundedSender<tilo_fs::tree_sync::Wake>,
    content: Arc<SessionContent>,
    workspace: WorkspaceId,
) {
    let mut look = tokio::time::interval(tilo_fs::tree_sync::LOOK_AGAIN);
    loop {
        tokio::select! {
            () = wake.notified() => {}
            _ = look.tick() => {}
        }
        let mut told = Vec::new();
        for event in events.drain() {
            if let Some(wake) = wake_of(event, workspace, &content) {
                told.push(wake);
            }
        }
        // A change of the session's own, with no event of the tree's: the sync looks again.
        if told.is_empty() {
            told.push(tilo_fs::tree_sync::Wake::Moved);
        }
        for wake in told {
            if wakes.send(wake).is_err() {
                // The sync is gone; nothing waits for the session's news any more.
                return;
            }
        }
    }
}

/// What `event` means to the tree of `workspace`, when it means anything to it.
fn wake_of(
    event: Event,
    workspace: WorkspaceId,
    content: &SessionContent,
) -> Option<tilo_fs::tree_sync::Wake> {
    let name = workspace.to_string();
    let here = |route: &DocumentRoute| route.cluster_id == name && route.app_id == FILES_APP;
    match event {
        Event::HostReady { ws, .. } if ws == workspace => {
            Some(tilo_fs::tree_sync::Wake::HostReady)
        }
        Event::Refused { route, refusal, .. } if here(&route) => {
            fs_protocol::tree::chunk_of_record(&route.document).map(|chunk| {
                tilo_fs::tree_sync::Wake::Refused {
                    chunk,
                    reason: refusal.to_string(),
                }
            })
        }
        Event::Landed { route, .. } if here(&route) => {
            fs_protocol::tree::chunk_of_record(&route.document)
                .map(tilo_fs::tree_sync::Wake::Landed)
        }
        Event::DocumentOpened {
            route: Some(route),
            answer,
            ..
        } if here(&route) => fs_protocol::tree::chunk_of_record(&route.document)
            .map(|chunk| tilo_fs::tree_sync::Wake::Opened { chunk, answer }),
        Event::DocumentContent { ws, frame, .. } if ws == workspace => {
            content.piece(&frame);
            Some(tilo_fs::tree_sync::Wake::Moved)
        }
        _ => None,
    }
}
