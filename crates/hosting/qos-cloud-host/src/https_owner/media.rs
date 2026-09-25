//! Same Store transaction for native media authority; no independent owner.
use super::{CloudStoreSource, CloudStoreTurn};
use ed25519_dalek::Signer;
use qos_relay::delivery_v3::{
    auth::VerificationContext, http_proof::VerifiedRequest, media_wire::ControlResult,
};
use qos_session_peer::ingress::{
    Denied, MediaAuthoritySource, MediaAuthorityTurn, MediaError, MediaObservation, MediaService,
    RelayAuthority, RoomMediaPolicy, SessionAuthorization,
};
use std::sync::Arc;

/// The existing shared media core, bound to one actual Cloud source. This
/// component constructor does not install ingress or attest memory custody.
pub(crate) struct CloudMediaService {
    source: CloudStoreSource,
    service: MediaService<CloudStoreSource>,
}
impl CloudMediaService {
    pub(crate) fn new_unqualified(
        source: CloudStoreSource,
        audience: &str,
    ) -> Result<Self, MediaError> {
        let service = MediaService::from_source(source.clone(), audience)?;
        Ok(Self { source, service })
    }
    pub(crate) fn control_in(
        &self,
        turn: &CloudStoreTurn,
        context: &VerificationContext<'_>,
        request: &VerifiedRequest<'_>,
    ) -> Result<ControlResult, MediaError> {
        if !self.source.owns_turn(turn) {
            return Err(MediaError::Unauthorized);
        }
        // The carrier context is derived inside this same Store turn from the
        // turn's own trusted clock, after the turn began. Media decisions use
        // that exact admission instant (the core requires the two to agree).
        let fresh = turn.fresh_now_ns().map_err(|_| MediaError::Unauthorized)?;
        if context.now_ns < turn.now_ns() || context.now_ns > fresh {
            return Err(MediaError::Unauthorized);
        }
        let current = CloudMediaTurn {
            turn,
            policy: RoomMediaPolicy::from_committed(turn.peer()),
            now: context.now_ns,
        };
        self.service
            .control_in(&current, context, request, |input| {
                Ok(turn.store.key().sign(input.digest()).to_bytes())
            })
    }
}
impl RelayAuthority for CloudMediaService {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        self.service.admit(proof)
    }
}

pub(crate) struct CloudMediaTurn<'a> {
    turn: &'a CloudStoreTurn,
    policy: RoomMediaPolicy,
    now: u64,
}
impl MediaAuthorityTurn for CloudMediaTurn<'_> {
    fn peer(&self) -> &crate::StatePeer {
        self.turn.peer()
    }
    fn policy(&self) -> &RoomMediaPolicy {
        &self.policy
    }
    fn workspace(&self) -> u64 {
        self.turn.workspace()
    }
    fn continuity(&self) -> u64 {
        self.turn.continuity()
    }
    fn now_ns(&self) -> u64 {
        self.now
    }
}
impl MediaAuthoritySource for CloudStoreSource {
    type Turn<'a> = CloudMediaTurn<'a>;
    fn with_current<R>(&self, action: impl FnOnce(&Self::Turn<'_>) -> R) -> MediaObservation<R> {
        match CloudStoreSource::with_current(self, |turn| {
            let policy = RoomMediaPolicy::from_committed(turn.peer());
            action(&CloudMediaTurn {
                turn,
                policy,
                now: turn.now_ns(),
            })
        }) {
            super::Current::Current(value) => MediaObservation::Current(value),
            super::Current::Busy => MediaObservation::Busy,
            super::Current::Denied => MediaObservation::Denied,
        }
    }
}
#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod tests;
