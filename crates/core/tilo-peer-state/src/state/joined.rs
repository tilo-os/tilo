//! A replica of a workspace another device governs, from its join's answer.

use super::{DeviceCert, NeighbourMsg, RevocationList, StatePeer, SystemTime, err, peer_handle};
use crate::TransportOrigin;

impl StatePeer {
    /// [`Self::joined`] from a join's answer as it travels (a
    /// [`crate::join::JoinBootstrap`] or its owner form): the workspace, the
    /// `authority`, its `roster` and the `revocations` its governance holds.
    /// The `authority` is the device the join was answered by, no particular
    /// device of the workspace's owner: it is the replica's origin, and the
    /// person of its owner only until the workspace's governance arrives,
    /// which names the owner the replica then holds to. The replica admits
    /// app documents by exact writer; its governance follows from the
    /// answer's signed messages or the workspace's replay.
    ///
    /// # Errors
    /// A revocation does not verify, or [`Self::joined`] refuses the workspace.
    pub fn from_join(
        workspace: &str,
        authority: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: &[tilo_user_identity::RevocationEntry],
        now: SystemTime,
    ) -> Result<Self, String> {
        let mut list = RevocationList::new();
        for entry in revocations {
            list.insert(entry.clone()).map_err(err)?;
        }
        let mut peer = Self::joined(workspace, authority, roster, list, now)?;
        peer.select_exact_admissions().map_err(err)?;
        Ok(peer)
    }

    /// Decide the app document `message`, which the workspace's host relayed
    /// from `origin`, as this replica's governance decides it, without
    /// taking it in: for a device that keeps the workspace's records itself and
    /// needs the workspace's verdict on each write first. `Ok(true)` admits it;
    /// `Ok(false)` holds it until what it is decided against is here (the
    /// workspace's governance, its author's certificate, its app's surface).
    /// A retained write is decided as of when it was signed.
    pub fn decide_relayed_document(
        &self,
        origin: TransportOrigin,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<bool, String> {
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return Err("not a document message".into());
        };
        let (source, retained) = match origin {
            TransportOrigin::ServerDelta(source) => (source, false),
            TransportOrigin::RetainedServerDelta(source) => (source, true),
            _ => return Err("a relayed document comes from the workspace's host".into()),
        };
        if *cluster_id != self.core.workspace
            || *peer != peer_handle(&source)
            || envelope.author.as_bytes() != &source
        {
            return Err("transport author or document workspace mismatch".into());
        }
        collab_types::verify_op(envelope).map_err(err)?;
        if !collab_types::names_document(app_id, cluster_id, document_id, envelope) {
            return Err("artifact does not match route".into());
        }
        if self.core.workspace_doc.is_none() || !self.core.certificates.contains_key(&source) {
            return Ok(false);
        }
        let at = if retained { envelope.timestamp } else { now };
        Ok(matches!(
            self.authorize_document(&source, app_id, document_id, envelope, at, retained)?,
            super::authority::Authorized::Admit { .. }
        ))
    }
}
