//! What an app says of its person on its connection: one frame of the workspace's live session,
//! checked and attributed here, then said to the workspace's other apps and devices.

use collab_types::presence::Presence;

use super::{ChannelSession, Refusal};

use super::super::*;

/// What a connection said on one record: the keys it said something under, which it leaves when
/// it goes, and the broker's sequence of each invocation it narrated.
#[derive(Default)]
pub(in crate::apps::collab_app_broker) struct Said {
    keys: RefCell<HashSet<PresenceKey>>,
    /// Publishers describe phases and targets but cannot reorder or resurrect their visual
    /// state: the sequence is the broker's.
    sequences: RefCell<HashMap<tilo_actions::InvocationId, u64>>,
}

impl ChannelSession {
    /// Say `frame`, the app's word of its person in its workspace, after the broker's own
    /// checks: it is the app's own, in its own workspace; a status is trimmed and bounded; an
    /// interaction is attributed to this connection and sequenced; the desktop's own word is
    /// refused.
    pub(in crate::apps::collab_app_broker) fn say(&self, frame: Presence) -> Result<(), String> {
        let route = self.route();
        if frame.app != route.app_id || frame.cluster != route.cluster_id {
            return Err("an app says only its own word, in its own workspace".into());
        }
        if frame.on_behalf.is_some() || frame.member.is_some() {
            return Err("an app speaks for its own member, never another".into());
        }
        self.state
            .revalidate_admission(&route.to_cluster_scope(), &self.verified_identity)
            .map_err(Refusal::refusal)?;
        let payload = match PresencePayload::from_live(frame.payload)? {
            Some(PresencePayload::Status { text, automatic }) => {
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.chars().count() > collab_types::presence::MAX_STATUS_CHARS {
                    return Err("status exceeds local protocol bound".into());
                }
                PresencePayload::Status { text, automatic }
            }
            Some(PresencePayload::Interaction(update)) => {
                PresencePayload::Interaction(Box::new(self.attributed(*update)?))
            }
            Some(payload) if payload.reaches_apps() => payload,
            _ => return Err("the desktop's own word is the shell's, never an app's".into()),
        };
        if matches!(payload, PresencePayload::Left) {
            self.said.keys.borrow_mut().remove(&frame.key);
        } else {
            self.said.keys.borrow_mut().insert(frame.key.clone());
        }
        self.tell(frame.key, payload);
        Ok(())
    }

    /// Say `payload` under `key` as this connection's member, to everyone but this connection.
    fn tell(&self, key: PresenceKey, payload: PresencePayload) {
        if let Some(fanout) = self.state.fanout.upgrade() {
            let member = AppBrokerState::member_id(&self.verified_identity);
            fanout.said(&member, key, payload, self.connection_id, &self.route());
        }
    }

    /// `update` as this connection's: its actor is the admitted one, and its sequence the
    /// broker's next for the invocation.
    fn attributed(
        &self,
        update: tilo_actions::SemanticInteraction,
    ) -> Result<tilo_actions::SemanticInteraction, String> {
        let actor =
            tilo_actions::Acting::member(AppBrokerState::member_id(&self.verified_identity));
        let sequence = {
            let mut sequences = self.said.sequences.borrow_mut();
            let current = sequences.entry(update.invocation).or_insert(0);
            *current = current.saturating_add(1);
            *current
        };
        let update = update.normalized_by_broker(actor, sequence);
        update
            .validate()
            .map_err(|error| format!("invalid interaction: {error}"))?;
        Ok(update)
    }
}

impl Drop for ChannelSession {
    /// A connection leaving its record leaves everything it said there: nobody is shown at a
    /// caret whose window closed.
    fn drop(&mut self) {
        let keys = std::mem::take(&mut *self.said.keys.borrow_mut());
        for key in keys {
            self.tell(key, PresencePayload::Left);
        }
    }
}
