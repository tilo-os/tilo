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
use crate::service::ServiceSlot;
use crate::workspace_documents::{DeviceDocuments, DocumentRead};
use crate::workspaces::Workspaces;

/// The workspaces a service holds, as their resources are read: the service itself, or where its
/// embedder installs it once it opens.
#[derive(Clone)]
enum Held {
    Service(Workspaces),
    Unopened(ServiceSlot),
}

/// What the workspaces a service holds answer a resource read with.
///
/// A read waits on the service's state from the calling thread, as the broker's read API is
/// synchronous: the service's own tasks must be driven by a runtime with a worker of its own, never
/// by the thread that calls here.
///
/// A workspace the service holds no copy of is answered by `fallback`, when there is one: a device
/// that joined a workspace another device serves keeps that workspace in its own replica, and the
/// service it governs its own workspaces on holds none of it.
#[derive(Clone)]
pub struct ServiceReads {
    held: Held,
    fallback: Option<std::sync::Arc<dyn NativeDocumentReads>>,
}

impl ServiceReads {
    /// Reads of the resources of the workspaces `workspaces` governs.
    #[must_use]
    pub const fn new(workspaces: Workspaces) -> Self {
        Self {
            held: Held::Service(workspaces),
            fallback: None,
        }
    }

    /// Reads of the resources of the workspaces held by the service whose embedder holds
    /// `service`, from the moment it is: a session reads a workspace's resources here without
    /// reaching the service's own thread.
    #[must_use]
    pub const fn on(service: ServiceSlot) -> Self {
        Self {
            held: Held::Unopened(service),
            fallback: None,
        }
    }

    /// The reads that answer for a workspace this service holds no copy of: the device's own
    /// replica of the workspaces another device serves.
    #[must_use]
    pub fn answered_by(self, fallback: std::sync::Arc<dyn NativeDocumentReads>) -> Self {
        Self {
            fallback: Some(fallback),
            ..self
        }
    }

    /// The workspaces to answer from, once a service holds any.
    fn workspaces(&self) -> Option<Workspaces> {
        match &self.held {
            Held::Service(workspaces) => Some(workspaces.clone()),
            Held::Unopened(service) => Some(service.service()?.workspaces().clone()),
        }
    }

    /// Whether this service holds anything of `workspace`: the one it governs, or a member copy.
    fn holds_workspace(&self, workspace: &str) -> bool {
        self.workspaces()
            .is_some_and(|workspaces| block_on(workspaces.holds_workspace(workspace)))
    }

    /// Whether this service holds `app`'s `resource` in a workspace it holds.
    fn service_holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.workspaces()
            .is_some_and(|workspaces| block_on(workspaces.holds_resource(workspace, app, resource)))
    }

    /// The reads for `workspace` when this service holds no copy of it, or `None` when it does or
    /// nothing else answers.
    fn elsewhere(&self, workspace: &str) -> Option<&std::sync::Arc<dyn NativeDocumentReads>> {
        if self.holds_workspace(workspace) {
            return None;
        }
        self.fallback.as_ref()
    }

    /// The reads for `app`'s `resource` in `workspace`: the service's when it holds the workspace
    /// and anything of the resource, and the device's own replica for a workspace it holds none of,
    /// or a resource only that replica holds.
    fn resource_elsewhere(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> Option<&std::sync::Arc<dyn NativeDocumentReads>> {
        let fallback = self.fallback.as_ref()?;
        if !self.holds_workspace(workspace) {
            return Some(fallback);
        }
        if self.service_holds_resource(workspace, app, resource) {
            return None;
        }
        fallback
            .holds_resource(workspace, app, resource)
            .then_some(fallback)
    }
}

impl NativeDocumentReads for ServiceReads {
    fn seals_documents(&self, workspace: &str) -> bool {
        if let Some(elsewhere) = self.elsewhere(workspace) {
            return elsewhere.seals_documents(workspace);
        }
        self.workspaces()
            .is_some_and(|workspaces| block_on(workspaces.seals_resources(workspace)))
    }

    fn own_resource(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> Option<collab_types::OwnResource> {
        if let Some(elsewhere) = self.resource_elsewhere(workspace, app, resource) {
            return elsewhere.own_resource(workspace, app, resource);
        }
        block_on(self.workspaces()?.own_resource(workspace, app, resource))
    }

    fn resource_read(&self, workspace: &str, app: &str, resource: &str) -> ResourceRead {
        if let Some(elsewhere) = self.resource_elsewhere(workspace, app, resource) {
            return elsewhere.resource_read(workspace, app, resource);
        }
        match self.workspaces() {
            Some(workspaces) if block_on(workspaces.holds_resource(workspace, app, resource)) => {
                ResourceRead::Sealed
            }
            _ => ResourceRead::Own,
        }
    }

    fn holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        if let Some(elsewhere) = self.resource_elsewhere(workspace, app, resource) {
            return elsewhere.holds_resource(workspace, app, resource);
        }
        self.workspaces()
            .is_some_and(|workspaces| block_on(workspaces.holds_resource(workspace, app, resource)))
    }

    fn await_arriving_document(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Option<AwaitingDocument> {
        let route = context.route();
        if let Some(elsewhere) =
            self.resource_elsewhere(&route.cluster_id, &route.app_id, &route.document_id)
        {
            return elsewhere.await_arriving_document(context);
        }
        // The service keeps what the workspace retains, and a resource it holds nothing of is one
        // the workspace has none of.
        None
    }

    fn chain_head_answer(&self, context: &NativeDocumentReadContext) -> Option<ChainHeadAnswer> {
        let route = context.route();
        if let Some(elsewhere) =
            self.resource_elsewhere(&route.cluster_id, &route.app_id, &route.document_id)
        {
            return elsewhere.chain_head_answer(context);
        }
        if !matches!(
            neighbour_wire::document_sync(&route.document_id),
            neighbour_wire::DocumentSync::ChainHead { .. }
        ) {
            return None;
        }
        let workspaces = self.workspaces()?;
        Some(
            if block_on(workspaces.chain_head_has_part(
                &route.cluster_id,
                &route.app_id,
                &route.document_id,
            )) {
                ChainHeadAnswer::NewestPartOnItsWay
            } else {
                ChainHeadAnswer::NoPart
            },
        )
    }

    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        let route = context.route();
        if let Some(elsewhere) =
            self.resource_elsewhere(&route.cluster_id, &route.app_id, &route.document_id)
        {
            return elsewhere.authorize(context);
        }
        let workspaces = self
            .workspaces()
            .ok_or("this device's workspace service holds no workspaces")?;
        block_on(workspaces.authorizes_resource_read(
            &route.cluster_id,
            context.certificate(),
            &route.app_id,
            &route.document_id,
        ))?;
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
        if let Some(elsewhere) =
            self.resource_elsewhere(&route.cluster_id, &route.app_id, &route.document_id)
        {
            return elsewhere.capture(context, known_revision, maximum);
        }
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
            if !workspaces.holds_resource(workspace, app, resource).await {
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
            if workspaces.resource_arriving(workspace, app, resource).await {
                return Ok(NativeDocumentRead::Pending);
            }
            let capture = workspaces
                .capture_resource(workspace, (app, resource), reader, maximum)
                .await
                .map_err(refusal)?;
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
        if let Some(elsewhere) =
            self.resource_elsewhere(&route.cluster_id, &route.app_id, &route.document_id)
        {
            return elsewhere.unchanged(context, known_revision);
        }
        let reader = context.certificate().device_public_key;
        let Some(workspaces) = self.workspaces() else {
            return Ok(None);
        };
        if !block_on(workspaces.holds_resource(
            &route.cluster_id,
            &route.app_id,
            &route.document_id,
        )) {
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

/// The most bytes of one document a session's own read hands out, as the desktop engine it
/// replaces did.
const MAX_DEVICE_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// Wait for `future` with this thread, or on a thread of its own where this one already drives a
/// runtime: what a read that must not jam its caller's executor waits with.
fn on_its_own<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_err() {
        return block_on(future);
    }
    std::thread::scope(|scope| match scope.spawn(|| block_on(future)).join() {
        Ok(output) => output,
        Err(panic) => std::panic::resume_unwind(panic),
    })
}

/// [`DeviceDocuments`] over the service's own copy of the workspaces it holds, resolved from
/// [`ServiceSlot`] on every call: what this session's own roles read on this device.
#[derive(Clone)]
pub struct ServiceDocuments(ServiceSlot);

impl ServiceDocuments {
    /// Reads of the documents of the workspaces held by the service whose embedder holds
    /// `service`, from the moment it is.
    #[must_use]
    pub const fn on(service: ServiceSlot) -> Self {
        Self(service)
    }

    /// The workspaces to answer from, once a service holds any.
    fn copies(&self) -> Option<Workspaces> {
        Some(self.0.service()?.workspaces().clone())
    }
}

impl DeviceDocuments for ServiceDocuments {
    fn read(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<DocumentRead, String> {
        let workspaces = self
            .copies()
            .ok_or("this device's workspace service holds no workspaces")?;
        let workspace = workspace.to_owned();
        let app = app.to_owned();
        let resource = resource.to_owned();
        let since = since.map(<[[u8; 32]]>::to_vec);
        on_its_own(async move {
            workspaces
                .own_read(
                    &workspace,
                    (&app, &resource),
                    since.as_deref().unwrap_or_default(),
                    MAX_DEVICE_DOCUMENT_BYTES,
                )
                .await
        })
    }

    fn listing(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let workspaces = self
            .copies()
            .ok_or("this device's workspace service holds no workspaces")?;
        let workspace = workspace.to_owned();
        on_its_own(async move { workspaces.own_listing(&workspace).await })
    }

    fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool {
        let Some(workspaces) = self.copies() else {
            return false;
        };
        let (workspace, app, resource) =
            (workspace.to_owned(), app.to_owned(), resource.to_owned());
        on_its_own(async move { workspaces.holds_resource(&workspace, &app, &resource).await })
    }

    fn workspaces(&self) -> Vec<String> {
        let Some(workspaces) = self.copies() else {
            return Vec::new();
        };
        on_its_own(async move { workspaces.held_workspaces().await })
    }
}

#[cfg(test)]
#[path = "service_reads/tests.rs"]
mod tests;
