//! The resources of the workspaces a service holds, as the broker's [`NativeDocumentReads`].
//!
//! An app's Open of a resource the workspace holds, and a session's own reads of one, are answered
//! from the service's copy: the workspace's governor holds each document whole, sealed writes
//! opened with the content keys the device the service is on holds. Every read is decided at the
//! resource's node for the certificate that asks, as a member's read is, and again before every
//! chunk the broker hands out.
//!
//! The service keeps every resource of the workspaces it serves: one it holds nothing of is new
//! here, and nothing is waited for. A chain's head names whichever of the chain's parts is the
//! newest and holds no document of its own: what the service holds of the chain says which.

use futures::executor::block_on;
use tilo_peer_state::PeerDocumentReadError;

use super::collab_app_broker::{
    AwaitingDocument, ChainHeadAnswer, DocumentBytes, NativeDocumentRead,
    NativeDocumentReadContext, NativeDocumentReads, ResourceRead,
};
#[cfg(test)]
use crate::device_documents::on_its_own;
use crate::workspaces::Workspaces;

/// What the workspaces a service holds answer a resource read with.
///
/// A read waits on the service's state from the calling thread, as the broker's read API is
/// synchronous: the service's own tasks must be driven by a runtime with a worker of its own, never
/// by the thread that calls here.
#[derive(Clone)]
pub struct ServiceReads {
    workspaces: Workspaces,
}

impl ServiceReads {
    /// Reads of the resources of the workspaces `workspaces` governs.
    #[must_use]
    pub const fn new(workspaces: Workspaces) -> Self {
        Self { workspaces }
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
        let route = context.route();
        if !matches!(
            neighbour_wire::document_sync(&route.document_id),
            neighbour_wire::DocumentSync::ChainHead { .. }
        ) {
            return None;
        }
        let workspaces = self.workspaces()?;
        Some(
            if workspaces.chain_head_has_part(&route.cluster_id, &route.app_id, &route.document_id)
            {
                ChainHeadAnswer::NewestPartOnItsWay
            } else {
                ChainHeadAnswer::NoPart
            },
        )
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
    ) -> Result<NativeDocumentRead, String> {
        context.revalidate()?;
        let route = context.route();
        let reader = context.certificate().device_public_key;
        let workspaces = self
            .workspaces()
            .ok_or("this device's workspace service holds no workspaces")?;
        let answer = block_on(async {
            let (workspace, app, resource) = (
                route.cluster_id.as_str(),
                route.app_id.as_str(),
                route.document_id.as_str(),
            );
            if !workspaces.holds_resource(workspace, app, resource) {
                return Ok::<NativeDocumentRead, String>(NativeDocumentRead::Absent);
            }
            // A reader that holds a revision reads again only what changed: the capture below
            // says which, cheaper than saving the whole document.
            if let Some(known) = known_revision
                && let Ok(Some(revision)) = workspaces
                    .resource_revision(workspace, (app, resource), reader)
                    .await
                && revision == known
            {
                return Ok(NativeDocumentRead::Unchanged(revision));
            }
            if workspaces.resource_arriving(workspace, app, resource) {
                return Ok(NativeDocumentRead::Pending);
            }
            // On its way (sealed under keys this device has not taken yet, or missing a write
            // it knows of): it reads once that arrives, which wakes the reader.
            let Some(capture) = workspaces
                .capture_resource(workspace, (app, resource), reader, maximum)
                .await
                .map_err(refusal)?
            else {
                return Ok(NativeDocumentRead::Pending);
            };
            Ok(NativeDocumentRead::Captured {
                revision: capture.revision(),
                heads: capture.heads().iter().map(|head| head.0).collect(),
                body: Box::new(Captured(capture.into_bytes())),
            })
        })?;
        context.revalidate()?;
        Ok(answer)
    }

    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        context.revalidate()?;
        let route = context.route();
        let reader = context.certificate().device_public_key;
        let Some(workspaces) = self.workspaces() else {
            return Ok(None);
        };
        if !workspaces.holds_resource(&route.cluster_id, &route.app_id, &route.document_id) {
            // A resource the service holds nothing of is new here: the broker captures.
            return Ok(None);
        }
        let revision = block_on(workspaces.resource_revision(
            &route.cluster_id,
            (&route.app_id, &route.document_id),
            reader,
        ));
        match revision {
            Ok(revision) => Ok(revision.map(|revision| Some(revision) == known_revision)),
            // A read the service cannot decide without capturing: the broker captures instead.
            Err(reason) => {
                tracing::warn!(%reason, "a held revision cannot be decided without a capture");
                Ok(None)
            }
        }
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
