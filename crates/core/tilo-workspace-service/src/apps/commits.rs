//! The broker's effect receipts for every session that holds this device's workspaces: the
//! receipt of every effect an app's action had is recorded in the workspace's own document,
//! through this device's workspace service.

use tilo_broker_core::workspace_commits::{EffectFuture, WorkspaceCommits};
use tilo_desktop_model::WorkspaceId;

/// The broker's effect receipts, kept in the workspace's own document.
#[derive(Clone)]
pub struct ServiceCommits {
    /// Where receipts are committed: this device's workspace service.
    service: crate::service::ServiceSlot,
}

impl ServiceCommits {
    /// Effect receipts kept in the workspaces `service` holds.
    #[must_use]
    pub fn new(service: crate::service::ServiceSlot) -> Self {
        Self { service }
    }
}

impl WorkspaceCommits for ServiceCommits {
    fn projects_workspace(&self, _workspace: WorkspaceId) -> bool {
        // The service holds every workspace this device takes part in: its effects are its own
        // to record, whichever copy it keeps.
        self.service.service().is_some()
    }

    fn health(&self) -> Result<(), String> {
        self.service
            .service()
            .map(|_| ())
            .ok_or_else(|| "this device's workspace service is not open".to_owned())
    }

    fn effect(&self, workspace: String, redacted: Vec<u8>) -> EffectFuture {
        let (copies, runtime) = self.service.service().map_or((None, None), |service| {
            (Some(service.workspaces().clone()), service.writes_runtime())
        });
        Box::pin(async move {
            let copies = copies.ok_or("this device's workspace service is not open")?;
            super::service_resources::on_the_service(
                runtime,
                Box::pin(async move {
                    copies
                        .commit_member_effect_receipt(&workspace, &redacted)
                        .await
                }),
            )
            .await
        })
    }
}
