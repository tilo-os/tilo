//! The workspace service with a language runtime mounted on it, as the daemon's program mounts
//! the one it links: the agents and triggers that read and change the records of the built-in
//! apps are driven here, and the packages those records are of compile from their sources.

mod hosted_own_agent_tests;
mod moderation_history_tests;
mod own_agent_tests;
mod record_roots_tests;
mod served_notices_tests;
mod task_work_tests;
mod triggers_tests;

use tilo_workspace_service::workspaces::Workspaces;

/// Mount the language runtime the daemon links on `workspaces`, as its program does when it
/// opens: its own roles read and change the built-in apps' records, and the records are served
/// to whoever the workspace's dispatch admits.
pub(crate) fn mount(workspaces: &Workspaces) {
    workspaces.provide_tilolang(tilolang_workspace::host());
    if let Some(tilolang) = workspaces.tilolang() {
        tilolang.records.serve(workspaces);
    }
}
