use super::{StatePeer, SystemTime};
use ed25519_dalek::SigningKey;
use tilo_desktop_model::WorkspaceEffectReceipt;

impl StatePeer {
    /// Decode the canonical, bounded broker terminal audit payload.
    ///
    /// # Errors
    /// Rejects private or extra facets, malformed identifiers and noncanonical encoding.
    pub fn validate_workspace_effect_receipt_encoding(
        invocation: uuid::Uuid,
        redacted: &[u8],
    ) -> Result<WorkspaceEffectReceipt, String> {
        if redacted.is_empty() || redacted.len() > 16 * 1024 {
            return Err("invalid redacted owner receipt".into());
        }
        let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
            serde_json::from_slice(redacted).map_err(|_| "malformed redacted effect receipt")?;
        if invocation.is_nil()
            || receipt.invocation.0 != invocation
            || receipt.actor.trim().is_empty()
            || receipt.actor.chars().count() > 160
            || receipt.actor.chars().any(char::is_control)
            || receipt.completed_at < 0
            || !bounded_context(&receipt.context)
            || matches!(&receipt.status, tilo_actions::EffectReceiptStatus::Failed { code }
            if code.is_empty() || code.len() > 96 || !code.bytes().all(|byte|
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
            || receipt.context.facets.len() != 2
            || !receipt
                .context
                .facets
                .iter()
                .any(|facet| facet.key == "tilo.receipt" && facet.value == "effect")
            || !receipt.context.facets.iter().any(|facet| {
                facet.key == "tilo.invocation" && facet.value == invocation.to_string()
            })
            || serde_json::to_vec(&receipt).map_err(|error| error.to_string())? != redacted
        {
            return Err("effect receipt is not the exact bounded broker terminal encoding".into());
        }
        Ok(receipt)
    }

    /// Record a currently admitted person's own terminal audit at its governor.
    /// Transport authentication must independently bind the caller's device key.
    ///
    /// # Errors
    /// Refuses stale certificates, revoked or unreadable rooms, impersonation,
    /// malformed payloads and conflicting terminal outcomes.
    pub fn apply_member_effect_receipt(
        &mut self,
        caller: &tilo_user_identity::DeviceCert,
        invocation: uuid::Uuid,
        redacted: &[u8],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
        if self.certificate(&caller.device_public_key, now)? != caller {
            return Err("effect caller is not exact current admitted evidence".into());
        }
        let (role, caps, person) = self.permissions(caller.device_public_key, now)?;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "effect clock precedes Unix epoch")?
            .as_secs();
        let actor = Actor::person(Some(role), person).in_room(self.core.room.clone());
        if decide_capability(
            &actor,
            &ResourceRef::Workspace,
            Verb::Read,
            &caps,
            Some(seconds),
        ) != CapDecision::Allow
        {
            return Err("effect caller cannot currently read this workspace".into());
        }
        let receipt = Self::validate_workspace_effect_receipt_encoding(invocation, redacted)?;
        if receipt.actor != hex::encode(caller.user_public_key) {
            return Err("effect actor is not the authenticated person's own identity".into());
        }
        let existing = tilo_desktop_model::projection::workspace_effect_receipts_of(
            self.admitted_room().ok_or("effect room is absent")?,
        );
        if !existing
            .iter()
            .any(|entry| entry.invocation == receipt.invocation)
            && (existing.len() >= 1024
                || existing
                    .iter()
                    .filter(|entry| entry.actor == receipt.actor)
                    .count()
                    >= 128)
        {
            return Err("member terminal receipt capacity exhausted".into());
        }
        self.apply_workspace_effect_receipt(&receipt, key, now)
    }
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

fn bounded_context(context: &tilo_actions::ActionContext) -> bool {
    fn text(value: &str, max: usize) -> bool {
        value.trim() == value
            && value.chars().count() <= max
            && !value.chars().any(char::is_control)
    }
    context
        .item
        .as_ref()
        .is_none_or(|item| text(&item.id, 512) && text(&item.label, 200))
        && context
            .app
            .as_ref()
            .is_none_or(|app| text(&app.id, 160) && text(&app.label, 160))
        && context.resource.as_ref().is_none_or(|resource| {
            text(&resource.id, 512) && text(&resource.kind, 160) && text(&resource.label, 200)
        })
        && context
            .purpose
            .as_ref()
            .is_none_or(|purpose| !purpose.is_empty() && text(purpose, 512))
        && context.group.as_ref().is_none_or(|group| {
            text(&group.id, 160) && text(&group.label, 200) && text(&group.kind, 160)
        })
}
