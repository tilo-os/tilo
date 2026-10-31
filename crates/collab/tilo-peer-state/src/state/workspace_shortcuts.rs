use super::SystemTime;
use super::facades::PeerWorkspaceMut;
use ed25519_dalek::SigningKey;
use tilo_automation::ShortcutChange;

/// The most shortcuts one room holds.
const MAX_SHORTCUTS: usize = 512;

impl PeerWorkspaceMut<'_> {
    /// Save or remove one shortcut of the room as the admitted member whose
    /// device `caller` is, signed by `key`, that device's own: written into
    /// the room and admitted the way every receiver admits it.
    ///
    /// A saved shortcut names as who saved it nobody or the caller's person,
    /// by the proof of a device of theirs signed in this room
    /// ([`crate::room::proven_saver`]): the same rule admission holds every
    /// writer to. Answers whether the room changed.
    ///
    /// # Errors
    ///
    /// A caller that is not current admitted evidence or may not change the
    /// room, a key that is not the caller's, a saver without proof, a full
    /// room, or removing a shortcut the room does not hold.
    pub fn apply_shortcut_change(
        self,
        caller: &tilo_user_identity::DeviceCert,
        change: &ShortcutChange,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        use automerge::{ObjType, ROOT, ReadDoc, Value, transaction::Transactable};
        use tilo_desktop_model::projection::ROOM_AUTOMATIONS_KEY;
        if key.verifying_key().to_bytes() != caller.device_public_key {
            return Err("a shortcut change is signed by its caller's own device".into());
        }
        if self
            .peer
            .certificate(&caller.device_public_key, now)
            .map_err(|error| error.to_string())?
            != caller
        {
            return Err("shortcut caller is not exact current admitted evidence".into());
        }
        let member = self.peer.permissions(caller.device_public_key, now).map_err(|error| error.to_string())?;
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
        let before = candidate.get_heads();
        candidate.set_actor(automerge::ActorId::from(caller.device_public_key.to_vec()));
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
                put_shortcut(&mut candidate, map, shortcut.id.as_str(), blob)?
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
        if !changed {
            return Ok(false);
        }
        candidate.commit();
        self.peer
            .apply_governance_doc(candidate.into(), &before, key, now)?;
        Ok(true)
    }
}

/// Put `blob` as shortcut `id` into the room's shortcuts `map`, made if the
/// room has none yet. Answers whether the room changed.
fn put_shortcut(
    candidate: &mut impl automerge::transaction::Transactable,
    map: Option<automerge::ObjId>,
    id: &str,
    blob: Vec<u8>,
) -> Result<bool, String> {
    use automerge::{ObjType, ROOT, ScalarValue, Value};
    use tilo_desktop_model::projection::ROOM_AUTOMATIONS_KEY;
    let map = match map {
        Some(map) => map,
        None => candidate
            .put_object(ROOT, ROOM_AUTOMATIONS_KEY, ObjType::Map)
            .map_err(|e| e.to_string())?,
    };
    match candidate.get(&map, id).map_err(|e| e.to_string())? {
        Some((Value::Scalar(held), _)) if matches!(held.as_ref(), ScalarValue::Bytes(bytes) if *bytes == blob) => {
            Ok(false)
        }
        held => {
            if held.is_none() && candidate.length(&map) >= MAX_SHORTCUTS {
                return Err("the workspace holds as many shortcuts as it can".into());
            }
            candidate
                .put(&map, id, ScalarValue::Bytes(blob))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
    }
}

#[cfg(test)]
#[path = "workspace_shortcuts_tests.rs"]
mod tests;
