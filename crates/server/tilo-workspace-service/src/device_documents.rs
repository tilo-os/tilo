//! A device's own documents, as its workspace service holds them.
//!
//! The service answers the reads and writes its own roles make of the workspaces it holds:
//! the ones it governs and the member copies it joined. A read is decided for the device the
//! service acts as, and a write is signed and sealed as that device, then committed through
//! the service's one commit path, so it is kept, carried and published as any member's write
//! is.
//!
//! Synchronous, as those roles read and write: the service's tasks must be driven by a
//! runtime with a worker of its own, never by the thread that calls here.

use futures::executor::block_on;

use crate::service::ServiceSlot;
use crate::workspace_documents::{DeviceDocuments, DeviceWrites, DocumentRead, DocumentWrite};
use crate::workspaces::Workspaces;

/// The most bytes of one document a session's own read hands out.
const MAX_DEVICE_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// The workspaces a service holds, as its own roles read and write them: the service itself,
/// or where its embedder installs it once it opens.
#[derive(Clone)]
enum Held {
    Service(Workspaces),
    Unopened(ServiceSlot),
}

impl Held {
    fn copies(&self) -> Option<Workspaces> {
        match self {
            Self::Service(workspaces) => Some(workspaces.clone()),
            Self::Unopened(unopened) => Some(unopened.service()?.workspaces().clone()),
        }
    }
}

/// Wait for `future` with this thread, or on a thread of its own where this one already drives a
/// runtime: what a read that must not jam its caller's executor waits with.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn on_its_own<F>(future: F) -> F::Output
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

/// Wait for `future` with this thread: a page drives no runtime beside it.
#[cfg(target_arch = "wasm32")]
pub(crate) fn on_its_own<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    block_on(future)
}

/// [`DeviceDocuments`] over the service's own copy of the workspaces it holds: what a device's
/// own roles read.
#[derive(Clone)]
pub struct ServiceDocuments(Held);

impl ServiceDocuments {
    /// Reads of the documents of the workspaces held by the service whose embedder holds
    /// `service`, from the moment it is.
    #[must_use]
    pub const fn on(service: ServiceSlot) -> Self {
        Self(Held::Unopened(service))
    }

    /// Reads of the documents of the workspaces `workspaces` holds already, for an embedder
    /// that holds them itself: the code that serves them in this same process.
    #[must_use]
    pub const fn of(workspaces: Workspaces) -> Self {
        Self(Held::Service(workspaces))
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
            .0
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
            .0
            .copies()
            .ok_or("this device's workspace service holds no workspaces")?;
        let workspace = workspace.to_owned();
        on_its_own(async move { workspaces.own_listing(&workspace).await })
    }

    fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool {
        let Some(workspaces) = self.0.copies() else {
            return false;
        };
        let (workspace, app, resource) =
            (workspace.to_owned(), app.to_owned(), resource.to_owned());
        on_its_own(async move { workspaces.holds_resource(&workspace, &app, &resource).await })
    }

    fn workspaces(&self) -> Vec<String> {
        let Some(workspaces) = self.0.copies() else {
            return Vec::new();
        };
        on_its_own(async move { workspaces.held_workspaces().await })
    }
}

/// [`DeviceWrites`] over the service's own copy of the workspaces it holds: what a device's own
/// roles write.
#[derive(Clone)]
pub struct ServiceWrites(Held);

impl ServiceWrites {
    /// Writes into the documents of the workspaces held by the service whose embedder holds
    /// `service`, from the moment it is.
    #[must_use]
    pub const fn on(service: ServiceSlot) -> Self {
        Self(Held::Unopened(service))
    }

    /// Writes into the documents of the workspaces `workspaces` holds already, for an embedder
    /// that holds them itself: the code that serves them in this same process.
    #[must_use]
    pub const fn of(workspaces: Workspaces) -> Self {
        Self(Held::Service(workspaces))
    }
}

impl DeviceWrites for ServiceWrites {
    fn write(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        changes: &[u8],
        founding: bool,
        sealed: Option<&key_group::ContentKeys>,
    ) -> DocumentWrite {
        let workspaces = self.0.copies();
        let workspace = workspace.to_owned();
        let app = app.to_owned();
        let document = document.to_owned();
        let changes = changes.to_vec();
        let sealed = sealed.cloned();
        Box::pin(async move {
            let workspaces = workspaces.ok_or_else(|| {
                tilo_peer_state::owner::WriteRefusal::Refused(
                    "this device's workspace service holds no workspaces".to_owned(),
                )
            })?;
            workspaces
                .own_write(
                    &workspace,
                    (&app, &document),
                    &changes,
                    founding,
                    sealed.as_ref(),
                )
                .await
        })
    }
}
