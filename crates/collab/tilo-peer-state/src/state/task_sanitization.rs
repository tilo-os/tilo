//! Governor-owned extraction and signed private-task history replacement.
use super::{StatePeer, SystemTime};
use automerge::transaction::Transactable as _;
mod sanitize;
use sanitize::sanitize_task_room;
#[cfg(test)]
mod tests;

/// A signed replacement must contain neutral automatic reports, not merely
/// carry a valid governor signature over an earlier privacy policy.
pub(super) fn validate_private_task_baseline(room: &automerge::AutoCommit) -> Result<(), String> {
    sanitize::validate_reports(room)
}

impl StatePeer {
    /// Move managed model text out of current room state before republishing it.
    /// Existing manual human coordination is intentionally unchanged.
    /// # Errors
    /// Refuses missing governor custody, unsettled input or malformed room data.
    pub fn extract_native_task_private_text(
        &mut self,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
        known_references: &crate::workspace_task::NativeTaskPrivateReferences,
    ) -> Result<
        std::collections::BTreeMap<String, crate::workspace_task::NativeTaskPrivateText>,
        String,
    > {
        if key.verifying_key().to_bytes() != self.core.authority.device_public_key {
            return Err("private task migration requires exact governor signing custody".into());
        }
        self.certificate(&self.core.authority.device_public_key, now)?;
        if self
            .core
            .pending
            .iter()
            .any(|(_, message, _)| matches!(message, crate::NeighbourMsg::SyncResp { .. }))
        {
            return Err("private task migration requires settled room input".into());
        }
        let room = self
            .core
            .room_doc
            .as_deref()
            .ok_or("private task room absent")?;
        let (mut candidate, extracted) = sanitize_task_room(room, known_references)?;
        let generation = super::sanitized_baseline::binding(room)?
            .map_or(0, |value| value.generation)
            .checked_add(1)
            .ok_or("baseline generation exhausted")?;
        let binding = super::sanitized_baseline::BaselineBinding {
            room: self.core.room.clone(),
            generation,
            prior_chain_hash: super::sanitized_baseline::chain_hash(room),
        };
        candidate
            .put(
                automerge::ROOT,
                super::sanitized_baseline::FIELD,
                automerge::ScalarValue::Bytes(
                    postcard::to_allocvec(&binding).map_err(|e| e.to_string())?,
                ),
            )
            .map_err(|e| e.to_string())?;
        candidate.commit();
        self.core.room_doc = Some(candidate.into());
        Ok(extracted)
    }

    /// Extract private text and return the signed replacement in one owner turn.
    /// Persist private text before publishing the replacement; never append its
    /// room bytes to a journal containing the retired history.
    /// # Errors
    /// Refuses non-governor custody, conflicted or malformed room data.
    pub fn extract_native_task_private_baseline(
        &mut self,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
        known_references: &crate::workspace_task::NativeTaskPrivateReferences,
    ) -> Result<
        (
            std::collections::BTreeMap<String, crate::workspace_task::NativeTaskPrivateText>,
            super::SanitizedRoomBaseline,
        ),
        String,
    > {
        let mut candidate = self.clone();
        let extracted = candidate.extract_native_task_private_text(key, now, known_references)?;
        let baseline = candidate.sanitized_room_baseline(key, now)?;
        *self = candidate;
        Ok((extracted, baseline))
    }
}
