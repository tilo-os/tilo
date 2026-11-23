//! The shared desktop record of a workspace this host governs, given to a
//! workspace made before every workspace held one from the start.

use super::{GovernedWorkspace, Workspaces};

impl Workspaces {
    /// Give `governed`, a workspace this host governs, its shared desktop record
    /// when it has none yet, so every member's layout has the record to
    /// land in. A workspace made now holds it from the start
    /// ([`tilo_peer_state::workspace_seed`]); one made before is given it
    /// here. Not in a workspace whose records are its members' sealed channels:
    /// the host writes nothing there.
    pub(super) async fn prepare_desktop_document(
        &self,
        governed: &mut GovernedWorkspace,
    ) -> Result<(), String> {
        use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
        if governed.seals()
            || !governed.governs_applications(&self.inner.host.cert)
            || governed
                .peer()
                .document(DESKTOP_APP, DESKTOP_DOCUMENT)
                .is_some()
        {
            return Ok(());
        }
        let at = tilo_clock::wall();
        let mut candidate = governed.peer().clone();
        candidate.publication_mut().publish_document(
            DESKTOP_APP,
            DESKTOP_DOCUMENT,
            automerge::AutoCommit::new().save(),
            &self.inner.host.key,
            at,
        )?;
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, at, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(governed).await
    }
}
