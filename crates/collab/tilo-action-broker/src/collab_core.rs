//! Transport-neutral collaboration governance state.
//!
//! Both the native Cap'n Proto service and peer-network drain validate against
//! this state. It deliberately contains no socket, framing, RPC, or UI request
//! types, so transport retirement cannot fork authorization and audit behavior.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use collab_types::{
    ArtifactId, ChannelId, ChannelState, Ed25519PublicKey, EncryptionMetadata, OpId, OpKind,
    SignedOp, ValidationError, parse_redact_payload,
};

const AUDIT_LOG_RING_CAPACITY: usize = 256;

#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub envelope: Box<SignedOp>,
    pub rejection: Option<ValidationError>,
    pub at: std::time::SystemTime,
}

#[derive(Default)]
pub struct AuditLog {
    pub entries: HashMap<ArtifactId, VecDeque<AuditEntry>>,
}

impl AuditLog {
    pub fn record(&mut self, artifact: ArtifactId, entry: AuditEntry) {
        let buf = self.entries.entry(artifact).or_default();
        if buf.len() >= AUDIT_LOG_RING_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    pub fn for_artifact(&self, artifact: ArtifactId) -> Vec<AuditEntry> {
        self.entries
            .get(&artifact)
            .map(|entries| entries.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub struct RedactionRecord {
    pub redact_envelope: Box<SignedOp>,
    pub target_op_id: OpId,
    pub target_op_hash: [u8; 32],
    pub redactor_pk: Ed25519PublicKey,
    pub reason: String,
    pub redacted_at: std::time::SystemTime,
}

#[derive(Default)]
pub struct RedactionLog {
    pub entries: HashMap<ArtifactId, VecDeque<RedactionRecord>>,
}

impl RedactionLog {
    pub fn record(&mut self, artifact: ArtifactId, record: RedactionRecord) {
        let buf = self.entries.entry(artifact).or_default();
        if buf.len() >= AUDIT_LOG_RING_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(record);
    }

    pub fn for_artifact(&self, artifact: ArtifactId) -> Vec<RedactionRecord> {
        self.entries
            .get(&artifact)
            .map(|entries| entries.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[derive(Default)]
pub struct BrokerChannels {
    pub states: HashMap<ArtifactId, ChannelState>,
    pub audit: AuditLog,
    pub redactions: RedactionLog,
    /// Registered per-route MLS journals. Their persisted gate is consulted
    /// on every local and peer document ingress; the broker holds no MLS key.
    pub governance: HashMap<ChannelId, channel_crypto::GovernanceJournal>,
    /// Ingress gates of the channels this device's channel encryption
    /// service stewards or takes part in. Registered before the first op of
    /// a channel is accepted and refreshed on every membership change; the
    /// broker holds the gate, never a key.
    pub gates: HashMap<ChannelId, channel_crypto::ChannelGate>,
    /// Durable history of content-key ciphertext, so members (including
    /// ones added later) can page back past the in-memory window.
    pub history: Option<super::collab_history::CollabHistoryStore>,
    /// History pages this device asked its peers for, by artifact: the
    /// `before_seq` bound and when the request left. Answers are accepted
    /// only while a request is outstanding.
    pub peer_history_requests: HashMap<ArtifactId, (u64, std::time::Instant)>,
    /// The route (app, room, record) each artifact's ops were validated on
    /// this session. A history page is only ever answered to that room.
    pub routes: HashMap<ArtifactId, crate::route::CrdtRoute>,
}

/// How long a peer history request stays open for answers.
const PEER_HISTORY_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// Minimum spacing between repeats of the same peer history request.
const PEER_HISTORY_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

impl BrokerChannels {
    pub fn state_for(&mut self, artifact: ArtifactId) -> &mut ChannelState {
        self.states.entry(artifact).or_default()
    }

    pub fn register_channel_state(&mut self, artifact: ArtifactId, state: ChannelState) {
        self.states.insert(artifact, state);
    }

    /// Install or refresh a channel's ingress gate.
    pub fn register_channel_gate(&mut self, channel: ChannelId, gate: channel_crypto::ChannelGate) {
        self.gates.insert(channel, gate);
    }

    /// A route with neither a registered journal nor a gate has not been
    /// opened encrypted on this device yet. Once either closes its plaintext
    /// gate, no plaintext envelope (and no content sealed before the latest
    /// membership change) can pass either ingress path, including after a
    /// restart.
    pub fn document_encryption_allowed(
        &self,
        channel: ChannelId,
        encryption: Option<&EncryptionMetadata>,
    ) -> bool {
        self.governance
            .get(&channel)
            .is_none_or(|journal| journal.accepts_document_encryption(encryption, channel))
            && self
                .gates
                .get(&channel)
                .is_none_or(|gate| gate.accepts(encryption, channel))
    }

    /// The verdict on an op for `channel` from its journal's gate and the
    /// registered one ([`channel_crypto::admission`]). This broker holds
    /// every channel's whole governance state, so a channel with no gate is
    /// one that is not encrypted.
    pub fn document_admission(
        &self,
        channel: ChannelId,
        encryption: Option<&EncryptionMetadata>,
    ) -> collab_types::inbound::OpAdmission {
        let gates = self
            .governance
            .get(&channel)
            .map(channel_crypto::GovernanceJournal::gate)
            .into_iter()
            .chain(self.gates.get(&channel).copied());
        channel_crypto::admission(gates, true, channel, encryption)
    }

    /// [`Self::document_encryption_allowed`] for history a workspace host
    /// replayed in its own order (see `ChannelGate::accepts_history`).
    pub fn document_history_allowed(
        &self,
        channel: ChannelId,
        encryption: Option<&EncryptionMetadata>,
    ) -> bool {
        self.governance
            .get(&channel)
            .is_none_or(|journal| journal.gate().accepts_history(encryption, channel))
            && self
                .gates
                .get(&channel)
                .is_none_or(|gate| gate.accepts_history(encryption, channel))
    }

    /// Append an op validated on `route` to durable history when it belongs
    /// there, bound to its record and room. Failure is logged, never fatal:
    /// the op is still delivered live.
    pub fn record_history(&mut self, route: &crate::route::CrdtRoute, op: &SignedOp) {
        if self.routes.get(&op.artifact) != Some(route) {
            self.routes.insert(op.artifact, route.clone());
        }
        if super::collab_history::CollabHistoryStore::retains(op)
            && let Some(history) = self.history.as_mut()
            && let Err(error) = history.bind_record(
                op.artifact,
                &route.cluster_id,
                &route.app_id,
                &route.record_id,
            )
        {
            tracing::warn!(%error, artifact = ?op.artifact, "collab history: record binding failed");
        }
        if let Some(history) = self.history.as_mut()
            && let Err(error) = history.record(op)
        {
            tracing::warn!(%error, artifact = ?op.artifact, "collab history: append failed");
        }
    }

    /// One scrollback page for `artifact`: the in-memory window merged with
    /// durable history, newest `max` ops below `before_seq`, ascending, plus
    /// whether nothing older remains. Redacted ops keep their marker.
    pub fn scrollback_page(
        &mut self,
        artifact: ArtifactId,
        before_seq: u64,
        max: u16,
    ) -> (Vec<SignedOp>, bool) {
        let (memory, memory_exhausted) = self
            .state_for(artifact)
            .scrollback_for(artifact, before_seq, max);
        let Some(history) = self.history.as_mut() else {
            return (memory, memory_exhausted);
        };
        let (durable, durable_exhausted) = match history.page_before(artifact, before_seq, max) {
            Ok(page) => page,
            Err(error) => {
                tracing::warn!(%error, ?artifact, "collab history: read failed");
                return (memory, memory_exhausted);
            }
        };
        let mut seen = std::collections::HashSet::new();
        let union: Vec<SignedOp> = memory
            .into_iter()
            .chain(durable)
            .filter(|op| seen.insert((op.author.0, op.seq)))
            .collect();
        let (mut page, union_exhausted) =
            collab_types::history::page_before(&union, artifact, before_seq, max);
        if let Some(state) = self.states.get(&artifact)
            && let Ok(marker) = collab_types::RedactedMarker::v1().canonical()
        {
            for op in &mut page {
                if state.is_redacted(OpId::from_op(op))
                    && op.kind != collab_types::OpKind::Redaction
                {
                    op.payload.clone_from(&marker);
                }
            }
        }
        (
            page,
            union_exhausted && memory_exhausted && durable_exhausted,
        )
    }

    /// Every sealed write of `artifact` this device holds, oldest first: the
    /// content-key ciphertext admitted here this session and kept on disk
    /// from earlier ones ([`Self::scrollback_page`]), the device's own
    /// writes and those the room's host replayed or relayed. A redacted
    /// write is left out: its marker opens to nothing.
    pub fn sealed_writes(&mut self, artifact: ArtifactId) -> Vec<SignedOp> {
        const PAGE: u16 = 256;
        let mut writes: Vec<SignedOp> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut before = u64::MAX;
        loop {
            let (page, exhausted) = self.scrollback_page(artifact, before, PAGE);
            let Some(oldest) = page.first().map(|op| op.seq) else {
                break;
            };
            // Sequences are per author: the next page starts at the oldest
            // sequence again, so another author's write of the same one is
            // not skipped, and what this page already held is dropped.
            let older: Vec<SignedOp> = page
                .into_iter()
                .filter(|op| seen.insert((op.author.0, op.seq)))
                .collect();
            if older.is_empty() {
                break;
            }
            writes.splice(0..0, older);
            if exhausted {
                break;
            }
            before = oldest.saturating_add(1).min(before.saturating_sub(1));
        }
        let redacted = self.states.get(&artifact);
        writes.retain(|op| {
            super::collab_history::CollabHistoryStore::retains(op)
                && op.kind != OpKind::Redaction
                && !redacted.is_some_and(|state| state.is_redacted(OpId::from_op(op)))
        });
        writes
    }

    /// Whether this device holds any sealed write of `artifact`
    /// ([`Self::sealed_writes`]), without reading them all.
    pub fn holds_sealed_writes(&mut self, artifact: ArtifactId) -> bool {
        let retained = super::collab_history::CollabHistoryStore::retains;
        if self
            .states
            .get(&artifact)
            .is_some_and(|state| state.op_log.iter().any(retained))
        {
            return true;
        }
        self.history.as_mut().is_some_and(|history| {
            history
                .page_before(artifact, u64::MAX, 1)
                .is_ok_and(|(page, _)| page.iter().any(retained))
        })
    }

    /// The records of trees ([`ui::record::RecordId::tree`]: a chain's
    /// parts, a container's children) of the app `subscriber` names in its
    /// room that this device holds: ops of them were admitted here this
    /// session ([`Self::record_history`]), or their history is kept on disk
    /// from an earlier one. An app window that subscribes is told them, so
    /// it holds what its room held before it ran, such as a chat's messages
    /// a joining device was replayed before any Chat window was open. Empty
    /// for a route that is not an app's in a room.
    pub fn tree_records_held(&mut self, subscriber: &crate::route::CrdtRoute) -> Vec<String> {
        if subscriber.app_id.is_empty()
            || !subscriber.record_id.is_empty()
            || !crate::route::CrdtRoute::is_addressable(&subscriber.cluster_id)
        {
            return Vec::new();
        }
        let (app_id, room) = (&subscriber.app_id, &subscriber.cluster_id);
        let admitted = self
            .routes
            .values()
            .filter(|route| route.app_id == *app_id && route.cluster_id == *room)
            .map(|route| route.record_id.clone());
        let kept = self
            .history
            .as_mut()
            .map(|history| history.records_in(app_id, room))
            .unwrap_or_default();
        let mut held: Vec<String> = admitted
            .chain(kept)
            .filter(|record| {
                ui::record::RecordId::parse(record)
                    .and_then(|id| id.tree())
                    .is_some()
            })
            .collect();
        held.sort();
        held.dedup();
        held
    }

    /// The room `artifact`'s ops were validated in, this session or, for
    /// durable history, an earlier one. An artifact never bound has no room
    /// whose members may page through it.
    pub fn room_of(&self, artifact: ArtifactId) -> Option<String> {
        self.routes
            .get(&artifact)
            .map(|route| route.cluster_id.clone())
            .or_else(|| {
                self.history
                    .as_ref()
                    .and_then(|history| history.room_of(artifact))
            })
    }

    /// Whether to ask peers for history of `artifact` below `before_seq`:
    /// only with durable history to receive it into, and not again while an
    /// identical request is fresh. Records the request when it says yes.
    pub fn request_peer_history(&mut self, artifact: ArtifactId, before_seq: u64) -> bool {
        if self.history.is_none() {
            return false;
        }
        let now = std::time::Instant::now();
        if let Some((bound, at)) = self.peer_history_requests.get(&artifact)
            && *bound == before_seq
            && now.saturating_duration_since(*at) < PEER_HISTORY_RETRY
        {
            return false;
        }
        self.peer_history_requests
            .insert(artifact, (before_seq, now));
        true
    }

    /// Take history ops a peer sent in answer to an outstanding request into
    /// durable history. Only signed content-key ciphertext of the requested
    /// artifact below the requested bound is kept; returns how many ops were
    /// new. The broker still holds no key: these are opaque to it.
    pub fn accept_peer_history(&mut self, artifact: ArtifactId, ops: &[SignedOp]) -> usize {
        let Some((bound, at)) = self.peer_history_requests.get(&artifact).copied() else {
            return 0;
        };
        if std::time::Instant::now().saturating_duration_since(at) > PEER_HISTORY_WINDOW {
            self.peer_history_requests.remove(&artifact);
            return 0;
        }
        let room = self
            .routes
            .get(&artifact)
            .map(|route| route.cluster_id.clone());
        let Some(history) = self.history.as_mut() else {
            return 0;
        };
        if let Some(room) = room
            && let Err(error) = history.bind_room(artifact, &room)
        {
            tracing::warn!(%error, ?artifact, "collab history: room binding failed");
        }
        let page: Vec<&SignedOp> = ops
            .iter()
            .filter(|op| {
                op.artifact == artifact
                    && op.seq < bound
                    && super::collab_history::CollabHistoryStore::retains(op)
                    && collab_types::verify_op(op).is_ok()
            })
            .collect();
        // One data sync for the whole page rather than one per op.
        history
            .record_all_if_new(artifact, &page)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, ?artifact, "collab history: peer page not stored");
                0
            })
    }
}

/// Apply a validated Redact envelope to the shared state and durable audit
/// record. All ingress transports call this same mutation.
pub fn apply_redaction_from_envelope(
    envelope: &SignedOp,
    artifact: ArtifactId,
    channels: &Arc<Mutex<BrokerChannels>>,
) {
    let req = match parse_redact_payload(envelope) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(?artifact, %error, "malformed Redact payload after validation");
            return;
        }
    };
    if let Ok(mut channels) = channels.lock() {
        let state = channels.state_for(artifact);
        match state.apply_redaction(&req) {
            Ok(rewritten) => {
                tracing::info!(?artifact, target = ?req.target_op_id, rewritten, "applied redaction");
            }
            Err(error) => {
                tracing::warn!(?artifact, target = ?req.target_op_id, %error, "apply_redaction failed");
                return;
            }
        }
        channels.redactions.record(
            artifact,
            RedactionRecord {
                redact_envelope: Box::new(envelope.clone()),
                target_op_id: req.target_op_id,
                target_op_hash: req.target_op_hash,
                redactor_pk: req.redacted_by,
                reason: req.reason,
                redacted_at: req.redacted_at,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use collab_types::{AttestationId, Ed25519PublicKey, OpKind, SignedOp};

    use super::*;

    fn envelope(artifact: ArtifactId, seq: u64) -> SignedOp {
        SignedOp::new_unsigned(
            // Test fixture payload byte — truncation is fine here (only
            // used as an opaque marker below AUDIT_LOG_RING_CAPACITY+3).
            vec![u8::try_from(seq % 256).unwrap_or(0)],
            artifact,
            Ed25519PublicKey([7; 32]),
            AttestationId::default(),
            OpKind::Change,
            SystemTime::UNIX_EPOCH,
            seq,
        )
    }

    #[test]
    fn audit_is_bounded_per_artifact_and_keeps_the_newest_entries() {
        let artifact = ArtifactId::new();
        let other = ArtifactId::new();
        let mut audit = AuditLog::default();

        for seq in 0..(AUDIT_LOG_RING_CAPACITY as u64 + 3) {
            audit.record(
                artifact,
                AuditEntry {
                    envelope: Box::new(envelope(artifact, seq)),
                    rejection: None,
                    at: SystemTime::UNIX_EPOCH,
                },
            );
        }
        audit.record(
            other,
            AuditEntry {
                envelope: Box::new(envelope(other, 99)),
                rejection: None,
                at: SystemTime::UNIX_EPOCH,
            },
        );

        let entries = audit.for_artifact(artifact);
        assert_eq!(entries.len(), AUDIT_LOG_RING_CAPACITY);
        assert_eq!(entries.first().unwrap().envelope.seq, 3);
        assert_eq!(entries.last().unwrap().envelope.seq, 258);
        assert_eq!(audit.for_artifact(other)[0].envelope.seq, 99);
    }

    #[test]
    fn channel_state_is_shared_by_artifact_not_transport() {
        let artifact = ArtifactId::new();
        let mut channels = BrokerChannels::default();

        let first = std::ptr::from_mut(channels.state_for(artifact));
        let second = std::ptr::from_mut(channels.state_for(artifact));

        assert_eq!(first, second);
        assert_eq!(channels.states.len(), 1);
    }
}
