//! The workspace's name, which its owner and those the workspace lets rename it
//! write (`workspace::membership`).
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{
    NeighbourMsg, ROOT, ReadDoc, SigningKey, SystemTime, Transactable, TransportOrigin, err,
};

impl PeerGovernance<'_> {
    /// The workspace's name, as its governance document records it.
    #[must_use]
    pub fn workspace_name(self) -> Option<String> {
        let doc = self.peer.core.workspace_doc.as_deref()?;
        match doc.get(ROOT, "name").ok().flatten()? {
            (automerge::Value::Scalar(value), _) => match value.as_ref() {
                automerge::ScalarValue::Str(name) => Some(name.to_string()),
                _ => None,
            },
            _ => None,
        }
    }
}
impl PeerGovernanceMut<'_> {
    /// Rename the workspace to `name`, written by `key`. Naming it what it is
    /// already called changes nothing.
    pub fn author_workspace_name(
        self,
        name: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        self.peer.require_member_writer(key, now)?;
        if self.peer.governance().workspace_name().as_deref() == Some(name) {
            return Ok(());
        }
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("missing workspace")?;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        doc.put(ROOT, "name", name).map_err(err)?;
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        self.peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                room: self.peer.core.workspace.clone(),
                changes,
            },
            now,
        )?;
        Ok(())
    }
}
