use super::SystemTime;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use collab_types::signer::DeviceSigner;
use tilo_automation::ShortcutChange;

/// The most shortcuts one workspace holds.
const MAX_SHORTCUTS: usize = 512;

impl PeerWorkspace<'_> {
    /// The shortcuts this peer's workspace holds, as every member reads them
    /// from its document: each one saved by the device whose proof it carries.
    #[must_use]
    pub fn workspace_shortcuts(&self) -> Vec<tilo_automation::SavedShortcut> {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map(tilo_desktop_model::projection::workspace_shortcuts_of)
            .unwrap_or_default()
    }
}

impl PeerWorkspaceMut<'_> {
    /// Save or remove one shortcut of the workspace as the admitted member whose
    /// device `caller` is, signed by `key`, that device's own: written into
    /// the workspace and admitted the way every receiver admits it.
    ///
    /// A saved shortcut names as who saved it nobody or the caller's person,
    /// by the proof of a device of theirs signed in this workspace
    /// ([`crate::workspace::proven_saver`]): the same rule admission holds every
    /// writer to. Answers whether the workspace changed.
    pub fn apply_shortcut_change(
        self,
        caller: &tilo_user_identity::DeviceCert,
        change: &ShortcutChange,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        use automerge::{ReadDoc, transaction::Transactable};
        use tilo_desktop_model::projection::WORKSPACE_AUTOMATIONS_KEY;
        if key.device_public_key() != caller.device_public_key {
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
        let member = self.peer.membership(caller.device_public_key, now)?;
        if !member.allows(
            &tilo_action_policy::TreePath::workspace(&self.peer.core.workspace),
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
        let workspace = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .ok_or("shortcut workspace is absent")?;
        let mut candidate = workspace.clone();
        let before = candidate.get_heads();
        candidate.set_actor(automerge::ActorId::from(caller.device_public_key.to_vec()));
        let map = super::governed_map(&candidate, WORKSPACE_AUTOMATIONS_KEY)?;
        let changed = match change {
            ShortcutChange::Save(shortcut) => {
                if let Some(saver) = &shortcut.saved_by {
                    let proven = crate::workspace::proven(
                        &self.peer.core.workspace,
                        shortcut,
                        &crate::workspace::workspace_revocations(workspace),
                        now_ns,
                    );
                    // A person's device commits what it signed; an agent's grant signed for
                    // its agent wherever the agent works, and the device it works on commits
                    // it.
                    let committed_by_saver = proven.as_ref().is_some_and(|proven| {
                        proven.agent.is_some() || *saver == hex::encode(caller.user_public_key)
                    });
                    if proven.as_ref().map(|proven| &proven.person) != Some(saver)
                        || !committed_by_saver
                    {
                        return Err(
                            "a shortcut is saved only as its caller, by their device's proof"
                                .into(),
                        );
                    }
                }
                let blob = serde_json::to_vec(shortcut).map_err(|e| e.to_string())?;
                put_shortcut(&mut candidate, &map, shortcut.id.as_str(), blob)?
            }
            ShortcutChange::Delete(id) => {
                if candidate.get(&map, id.as_str()).ok().flatten().is_none() {
                    return Err(format!("there is no shortcut `{id}`"));
                }
                candidate
                    .delete(&map, id.as_str())
                    .map_err(|e| e.to_string())?;
                true
            }
        };
        if !changed {
            return Ok(false);
        }
        self.peer
            .write_governance(candidate.into(), &before, key, now)?;
        Ok(true)
    }
}

/// Put `blob` as shortcut `id` into the workspace's shortcuts `map`. Answers
/// whether the workspace changed.
fn put_shortcut(
    candidate: &mut impl automerge::transaction::Transactable,
    map: &automerge::ObjId,
    id: &str,
    blob: Vec<u8>,
) -> Result<bool, String> {
    use automerge::{ScalarValue, Value};
    match candidate.get(map, id).map_err(|e| e.to_string())? {
        Some((Value::Scalar(held), _)) if matches!(held.as_ref(), ScalarValue::Bytes(bytes) if *bytes == blob) => {
            Ok(false)
        }
        held => {
            if held.is_none() && candidate.length(map) >= MAX_SHORTCUTS {
                return Err("the workspace holds as many shortcuts as it can".into());
            }
            candidate
                .put(map, id, ScalarValue::Bytes(blob))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
    }
}

#[cfg(test)]
#[path = "workspace_shortcuts_tests.rs"]
mod tests;
