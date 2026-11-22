//! The broker's owner commits, through the profile's workspace service.
//!
//! Every workspace this runtime's apps write in is held by the device's
//! workspace service: the profile's own workspace, which the service governs,
//! and the member copies of the workspaces other devices serve. A document
//! write is admitted there, in the writing device's name, as every device's
//! service admits one ([`ServiceCommits`]). The workspaces the desktop session
//! governs are refused, as they always were.

use std::sync::Arc;

use tilo_workspace_service::apps::service_workspaces::ServiceCommits;

use super::service::ServiceWorkspaces;

/// The resource kinds an installed app declares its records hold; `None`
/// for an app this profile has not installed.
pub(in crate::standalone) type DeclaredKinds =
    Box<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// In which kinds an app may write a resource this device's own workspace does not hold yet.
pub(in crate::standalone) struct RecordCreation {
    pub(in crate::standalone) declared_kinds: DeclaredKinds,
    /// Started with `--dev`: an app that is not installed is a build under
    /// development, and may create records in the kinds it names.
    pub(in crate::standalone) dev: bool,
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
        workspaces.refused_workspaces(),
        tilo_rt::app().handle().clone(),
    )
}
