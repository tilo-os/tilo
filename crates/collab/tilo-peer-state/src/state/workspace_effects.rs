use super::SystemTime;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use crate::room::AGENT_ACTOR_PREFIX;
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
    /// The anchor of `device`'s archived effect receipts, in its person's
    /// slice of the room.
    ///
    /// # Errors
    /// A malformed anchor must not be mistaken for an empty index.
    pub fn workspace_effect_history_root(
        self,
        device: &tilo_user_identity::DeviceCert,
    ) -> Result<Option<[u8; 32]>, String> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        use tilo_desktop_model::projection::ROOM_EFFECT_HISTORY_KEY;
        let doc = self
            .peer
            .core
            .room_doc
            .as_deref()
            .ok_or("effect room is absent")?;
        let malformed = || "malformed effect history root".to_owned();
        let Some((Value::Object(_), history)) = doc
            .get(&ROOT, ROOM_EFFECT_HISTORY_KEY)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let Some((Value::Object(_), slice)) = doc
            .get(&history, hex::encode(device.user_public_key))
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        match doc
            .get(&slice, hex::encode(device.device_public_key))
            .map_err(|e| e.to_string())?
        {
            None => Ok(None),
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => bytes
                    .as_slice()
                    .try_into()
                    .map(Some)
                    .map_err(|_| malformed()),
                _ => Err(malformed()),
            },
            _ => Err(malformed()),
        }
    }
}

impl PeerWorkspaceMut<'_> {
    /// Record `receipt`, the terminal audit of an invocation this device
    /// served, in its person's own slice of the room, signed by `key`, the
    /// device's own: the rule every receiver holds a writer to
    /// ([`crate::room::effect_slices_kept`]). The actor is the device's
    /// person, or an agent acting for them. This creates no application
    /// record or capability. Answers whether the room changed.
    ///
    /// # Errors
    /// Refuses a caller that is not current admitted evidence or cannot
    /// read the room, a key that is not the caller's, another's receipt, a
    /// malformed encoding, a conflicting terminal outcome or a failed CRDT
    /// write.
    pub fn apply_own_effect_receipt(
        self,
        caller: &tilo_user_identity::DeviceCert,
        receipt: &WorkspaceEffectReceipt,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let candidate = self.own_effect_candidate(caller, key, now)?;
        let Some(mut candidate) = candidate else {
            return Err("effect room is absent".into());
        };
        let redacted = serde_json::to_vec(receipt).map_err(|error| error.to_string())?;
        PeerWorkspace::validate_workspace_effect_receipt_encoding(receipt.invocation.0, &redacted)?;
        let person = hex::encode(caller.user_public_key);
        if receipt.actor != person && !receipt.actor.starts_with(AGENT_ACTOR_PREFIX) {
            return Err("effect actor is not the recording person or their agent".into());
        }
        let existing = tilo_desktop_model::projection::workspace_effect_receipts_of(&candidate);
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
        tilo_desktop_model::projection::project_effect_receipts(
            &mut candidate,
            &person,
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

    /// The room as `caller`, this device, writes its own effect receipts
    /// into it: under its actor, once its certificate and read permission
    /// hold and `key` is its own.
    fn own_effect_candidate(
        &self,
        caller: &tilo_user_identity::DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Option<automerge::AutoCommit>, String> {
        if key.verifying_key().to_bytes() != caller.device_public_key {
            return Err("an effect receipt is signed by its recorder's own device".into());
        }
        if self.peer.certificate(&caller.device_public_key, now)? != caller {
            return Err("effect recorder is not exact current admitted evidence".into());
        }
        let member = self.peer.permissions(caller.device_public_key, now)?;
        if !member.allows(
            &tilo_action_policy::TreePath::room(&self.peer.core.room),
            tilo_action_policy::ActionKind::Read,
            now,
        ) {
            return Err("effect recorder cannot currently read this workspace".into());
        }
        let Some(room) = self.peer.core.room_doc.as_deref() else {
            return Ok(None);
        };
        let mut candidate = room.clone();
        candidate.set_actor(self.peer.room_actor_for(&caller.device_public_key));
        Ok(Some(candidate))
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

    /// Anchor `caller`'s archived effect receipts at `root`, in its
    /// person's slice of the room, signed by `key`, the device's own.
    ///
    /// # Errors
    /// A caller that may not record receipts here, or a failed CRDT write.
    pub fn set_workspace_effect_history_root(
        self,
        caller: &tilo_user_identity::DeviceCert,
        root: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        use tilo_desktop_model::projection::ROOM_EFFECT_HISTORY_KEY;
        let mut candidate = self
            .own_effect_candidate(caller, key, now)?
            .ok_or("effect room is absent")?;
        let object =
            |doc: &mut automerge::AutoCommit, parent: &automerge::ObjId, key: &str| match doc
                .get(parent, key)
                .map_err(|e| e.to_string())?
            {
                Some((Value::Object(ObjType::Map), id)) => Ok(id),
                _ => doc
                    .put_object(parent, key, ObjType::Map)
                    .map_err(|e| e.to_string()),
            };
        let history = object(&mut candidate, &ROOT, ROOM_EFFECT_HISTORY_KEY)?;
        let slice = object(
            &mut candidate,
            &history,
            &hex::encode(caller.user_public_key),
        )?;
        candidate
            .put(
                &slice,
                hex::encode(caller.device_public_key),
                ScalarValue::Bytes(root.to_vec()),
            )
            .map_err(|e| e.to_string())?;
        self.peer.core.room_doc = Some(candidate.into());
        Ok(())
    }
}
