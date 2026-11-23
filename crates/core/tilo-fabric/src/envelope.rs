//! What one exchange between two of the person's devices is bound to: the fresh id that names it,
//! the device that sent it, the one it is for, and when it was sent. A request that is not fresh,
//! names another sender than the one that proved itself, or was already taken is refused before
//! anything reads what it asks.

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{DeviceId, FabricError};

/// The oldest a request may be when it arrives.
pub const MAX_FABRIC_ACTION_AGE_MS: u64 = 2 * 60 * 1000;
/// How far ahead of this device's clock a sender's may run.
pub const MAX_FABRIC_CLOCK_SKEW_MS: u64 = 30_000;
/// Requests remembered as taken: more than arrive within [`MAX_FABRIC_ACTION_AGE_MS`].
const MAX_REMEMBERED: usize = 4096;

/// Who sent a request, to whom, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub request_id: [u8; 16],
    pub source: DeviceId,
    pub target: DeviceId,
    pub issued_unix_ms: u64,
}

impl Envelope {
    /// A fresh envelope from `source` to `target`, sent now.
    #[must_use]
    pub fn now(source: DeviceId, target: DeviceId) -> Self {
        Self {
            request_id: crate::nonzero::<16>(),
            source,
            target,
            issued_unix_ms: tilo_clock::unix_now_ms(),
        }
    }

    /// Whether this is a request the device `transport_source` proved itself as sent to
    /// `local_target`, fresh at `now_unix_ms`.
    ///
    /// # Errors
    /// [`FabricError::InvalidAction`] for a request from another sender, for another device, to
    /// itself, sent too long ago or from too far ahead.
    pub fn check_at(
        &self,
        transport_source: &DeviceId,
        local_target: &DeviceId,
        now_unix_ms: u64,
    ) -> Result<(), FabricError> {
        if self.request_id == [0; 16]
            || &self.source != transport_source
            || &self.target != local_target
            || self.source == self.target
            || self.issued_unix_ms > now_unix_ms.saturating_add(MAX_FABRIC_CLOCK_SKEW_MS)
            || now_unix_ms.saturating_sub(self.issued_unix_ms) > MAX_FABRIC_ACTION_AGE_MS
        {
            return Err(FabricError::InvalidAction);
        }
        Ok(())
    }
}

/// The requests this device took lately, so one sent twice is taken once.
#[derive(Debug, Default)]
pub struct Replays {
    seen: BTreeSet<(DeviceId, [u8; 16])>,
    order: VecDeque<(DeviceId, [u8; 16])>,
}

impl Replays {
    /// Take `envelope` once: `false` for a request already taken.
    pub fn take(&mut self, envelope: &Envelope) -> bool {
        let key = (envelope.source.clone(), envelope.request_id);
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > MAX_REMEMBERED {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> DeviceId {
        DeviceId::new(name).unwrap()
    }

    #[test]
    fn a_request_is_taken_once_while_fresh_and_only_from_its_sender() {
        let laptop = id("laptop");
        let desktop = id("desktop");
        let now = 10_000_000;
        let envelope = Envelope {
            request_id: [7; 16],
            source: laptop.clone(),
            target: desktop.clone(),
            issued_unix_ms: now,
        };
        assert_eq!(envelope.check_at(&laptop, &desktop, now), Ok(()));
        assert_eq!(
            envelope.check_at(&id("phone"), &desktop, now),
            Err(FabricError::InvalidAction),
            "a device asks only as itself"
        );
        assert_eq!(
            envelope.check_at(&laptop, &laptop, now),
            Err(FabricError::InvalidAction),
            "a request for another device is not this one's"
        );
        assert_eq!(
            envelope.check_at(&laptop, &desktop, now + MAX_FABRIC_ACTION_AGE_MS + 1),
            Err(FabricError::InvalidAction),
            "a request too old is refused"
        );
        assert_eq!(
            envelope.check_at(&laptop, &desktop, now - MAX_FABRIC_CLOCK_SKEW_MS - 1),
            Err(FabricError::InvalidAction),
            "a request from too far ahead is refused"
        );
        let zero = Envelope {
            request_id: [0; 16],
            ..envelope.clone()
        };
        assert_eq!(
            zero.check_at(&laptop, &desktop, now),
            Err(FabricError::InvalidAction)
        );

        let mut replays = Replays::default();
        assert!(replays.take(&envelope));
        assert!(!replays.take(&envelope), "the same request is taken once");
        let other = Envelope {
            request_id: [8; 16],
            ..envelope
        };
        assert!(replays.take(&other));
    }
}
