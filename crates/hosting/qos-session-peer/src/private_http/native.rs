//! Native owner wrapper. Transport polling itself yields no authorized bytes.
use super::*;
use crate::committed_authority::{CommittedView, NativeDisclosureFence};
use std::collections::HashMap;
#[derive(Clone)]
struct Disclosure {
    policy: NativeDisclosureFence,
    session_expiry_ns: u64,
}
impl Disclosure {
    fn check(&self, view: &CommittedView) -> Result<(), PrivateError> {
        let now = self
            .policy
            .check(view)
            .map_err(|_| PrivateError::Unavailable)?;
        if now >= self.session_expiry_ns {
            return Err(PrivateError::Unavailable);
        }
        Ok(())
    }
}
pub(crate) struct NativePrivateService {
    carrier: PrivateService,
    clock: qos_action_broker::peer_actions::OwnerPolicyClock,
    fences: HashMap<WatchId, Disclosure>,
}
pub(crate) struct NativePendingPrivate {
    carrier: PendingPrivate,
    disclosure: Disclosure,
}
impl NativePrivateService {
    pub(crate) fn install(actions: &ActionHost) -> Result<Self, PrivateError> {
        Ok(Self {
            carrier: PrivateService::install(actions)?,
            clock: actions
                .policy_clock()
                .map_err(|_| PrivateError::Unavailable)?,
            fences: HashMap::new(),
        })
    }
    pub(crate) fn submit(
        &mut self,
        proof: VerifiedRequest<'_>,
        view: &CommittedView,
        deadline: Instant,
    ) -> Result<NativePendingPrivate, PrivateError> {
        let disclosure = Disclosure {
            policy: view
                .private_disclosure_fence()
                .map_err(|_| PrivateError::Unavailable)?,
            session_expiry_ns: proof.session().expires_at_ns(),
        };
        let time = disclosure.policy.policy_time().clone();
        if !time.belongs_to(&self.clock) {
            return Err(PrivateError::Unavailable);
        }
        view.check_policy_time(&time)
            .map_err(|_| PrivateError::Unavailable)?;
        let carrier = self
            .carrier
            .submit_with_policy_time(proof, view.peer(), deadline, time)?;
        Ok(NativePendingPrivate {
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
        pending: &NativePendingPrivate,
        reply: &DirectedAction,
        view: &CommittedView,
    ) -> Result<(), PrivateError> {
        pending.disclosure.check(view)?;
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
        view: &CommittedView,
    ) -> Result<Option<(NativePendingPrivate, DirectedAction)>, PrivateError> {
        let result = self.carrier.poll_events(proof, view.peer());
        self.fenced_event(result, view)
    }
    /// Held poll continuation: the owner revalidated the parked original.
    pub(crate) fn poll_events_resumed(
        &mut self,
        session: &tilo_relay::delivery_v3::auth::VerifiedSession,
        original: &crate::https::BufferedRequest,
        view: &CommittedView,
    ) -> Result<Option<(NativePendingPrivate, DirectedAction)>, PrivateError> {
        let result = self
            .carrier
            .poll_events_resumed(session, original, view.peer());
        self.fenced_event(result, view)
    }
    fn fenced_event(
        &mut self,
        result: Result<Option<(PendingPrivate, DirectedAction)>, PrivateError>,
        view: &CommittedView,
    ) -> Result<Option<(NativePendingPrivate, DirectedAction)>, PrivateError> {
        let result = match result {
            Ok(result) => result,
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
            .cloned()
            .ok_or(PrivateError::Unavailable)?;
        self.reconcile();
        disclosure.check(view)?;
        Ok(Some((
            NativePendingPrivate {
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
    /// Cheap pre-check before resuming held events polls (see `Private::pump`).
    pub(crate) fn has_ready_event(&self) -> bool {
        self.carrier.has_ready_event()
    }
    pub(crate) fn retire_session(&mut self, session: [u8; 16]) {
        self.carrier.retire_session(session);
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
impl NativePendingPrivate {
    pub(crate) fn try_reply(&mut self) -> Result<Option<DirectedAction>, PrivateError> {
        self.carrier.try_reply()
    }
    /// Must run inside the actual owner turn, after the parent freshly validates
    /// the original registry continuation. No authorized byte buffer escapes.
    pub(crate) fn respond(
        self,
        view: &CommittedView,
        reply: &DirectedAction,
        request: crate::https::BufferedRequest,
        mut credits: crate::https::ResponseCredits,
    ) -> Result<(), PrivateError> {
        // Final owner policy checks are exercised separately from inert codecs.
        self.disclosure.check(view)?;
        self.carrier.check_reply(reply)?;
        let bytes = tilo_relay::delivery_v3::private_wire::encode_response(
            self.carrier.operation,
            self.carrier.original_request(),
            reply,
        )?;
        self.carrier.check_reply(reply)?;
        self.disclosure.check(view)?;
        if credits.private_reply.is_some() {
            return Err(PrivateError::Unavailable);
        }
        credits.private_reply = Some(self.carrier);
        request
            .respond_with_credits(hyper::StatusCode::OK, bytes, credits)
            .map_err(|_| PrivateError::Unavailable)
    }
}
#[cfg(test)]
mod tests;
