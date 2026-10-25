//! Launches whose window is a live view ([`crate::shell::live_embeds`]):
//! drawn inside the window that embeds it, never placed as one of its own.

use super::{
    InstalledApp, LaunchError, LaunchSession, PENDING_PRESENTATION_TIMEOUT, PendingPlacement,
    QueuedOpen, identities_for_package, lock, pending_placement_deadline, queue_prepared_launch,
};
use crate::shell::state::Workspace;

/// Start `app` in `workspace` as a viewer of the record `uri` names (none:
/// the app's own record of the workspace), its
/// window the live view `embed` ([`crate::shell::live_embeds`]): drawn
/// inside the window that embeds it, never placed as one of its own. It
/// may read what the person's copy of the record holds and write nothing:
/// the workspace's grants, without authorship of the app. The launch's file
/// work runs on a preflight worker, as [`super::queue_launch_target_in_workspace`]'s.
///
/// # Errors
///
/// As [`super::queue_launch_in_workspace`].
pub(crate) fn queue_embed_launch(
    app: InstalledApp,
    workspace: &Workspace,
    uri: Option<String>,
    embed: u64,
) -> Result<(), LaunchError> {
    let mut grants = workspace.launch_grants(tilo_clock::unix_now());
    grants.authors.retain(|author| author != &app.package_id);
    LaunchSession::with(|session| {
        let launch_metric_id = tilo_startup_metrics::launch_requested(&app.package_id);
        lock(&session.launch_history).requested(launch_metric_id, &app.package_id, None);
        lock(&session.pending).push(PendingPlacement {
            key: app.package_id.clone(),
            identities: identities_for_package(&app.package_id),
            placement: crate::shell::tab_binding::Placement::NewGroup,
            continuity_instance: None,
            claimed_by: None,
            retire_after: None,
            present_before: pending_placement_deadline(PENDING_PRESENTATION_TIMEOUT),
            launch_metric_id,
            metric_spawned: false,
            diagnostic_reserved: false,
            embed: Some(embed),
        });
    });
    queue_prepared_launch(
        app,
        workspace.room(),
        move || Ok(grants),
        QueuedOpen::uri(uri),
        None,
    )
}
