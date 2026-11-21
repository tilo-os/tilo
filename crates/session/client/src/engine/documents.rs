//! What a device whose engine holds its workspaces' records in its own replica reads and writes of
//! them: the page and the sessions a device runs without a workspace service of its own, and the
//! command line's peer. A desktop session reads and writes its workspaces' records through the
//! device's workspace service instead ([`tilo_workspace_service::documents::HeldDocuments`]).
//!
//! The engine's own document calls are the command line's: it mirrors a workspace into a folder
//! and reads what changed since the heads it holds. Everything else reaches the replica through
//! this holder, as a page does ([`crate::Link::documents`]).

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::{Holding, LocalWrite, WriteRefusal, revision_of_heads};
use tilo_peer_state::workspace_documents::{
    DeviceDocuments, DeviceWrites, DocumentRead, DocumentWrite,
};

use super::{Engine, MAX_DOCUMENT_BYTES, ResourceReading};

impl Engine {
    /// This device's documents, read and written through the replica this engine holds, as the
    /// page holds them: each write is drafted, sealed and signed into the replica as the device
    /// its identity certifies, and a reader reads what the replica admitted.
    #[must_use]
    pub fn documents(self: &Arc<Self>) -> Arc<dyn DeviceDocuments> {
        Arc::new(EngineDocuments(Arc::clone(self)))
    }

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

/// A device's documents held and written through the replica its engine keeps.
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
            Some(self.0.owner().clone()),
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

impl DeviceDocuments for EngineDocuments {
    fn read(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<DocumentRead, String> {
        if !WorkspaceId::parse(workspace).is_some_and(|id| self.0.holds(id)) {
            return Ok(DocumentRead::Absent);
        }
        let heads = since.unwrap_or_default();
        let holding = Holding {
            revision: (!heads.is_empty()).then(|| revision_of_heads(heads)),
            heads,
        };
        Ok(
            match self.0.read_resource(
                workspace,
                (app, resource),
                holding,
                MAX_DOCUMENT_BYTES,
            )? {
                ResourceReading::Captured {
                    revision,
                    heads,
                    bytes,
                } => DocumentRead::Whole {
                    revision,
                    heads,
                    bytes,
                },
                ResourceReading::Changes {
                    revision,
                    heads,
                    bytes,
                } => DocumentRead::Changes {
                    revision,
                    heads,
                    bytes,
                },
                ResourceReading::Unchanged(revision) => DocumentRead::Unchanged(revision),
                ResourceReading::Pending => DocumentRead::Pending,
            },
        )
    }

    fn listing(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let id = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let own = self.0.identity().device_cert.device_public_key;
        let now = tilo_clock::wall();
        self.0.owner().with_current(id, |view| {
            let reader = super::Admission::of(view, own).reader(own);
            let mut documents = view.peer().records();
            documents.retain(|(app, document_id)| {
                view.peer()
                    .check_document_read(reader, app, document_id, now)
                    .is_ok()
            });
            Ok(documents)
        })
    }

    fn own_records(
        &self,
        workspace: &str,
    ) -> std::collections::BTreeMap<(String, String), [u8; 32]> {
        WorkspaceId::parse(workspace)
            .and_then(|id| {
                self.0
                    .owner()
                    .with_current(id, |view| Ok(view.peer().own_resources()))
                    .ok()
            })
            .unwrap_or_default()
    }

    fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.0.holds_resource(workspace, app, resource)
    }

    fn workspaces(&self) -> Vec<String> {
        self.0.workspaces().iter().map(ToString::to_string).collect()
    }
}
