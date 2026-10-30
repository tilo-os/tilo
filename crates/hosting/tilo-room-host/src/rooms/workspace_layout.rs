//! The shared desktop record of a room this host governs, given to a room
//! made before rooms held it from the start.

use super::{GovernedRoom, Rooms};

impl Rooms {
    /// Give `governed`, a room this host governs, its shared desktop record
    /// when it has none yet, so every member's layout has the record to
    /// land in. A room made now holds it from the start
    /// ([`tilo_peer_state::workspace_seed`]); one made before is given it
    /// here. Not in a room whose records are its members' sealed channels:
    /// the host writes nothing there.
    pub(super) async fn prepare_workspace_layout(
        &self,
        governed: &mut GovernedRoom,
    ) -> Result<(), String> {
        use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
        if governed.participation.is_some()
            || governed.record.authority != self.inner.host.cert
            || governed
                .peer()
                .document(DESKTOP_APP, DESKTOP_RECORD)
                .is_some()
        {
            return Ok(());
        }
        let at = std::time::SystemTime::now();
        let mut candidate = governed.peer().clone();
        candidate.publication_mut().publish_document(
            DESKTOP_APP,
            DESKTOP_RECORD,
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
