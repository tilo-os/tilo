use super::SystemTime;
use super::facades::PeerWorkspaceMut;
use ed25519_dalek::SigningKey;
use tilo_automation::ShortcutChange;

/// The most shortcuts one room holds.
const MAX_SHORTCUTS: usize = 512;

impl PeerWorkspaceMut<'_> {
    /// Save or remove one shortcut of the room, for the admitted member whose
    /// device `caller` asked, under the governor's signing custody `key`.
    ///
    /// A saved shortcut names as who saved it nobody or the caller's person,
    /// by the proof of a device of theirs signed in this room
    /// ([`crate::room::proven_saver`]): the same rule admission holds every
    /// writer to. Transport authentication must independently bind the
    /// caller's device key. Answers whether the room changed.
    ///
    /// # Errors
    ///
    /// A caller that is not current admitted evidence or may not change the
    /// room, a saver without proof, a full room, removing a shortcut the room
    /// does not hold, or absent governor custody.
    pub fn apply_shortcut_change(
        self,
        caller: &tilo_user_identity::DeviceCert,
        change: &ShortcutChange,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        use tilo_desktop_model::projection::ROOM_AUTOMATIONS_KEY;
        self.peer.require_governance_writer(key, now)?;
        if self.peer.certificate(&caller.device_public_key, now)? != caller {
            return Err("shortcut caller is not exact current admitted evidence".into());
        }
        let member = self.peer.permissions(caller.device_public_key, now)?;
        if !member.allows(
            &tilo_action_policy::TreePath::room(&self.peer.core.room),
            tilo_action_policy::ActionKind::Undoable,
            now,
        ) {
            return Err("shortcut caller may not change this workspace".into());
        }
        let now_ns = u64::try_from(
            now.duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| "shortcut clock precedes Unix epoch")?
                .as_nanos(),
        )
        .map_err(|_| "shortcut clock overflows")?;
        let room = self
            .peer
            .core
            .room_doc
            .as_deref()
            .ok_or("shortcut room is absent")?;
        let mut candidate = room.clone();
        let map = match candidate
            .get(ROOT, ROOM_AUTOMATIONS_KEY)
            .map_err(|e| e.to_string())?
        {
            Some((Value::Object(ObjType::Map), map)) => Some(map),
            Some(_) => return Err("the room's shortcuts are malformed".into()),
            None => None,
        };
        let changed = match change {
            ShortcutChange::Save(shortcut) => {
                if let Some(saver) = &shortcut.saved_by {
                    let proven = crate::room::proven_saver(
                        &self.peer.core.room,
                        shortcut,
                        &crate::room::room_revocations(room),
                        now_ns,
                    );
                    if proven.as_ref() != Some(saver)
                        || *saver != hex::encode(caller.user_public_key)
                    {
                        return Err(
                            "a shortcut is saved only as its caller, by their device's proof"
                                .into(),
                        );
                    }
                }
                let blob = serde_json::to_vec(shortcut).map_err(|e| e.to_string())?;
                let map = match map {
                    Some(map) => map,
                    None => candidate
                        .put_object(ROOT, ROOM_AUTOMATIONS_KEY, ObjType::Map)
                        .map_err(|e| e.to_string())?,
                };
                match candidate
                    .get(&map, shortcut.id.as_str())
                    .map_err(|e| e.to_string())?
                {
                    Some((Value::Scalar(held), _)) if matches!(held.as_ref(), ScalarValue::Bytes(bytes) if *bytes == blob) => {
                        false
                    }
                    held => {
                        if held.is_none() && candidate.length(&map) >= MAX_SHORTCUTS {
                            return Err("the workspace holds as many shortcuts as it can".into());
                        }
                        candidate
                            .put(&map, shortcut.id.as_str(), ScalarValue::Bytes(blob))
                            .map_err(|e| e.to_string())?;
                        true
                    }
                }
            }
            ShortcutChange::Delete(id) => {
                let held = map
                    .as_ref()
                    .and_then(|map| candidate.get(map, id.as_str()).ok().flatten())
                    .is_some();
                let Some(map) = map.filter(|_| held) else {
                    return Err(format!("there is no shortcut `{id}`"));
                };
                candidate
                    .delete(&map, id.as_str())
                    .map_err(|e| e.to_string())?;
                true
            }
        };
        if changed {
            self.peer.core.room_doc = Some(candidate.into());
        }
        Ok(changed)
    }
}

#[cfg(test)]
#[path = "workspace_shortcuts_tests.rs"]
mod tests;
