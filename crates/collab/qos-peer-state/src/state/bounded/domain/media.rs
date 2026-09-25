//! Inline current call facts, not a caller or publication credential.
use super::{BoundedPeerError, PeerRead, SystemTime, backend, room};
pub struct PeerMediaCall {
    epoch: u64,
    authorization_revision: u64,
    publish: bool,
    subscribe: bool,
}
impl PeerMediaCall {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn authorization_revision(&self) -> u64 {
        self.authorization_revision
    }
    pub fn publish(&self) -> bool {
        self.publish
    }
    pub fn subscribe(&self) -> bool {
        self.subscribe
    }
}
impl PeerRead<'_> {
    /// Uses retained media readiness, then rechecks actual authority and member
    /// at the supplied time. The parent binds its authenticated source, request
    /// room, original clock and publication guard. No policy DTO is exported.
    ///
    /// # Errors
    ///
    /// Returns a [`BoundedPeerError`] if the device is not a current member or the call
    /// binding cannot be read within its limits.
    pub fn media_call(
        &self,
        device: [u8; 32],
        call_id: &str,
        now: SystemTime,
    ) -> Result<Option<PeerMediaCall>, BoundedPeerError> {
        use crate::room::media::read::PolicyValue;
        let backend::media::Cache::Ready(policy) = &self.peer.media else {
            return Ok(None);
        };
        let Ok(id) = room::media::parse_call_id(call_id) else {
            return Ok(None);
        };
        let policy = policy.policy();
        let Some(call) = policy.calls.get(&id) else {
            return Ok(None);
        };
        if call.phase != room::media::CallPhaseV1::Active {
            return Ok(None);
        }
        self.member(self.authority_device(), now)?;
        let member = self.member(device, now)?;
        let denied = call.denied.get(&member.user_root());
        Ok(Some(PeerMediaCall {
            epoch: call.epoch,
            authorization_revision: policy.authorization_revision,
            publish: call.allowed.publish && !denied.is_some_and(|value| value.publish),
            subscribe: call.allowed.subscribe && !denied.is_some_and(|value| value.subscribe),
        }))
    }
}
#[cfg(test)]
mod tests;
