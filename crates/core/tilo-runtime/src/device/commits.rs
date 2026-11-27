//! What the broker keeps through the device's workspace service: its effect receipts, and the
//! kinds an app may found records in where the service hosts the workspace.

use std::sync::Arc;

use tilo_app_host::workspace_commits::{EffectFuture, WorkspaceCommits};
use tilo_desktop_model::WorkspaceId;
use tilo_workspace_service::service::WorkspaceService;
use tilo_workspace_service::workspaces::Workspaces;

use super::workspaces::ServiceWorkspaces;

/// The resource kinds an installed app declares its records hold; `None`
/// for an app this profile has not installed.
pub(super) type DeclaredKinds = Box<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// In which kinds an app may write a resource this device's own workspace does not hold yet.
pub(super) struct RecordCreation {
    pub(super) declared_kinds: DeclaredKinds,
    /// Started with `--dev`: an app that is not installed is a build under
    /// development, and may create records in the kinds it names.
    pub(super) dev: bool,
}

impl RecordCreation {
    pub(super) fn allows(&self, app: &str, kind: &str) -> bool {
        match (self.declared_kinds)(app) {
            Some(declared) => declared.iter().any(|declared| declared == kind),
            None => self.dev,
        }
    }
}

/// The broker's commits into the workspaces this device's service holds.
pub(super) struct ServiceCommits {
    service: Arc<WorkspaceService>,
}

impl ServiceCommits {
    pub(super) fn new(workspaces: &Arc<ServiceWorkspaces>) -> Arc<Self> {
        Arc::new(Self {
            service: Arc::clone(workspaces.service()),
        })
    }

    fn copies(&self) -> &Workspaces {
        self.service.workspaces()
    }
}

impl WorkspaceCommits for ServiceCommits {
    /// The service keeps each device's own effect receipts in its copy of the
    /// workspace: a member copy's are this device's. Answered from the copy the
    /// service keeps read of, without waiting on it: the broker asks from the
    /// runtime that serves the workspaces, which must not wait on itself.
    fn projects_workspace(&self, workspace: WorkspaceId) -> bool {
        self.copies()
            .copy_now(workspace)
            .is_some_and(|copy| copy.member)
    }

    fn effect(&self, workspace: String, redacted: Vec<u8>) -> EffectFuture {
        let copies = self.copies().clone();
        Box::pin(async move {
            copies
                .commit_member_effect_receipt(&workspace, &redacted)
                .await
                .map_err(|error| format!("the effect receipt was not recorded: {error}"))
        })
    }

    fn health(&self) -> Result<(), String> {
        Ok(())
    }
}
