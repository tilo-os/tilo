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

    /// Read the cold-history anchor from actual admitted governance state.
    ///
    /// # Errors
    /// A malformed anchor must not be mistaken for an empty index.
    pub fn workspace_effect_history_root(&self) -> Result<Option<[u8; 32]>, String> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("effect room is absent")?;
        match doc
            .get(&ROOT, "effect_history_root_v1")
            .map_err(|e| e.to_string())?
        {
            None => Ok(None),
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => bytes
                    .as_slice()
                    .try_into()
                    .map(Some)
                    .map_err(|_| "malformed effect history root".into()),
                _ => Err("malformed effect history root".into()),
            },
            _ => Err("malformed effect history root".into()),
        }
    }

    /// Anchor retained cold history under genuine current governor signing custody.
    ///
    /// # Errors
    /// Invalid governance custody or a failed CRDT write.
    pub fn set_workspace_effect_history_root(
        &mut self,
        root: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        self.require_governance_writer(key, now)?;
        let mut candidate = self
            .core
            .room_doc
            .as_deref()
            .ok_or("effect room is absent")?
            .clone();
        candidate
            .put(
                &ROOT,
                "effect_history_root_v1",
                ScalarValue::Bytes(root.to_vec()),
            )
            .map_err(|e| e.to_string())?;
        self.core.room_doc = Some(candidate.into());
        Ok(())
    }
}
