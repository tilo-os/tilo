//! The broker's owner commits, through the device's workspace service.
//!
//! Every workspace this daemon's apps write in is held by the device's
//! workspace service: the member copies of the workspaces other devices serve,
//! or the workspace founded here, which the service governs. A document
//! write is admitted here, in the writing device's name, and answered with the
//! receipt that binds it; the service keeps it in the workspace's ordered
//! persistence and carries it to the workspace's other devices.

use std::sync::Arc;

use tilo_workspace_service::apps::service_workspaces::ServiceCommits;

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
    fn allows(&self, app: &str, kind: &str) -> bool {
        match (self.declared_kinds)(app) {
            Some(declared) => declared.iter().any(|declared| declared == kind),
            None => self.dev,
        }
    }
}

/// The broker's commits into the workspaces this device's service holds.
pub(super) fn service_commits(
    workspaces: &Arc<ServiceWorkspaces>,
    creation: Arc<RecordCreation>,
) -> Arc<ServiceCommits> {
    ServiceCommits::new(
        workspaces.copies().clone(),
        workspaces.device_cert().device_public_key,
        Box::new(move |app, resource_type| creation.allows(app, resource_type)),
        tilo_rt::app().handle().clone(),
    )
}
