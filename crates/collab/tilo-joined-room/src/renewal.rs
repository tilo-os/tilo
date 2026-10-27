//! A renewed device certificate in the rooms this device joined. A
//! certificate that expires is renewed by its person's root before it runs
//! out (`tilo_identity_host`); the device proves its membership again with
//! the renewed one, the room's governor puts it in place of the one the
//! room held, and the room's record here follows. The device stays the same
//! member, with its role and everything it wrote.
use crate::{NativeWorkspaceOwner, RoomId};
use std::time::SystemTime;
use tilo_peer_state::join::JoinBootstrap;
use tilo_user_identity::DeviceCert;

/// Whether `renewed` is a later certificate of `held`'s device from the same
/// person's root. The certificates' signatures are checked where each is
/// taken in: the device's custody and the room's governance.
#[must_use]
pub fn renews(renewed: &DeviceCert, held: &DeviceCert) -> bool {
    renewed.device_public_key == held.device_public_key
        && renewed.user_public_key == held.user_public_key
        && renewed.issued_at_ns > held.issued_at_ns
}

impl NativeWorkspaceOwner {
    /// Take `bootstrap`, the answer of `room`'s governor to this device
    /// proving its membership with `renewed`, into the room: its
    /// governance, which now holds `renewed`, and the room's record of this
    /// device. Published before it returns.
    ///
    /// # Errors
    /// The room is governed here, `renewed` does not renew the certificate
    /// the room's record holds, the answer is not the room's or its
    /// governance does not hold `renewed`, or the record cannot be
    /// published.
    pub fn renew_membership(
        &self,
        room: RoomId,
        renewed: &DeviceCert,
        bootstrap: &JoinBootstrap,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if candidate.governed_here() {
            return Err("a room governed here holds its own certificate".into());
        }
        if &candidate.record.device == renewed {
            return Ok(());
        }
        if !renews(renewed, &candidate.record.device) {
            return Err("the certificate does not renew this device's in the room".into());
        }
        if bootstrap.device_cert != *renewed
            || bootstrap.room != room.to_string()
            || bootstrap.authority != *candidate.peer.authority()
        {
            return Err("the governor's answer is not this room's renewal".into());
        }
        crate::enrollment::install_governance(&mut candidate.peer, bootstrap, now)?;
        if !candidate.peer.certificates().contains(renewed) {
            return Err("the room's governance does not hold the renewed certificate".into());
        }
        candidate.record.device = renewed.clone();
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(room, candidate, None)
    }
}

#[cfg(test)]
mod tests;
