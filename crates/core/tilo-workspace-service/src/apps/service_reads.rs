//! The resources of the workspaces a service holds, as the broker's [`NativeDocumentReads`].
//!
//! An app's Open of a resource the workspace holds, and a session's own reads of one, are answered
//! from the workspace's store, with what the person whose certificate asks may read of it. Every
//! read is decided at the resource's node for that certificate, as a member's read is, and again
//! before every chunk the broker hands out.
//!
//! The service keeps every resource of the workspaces it serves: one it holds nothing of is new
//! here, and nothing is waited for.

use tilo_peer_state::PeerDocumentReadError;

use super::collab_app_broker::{
    AwaitingDocument, ChainHeadAnswer, DocumentBytes, NativeDocumentRead,
    NativeDocumentReadContext, NativeDocumentReads, ReadAnswering, ResourceRead,
};
#[cfg(test)]
use crate::device_documents::on_its_own;
use crate::workspaces::Workspaces;

/// What the workspaces a service holds answer a resource read with.
///
/// A read of a record's document is awaited by whoever asks (the broker's event loop), never
/// waited for on its thread: the service's state may wait on what that same loop carries.
#[derive(Clone)]
pub struct ServiceReads {
    workspaces: Workspaces,
    /// The service's counters: one document read each time a capture opens a record.
    metrics: std::sync::Arc<crate::metrics::Metrics>,
}

impl ServiceReads {
    /// Reads of the resources of the workspaces `workspaces` hosts, counted in `metrics`.
    #[must_use]
    pub const fn new(
        workspaces: Workspaces,
        metrics: std::sync::Arc<crate::metrics::Metrics>,
    ) -> Self {
        Self {
            workspaces,
            metrics,
        }
    }

    /// The resources of the workspaces `service` holds, as the broker answers an app's Open of
    /// one and the session's own reads of one.
    #[cfg(test)]
    pub(crate) fn of(
        service: &crate::service::WorkspaceService,
    ) -> std::sync::Arc<dyn crate::apps::services::NativeDocumentReads> {
        std::sync::Arc::new(Self::new(
            service.workspaces().clone(),
            service.metrics_counter(),
        ))
    }

    /// The workspaces to answer from.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "each read answers as a service that keeps no workspace yet would"
    )]
    fn workspaces(&self) -> Option<Workspaces> {
        Some(self.workspaces.clone())
    }
}

impl NativeDocumentReads for ServiceReads {
    fn seals_documents(&self, workspace: &str) -> bool {
        self.workspaces()
            .is_some_and(|workspaces| workspaces.seals_resources(workspace))
    }

    fn resource_read(&self, workspace: &str, app: &str, resource: &str) -> ResourceRead {
        match self.workspaces() {
            Some(workspaces) if workspaces.holds_resource(workspace, app, resource) => {
                ResourceRead::Sealed
            }
            _ => ResourceRead::Own,
        }
    }

    fn holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.workspaces()
            .is_some_and(|workspaces| workspaces.holds_resource(workspace, app, resource))
    }

    fn await_arriving_document(
        &self,
        _context: &NativeDocumentReadContext,
    ) -> Option<AwaitingDocument> {
        // The service keeps what the workspace retains, and a resource it holds nothing of is one
        // the workspace has none of.
        None
    }

    fn chain_head_answer(&self, context: &NativeDocumentReadContext) -> Option<ChainHeadAnswer> {
        // A store keeps a record whole, in no parts: a chain's head names none here.
        matches!(
            neighbour_wire::document_sync(&context.route().document_id),
            neighbour_wire::DocumentSync::ChainHead { .. }
        )
        .then_some(ChainHeadAnswer::NoPart)
    }

    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        let route = context.route();
        let workspaces = self
            .workspaces()
            .ok_or("this device's workspace service holds no workspaces")?;
        workspaces.authorizes_resource_read(
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
    ) -> ReadAnswering<'_, NativeDocumentRead> {
        let context = context.clone();
        Box::pin(async move { self.captured(&context, known_revision, maximum).await })
    }

    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
    ) -> ReadAnswering<'_, Option<bool>> {
        let context = context.clone();
        Box::pin(async move { self.held_unchanged(&context, known_revision).await })
    }

    fn next_change(
        &self,
        _context: &NativeDocumentReadContext,
        _after: u64,
    ) -> Result<Option<u64>, String> {
        // The service holds every record its workspaces have whole, so what it answers changes by
        // what is written to a record — announced — or by this device's access to it and to its
        // keys, which is announced too. Time alone changes nothing here: a grant and a certificate
        // ending are the workspace's and the reader's, which the broker registers with the policy
        // clock itself. A held read on one of these records therefore waits for a signal rather
        // than re-checking on a clock.
        Ok(None)
    }
}

impl ServiceReads {
    /// [`NativeDocumentReads::capture`].
    async fn captured(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        context.revalidate_read().await?;
        let route = context.route();
        let reader = hex::encode(context.certificate().user_public_key);
        let workspaces = self
            .workspaces()
            .ok_or("this device's workspace service holds no workspaces")?;
        let answer = async {
            let (workspace, app, resource) = (
                route.cluster_id.as_str(),
                route.app_id.as_str(),
                route.document_id.as_str(),
            );
            if !workspaces.holds_resource(workspace, app, resource) {
                return Ok::<NativeDocumentRead, String>(NativeDocumentRead::Absent);
            }
            // Decided at the resource's node for the certificate that asks, as every read is.
            workspaces.authorizes_resource_read(workspace, context.certificate(), app, resource)?;
            // One read of one record's document, whether it answers the whole record, the part
            // past the revision the reader held, or that nothing changed since it.
            self.metrics.inc_documents_read();
            // A reader that holds a revision reads again only what changed: the capture below
            // says which, cheaper than saving the whole document.
            if let Some(known) = known_revision
                && let Ok(Some(revision)) =
                    workspaces.resource_revision(workspace, (app, resource), &reader)
                && revision == known
            {
                return Ok(NativeDocumentRead::Unchanged(revision));
            }
            let Some(capture) = workspaces
                .capture_resource(workspace, (app, resource), &reader, maximum)
                .map_err(refusal)?
            else {
                return Ok(NativeDocumentRead::Absent);
            };
            Ok(NativeDocumentRead::Captured {
                revision: capture.revision,
                heads: capture.heads.iter().map(|head| head.0).collect(),
                body: Box::new(Captured(capture.bytes)),
            })
        }
        .await?;
        context.revalidate_read().await?;
        Ok(answer)
    }

    /// [`NativeDocumentReads::unchanged`].
    async fn held_unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        context.revalidate_read().await?;
        let route = context.route();
        let reader = hex::encode(context.certificate().user_public_key);
        let Some(workspaces) = self.workspaces() else {
            return Ok(None);
        };
        if !workspaces.holds_resource(&route.cluster_id, &route.app_id, &route.document_id) {
            // A resource the service holds nothing of is new here: the broker captures.
            return Ok(None);
        }
        workspaces.authorizes_resource_read(
            &route.cluster_id,
            context.certificate(),
            &route.app_id,
            &route.document_id,
        )?;
        let revision = workspaces.resource_revision(
            &route.cluster_id,
            (&route.app_id, &route.document_id),
            &reader,
        );
        match revision {
            Ok(revision) => Ok(revision.map(|revision| Some(revision) == known_revision)),
            // A read the service cannot decide without capturing: the broker captures instead.
            Err(reason) => {
                tracing::warn!(%reason, "a held revision cannot be decided without a capture");
                Ok(None)
            }
        }
    }
}

/// What a refused capture says to the broker.
fn refusal(error: PeerDocumentReadError) -> String {
    match error {
        PeerDocumentReadError::Membership(why) => why.to_string(),
        other => other.to_string(),
    }
}

/// The bytes of one bounded capture, held until the broker has sent them.
struct Captured(Vec<u8>);

impl DocumentBytes for Captured {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

pub use crate::device_documents::ServiceDocuments;

#[cfg(test)]
#[path = "service_reads/tests.rs"]
mod tests;
