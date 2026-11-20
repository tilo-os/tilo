//! The documents of the workspaces a service holds, written as the device it serves.
//!
//! [`DeviceWrites`] over the service's own copy of the workspaces: a session's own roles draft
//! their writes there and commit them through the service, so what the session writes reaches
//! the workspace's other devices as any member's write does.

use crate::service::ServiceSlot;
use crate::workspaces::Workspaces;
use tilo_peer_state::workspace_documents::{DeviceWrites, DocumentWrite};

/// [`DeviceWrites`] over the service's own copy of the workspaces it holds, resolved from
/// [`ServiceSlot`] on every call: what this session's own roles write on this device.
#[derive(Clone)]
pub struct ServiceWrites(ServiceSlot);

impl ServiceWrites {
    /// Writes into the documents of the workspaces held by the service whose embedder holds
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
        let workspaces = self.copies();
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
