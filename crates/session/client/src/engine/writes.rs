//! What a device whose engine holds its workspaces' records in its own replica writes of them:
//! the page and the sessions a device runs without a workspace service of its own, and the
//! command line's peer. A desktop session writes its workspaces' records through the device's
//! workspace service instead ([`tilo_workspace_service::documents::HeldDocuments`]).

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
        Arc::new(EngineDocuments(Arc::clone(self)))
    }
}

/// A device's documents written through the replica its engine holds.
struct EngineDocuments(Arc<Engine>);

impl DeviceWrites for EngineDocuments {
    fn write(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        readers: Option<&str>,
        changes: &[u8],
        founding: bool,
        snapshot: bool,
        sealed: Option<&key_group::ContentKeys>,
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
            identity.device_cert.device_public_key,
            Arc::clone(&identity.signer),
            workspace,
            LocalWrite {
                app,
                document,
                readers,
                changes,
                founding,
                snapshot,
                sealed,
                now: crate::clock::system_time(tilo_clock::unix_now_ns()),
            },
        )
    }
}
