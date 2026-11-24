//! Transport-neutral collaboration governance state: both the desktop service and the peer
//! network's drain validate against it, so no socket, framing or RPC type appears here and a
//! transport change cannot fork authorization and audit behavior.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use collab_types::{
    ArtifactId, Ed25519PublicKey, EncryptionMetadata, InboundState, OpId, SignedOp,
    ValidationError, parse_redact_payload,
};

use crate::collab_history::Unsynced;

const AUDIT_LOG_RING_CAPACITY: usize = 256;

#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub envelope: Box<SignedOp>,
    pub rejection: Option<ValidationError>,
    pub at: tilo_clock::time::SystemTime,
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
    pub redacted_at: tilo_clock::time::SystemTime,
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
    pub states: HashMap<ArtifactId, InboundState>,
    pub audit: AuditLog,
    pub redactions: RedactionLog,
    /// Ingress checks of the channels this device's channel encryption service takes part in,
    /// consulted on every local and peer document ingress. Registered before the first op of a
    /// channel is accepted and refreshed on every membership change; the broker holds the check,
    /// never a key.
    pub checks: tilo_workspace_keys::SealChecks,
    /// Durable history of content-key ciphertext, so members (including
    /// ones added later) can page back past the in-memory window.
    pub history: Option<crate::collab_history::CollabHistoryStore>,
    /// The route (app, workspace, record) each artifact's ops were validated on
    /// this session. A history page is only ever answered to that workspace.
    pub routes: HashMap<ArtifactId, crate::route::CrdtRoute>,
    /// The records a sealed write of entered here: from it on, a write to
    /// one in the clear is a downgrade ([`Self::document_order_allowed`]).
    pub sealed: std::collections::HashSet<ArtifactId>,
}

impl BrokerChannels {
    pub fn state_for(&mut self, artifact: ArtifactId) -> &mut InboundState {
        self.states.entry(artifact).or_default()
    }

    pub fn register_inbound_state(&mut self, artifact: ArtifactId, state: InboundState) {
        self.states.insert(artifact, state);
    }

    /// Install or refresh a workspace's ingress check.
    pub fn register_seal_check(
        &mut self,
        workspace: collab_types::WorkspaceId,
        check: key_group::SealCheck,
    ) {
        self.checks.register(workspace, check);
    }

    /// A route with no check has not been opened encrypted on this device
    /// yet. Once its check closes plaintext, this device writes none to it,
    /// including after a restart. Content sealed under any of the channel's
    /// epochs passes: a member's write reaches this device after the
    /// channel's next membership change.
    pub fn document_encryption_allowed(
        &self,
        workspace: &str,
        encryption: Option<&EncryptionMetadata>,
    ) -> bool {
        self.checks.allows_document(workspace, encryption)
    }

    /// Whether another device's write of `artifact`'s record may enter, by
    /// its place in the record's order. A record seals from some write on,
    /// and its cleartext writes before that are the base its sealed writes
    /// are changes over: a device that was away while the record sealed, or
    /// joins it later, reads the record only by taking that base in first,
    /// however it arrives. So a write in the clear enters until a sealed
    /// write of the record has, and never after; a sealed one enters when
    /// the channel's check takes its epoch.
    pub fn document_order_allowed(
        &self,
        workspace: &str,
        artifact: ArtifactId,
        encryption: Option<&EncryptionMetadata>,
    ) -> bool {
        match encryption {
            None => !self.sealed.contains(&artifact),
            Some(_) => self.checks.allows_document(workspace, encryption),
        }
    }

    /// Bind the record of an op validated on `route` to its workspace, when the op is sealed
    /// history. Failure is logged, never fatal: the op is still delivered live. The caller syncs what this wrote once
    /// it has released the channels' lock, and before it delivers the op
    /// ([`Unsynced`]).
    pub fn document_history(&mut self, route: &crate::route::CrdtRoute, op: &SignedOp) -> Unsynced {
        if self.routes.get(&op.artifact) != Some(route) {
            self.routes.insert(op.artifact, route.clone());
        }
        let Some(history) = self.history.as_mut() else {
            return Unsynced::default();
        };
        if !crate::collab_history::CollabHistoryStore::retains(op) {
            return Unsynced::default();
        }
        history
            .bind_document(op.artifact, &route.cluster_id, &route.app_id, &route.document_id)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, artifact = ?op.artifact, "collab history: record binding failed");
                Unsynced::default()
            })
    }

    /// The records of trees ([`collab_types::document::DocumentId::tree`]: a chain's
    /// parts, a container's children) of the app `subscriber` names in its
    /// workspace that this device holds: ops of them were admitted here this
    /// session ([`Self::document_history`]), or their history is kept on disk
    /// from an earlier one. An app window that subscribes is told them, so
    /// it holds what its workspace held before it ran, such as a chat's messages
    /// a joining device was replayed before any Chat window was open. Empty
    /// for a route that is not an app's in a workspace.
    pub fn tree_records_held(&mut self, subscriber: &crate::route::CrdtRoute) -> Vec<String> {
        if subscriber.app_id.is_empty()
            || !subscriber.document_id.is_empty()
            || !crate::route::CrdtRoute::is_addressable(&subscriber.cluster_id)
        {
            return Vec::new();
        }
        let (app_id, workspace) = (&subscriber.app_id, &subscriber.cluster_id);
        let admitted = self
            .routes
            .values()
            .filter(|route| route.app_id == *app_id && route.cluster_id == *workspace)
            .map(|route| route.document_id.clone());
        let kept = self
            .history
            .as_mut()
            .map(|history| history.records_in(app_id, workspace))
            .unwrap_or_default();
        let mut held: Vec<String> = admitted
            .chain(kept)
            .filter(|record| {
                collab_types::document::DocumentId::parse(record)
                    .and_then(|id| id.tree())
                    .is_some()
            })
            .collect();
        held.sort();
        held.dedup();
        held
    }

    /// The workspace `artifact`'s ops were validated in, this session or, for
    /// durable history, an earlier one. An artifact never bound has no workspace
    /// whose members may page through it.
    pub fn workspace_of(&self, artifact: ArtifactId) -> Option<String> {
        self.routes
            .get(&artifact)
            .map(|route| route.cluster_id.clone())
            .or_else(|| {
                self.history
                    .as_ref()
                    .and_then(|history| history.workspace_of(artifact))
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

/// Where a channel service registers the checks this broker refuses plaintext with.
#[derive(Clone, Default)]
pub struct BrokerSealChecks(pub Arc<Mutex<BrokerChannels>>);

impl tilo_workspace_keys::SealCheckRegistry for BrokerSealChecks {
    fn register_seal_check(
        &self,
        workspace: collab_types::WorkspaceId,
        check: key_group::SealCheck,
    ) {
        if let Ok(mut channels) = self.0.lock() {
            channels.register_seal_check(workspace, check);
        }
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
