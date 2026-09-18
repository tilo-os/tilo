//! Recover an expired, durable incremental only after fresh governed coverage.
//! Original signed evidence stays in the outbox until the governor covers it.
use crate::{DocumentMutation, NeighbourMsg, StatePeer};
use ed25519_dalek::SigningKey;
use web_time::{Duration, SystemTime};
#[derive(Debug, Default)]
pub struct OutboxStatus {
    pub pending: usize,
    pub blocked_expired: usize,
}
pub fn fresh(message: &NeighbourMsg, now: SystemTime) -> bool {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return false;
    };
    let earliest = now
        .checked_sub(Duration::from_secs(300))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let latest = now.checked_add(Duration::from_secs(300)).unwrap_or(now);
    envelope.timestamp >= earliest && envelope.timestamp <= latest
}
fn replacement(original: &NeighbourMsg, candidate: &NeighbourMsg, now: SystemTime) -> bool {
    match (original, candidate) {
        (
            NeighbourMsg::AppDocument {
                app_id: a,
                cluster_id: c,
                record_id: r,
                envelope: old,
                ..
            },
            NeighbourMsg::AppDocument {
                app_id: b,
                cluster_id: d,
                record_id: s,
                envelope: new,
                ..
            },
        ) => {
            a == b
                && c == d
                && r == s
                && old.author == new.author
                && old.payload == new.payload
                && old.encryption == new.encryption
                && new.seq > old.seq
                && fresh(candidate, now)
        }
        _ => false,
    }
}
impl StatePeer {
    pub fn outbox_status(&self, outbox: &[NeighbourMsg], now: SystemTime) -> OutboxStatus {
        let pending = outbox
            .iter()
            .filter(|message| !self.covers(message))
            .collect::<Vec<_>>();
        OutboxStatus {
            pending: pending.len(),
            blocked_expired: pending
                .iter()
                .filter(|message| {
                    !fresh(message, now)
                        && !pending.iter().any(|other| replacement(message, other, now))
                })
                .count(),
        }
    }
    pub fn reconcile_outbox(
        &mut self,
        outbox: &[NeighbourMsg],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.clone();
        let mut retained = outbox
            .iter()
            .filter(|message| !candidate.covers(message))
            .cloned()
            .collect::<Vec<_>>();
        for original in retained.clone() {
            if fresh(&original, now)
                || retained
                    .iter()
                    .any(|other| replacement(&original, other, now))
            {
                continue;
            }
            let NeighbourMsg::AppDocument {
                app_id,
                cluster_id,
                record_id,
                envelope,
                ..
            } = &original
            else {
                continue;
            };
            if envelope.author.as_bytes() != &key.verifying_key().to_bytes()
                || !candidate.fresh_checkpoint_for(app_id, record_id, now)
            {
                continue;
            }
            collab_types::verify_op(envelope).map_err(|e| e.to_string())?;
            let payload = collab_types::validate_document_payload_route(
                app_id, cluster_id, record_id, envelope,
            )
            .map_err(|e| e.to_string())?;
            let collab_types::RoutedDocumentPayload::Cleartext(DocumentMutation::Incremental(
                bytes,
            )) = payload
            else {
                continue;
            };
            // A new authorized envelope, never a changed timestamp/signature on
            // the original. Identical Automerge change hashes make a late old
            // delivery or an acknowledgement race idempotent in either order.
            let renewed = candidate.publish_incremental(app_id, record_id, bytes, key, now)?;
            retained.push(renewed);
        }
        *self = candidate;
        Ok(retained)
    }
}
