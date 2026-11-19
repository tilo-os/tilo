//! The documents of a workspace as this device's own replica holds them, read outside their app:
//! an app's Open of a resource the workspace holds, and an agent's step reading one, answered here
//! when the device's workspace service holds no copy of it.
//!
//! A workspace another device serves arrives in this device's replica as its members write it
//! ([`Engine`]): the layout, an app's documents and the writes that reached this device alone. The
//! service the session reads its own workspaces from keeps them too, and answers for them; a
//! workspace it holds no copy of yet, or a document only the replica has, reads here.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::Holding;
use tilo_workspace_service::apps::services::{
    DocumentBytes, HeldRevision, NativeDocumentRead, NativeDocumentReadContext,
    NativeDocumentReads, ResourceRead,
};

use crate::engine::{Engine, ResourceReading};

/// This device's own replica of the workspaces it joined at the devices serving them.
pub(crate) struct OwnReplica {
    engine: Arc<Engine>,
}

impl OwnReplica {
    pub(crate) fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }

    /// What the replica answers a read of `document` of `app` in `workspace`, holding up to
    /// `holding`.
    fn read(
        &self,
        context: &NativeDocumentReadContext,
        holding: Holding<'_>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        let route = context.route();
        let read = self.engine.read_resource(
            &route.cluster_id,
            (&route.app_id, &route.document_id),
            holding,
            maximum,
        )?;
        Ok(match read {
            ResourceReading::Captured {
                revision,
                heads,
                bytes,
            } => NativeDocumentRead::Captured {
                revision,
                heads,
                body: Box::new(Captured(bytes)),
            },
            ResourceReading::Changes {
                revision,
                heads,
                bytes,
            } => NativeDocumentRead::Changes {
                revision,
                heads,
                base: holding.heads.to_vec(),
                body: Box::new(Captured(bytes)),
            },
            ResourceReading::Unchanged(revision) => NativeDocumentRead::Unchanged(revision),
            ResourceReading::Pending => NativeDocumentRead::Pending,
        })
    }
}

impl NativeDocumentReads for OwnReplica {
    /// A workspace this device's replica holds seals every write of it under the workspace's keys.
    fn seals_documents(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|workspace| self.engine.holds(workspace))
    }

    fn resource_read(&self, workspace: &str, app: &str, resource: &str) -> ResourceRead {
        if WorkspaceId::parse(workspace).is_none()
            || !self.engine.holds_resource(workspace, app, resource)
        {
            return ResourceRead::Own;
        }
        ResourceRead::Sealed
    }

    fn holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.engine.holds_resource(workspace, app, resource)
    }

    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        let route = context.route();
        self.engine.authorizes_read(
            &route.cluster_id,
            context.certificate(),
            &route.app_id,
            &route.document_id,
        )?;
        context.revalidate()
    }

    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        let holding = Holding {
            revision: known_revision,
            heads: &[],
        };
        self.read(context, holding, maximum)
    }

    fn capture_since(
        &self,
        context: &NativeDocumentReadContext,
        held: &HeldRevision,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        let holding = Holding {
            revision: Some(held.revision),
            heads: &held.heads,
        };
        self.read(context, holding, maximum)
    }

    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        let route = context.route();
        Ok(self
            .engine
            .document_revision(&route.cluster_id, &route.app_id, &route.document_id)?
            .map(|revision| Some(revision) == known_revision))
    }

    fn next_change(
        &self,
        context: &NativeDocumentReadContext,
        after: u64,
    ) -> Result<Option<u64>, String> {
        self.engine
            .next_policy_change(&context.route().cluster_id, after)
    }
}

/// The bytes of one bounded capture, held until the broker has sent them.
struct Captured(Vec<u8>);

impl DocumentBytes for Captured {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}
