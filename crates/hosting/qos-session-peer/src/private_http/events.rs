//! One bounded watcher slot includes its in-flight or ready private event.
//! Delivery is best effort: lost replies recover only through original Status.
use super::*;
use tilo_relay::{
    delivery_v3::{
        auth::Carrier, capabilities, http_proof::Method, private_wire::PRIVATE_EVENTS_TARGET,
    },
    peer_actions::ActionMessage,
};

pub(super) struct Watch {
    session: [u8; 16],
    id: WatchId,
    evidence: CertifiedAction,
    policy_time: Option<PolicyTime>,
    operation: PrivateOperation,
    previous: ActionMessage,
    pending: Option<PendingPrivate>,
    ready: Option<DirectedAction>,
}
fn active(message: &ActionMessage) -> bool {
    matches!(message,ActionMessage::Observation{update:Some(update),uncertain:false,..} if !update.is_terminal())
}
fn sequence(message: &ActionMessage) -> Option<u64> {
    match message {
        ActionMessage::Observation {
            update:
                Some(
                    tilo_actions::InvocationUpdate::Progress { sequence, .. }
                    | tilo_actions::InvocationUpdate::Item { sequence, .. },
                ),
            ..
        } => Some(*sequence),
        _ => None,
    }
}
impl PrivateService {
    pub fn observe_reply(
        &mut self,
        pending: &PendingPrivate,
        reply: &DirectedAction,
    ) -> Result<WatchUpdate, PrivateError> {
        if self.is_closed() || !Arc::ptr_eq(&self.closed, &pending.closed) {
            return Err(PrivateError::Unavailable);
        }
        pending.validate_response_unqualified(reply)?;
        let ActionMessage::Observation { invocation, .. } = &reply.message else {
            return Ok(WatchUpdate::Unchanged);
        };
        let invocation = *invocation;
        let index = self.watches.iter().position(|w| {
            w.session == pending.session
                && w.evidence.action().source == pending.pending.evidence().action().source
                && w.evidence.action().message.invocation() == Some(invocation)
        });
        if !active(&reply.message) {
            if let Some(index) = index {
                self.watches.remove(index);
            }
            return Ok(WatchUpdate::Unchanged);
        }
        if let Some(index) = index {
            // A fresh Status may observe this invocation, but cannot replace
            // the older watch's clock origin or disclosure fence.
            if self.watches[index].pending.is_none() {
                self.watches[index].previous = reply.message.clone();
            }
            return Ok(WatchUpdate::Unchanged);
        }
        if index.is_none() && self.watches.len() == 32 {
            self.close();
            return Err(PrivateError::Unavailable);
        }
        let id = WatchId(self.next_watch);
        let Some(next) = self.next_watch.checked_add(1) else {
            self.close();
            return Err(PrivateError::Unavailable);
        };
        self.next_watch = next;
        let watch = Watch {
            id,
            session: pending.session,
            evidence: pending.pending.evidence().clone(),
            policy_time: pending.pending.policy_time().cloned(),
            operation: pending.operation,
            previous: reply.message.clone(),
            pending: None,
            ready: None,
        };
        if let Some(index) = index {
            self.watches[index] = watch;
        } else {
            self.watches.push(watch);
        }
        Ok(WatchUpdate::Installed(id))
    }
    pub fn pump(&mut self) -> Result<(), PrivateError> {
        let result = self.pump_inner();
        if result.is_err() {
            self.close();
        }
        result
    }
    fn pump_inner(&mut self) -> Result<(), PrivateError> {
        if self.is_closed() {
            return Err(PrivateError::Unavailable);
        }
        for watch in &mut self.watches {
            if watch.ready.is_some() {
                continue;
            }
            if let Some(pending) = &mut watch.pending {
                if let Some(reply) = pending.try_reply()? {
                    pending.validate_response_unqualified(&reply)?;
                    if reply.message == watch.previous {
                        watch.pending = None;
                    } else {
                        watch.ready = Some(reply);
                    }
                }
            }
        }
        // Do not consume another transition until all recipients of this one
        // have drained. The actual bounded broker broadcast detects lag.
        if self.watches.iter().any(|w| w.pending.is_some()) {
            return Ok(());
        }
        for _ in 0..32 {
            let Some(transition) = self.transitions.try_next()? else {
                break;
            };
            let source = transition.source().action();
            let invocation = transition.update().invocation();
            let next = match transition.update() {
                tilo_actions::InvocationUpdate::Progress { sequence, .. }
                | tilo_actions::InvocationUpdate::Item { sequence, .. } => Some(*sequence),
                _ => None,
            };
            let mut submitted = false;
            for watch in &mut self.watches {
                let original = watch.evidence.action();
                if original.room != source.room
                    || original.source != source.source
                    || original.target != source.target
                    || original.message.invocation() != Some(invocation)
                {
                    continue;
                }
                if next
                    .zip(sequence(&watch.previous))
                    .is_some_and(|(new, old)| new <= old)
                {
                    continue;
                }
                let deadline = Instant::now() + std::time::Duration::from_secs(5);
                let pending = match &watch.policy_time {
                    Some(time) => self.queue.submit_transition_with_policy_time(
                        watch.evidence.clone(),
                        transition.clone(),
                        deadline,
                        time.clone(),
                    )?,
                    None => self.queue.submit_transition(
                        watch.evidence.clone(),
                        transition.clone(),
                        deadline,
                    )?,
                };
                watch.pending = Some(PendingPrivate {
                    pending,
                    queue: self.queue.clone(),
                    operation: watch.operation,
                    session: watch.session,
                    watch: Some(watch.id),
                    host: self.host,
                    closed: self.closed.clone(),
                });
                submitted = true;
            }
            if submitted {
                break;
            }
        }
        Ok(())
    }
    pub fn poll_events(
        &mut self,
        request: VerifiedRequest<'_>,
        peer: &qos_peer_state::StatePeer,
    ) -> Result<Option<(PendingPrivate, DirectedAction)>, PrivateError> {
        self.poll_events_for(request.session(), request.request(), peer)
    }
    /// A held original poll, resumed by the owner. The caller must have freshly
    /// revalidated its parked continuation; the exact original bytes are rechecked.
    pub fn poll_events_resumed(
        &mut self,
        session: &tilo_relay::delivery_v3::auth::VerifiedSession,
        original: &crate::https::BufferedRequest,
        peer: &qos_peer_state::StatePeer,
    ) -> Result<Option<(PendingPrivate, DirectedAction)>, PrivateError> {
        let exact = tilo_relay::delivery_v3::http_proof::Request::new(
            original.method().as_str(),
            original.target(),
            original.body(),
        )
        .map_err(|_| PrivateError::Unavailable)?;
        self.poll_events_for(session, &exact, peer)
    }
    fn poll_events_for(
        &mut self,
        session: &tilo_relay::delivery_v3::auth::VerifiedSession,
        exact: &tilo_relay::delivery_v3::http_proof::Request<'_>,
        peer: &qos_peer_state::StatePeer,
    ) -> Result<Option<(PendingPrivate, DirectedAction)>, PrivateError> {
        if self.is_closed() {
            return Err(PrivateError::Unavailable);
        }
        let binding = session.binding();
        if binding.carrier != Carrier::Https
            || exact.method() != Method::Get
            || exact.target() != PRIVATE_EVENTS_TARGET
            || !exact.body().is_empty()
            || !capabilities::validate(session.capabilities())
            || session.capabilities() & capabilities::PRIVATE_COMMANDS_V1 == 0
        {
            return Err(WireError::Direction.into());
        }
        if peer.admitted_room().is_none()
            || !tilo_relay::room_workspace_id_matches(peer.room(), binding.workspace)
            || peer.authority().user_public_key != binding.workspace_root
        {
            return Err(WireError::Scope.into());
        }
        let room = uuid::Uuid::parse_str(peer.room()).map_err(|_| WireError::Scope)?;
        let Some(index) = self.watches.iter().position(|w| {
            w.session == session.session_id()
                && w.evidence.action().room == room
                && w.evidence.action().source == binding.device
                && w.evidence.action().target == peer.authority().device_public_key
                && w.ready.is_some()
        }) else {
            return Ok(None);
        };
        let watch = &mut self.watches[index];
        let pending = watch.pending.take().ok_or(PrivateError::Unavailable)?;
        let reply = watch.ready.take().ok_or(PrivateError::Unavailable)?;
        if let Err(error) = pending.validate_response_unqualified(&reply) {
            self.close();
            return Err(error);
        }
        if active(&reply.message) {
            watch.previous = reply.message.clone();
        } else {
            self.watches.remove(index);
        }
        Ok(Some((pending, reply)))
    }
    /// Whether any watch holds a ready event. A held events poll can only
    /// become answerable with an event while this is true.
    pub fn has_ready_event(&self) -> bool {
        self.watches.iter().any(|watch| watch.ready.is_some())
    }
    pub fn live_watch_ids(&self) -> impl Iterator<Item = WatchId> + '_ {
        self.watches.iter().map(|watch| watch.id)
    }
    pub fn retire_session(&mut self, session: [u8; 16]) {
        self.watches.retain(|watch| watch.session != session);
    }
}
