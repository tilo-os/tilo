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
/// The shared sender selection used by the browser transport and recovery tests.
pub fn publication_batch(
    outbox: &[NeighbourMsg],
    now: SystemTime,
) -> impl Iterator<Item = &NeighbourMsg> {
    outbox.iter().filter(move |message| fresh(message, now))
}
fn replacement(original: &NeighbourMsg, candidate: &NeighbourMsg, now: SystemTime) -> bool {
    match (original, candidate) {
        (
            NeighbourMsg::AppDocument {
                app_id: old_app,
                cluster_id: old_cluster,
                record_id: old_record,
                envelope: old,
                ..
            },
            NeighbourMsg::AppDocument {
                app_id: new_app,
                cluster_id: new_cluster,
                record_id: new_record,
                envelope: new,
                ..
            },
        ) => {
            old_app == new_app
                && old_cluster == new_cluster
                && old_record == new_record
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
    /// True only when causal coverage is backed by the exact governed durable receipt.
    pub fn outbox_message_retired(&self, message: &NeighbourMsg) -> bool {
        let NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } = message
        else {
            return false;
        };
        self.covers(message)
            && self
                .durable_coverage(app_id, record_id)
                .ok()
                .flatten()
                .is_some()
    }

    pub fn outbox_status(&self, outbox: &[NeighbourMsg], now: SystemTime) -> OutboxStatus {
        let pending = outbox
            .iter()
            .filter(|message| !self.outbox_message_retired(message))
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
    /// Reconcile a durable outbox with this peer, returning the queue to send.
    ///
    /// # Errors
    ///
    /// Returns an error if a queued message is not authentic or a successor cannot be
    /// signed.
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
            if matches!(&original, NeighbourMsg::AppDocument { cluster_id, .. } if cluster_id != candidate.room())
            {
                return Err("document outbox room mismatch".into());
            }
            // A fresh envelope may already be impossible to deliver: renewing
            // its missing predecessor can put the governor's sequence floor
            // above it before its causal changes were ever materialized.
            if (fresh(&original, now) && !candidate.checkpoint_sequence_covers(&original))
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
            let payload = collab_types::validate_whole_document_payload_route(
                app_id, cluster_id, record_id, envelope,
            )
            .map_err(|e| e.to_string())?;
            let collab_types::WholeDocumentPayload::Cleartext(DocumentMutation::Incremental(bytes)) =
                payload
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
