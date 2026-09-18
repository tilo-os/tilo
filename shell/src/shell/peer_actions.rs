//! Permissioned peer adapter for the ordinary native action broker.
//! Only inert authenticated requests cross into its bounded LocalSet queue.
use qos_relay::client::{CertifiedAction, CertifiedActionEvent};
use qos_relay::peer_actions::{ActionMessage, DirectedAction};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use uuid::Uuid;

/// Host-owned current certificate/revocation authority. Implementations read
/// the owner's explicit identity store on every check; this is never supplied
/// by a wire request. Signature, expiry, device role, room membership and action
/// policy remain mandatory broker checks independent of this provider.
pub trait CertificateAuthority: Send + Sync {
    fn authorize(
        &self,
        room: Uuid,
        certificate: &qos_user_identity::DeviceCert,
    ) -> Result<(), String>;
}

/// Explicit inputs for an ordinary peer hosting the native broker service.
/// The owner supplies its real publisher and current room tree separately.
#[derive(Clone)]
pub struct NativePeerBrokerConfig {
    pub journal_root: std::path::PathBuf,
    pub certificate_authority: Arc<dyn CertificateAuthority>,
}

const CAPACITY: usize = 32;

#[derive(Clone, Debug)]
pub struct PeerActionQueue {
    sender: mpsc::Sender<Request>,
    permits: Arc<Semaphore>,
    transitions:
        tokio::sync::broadcast::Sender<(CertifiedAction, Option<qos_actions::InvocationUpdate>)>,
}
pub(super) struct Request {
    pub evidence: Option<CertifiedAction>,
    pub poll: Option<qos_actions::InvocationId>,
    pub notification: Option<qos_actions::InvocationUpdate>,
    pub reply: oneshot::Sender<Result<ActionMessage, ()>>,
    _permit: OwnedSemaphorePermit,
}
impl PeerActionQueue {
    pub(super) fn channel() -> (Self, mpsc::Receiver<Request>) {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        (
            Self {
                sender,
                permits: Arc::new(Semaphore::new(CAPACITY)),
                transitions: tokio::sync::broadcast::channel(CAPACITY).0,
            },
            receiver,
        )
    }
    async fn request(
        &self,
        evidence: Option<CertifiedAction>,
        poll: Option<qos_actions::InvocationId>,
        notification: Option<qos_actions::InvocationUpdate>,
    ) -> Result<ActionMessage, ()> {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| ())?;
        let (reply, rx) = oneshot::channel();
        self.sender
            .try_send(Request {
                evidence,
                poll,
                notification,
                reply,
                _permit: permit,
            })
            .map_err(|_| ())?;
        // Timeout is transport uncertainty. It never cancels admitted work.
        tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?
    }

    #[cfg(test)]
    pub(crate) fn transitions_for_test(
        &self,
    ) -> tokio::sync::broadcast::Receiver<(CertifiedAction, Option<qos_actions::InvocationUpdate>)>
    {
        self.transitions.subscribe()
    }

    pub(super) fn publish(&self, evidence: CertifiedAction, update: qos_actions::InvocationUpdate) {
        // Called only after the durable transition barrier. Lag closes the
        // observing session rather than silently skipping streaming items.
        let bounded = ActionMessage::Update {
            update: update.clone(),
        }
        .validate()
            && serde_json::to_vec(&update).is_ok_and(|bytes| {
                bytes.len() <= qos_relay::peer_actions::MAX_PRIVATE_BYTES.saturating_sub(1024)
            });
        let _ = self.transitions.send((evidence, bounded.then_some(update)));
    }

    /// Serve an attached broker on the SAME certified state/private session.
    /// The caller must retain this task alongside its state receiver; failure
    /// must tear down that session, so stale capability cannot survive.
    pub async fn serve(
        &self,
        session: Arc<qos_relay::RelayCoeditSession>,
        room: Uuid,
        host: Uuid,
    ) -> Result<(), ()> {
        let result = self.serve_inner(session.clone(), room, host).await;
        session.close().await;
        result
    }
    async fn serve_inner(
        &self,
        session: Arc<qos_relay::RelayCoeditSession>,
        room: Uuid,
        host: Uuid,
    ) -> Result<(), ()> {
        let ActionMessage::BrokerCatalogue { host: current, .. } =
            self.request(None, None, None).await?
        else {
            return Err(());
        };
        if current != host {
            return Err(());
        }
        session.enable_actions(room).map_err(|_| ())?;
        session.register_action_broker(room, host).map_err(|_| ())?;
        let mut watches: Vec<(CertifiedAction, qos_actions::InvocationId, ActionMessage)> =
            Vec::new();
        let mut transitions = self.transitions.subscribe();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {
                event = session.recv_certified_action() => match event {
                    Some(CertifiedActionEvent::Delivered(evidence)) => {
                        if evidence.action().room != room { return Err(()); }
                        let response = self.request(Some(evidence.clone()), None, None).await?;
                        send(&session, &evidence, response.clone())?;
                        if let ActionMessage::Observation { invocation, ref update, uncertain, .. } = response {
                            if !uncertain && update.as_ref().is_some_and(|u| !u.is_terminal()) {
                                if let Some(watch) = watches.iter_mut().find(|(old, id, _)| old.action().source == evidence.action().source && *id == invocation) { *watch = (evidence, invocation, response); }
                                else if watches.len() < CAPACITY { watches.push((evidence, invocation, response)); }
                                else { return Err(()); }
                            } else { watches.retain(|(old, id, _)| old.action().source != evidence.action().source || *id != invocation); }
                        }
                    }
                    Some(CertifiedActionEvent::Control(qos_relay::transport_v2::EnvelopeV2::Unavailable { request_id, reason, .. })) => {
                        watches.retain(|(evidence, _, _)| evidence.action().request_id != request_id);
                        if reason == qos_relay::peer_actions::UnavailableReason::Backpressure { return Err(()); }
                    },
                    Some(CertifiedActionEvent::Control(_)) => {},
                    None => return Err(()),
                },
                transition = transitions.recv() => {
                    let (source, update) = transition.map_err(|_| ())?;
                    let update = update.ok_or(())?;
                    if source.action().room != room { continue; }
                    if let Some((evidence, invocation, previous)) = watches.iter_mut().find(|(evidence, invocation, _)| evidence.action().source == source.action().source && *invocation == update.invocation()) {
                        if let ActionMessage::Observation { update: Some(prior), .. } = previous {
                            if sequence(&update).zip(sequence(prior)).is_some_and(|(next, old)| next <= old) { continue; }
                        }
                        let response = self.request(Some(evidence.clone()), Some(*invocation), Some(update)).await?;
                        if response != *previous { send(&session, evidence, response.clone())?; *previous = response; }
                    }
                    watches.retain(|(_, _, response)| matches!(response, ActionMessage::Observation { update: Some(update), uncertain: false, .. } if !update.is_terminal()));
                }
                _ = tick.tick() => {
                    self.request(None, None, None).await?;
                }
            }
        }
    }
}
fn sequence(update: &qos_actions::InvocationUpdate) -> Option<u64> {
    match update {
        qos_actions::InvocationUpdate::Progress { sequence, .. }
        | qos_actions::InvocationUpdate::Item { sequence, .. } => Some(*sequence),
        _ => None,
    }
}

fn send(
    session: &qos_relay::RelayCoeditSession,
    evidence: &CertifiedAction,
    message: ActionMessage,
) -> Result<(), ()> {
    let request = evidence.action();
    session
        .send_action(DirectedAction {
            room: request.room,
            request_id: request.request_id,
            source: request.target,
            target: request.source,
            message,
        })
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_peer_facade_queue_bounds_total_inflight_and_disconnects() {
        let (queue, receiver) = PeerActionQueue::channel();
        let mut pending = Vec::new();
        for _ in 0..CAPACITY {
            let queue = queue.clone();
            pending.push(tokio::spawn(async move {
                queue.request(None, None, None).await
            }));
        }
        tokio::task::yield_now().await;
        assert!(queue.request(None, None, None).await.is_err());
        drop(receiver);
        for request in pending {
            assert!(request.await.unwrap().is_err());
        }
        assert!(queue.request(None, None, None).await.is_err());
    }
}
