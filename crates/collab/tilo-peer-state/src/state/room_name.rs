//! The room's name, which its governor alone writes.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{
    NeighbourMsg, ROOT, ReadDoc, SigningKey, SystemTime, Transactable, TransportOrigin, err,
};

impl PeerGovernance<'_> {
    /// The room's name, as its governance document records it.
    #[must_use]
    pub fn room_name(self) -> Option<String> {
        let doc = self.peer.core.room_doc.as_deref()?;
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
    /// Rename the room to `name`, authored by the governor `key`. Naming it
    /// what it is already called changes nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer or the change is
    /// refused.
    pub fn author_room_name(
        self,
        name: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        self.peer.require_governance_writer(key, now)?;
        if self.peer.governance().room_name().as_deref() == Some(name) {
            return Ok(());
        }
        let mut doc = self.peer.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
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
                room: self.peer.core.room.clone(),
                changes,
            },
            now,
        )?;
        Ok(())
    }
}
