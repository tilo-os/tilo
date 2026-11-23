//! The session's side of the lock following the person across their devices: each pass reads this
//! device's lock and the person's last input, follows what the rules in [`tilo_fabric::owner`] decide
//! and says this device's own session to the others.

use std::time::Instant;

use crate::platform::fabric::DevicePresence;
use tilo_fabric::owner::{Follow, latest_use_nearby, shares_network};

use crate::Session;
use tilo_compositor::Error;

impl Session {
    /// Follow the person's session on their other devices, every pass: idle time here counts from
    /// their use of one nearby, a lock or unlock there is followed on the pass that brings it, and
    /// this device says its own session to them.
    pub(crate) fn follow_owner_session(&mut self) -> Result<(), Error> {
        let Some(fabric) = self.shared.fabric.clone() else {
            return Ok(());
        };
        let now = Instant::now();
        let now_ms = tilo_clock::unix_now_ms();
        let since_input =
            u64::try_from(self.compositor.input_idle().as_millis()).unwrap_or(u64::MAX);
        let input_ms = now_ms.saturating_sub(since_input);
        let locked = self.compositor.locked();
        let in_use = self.compositor.in_use();
        let device = fabric.devices();
        let local = fabric
            .local_device_fabric_identity()
            .map(|(local, _)| local);
        let own = local
            .as_ref()
            .and_then(|local| device.get(local))
            .map(|device| device.presence.clone());
        let others: Vec<(&DevicePresence, bool)> = device
            .devices()
            .iter()
            .filter(|(id, _)| Some(*id) != local.as_ref())
            .map(|(_, device)| (&device.presence, device.online))
            .collect();
        // Idle time here counts from the person's last input on a device of theirs nearby: neither
        // locks while they are on the other.
        let used_nearby = own
            .as_ref()
            .and_then(|own| latest_use_nearby(own, others.iter().copied()));
        if let Some(used) = used_nearby
            && let Some(at) = now.checked_sub(std::time::Duration::from_millis(
                now_ms.saturating_sub(used),
            ))
        {
            self.compositor.note_use_elsewhere(at);
        }
        let followed = self.shared.owner_session.decide(
            locked,
            input_ms,
            in_use,
            others.iter().map(|(other, _)| {
                let nearby = own.as_ref().is_some_and(|own| shares_network(own, other));
                (other.session, nearby)
            }),
        );
        match followed {
            Some(Follow::Lock) => self.lock_by_person()?,
            // The person unlocked on one of their other devices: this device unlocks the way it
            // does when they type the password here.
            Some(Follow::Unlock) => self.unlock_by_person()?,
            None => {}
        }
        // What this device says of itself is what it is now: a lock or unlock followed this pass is
        // said on the same one.
        let locked = self.compositor.locked();
        fabric.publish_owner_session(self.shared.owner_session.presence(locked, input_ms));
        Ok(())
    }
}
