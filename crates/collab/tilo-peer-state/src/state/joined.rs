//! A replica of a room another device governs, from its join's answer.

use super::{DeviceCert, RevocationList, StatePeer, SystemTime, err};

impl StatePeer {
    /// [`Self::joined`] from a join's answer as it travels (a
    /// [`crate::join::JoinBootstrap`] or its owner form): the room, its
    /// governing `authority`, its `roster` and the `revocations` its
    /// governance holds. The replica admits app documents by exact writer;
    /// its governance follows from the answer's signed messages or the
    /// room's replay.
    ///
    /// # Errors
    /// A revocation does not verify, or [`Self::joined`] refuses the room.
    pub fn from_join(
        room: &str,
        authority: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: &[tilo_user_identity::RevocationEntry],
        now: SystemTime,
    ) -> Result<Self, String> {
        let mut list = RevocationList::new();
        for entry in revocations {
            list.insert(entry.clone()).map_err(err)?;
        }
        let mut peer = Self::joined(room, authority, roster, list, now)?;
        peer.select_admission_v5().map_err(err)?;
        Ok(peer)
    }
}
