//! What a device whose engine holds its workspaces' records in its own replica writes of them:
//! each write drafted, sealed and signed into the replica as the device its identity certifies.
//! Reads are not the engine's: a page reads its workspace through the workspace service's document
//! core over the same replica (`tilo_workspace_service::documents::HeldDocuments`), and a desktop
//! session through the device's workspace service.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::{LocalWrite, WriteRefusal};
use tilo_peer_state::workspace_documents::{DeviceWrites, DocumentWrite};

use super::Engine;

impl Engine {
    /// This device's documents, written as the device its identity certifies: each write is
    /// drafted, sealed and signed into the replica this engine owns, and whoever holds this can
    /// publish it as it was signed ([`crate::Outbox::publish_signed`]).
    ///
    /// A write the engine holds no replica of `workspace` for is refused, as is one whose
    /// workspace has not admitted what it needs here yet ([`WriteRefusal::Waiting`]): the caller
    /// keeps it and writes it again once it can.
    #[must_use]
    pub fn document_writes(self: &Arc<Self>) -> Arc<dyn DeviceWrites> {
        Arc::new(EngineWrites(Arc::clone(self)))
    }
}

/// A device's documents written through the replica its engine keeps.
struct EngineWrites(Arc<Engine>);

impl DeviceWrites for EngineWrites {
    fn write(
        &self,
        workspace: &str,
        changes: &[u8],
        writing: tilo_peer_state::workspace_documents::DocumentWriting<'_>,
    ) -> DocumentWrite {
        let Some(workspace_id) = WorkspaceId::parse(workspace) else {
            let named = workspace.to_owned();
            return Box::pin(async move {
                Err(WriteRefusal::Refused(format!("{named} is not a workspace")))
            });
        };
        let Ok(replica) = self.0.owner().replica(workspace_id) else {
            let named = workspace.to_owned();
            return Box::pin(async move {
                Err(WriteRefusal::Refused(format!(
                    "this device holds no workspace {named}"
                )))
            });
        };
        let identity = self.0.identity();
        tilo_peer_state::owner::admit_local_write(
            replica,
            Some(self.0.owner().clone()),
            identity.device_cert.device_public_key,
            Arc::clone(&identity.signer),
            workspace,
            LocalWrite {
                app: writing.app,
                document: writing.document,
                readers: writing.readers,
                changes,
                founding: writing.founding,
                snapshot: writing.snapshot,
                sealed: writing.sealed,
                commit: None,
                now: crate::clock::system_time(tilo_clock::unix_now_ns()),
            },
        )
    }
}

/// `engine`'s replica of `workspace` as a page holds it: the workspace service's document core,
/// read as the device the replica is held as (the device a copied machine was copied from, until
/// the workspace admits it), with the device's keys as its own workspace service asks them.
/// `None` while the engine holds no such workspace.
#[cfg(test)]
pub(crate) fn page_holder(
    engine: &Arc<Engine>,
    workspace: WorkspaceId,
) -> Option<tilo_workspace_service::documents::HeldDocuments> {
    let replica = engine.owner().replica(workspace).ok()?;
    let reader = engine
        .owner()
        .with_current(workspace, |view| Ok(view.device().device_public_key))
        .ok()?;
    let handed = Arc::downgrade(engine);
    let behind = Arc::downgrade(engine);
    let keys = tilo_workspace_service::own_device::key_groups(
        engine.keys(),
        Some(Box::new(move |workspace| {
            WorkspaceId::parse(workspace).and_then(|id| handed.upgrade()?.handed_keys(id))
        })),
        None,
        Some(Arc::new(move |group, held| {
            if let Some(link) = behind.upgrade().and_then(|engine| engine.link()) {
                link.want_frames(group, held);
            }
        })),
    )
    .keys;
    Some(tilo_workspace_service::documents::HeldDocuments::over(
        &workspace.to_string(),
        replica,
        reader,
        None,
        Some(keys),
    ))
}
