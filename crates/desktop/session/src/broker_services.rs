//! What this session reaches of the device's daemon on the shell link it was handed: the person's
//! workspaces' records, and an app's resources.

use std::sync::Arc;

/// What the session's own roles read and write of the person's workspaces: the device's daemon
/// keeps them, and answers on the shell `link` it handed this session; and each write the daemon
/// takes, which the desktop reads its layouts again by.
pub(crate) fn session_documents(
    link: tilo_workspace_service::apps::services::DeviceDaemon,
    runtime: tokio::runtime::Handle,
) -> (
    Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    tilo_session_client::daemon_documents::DaemonWritesFeed,
    tilo_session_client::daemon_documents::DaemonShell,
) {
    let (documents, writes, shell) =
        tilo_session_client::daemon_documents::on_daemon(link, runtime);
    let feed = tilo_session_client::daemon_documents::DaemonWritesFeed::start(&shell);
    (documents, writes, feed, shell)
}

/// `command` of `app`'s resources in `workspace`, answered by the daemon as the device's person.
pub(crate) async fn daemon_resource_command(
    shell: &tilo_session_client::daemon_documents::DaemonShell,
    workspace: String,
    app: String,
    command: &collab_types::store::wire::Command,
) -> Result<collab_types::store::wire::Reply, String> {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    let command = serde_json::to_string(command)
        .map_err(|error| format!("a resource command does not encode: {error}"))?;
    match shell
        .ask(ShellCall::Resources {
            workspace,
            app,
            command,
        })
        .await?
    {
        ShellReply::Resources(answer) => serde_json::from_str(&answer?)
            .map_err(|error| format!("the daemon's answer does not read: {error}")),
        other => Err(format!("the daemon answered something else: {other:?}")),
    }
}
