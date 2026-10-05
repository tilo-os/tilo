use super::{StatePeer, SystemTime};
use ed25519_dalek::SigningKey;
use tilo_desktop_model::WorkspaceEffectReceipt;

impl StatePeer {
    /// Publish a bounded terminal audit receipt using actual governor custody.
    /// This creates no application record or capability.
    ///
    /// # Errors
    /// Refuses unavailable governance custody, conflicting outcomes, exhausted
    /// receipt capacity or a failed CRDT write.
    pub fn apply_workspace_effect_receipt(
        &mut self,
        receipt: &WorkspaceEffectReceipt,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_governance_writer(key, now)?;
        let room = self
            .core
            .room_doc
            .as_deref()
            .ok_or("effect room is absent")?;
        let existing = tilo_desktop_model::projection::workspace_effect_receipts_of(room);
        if let Some(previous) = existing
            .iter()
            .find(|entry| entry.invocation == receipt.invocation)
        {
            return if previous == receipt {
                Ok(false)
            } else {
                Err("effect invocation already has another terminal receipt".into())
            };
        }
        if existing.len() >= 128 {
            return Err("workspace effect receipt capacity exhausted".into());
        }
        let mut candidate = room.clone();
        tilo_desktop_model::projection::project_effect_receipts(
            &mut candidate,
            std::slice::from_ref(receipt),
        )
        .map_err(|error| error.to_string())?;
        if !tilo_desktop_model::projection::workspace_effect_receipts_of(&candidate)
            .contains(receipt)
        {
            return Err("workspace effect receipt write did not materialize".into());
        }
        self.core.room_doc = Some(candidate.into());
        Ok(true)
    }
}
