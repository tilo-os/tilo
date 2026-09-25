//! Store-owned private disclosure. No handler-installation or allocation witness.
use super::{CloudStoreSource, CloudStoreTurn, Current};
use qos_action_broker::peer_actions::{PeerActionQueue, PrivatePolicyBasis};
use qos_relay::{
    delivery_v3::{http_proof::VerifiedRequest, private_wire},
    peer_actions::DirectedAction,
};
use qos_session_peer::ingress::{
    BufferedRequest, PendingPrivate, PrivateCarrier, PrivateError, ResponseCredits, WatchId,
    WatchUpdate,
};
use std::collections::HashMap;
use tokio::time::Instant;

#[derive(Clone)]
struct Disclosure {
    source: CloudStoreSource,
    lineage: u64,
    continuity: u64,
    revision: String,
    lower_ns: u64,
    expiry_ns: u64,
}
impl Disclosure {
    fn capture(
        source: &CloudStoreSource,
        turn: &CloudStoreTurn,
        session_expiry_ns: u64,
    ) -> Result<Self, PrivateError> {
        if !source.owns_turn(turn) {
            return Err(PrivateError::Unavailable);
        }
        let basis =
            PrivatePolicyBasis::from_peer(turn.peer()).map_err(|_| PrivateError::Unavailable)?;
        let now = turn.fresh_now_ns().map_err(|_| PrivateError::Unavailable)?;
        let expiry = basis
            .next_expiry_after(now / 1_000_000_000)
            .map(|at| {
                at.checked_mul(1_000_000_000)
                    .ok_or(PrivateError::Unavailable)
            })
            .transpose()?
            .unwrap_or(u64::MAX)
            .min(session_expiry_ns);
        if now >= expiry {
            return Err(PrivateError::Unavailable);
        }
        Ok(Self {
            source: source.clone(),
            lineage: turn.policy_lineage(),
            continuity: turn.continuity(),
            revision: basis.revision().into(),
            lower_ns: now,
            expiry_ns: expiry,
        })
    }
    fn check(&self, turn: &CloudStoreTurn) -> Result<(), PrivateError> {
        if !self.source.owns_turn(turn)
            || self.lineage != turn.policy_lineage()
            || self.continuity != turn.continuity()
        {
            return Err(PrivateError::Unavailable);
        }
        let basis =
            PrivatePolicyBasis::from_peer(turn.peer()).map_err(|_| PrivateError::Unavailable)?;
        if basis.revision() != self.revision {
            return Err(PrivateError::Unavailable);
        }
        let now = turn.fresh_now_ns().map_err(|_| PrivateError::Unavailable)?;
        if now < self.lower_ns || now >= self.expiry_ns {
            return Err(PrivateError::Unavailable);
        }
        Ok(())
    }
}
pub(crate) struct CloudPrivateService {
    source: CloudStoreSource,
    carrier: PrivateCarrier,
    fences: HashMap<WatchId, Disclosure>,
}
pub(crate) struct CloudPendingPrivate {
    carrier: PendingPrivate,
    disclosure: Disclosure,
}
impl CloudPrivateService {
    pub(crate) fn new_unqualified(
        source: &CloudStoreSource,
        queue: &PeerActionQueue,
    ) -> Current<Result<Self, PrivateError>> {
        let retained = source.clone();
        source.with_current(|turn| {
            let host = turn
                .peer()
                .room()
                .parse()
                .map_err(|_| PrivateError::Unavailable)?;
            Ok(Self {
                source: retained,
                carrier: PrivateCarrier::from_queue_unqualified(queue.clone(), host)?,
                fences: HashMap::new(),
            })
        })
    }
    pub(crate) fn submit(
        &mut self,
        proof: VerifiedRequest<'_>,
        turn: &CloudStoreTurn,
        deadline: Instant,
    ) -> Result<CloudPendingPrivate, PrivateError> {
        let disclosure = Disclosure::capture(&self.source, turn, proof.session().expires_at_ns())?;
        let time = turn.policy_time().map_err(|_| PrivateError::Unavailable)?;
        let carrier = self
            .carrier
            .submit_with_policy_time(proof, turn.peer(), deadline, time)?;
        Ok(CloudPendingPrivate {
            carrier,
            disclosure,
        })
    }
    fn reconcile(&mut self) {
        self.fences
            .retain(|id, _| self.carrier.live_watch_ids().any(|live| live == *id));
    }
    pub(crate) fn observe_reply(
        &mut self,
        pending: &CloudPendingPrivate,
        reply: &DirectedAction,
        turn: &CloudStoreTurn,
    ) -> Result<(), PrivateError> {
        if !self.source.owns_turn(turn) {
            return Err(PrivateError::Unavailable);
        }
        pending.disclosure.check(turn)?;
        let result = self.carrier.observe_reply(&pending.carrier, reply);
        self.reconcile();
        if let WatchUpdate::Installed(id) = result? {
            if self.fences.len() >= 32 {
                self.close();
                return Err(PrivateError::Unavailable);
            }
            self.fences.insert(id, pending.disclosure.clone());
        }
        Ok(())
    }
    pub(crate) fn poll_events(
        &mut self,
        proof: VerifiedRequest<'_>,
        turn: &CloudStoreTurn,
    ) -> Result<Option<(CloudPendingPrivate, DirectedAction)>, PrivateError> {
        if !self.source.owns_turn(turn) {
            return Err(PrivateError::Unavailable);
        }
        let result = self.carrier.poll_events(proof, turn.peer());
        self.fenced_event(result, turn)
    }
    /// Whether any watch holds a ready event: a held events poll can only
    /// become answerable with an event while this is true.
    pub(crate) fn has_ready_event(&self) -> bool {
        self.carrier.has_ready_event()
    }
    /// A held original events poll, resumed after its parked continuation was
    /// freshly revalidated under `turn`; the exact original bytes are rechecked.
    pub(crate) fn poll_events_resumed(
        &mut self,
        session: &qos_relay::delivery_v3::auth::VerifiedSession,
        original: &BufferedRequest,
        turn: &CloudStoreTurn,
    ) -> Result<Option<(CloudPendingPrivate, DirectedAction)>, PrivateError> {
        if !self.source.owns_turn(turn) {
            return Err(PrivateError::Unavailable);
        }
        let result = self
            .carrier
            .poll_events_resumed(session, original, turn.peer());
        self.fenced_event(result, turn)
    }
    fn fenced_event(
        &mut self,
        result: Result<Option<(PendingPrivate, DirectedAction)>, PrivateError>,
        turn: &CloudStoreTurn,
    ) -> Result<Option<(CloudPendingPrivate, DirectedAction)>, PrivateError> {
        let result = match result {
            Ok(value) => value,
            Err(error) => {
                self.reconcile();
                return Err(error);
            }
        };
        let Some((carrier, reply)) = result else {
            self.reconcile();
            return Ok(None);
        };
        let disclosure = carrier
            .watch_id()
            .and_then(|id| self.fences.get(&id))
            .cloned();
        self.reconcile();
        let disclosure = disclosure.ok_or(PrivateError::Unavailable)?;
        disclosure.check(turn)?;
        Ok(Some((
            CloudPendingPrivate {
                carrier,
                disclosure,
            },
            reply,
        )))
    }
    pub(crate) fn pump(&mut self) -> Result<(), PrivateError> {
        let result = self.carrier.pump();
        self.reconcile();
        result
    }
    pub(crate) fn retire_session(&mut self, id: [u8; 16]) {
        self.carrier.retire_session(id);
        self.reconcile();
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.carrier.is_closed()
    }
    pub(crate) fn close(&mut self) {
        self.carrier.close();
        self.fences.clear();
    }
}
impl CloudPendingPrivate {
    pub(crate) fn try_reply(&mut self) -> Result<Option<DirectedAction>, PrivateError> {
        self.carrier.try_reply()
    }
    /// Busy takes nothing: the caller retains the original request, permit,
    /// deadline and pending reply. A current turn is the actual Store lock.
    pub(crate) fn respond(
        pending: &mut Option<Self>,
        reply: &DirectedAction,
        request: &mut Option<BufferedRequest>,
        credits: &mut Option<ResponseCredits>,
    ) -> Current<Result<(), PrivateError>> {
        let Some(source) = pending
            .as_ref()
            .map(|pending| pending.disclosure.source.clone())
        else {
            return Current::Current(Err(PrivateError::Unavailable));
        };
        source.with_current(|turn| {
            let retained = pending.as_ref().ok_or(PrivateError::Unavailable)?;
            retained.disclosure.check(turn)?;
            retained.carrier.validate_response_unqualified(reply)?;
            if request.is_none()
                || credits
                    .as_ref()
                    .is_none_or(|credit| credit.private_reply.is_some())
            {
                return Err(PrivateError::Unavailable);
            }
            let bytes = private_wire::encode_response(
                retained.carrier.operation(),
                retained.carrier.original_request(),
                reply,
            )?;
            retained.carrier.validate_response_unqualified(reply)?;
            retained.disclosure.check(turn)?;
            let retained = pending.take().ok_or(PrivateError::Unavailable)?;
            let request = request.take().ok_or(PrivateError::Unavailable)?;
            let mut credits = credits.take().ok_or(PrivateError::Unavailable)?;
            credits.private_reply = Some(retained.carrier);
            request
                .respond_with_credits(reqwest::StatusCode::OK, bytes, credits)
                .map_err(|_| PrivateError::Unavailable)
        })
    }
}
#[cfg(test)]
mod tests;
