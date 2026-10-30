//! A channel's presence on the wire: what one endpoint publishes, and the
//! events everyone else's presence reaches it as.

use runtime_collab::wire::Event;

use super::{Feed, Refusal};

use super::super::*;

/// A channel's presence as one admitted endpoint has it: what it publishes,
/// and the feed of everyone else's.
pub(in crate::collab_broker_capnp) struct PresenceSession {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
    verified_identity: AdmittedJoinIdentity,
    /// Broker-owned monotonic sequence per invocation. Publishers describe
    /// phases and targets but cannot reorder or resurrect their visual state.
    interaction_sequences: Mutex<HashMap<tilo_actions::InvocationId, u64>>,
}

impl PresenceSession {
    pub(in crate::collab_broker_capnp) fn new(
        state: Arc<CapnpBrokerState>,
        channel_id: String,
        peer_id: u64,
        verified_identity: AdmittedJoinIdentity,
    ) -> Self {
        Self {
            state,
            channel_id,
            peer_id,
            verified_identity,
            interaction_sequences: Mutex::new(HashMap::new()),
        }
    }

    /// Widened to room scope — presence is room-wide, only document
    /// payloads are confined to a record.
    fn route(&self) -> CrdtRoute {
        self.full_route().to_cluster_scope()
    }

    /// Unwidened, so peer replication carries the full app/cluster route.
    fn full_route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }

    fn revalidate(&self) -> Result<(), String> {
        let route = if self.verified_identity.host_scope.is_some() {
            self.full_route()
        } else {
            self.route()
        };
        self.state
            .revalidate_admission(&route, &self.verified_identity)
            .map(|_| ())
            .map_err(Refusal::refusal)
    }

    /// Publish one keyed presence update after the broker's own checks: a
    /// status is trimmed and bounded, an interaction is attributed to this
    /// endpoint and sequenced, and the desktop's own presence is refused.
    pub(in crate::collab_broker_capnp) fn publish(
        &self,
        key: PresenceKey,
        payload: PresencePayload,
    ) -> Result<(), String> {
        self.revalidate()?;
        let payload = match payload {
            PresencePayload::Status { text, automatic } => {
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.chars().count() > ui::MAX_STATUS_CHARS {
                    return Err("status exceeds local protocol bound".into());
                }
                PresencePayload::Status { text, automatic }
            }
            PresencePayload::Interaction(update) => {
                PresencePayload::Interaction(Box::new(self.attributed(*update)?))
            }
            PresencePayload::Availability { .. } => {
                return Err("desktop presence is the shell's, never an app's".into());
            }
            other => other,
        };
        let route = self.route();
        self.state
            .publish_presence(&self.full_route(), &key, &payload);
        let event = CollabEvent::RemotePresenceUpdate {
            user_id: CapnpBrokerState::participant_id(&self.verified_identity),
            key,
            payload,
        };
        if let Some(fanout) = self.state.fanout.upgrade() {
            fanout.fanout_presence(&event, self.peer_id, &route, &self.full_route());
        }
        Ok(())
    }

    /// `update` as this endpoint's: its actor is the admitted one, and its
    /// sequence the broker's next for the invocation.
    fn attributed(
        &self,
        update: tilo_actions::SemanticInteraction,
    ) -> Result<tilo_actions::SemanticInteraction, String> {
        let actor = self.verified_identity.broker_actor();
        let normalized_actor = tilo_actions::InteractionActor::new(
            CapnpBrokerState::participant_id(&self.verified_identity),
            if actor.is_agent {
                tilo_actions::ActorKind::Agent
            } else {
                tilo_actions::ActorKind::Person
            },
        );
        let mut sequences = self
            .interaction_sequences
            .lock()
            .map_err(|_| "interaction sequence registry unavailable".to_owned())?;
        let current = sequences.entry(update.invocation).or_insert(0);
        *current = current.saturating_add(1);
        let sequence = *current;
        drop(sequences);
        let update = update.normalized_by_broker(normalized_actor, sequence);
        update
            .validate()
            .map_err(|error| format!("invalid interaction: {error}"))?;
        Ok(update)
    }

    /// Everyone else's presence in this room from now on.
    pub(in crate::collab_broker_capnp) fn subscribe(
        &self,
    ) -> Result<Feed<PresenceEnvelope>, String> {
        self.revalidate()?;
        Ok(Feed::new(
            HostWorkerReadGuard::for_channel(&self.state, self.peer_id, &self.verified_identity),
            self.state
                .subscribe_presence(&self.channel_id, self.peer_id),
        ))
    }
}

/// Another participant's presence as the wire carries it; `None` for what
/// no app receives or what does not fit the wire.
pub(in crate::collab_broker_capnp) fn presence_event(envelope: PresenceEnvelope) -> Option<Event> {
    if !envelope.payload.reaches_apps() {
        return None;
    }
    Some(Event::Presence {
        user_id: envelope.user_id,
        presence: collab_types::PeerPresence {
            key: envelope.key_bytes,
            payload: crate::collab_presence::payload_to_peer(&envelope.payload)?,
        },
    })
}
