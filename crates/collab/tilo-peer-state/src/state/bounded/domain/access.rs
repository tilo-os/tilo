//! Current access facts; never an owner or caller credential.
use super::{BoundedPeerError, PeerRead, SystemTime, backend};
/// Data from actual current membership and the room's grants.
/// The embedding owner still binds its authenticated caller, route and time.
pub struct PeerRoomAccess {
    read: bool,
    write: bool,
    user_root: [u8; 32],
}
impl PeerRoomAccess {
    pub fn read(&self) -> bool {
        self.read
    }
    pub fn write(&self) -> bool {
        self.write
    }
    pub fn user_root(&self) -> [u8; 32] {
        self.user_root
    }
}
impl PeerRead<'_> {
    /// Rechecks actual current membership, then the same canonical record
    /// capability decision. This is data, not a durable receipt or caller proof.
    ///
    /// # Errors
    ///
    /// Returns a [`BoundedPeerError`] if membership cannot be read within its limits.
    pub fn document_read_allowed(
        &self,
        device: [u8; 32],
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<bool, BoundedPeerError> {
        let member = self.member(device, now)?;
        let policy = member.room_policy();
        let bound = tilo_action_policy::roles::foreign_device_bound();
        Ok(backend::access::document_read(
            &member.authority(&policy, &bound),
            self.room(),
            app,
            record,
            now,
        ))
    }
    /// Current room read/write facts from the same retained membership data.
    /// The parent still pins the authenticated channel, actual owner and clock.
    ///
    /// # Errors
    ///
    /// Returns a [`BoundedPeerError`] if membership cannot be read within its limits.
    pub fn room_access(
        &self,
        device: [u8; 32],
        now: SystemTime,
    ) -> Result<PeerRoomAccess, BoundedPeerError> {
        let member = self.member(device, now)?;
        let policy = member.room_policy();
        let bound = tilo_action_policy::roles::foreign_device_bound();
        let (read, write) =
            backend::access::room(&member.authority(&policy, &bound), self.room(), now);
        Ok(PeerRoomAccess {
            read,
            write,
            user_root: member.user_root(),
        })
    }
}
#[cfg(test)]
mod tests;
