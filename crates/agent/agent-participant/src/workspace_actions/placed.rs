//! Apps whose windows another device holds: a hosted workspace's Browser,
//! whose window runs in the workspace's own VM. This host keeps their records
//! and opens one for them, and mounts nothing of them.

use tilo_peer_state::document_frame::DocumentRoute;

use super::{DocumentMutation, Outcome, Worker};
use tilo_relay_protocol::WorkspaceId;

impl Worker {
    /// The route an app another device holds the windows of opens, and the
    /// genesis of its record when the workspace holds none: the workspace's one
    /// record of the app, started empty here for the device holding its window
    /// to fill.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn placed_route(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
        app: &str,
    ) -> Result<(DocumentRoute, Option<DocumentMutation>), Outcome> {
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            record: ui::record::RecordId::workspace_default().route().to_owned(),
        };
        let start = !self.held_or_owed(workspace_id, &route)?;
        let genesis =
            start.then(|| DocumentMutation::Snapshot(automerge::AutoCommit::new().save()));
        Ok((route, genesis))
    }
}
