//! Transport-neutral collaboration governance state.
//!
//! Both the native Cap'n Proto service and peer-network drain validate against
//! this state. It deliberately contains no socket, framing, RPC, or UI request
//! types, so transport retirement cannot fork authorization and audit behavior.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use collab_types::{
    ArtifactId, ChannelState, Ed25519PublicKey, OpId, SignedOp, ValidationError,
    parse_redact_payload,
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
}

impl BrokerChannels {
    pub fn state_for(&mut self, artifact: ArtifactId) -> &mut ChannelState {
        self.states.entry(artifact).or_default()
    }

    pub fn register_channel_state(&mut self, artifact: ArtifactId, state: ChannelState) {
        self.states.insert(artifact, state);
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

    use collab_types::{AttestationId, Ed25519PublicKey, OpClass, SignedOp};

    use super::*;

    fn envelope(artifact: ArtifactId, seq: u64) -> SignedOp {
        SignedOp::new_unsigned(
            vec![seq as u8],
            artifact,
            Ed25519PublicKey([7; 32]),
            AttestationId::default(),
            OpClass::Splice,
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

        let first = channels.state_for(artifact) as *mut ChannelState;
        let second = channels.state_for(artifact) as *mut ChannelState;

        assert_eq!(first, second);
        assert_eq!(channels.states.len(), 1);
    }
}
