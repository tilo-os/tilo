use super::SystemTime;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use ed25519_dalek::SigningKey;
use tilo_desktop_model::WorkspaceEffectReceipt;

impl PeerWorkspace<'_> {
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
        let receipt: WorkspaceEffectReceipt =
            serde_json::from_slice(redacted).map_err(|_| "malformed redacted effect receipt")?;
        if receipt.invocation.0 != invocation
            || !receipt.is_redacted()
            || matches!(&receipt.status, tilo_actions::EffectReceiptStatus::Failed { code }
            if code.is_empty() || code.len() > 96 || !code.bytes().all(|byte|
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
            || serde_json::to_vec(&receipt).map_err(|error| error.to_string())? != redacted
        {
            return Err("effect receipt is not the exact bounded broker terminal encoding".into());
        }
        Ok(receipt)
    }
    /// Authenticate a historical reader against exact current certificate evidence.
    ///
    /// # Errors
    /// Revoked, stale or substituted certificate, or current route read refusal.
    pub fn check_checkpoint_reader(
        self,
        caller: &tilo_user_identity::DeviceCert,
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        if self.peer.certificate(&caller.device_public_key, now)? != caller {
            return Err("checkpoint caller is not exact current admitted evidence".into());
        }
        self.peer
            .check_document_read(caller.device_public_key, app, record, now)
    }
    /// Require current room undoable-write permission for checkpoint capture and naming.
    /// # Errors
    /// Refuses substituted certificates or revoked/missing current undoable-write permission.
    pub fn check_checkpoint_namer(
        self,
        caller: &tilo_user_identity::DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        if self.peer.certificate(&caller.device_public_key, now)? != caller {
            return Err("checkpoint caller is not exact current admitted evidence".into());
        }
        if !self
            .peer
            .permissions(caller.device_public_key, now)?
            .allows(
                &tilo_action_policy::TreePath::room(&self.peer.core.room),
                tilo_action_policy::ActionKind::Undoable,
                now,
            )
        {
            return Err(
                "checkpoint capture and naming require current room undoable-write permission"
                    .into(),
            );
        }
        Ok(())
    }
    /// Check current native checkpoint governor signing custody, including retries.
    ///
    /// # Errors
    /// Stale certificate, revoked writer or mismatching governor key.
    pub fn check_checkpoint_writer(self, key: &SigningKey, now: SystemTime) -> Result<(), String> {
        self.peer.require_governance_writer(key, now)
    }
    /// Read the native checkpoint content anchor, never treating corruption as absence.
    ///
    /// # Errors
    /// Missing room or malformed anchor.
    pub fn workspace_checkpoint_root(self) -> Result<Option<[u8; 32]>, String> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let doc = self
            .peer
            .core
            .room_doc
            .as_deref()
            .ok_or("checkpoint room is absent")?;
        let values = doc
            .get_all(ROOT, "native_checkpoint_root_v1")
            .map_err(|e| e.to_string())?;
        match values.as_slice() {
            [] => Ok(None),
            [(Value::Scalar(value), _)] => match value.as_ref() {
                ScalarValue::Bytes(bytes) => bytes
                    .as_slice()
                    .try_into()
                    .map(Some)
                    .map_err(|_| "malformed native checkpoint root".into()),
                _ => Err("malformed native checkpoint root".into()),
            },
            _ => Err("conflicting native checkpoint roots".into()),
        }
    }
    /// Read the cold-history anchor from actual admitted governance state.
    ///
    /// # Errors
    /// A malformed anchor must not be mistaken for an empty index.
    pub fn workspace_effect_history_root(self) -> Result<Option<[u8; 32]>, String> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let doc = self
            .peer
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
}
impl PeerWorkspaceMut<'_> {
    /// Record a currently admitted person's own terminal audit at its governor.
    /// Transport authentication must independently bind the caller's device key.
    ///
    /// # Errors
    /// Refuses stale certificates, revoked or unreadable rooms, impersonation,
    /// malformed payloads and conflicting terminal outcomes.
    pub fn apply_member_effect_receipt(
        self,
        caller: &tilo_user_identity::DeviceCert,
        invocation: uuid::Uuid,
        redacted: &[u8],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        if self.peer.certificate(&caller.device_public_key, now)? != caller {
            return Err("effect caller is not exact current admitted evidence".into());
        }
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "effect clock precedes Unix epoch")?;
        let member = self.peer.permissions(caller.device_public_key, now)?;
        if !member.allows(
            &tilo_action_policy::TreePath::room(&self.peer.core.room),
            tilo_action_policy::ActionKind::Read,
            now,
        ) {
            return Err("effect caller cannot currently read this workspace".into());
        }
        let receipt =
            PeerWorkspace::validate_workspace_effect_receipt_encoding(invocation, redacted)?;
        if receipt.actor != hex::encode(caller.user_public_key) {
            return Err("effect actor is not the authenticated person's own identity".into());
        }
        let existing = tilo_desktop_model::projection::workspace_effect_receipts_of(
            self.peer.admitted_room().ok_or("effect room is absent")?,
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
        self,
        receipt: &WorkspaceEffectReceipt,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let room = self
            .peer
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
        self.peer.core.room_doc = Some(candidate.into());
        Ok(true)
    }
    /// Anchor retained checkpoint content under current governor signing custody.
    ///
    /// # Errors
    /// Invalid custody or failed document write.
    pub fn set_workspace_checkpoint_root(
        self,
        root: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        self.peer.require_governance_writer(key, now)?;
        let mut candidate = self
            .peer
            .core
            .room_doc
            .as_deref()
            .ok_or("checkpoint room is absent")?
            .clone();
        candidate
            .put(
                ROOT,
                "native_checkpoint_root_v1",
                ScalarValue::Bytes(root.to_vec()),
            )
            .map_err(|e| e.to_string())?;
        self.peer.core.room_doc = Some(candidate.into());
        Ok(())
    }
    /// Anchor retained cold history under genuine current governor signing custody.
    ///
    /// # Errors
    /// Invalid governance custody or a failed CRDT write.
    pub fn set_workspace_effect_history_root(
        self,
        root: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        self.peer.require_governance_writer(key, now)?;
        let mut candidate = self
            .peer
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
        self.peer.core.room_doc = Some(candidate.into());
        Ok(())
    }
}
