//! How a workspace's replication document carries a workspace, and how it is
//! read back.
//!
//! Pure functions over an [`AutoCommit`] the host owns: the workspace's
//! governance, people, shortcuts, receipts and record index. Its layout
//! (strips, groups and windows) is the workspace's layout record, kept at
//! the document's root in [`crate::shared_record`]'s shape, which every
//! device writes and folds ([`crate::shared_record::device`]). Admission —
//! deciding whether a peer's batch may land at all — and the documents'
//! lifetimes stay with the host.
//!
//! This is the one workspace schema every host reads and writes. Versioning is
//! additive and per key: a key a build does not know is left alone, and a
//! key's shape never changes under the same name.

use automerge::{AutoCommit, ReadDoc};
use tilo_automerge::{ReadTyped as _, WriteTyped as _};
use tilo_sort::CompactSort as _;

use crate::ids::{WorkspaceId, WorkspaceSlot};
use crate::layout::TileContent;
use crate::receipt::WorkspaceEffectReceipt;
use crate::tree::DesktopTree;
use crate::workspace::Workspace;

/// Key naming the workspace a document describes (a UUID string).
pub const WORKSPACE_ID_KEY: &str = "room";
/// Key of the workspace's display name.
pub const WORKSPACE_NAME_KEY: &str = "name";
/// Key of the workspace owner's user id.
pub const WORKSPACE_OWNER_KEY: &str = "owner";
/// Key of the next id counter a durable topology owner mints from, so a
/// restarted owner never reuses an id a closed tab once had.
pub const WORKSPACE_DESKTOP_COUNTER_KEY: &str = "desktop_counter_v1";
/// Key a workspace document stores its effect receipts under: one slice per
/// member, `{person → {invocation → JSON receipt}}`, which only that
/// member's devices write.
pub const WORKSPACE_EFFECT_RECEIPTS_KEY: &str = "effect_receipts_v1";
/// Key a workspace document anchors each member's archived effect receipts
/// under: `{person → {device → history root}}`, each device's own.
pub const WORKSPACE_EFFECT_HISTORY_KEY: &str = "effect_history_root_v1";
/// Key a workspace document holds the writes a device refused under:
/// `{person → {device → {route and author → postcard refusal}}}`, each
/// device's own verdicts, kept until the write it refused lands.
pub const WORKSPACE_WRITE_REFUSALS_KEY: &str = "write_refusals_v1";
/// Key a workspace document stores its saved shortcuts under.
pub const WORKSPACE_AUTOMATIONS_KEY: &str = "automations_v1";
/// Key a workspace document stores its named grant sets under, by lowercase name.
pub const WORKSPACE_PRESETS_KEY: &str = "presets_v1";
/// Key of the map from member person id to the role preset they hold.
pub const WORKSPACE_MEMBERS_KEY: &str = "members";
/// Key of the map from member person id to when the workspace admitted them,
/// Unix milliseconds (JSON bytes). Written once by the admitting authority.
pub const WORKSPACE_MEMBERS_JOINED_KEY: &str = "members_joined_v1";
/// Key of the map from grant id to the grant (JSON bytes).
pub const WORKSPACE_GRANTS_KEY: &str = "grants";
/// Key of the map from sandbox capability id to the capability (JSON bytes).
pub const WORKSPACE_SANDBOX_KEY: &str = "sandbox";
/// Key a workspace document stores its schedule claims under.
pub const WORKSPACE_SCHEDULE_CLAIMS_KEY: &str = "schedule_claims_v1";

/// Project a workspace's shortcuts as one map value per shortcut id, so two
/// collaborators saving different shortcuts concurrently merge as two keys.
/// A key this workspace no longer holds is deleted, which is how deleting a
/// shortcut reaches the workspace: a peer's addition lands in the local list when
/// its change is merged, before this runs again.
///
/// # Errors
///
/// The document's error when the shortcuts map cannot be created.
pub fn project_workspace_shortcuts(
    doc: &mut AutoCommit,
    shortcuts: &[tilo_automation::SavedShortcut],
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    let map = match doc.get(&ROOT, WORKSPACE_AUTOMATIONS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None if shortcuts.is_empty() => return Ok(()),
        None => doc.put_object(&ROOT, WORKSPACE_AUTOMATIONS_KEY, ObjType::Map)?,
    };
    let stale: Vec<String> = doc
        .keys(&map)
        .filter(|key| !shortcuts.iter().any(|shortcut| shortcut.id() == key))
        .collect();
    for key in stale {
        doc.delete(&map, key.as_str())?;
    }
    for shortcut in shortcuts {
        let blob = shortcut.json().as_bytes().to_vec();
        let unchanged = matches!(
            doc.get(&map, shortcut.id()).ok().flatten(),
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(current) if current == &blob)
        );
        if !unchanged {
            doc.put(&map, shortcut.id(), ScalarValue::Bytes(blob))?;
        }
    }
    Ok(())
}

/// Make the map at `key` hold exactly `entries` (key → JSON bytes), writing
/// only what changed. A missing map is created only when there is something
/// to write.
/// The map at `key`, created empty when there is none.
fn ensure_map(doc: &mut AutoCommit, key: &str) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    if doc.get(&ROOT, key)?.is_none() {
        doc.put_object(&ROOT, key, ObjType::Map)?;
    }
    Ok(())
}

fn project_blob_map(
    doc: &mut AutoCommit,
    key: &str,
    entries: Vec<(String, Vec<u8>)>,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    let map = match doc.get(&ROOT, key).ok().flatten() {
        Some((_, id)) => id,
        None if entries.is_empty() => return Ok(()),
        None => doc.put_object(&ROOT, key, ObjType::Map)?,
    };
    let stale: Vec<String> = doc
        .keys(&map)
        .filter(|held| !entries.iter().any(|(name, _)| name == held))
        .collect();
    for name in stale {
        doc.delete(&map, name.as_str())?;
    }
    for (name, blob) in entries {
        let unchanged = matches!(
            doc.get(&map, name.as_str()).ok().flatten(),
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(current) if current == &blob)
        );
        if !unchanged {
            doc.put(&map, name.as_str(), ScalarValue::Bytes(blob))?;
        }
    }
    Ok(())
}

/// The values of the map at `key` that decode, with their keys.
fn blob_map_of<T: serde::de::DeserializeOwned>(doc: &AutoCommit, key: &str) -> Vec<(String, T)> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, map)) = doc.get(&ROOT, key).ok().flatten() else {
        return Vec::new();
    };
    doc.keys(&map)
        .filter_map(|name| match doc.get(&map, name.as_str()).ok().flatten() {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(blob) => {
                    serde_json::from_slice(blob).ok().map(|value| (name, value))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Project a workspace's named grant sets as one map value per lowercase
/// name. A set itself grants nothing; the grants an assignment expands to
/// travel with the workspace's policy.
///
/// # Errors
///
/// The document's error when the map cannot be written.
pub fn project_workspace_presets(
    doc: &mut AutoCommit,
    presets: &[tilo_action_policy::Preset],
) -> Result<(), automerge::AutomergeError> {
    let entries = presets
        .iter()
        .filter_map(|preset| {
            Some((
                preset.name.trim().to_lowercase(),
                serde_json::to_vec(preset).ok()?,
            ))
        })
        .collect();
    project_blob_map(doc, WORKSPACE_PRESETS_KEY, entries)
}

/// The named grant sets a workspace document carries, by name. A value that does
/// not decode or validate is skipped.
pub fn workspace_presets_of(doc: &AutoCommit) -> Vec<tilo_action_policy::Preset> {
    let mut presets: Vec<tilo_action_policy::Preset> = blob_map_of(doc, WORKSPACE_PRESETS_KEY)
        .into_iter()
        .map(|(_, preset)| preset)
        .filter(|preset: &tilo_action_policy::Preset| preset.validate().is_ok())
        .collect();
    crate::grants::sort_presets(&mut presets);
    presets
}

/// Project a workspace's authority: its owner, each member's role and every grant.
///
/// # Errors
///
/// The document's error when a value cannot be written.
pub fn project_workspace_policy(
    doc: &mut AutoCommit,
    policy: &tilo_action_policy::WorkspacePolicy,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};

    match &policy.owner {
        Some(owner) => {
            doc.put_str_if_changed(&ROOT, WORKSPACE_OWNER_KEY, owner)?;
        }
        None => {
            if doc.get(&ROOT, WORKSPACE_OWNER_KEY).ok().flatten().is_some() {
                doc.delete(&ROOT, WORKSPACE_OWNER_KEY)?;
            }
        }
    }
    let members = policy
        .members
        .iter()
        .filter_map(|(person, role)| Some((person.clone(), serde_json::to_vec(role).ok()?)))
        .collect();
    // The workspace's members and grants are always present, if empty: readers
    // of the workspace's security basis require both.
    ensure_map(doc, WORKSPACE_MEMBERS_KEY)?;
    ensure_map(doc, WORKSPACE_GRANTS_KEY)?;
    project_blob_map(doc, WORKSPACE_MEMBERS_KEY, members)?;
    let joined = policy
        .joined
        .iter()
        .filter_map(|(person, at)| Some((person.clone(), serde_json::to_vec(at).ok()?)))
        .collect();
    project_blob_map(doc, WORKSPACE_MEMBERS_JOINED_KEY, joined)?;
    let grants = policy
        .grants
        .iter()
        .filter_map(|grant| {
            Some((
                grant.id.as_str().to_owned(),
                serde_json::to_vec(grant).ok()?,
            ))
        })
        .collect();
    project_blob_map(doc, WORKSPACE_GRANTS_KEY, grants)
}

/// The authority a workspace document records for `workspace`. Values that do not
/// decode are skipped; grants on another workspace's nodes are dropped.
pub fn workspace_policy_of(
    doc: &AutoCommit,
    workspace: &str,
) -> tilo_action_policy::WorkspacePolicy {
    use automerge::{ROOT, ReadDoc};

    let owner = doc
        .get(&ROOT, WORKSPACE_OWNER_KEY)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok());
    let members = blob_map_of(doc, WORKSPACE_MEMBERS_KEY)
        .into_iter()
        .collect();
    let joined = blob_map_of(doc, WORKSPACE_MEMBERS_JOINED_KEY)
        .into_iter()
        .collect();
    let grants = blob_map_of(doc, WORKSPACE_GRANTS_KEY)
        .into_iter()
        .map(|(_, grant): (String, tilo_action_policy::Grant)| grant)
        .filter(|grant| grant.node.workspace_id() == Some(workspace))
        .collect();
    tilo_action_policy::WorkspacePolicy {
        workspace: workspace.to_owned(),
        owner,
        members,
        joined,
        grants,
    }
    .with_current_roles()
}

/// Project a workspace's sandbox capabilities, by id.
///
/// # Errors
///
/// The document's error when the map cannot be written.
pub fn project_workspace_sandbox(
    doc: &mut AutoCommit,
    sandbox: &[collab_types::caps::CapOverride],
) -> Result<(), automerge::AutomergeError> {
    let entries = sandbox
        .iter()
        .filter_map(|cap| Some((cap.id.clone(), serde_json::to_vec(cap).ok()?)))
        .collect();
    project_blob_map(doc, WORKSPACE_SANDBOX_KEY, entries)
}

/// The sandbox capabilities a workspace document carries.
pub fn workspace_sandbox_of(doc: &AutoCommit) -> Vec<collab_types::caps::CapOverride> {
    blob_map_of(doc, WORKSPACE_SANDBOX_KEY)
        .into_iter()
        .map(|(_, cap)| cap)
        .collect()
}

/// The shortcuts a workspace document carries, by id. A value that does not
/// decode is skipped.
pub fn workspace_shortcuts_of(doc: &AutoCommit) -> Vec<tilo_automation::SavedShortcut> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, map)) = doc.get(&ROOT, WORKSPACE_AUTOMATIONS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut shortcuts: Vec<tilo_automation::SavedShortcut> = doc
        .keys(&map)
        .filter_map(|key| match doc.get(&map, key.as_str()).ok().flatten() {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(blob) => std::str::from_utf8(blob)
                    .ok()
                    .and_then(|json| tilo_automation::SavedShortcut::parse(json).ok()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    shortcuts.compact_sort_by(|left, right| left.id().cmp(right.id()));
    shortcuts
}

/// The schedule claims a workspace document carries. A value that does not
/// decode, or that is filed under another schedule's key, is skipped.
pub fn schedule_claims_of(doc: &AutoCommit) -> Vec<tilo_automation::ScheduleClaim> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, map)) = doc.get(&ROOT, WORKSPACE_SCHEDULE_CLAIMS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut claims: Vec<tilo_automation::ScheduleClaim> = doc
        .keys(&map)
        .filter_map(|key| {
            let claim: tilo_automation::ScheduleClaim =
                match doc.get(&map, key.as_str()).ok().flatten() {
                    Some((Value::Scalar(value), _)) => match value.as_ref() {
                        ScalarValue::Bytes(blob) => serde_json::from_slice(blob).ok(),
                        _ => None,
                    },
                    _ => None,
                }?;
            (claim.key() == key).then_some(claim)
        })
        .collect();
    claims.compact_sort_by_key(tilo_automation::ScheduleClaim::key);
    claims
}

/// Project `receipts` into `person`'s slice of the workspace's effect receipts,
/// keyed by invocation, so receipts from two devices merge as separate
/// keys.
///
/// # Errors
///
/// The document's error when a receipts map cannot be created.
pub fn project_effect_receipts(
    doc: &mut AutoCommit,
    person: &str,
    receipts: &[WorkspaceEffectReceipt],
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    if receipts.is_empty() {
        return Ok(());
    }
    let ledger = match doc.get(&ROOT, WORKSPACE_EFFECT_RECEIPTS_KEY)? {
        Some((Value::Object(ObjType::Map), id)) => id,
        _ => doc.put_object(&ROOT, WORKSPACE_EFFECT_RECEIPTS_KEY, ObjType::Map)?,
    };
    let slice = match doc.get(&ledger, person)? {
        Some((Value::Object(ObjType::Map), id)) => id,
        _ => doc.put_object(&ledger, person, ObjType::Map)?,
    };
    for receipt in receipts {
        let key = receipt.invocation.0.to_string();
        let Ok(blob) = serde_json::to_vec(receipt) else {
            continue;
        };
        let unchanged = matches!(
            doc.get(&slice, key.as_str())?,
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(current) if current == &blob)
        );
        if !unchanged {
            doc.put(&slice, key, ScalarValue::Bytes(blob))?;
        }
    }
    Ok(())
}

/// Every effect receipt the workspace holds, each member's slice together,
/// oldest first. Receipts that do not decode are skipped.
pub fn workspace_effect_receipts_of(doc: &AutoCommit) -> Vec<WorkspaceEffectReceipt> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value};
    let Some((Value::Object(ObjType::Map), ledger)) =
        doc.get(&ROOT, WORKSPACE_EFFECT_RECEIPTS_KEY).ok().flatten()
    else {
        return Vec::new();
    };
    let mut receipts = Vec::new();
    for person in doc.keys(&ledger) {
        let Some((Value::Object(ObjType::Map), slice)) = doc.get(&ledger, &person).ok().flatten()
        else {
            continue;
        };
        receipts.extend(doc.keys(&slice).filter_map(|key| {
            let (Value::Scalar(value), _) = doc.get(&slice, key).ok().flatten()? else {
                return None;
            };
            let ScalarValue::Bytes(blob) = value.as_ref() else {
                return None;
            };
            serde_json::from_slice::<WorkspaceEffectReceipt>(blob).ok()
        }));
    }
    receipts.compact_sort_by_key(|receipt: &WorkspaceEffectReceipt| receipt.completed_at);
    receipts
}

/// Make a peer's tile mean something on this machine: a
/// [`TileContent::Foreign`] `wl_surface` id only resolves on the
/// compositor hosting that client, so the surface is dropped and the
/// app id survives as [`TileContent::PeerWindow`]. `current` is
/// preferred once a local instance is bound, or the peer's
/// re-projection would take the window away again every round.
///
/// A peer's [`TileContent::LocalApp`] (a window it is still restoring) is
/// the same news as its live window. A [`TileContent::HostedWindow`] stays
/// exactly as published: that window runs only on its host.
pub fn localise_tile(
    tile: TileContent,
    current: Option<&TileContent>,
    tab_title: &str,
) -> TileContent {
    match tile {
        TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
            // Already resolved here, to our own instance or the same
            // waypoint — the remote tile has nothing to add.
            match current {
                Some(
                    matched @ (TileContent::Foreign { app_id: local, .. }
                    | TileContent::PeerWindow { app_id: local, .. }),
                ) if local.eq_ignore_ascii_case(&app_id) => {
                    return matched.clone();
                }
                _ => {}
            }
            let label = if tab_title.trim().is_empty() {
                app_id.clone()
            } else {
                tab_title.to_string()
            };
            TileContent::PeerWindow {
                app_id,
                label,
                note: String::new(),
                missing: None,
            }
        }
        // A governed workspace's owner places in-process kinds; a member that bound
        // its own instance of that app to the tab keeps it.
        placeholder @ (TileContent::AppPlaceholder { .. } | TileContent::Stub { .. }) => {
            let app = crate::apps::canonical_app_id(&placeholder);
            match current {
                Some(
                    bound @ (TileContent::Foreign { app_id, .. }
                    | TileContent::PeerWindow { app_id, .. }),
                ) if app
                    .as_deref()
                    .is_some_and(|app| app.eq_ignore_ascii_case(app_id)) =>
                {
                    bound.clone()
                }
                _ => placeholder,
            }
        }
        other => other,
    }
}

/// Key the workspace document stores a workspace's record index under, mirroring
/// the record index's shape (id -> `kind`) in the document that
/// crosses the wire.
pub const WORKSPACE_RECORDS_KEY: &str = "records";

/// Project a workspace's record index into that workspace's replication
/// document. A workspace holds a *reference* (id and type), never the
/// record itself — documents travel per-record over the broker.
///
/// Additive only: a key this machine doesn't hold is left alone,
/// not swept, since sweeping would race a peer's record that hasn't
/// reached our index yet. Cost: an un-reference doesn't propagate
/// until removal becomes an explicit tombstone.
///
/// # Errors
///
/// The document's error when a map this projection needs cannot be
/// created.
pub fn project_workspace_records(
    doc: &mut AutoCommit,
    held: &std::collections::BTreeMap<String, String>,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};

    if held.is_empty() {
        return Ok(());
    }
    let records_id = match doc.get(&ROOT, WORKSPACE_RECORDS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None => doc.put_object(&ROOT, WORKSPACE_RECORDS_KEY, ObjType::Map)?,
    };
    for (id, kind) in held {
        let entry = match doc.get(&records_id, id.as_str()).ok().flatten() {
            Some((_, obj)) => obj,
            None => match doc.put_object(&records_id, id.as_str(), ObjType::Map) {
                Ok(obj) => obj,
                Err(_) => continue,
            },
        };
        // Compared before writing, like every other value here — a
        // re-projection of unchanged state must not manufacture one.
        let _ = doc.put_str_if_changed(&entry, "kind", kind);
    }
    Ok(())
}

/// Read a workspace document's record index back as `(record id, kind)`
/// pairs, sorted by id so two peers read one document the same way.
pub fn workspace_records_of(doc: &AutoCommit) -> Vec<(String, String)> {
    use automerge::{ObjType, ROOT, ReadDoc, Value};

    // `records` is created on first projection, so two peers whose workspace
    // documents started empty may each have minted one: a key conflict
    // where a plain `get` hides one peer's index. Join every candidate
    // map (the `get` winner first; the first copy of a record wins).
    let mut candidates: Vec<automerge::ObjId> = Vec::new();
    if let Ok(Some((Value::Object(ObjType::Map), winner))) = doc.get(&ROOT, WORKSPACE_RECORDS_KEY) {
        candidates.push(winner);
    }
    if let Ok(values) = doc.get_all(&ROOT, WORKSPACE_RECORDS_KEY) {
        for (value, id) in values {
            if matches!(value, Value::Object(ObjType::Map)) && !candidates.contains(&id) {
                candidates.push(id);
            }
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for (key, entry) in candidates.iter().flat_map(|records_id| {
        doc.keys(records_id)
            .filter_map(|key| {
                let (_, entry) = doc.get(records_id, key.as_str()).ok().flatten()?;
                Some((key, entry))
            })
            .collect::<Vec<_>>()
    }) {
        if out.iter().any(|(seen, _)| *seen == key) {
            continue;
        }
        let kind = doc.str_at(&entry, "kind").unwrap_or_default();
        out.push((key, kind));
    }
    out.compact_sort();
    out
}

/// The workspace a candidate document says it describes, when it names a valid
/// one. A payload naming a different workspace than the one it answers must be
/// refused before anything else reads it.
pub fn declared_workspace(candidate: &AutoCommit) -> Option<WorkspaceId> {
    WorkspaceId::parse(&candidate.str_at(&automerge::ROOT, WORKSPACE_ID_KEY)?)
}

/// Apply one already-admitted workspace document to `tree`: create the workspace's
/// workspace if the tree has none (under [`WorkspaceSlot::of_workspace`]),
/// take the workspace's name and owner, and fold its layout in as a viewer that
/// shows every window as the workspace does. Existing local workspace identity
/// and local selection remain untouched. Returns whether anything changed.
///
/// # Errors
///
/// A description when the document does not declare exactly `identity`, or
/// another workspace already holds the id derived from it.
pub fn apply_admitted_workspace(
    tree: &mut DesktopTree,
    identity: WorkspaceId,
    doc: &AutoCommit,
) -> Result<bool, String> {
    use automerge::ROOT;
    if declared_workspace(doc) != Some(identity) {
        return Err("admitted workspace identity mismatch".into());
    }
    let text = |key: &str| -> Result<Option<String>, String> {
        Ok(doc
            .get(ROOT, key)
            .map_err(|e| e.to_string())?
            .and_then(|(v, _)| v.as_str().map(str::to_owned)))
    };
    let name = text(WORKSPACE_NAME_KEY)?.unwrap_or_default();
    if !tree.workspaces.iter().any(|w| w.id() == identity) {
        let slot = WorkspaceSlot::of_workspace(identity);
        if tree.workspaces.iter().any(|w| w.slot == slot) {
            return Err("another workspace holds that workspace id".into());
        }
        let workspace = Workspace::in_workspace(slot, identity, &name);
        if tree.workspaces.is_empty() {
            tree.active = workspace.slot;
        }
        tree.workspaces.push(workspace);
    }
    let owner = text(WORKSPACE_OWNER_KEY)?;
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|w| w.id() == identity)
        .ok_or("workspace missing")?;
    let renamed = workspace.name != name;
    workspace.name = name;
    workspace.policy.owner = owner;
    let shortcuts = workspace_shortcuts_of(doc);
    let reshortcut = workspace.shortcuts != shortcuts;
    workspace.shortcuts = shortcuts;
    let presets = workspace_presets_of(doc);
    let reprofiled = workspace.presets != presets;
    workspace.presets = presets;
    let changed = crate::shared_record::device::fold(
        doc,
        workspace,
        "",
        &crate::shared_record::device::Kept::NOTHING,
    );
    Ok(changed || renamed || reshortcut || reprofiled)
}

/// The people the workspace `doc` governs lists as its owner and members, by
/// person id: who still holds what they made on its desktop
/// ([`crate::ownership::released`]). `None` when `doc` lists nobody (not
/// heard yet), so nothing is taken as released before the list arrives.
#[must_use]
pub fn workspace_members(doc: &AutoCommit) -> Option<std::collections::BTreeSet<String>> {
    use automerge::{ROOT, ReadDoc};
    let owner = doc
        .get(&ROOT, WORKSPACE_OWNER_KEY)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok());
    let people: std::collections::BTreeSet<String> =
        blob_map_of::<serde_json::Value>(doc, WORKSPACE_MEMBERS_KEY)
            .into_iter()
            .map(|(person, _)| person)
            .chain(owner)
            .collect();
    (!people.is_empty()).then_some(people)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppKind;
    use tilo_automerge::ApplyIncoming;

    /// A grant given and revoked in a workspace is what the workspace document holds:
    /// the grant is in it under its id while it stands, gone once revoked,
    /// signed by whoever gave it, and with it a member's role preset.
    #[test]
    fn a_grant_and_its_revocation_are_what_the_workspace_document_carries() {
        use tilo_action_policy::{Answer, Grant, KindSet, Level, Subject, TreePath};

        let mut policy = tilo_action_policy::WorkspacePolicy::new("r", "owner");
        let mut doc = AutoCommit::new();
        project_workspace_policy(&mut doc, &policy).expect("projected");
        let roles = workspace_policy_of(&doc, "r");
        assert_eq!(roles.owner.as_deref(), Some("owner"));
        assert!(
            roles
                .grants
                .iter()
                .any(|grant| grant.subject == Subject::role(tilo_action_policy::Role::Editor)),
            "a workspace document carries its roles' presets"
        );

        let grant = Grant::new(
            "bob-mail",
            Subject::person("bob"),
            TreePath::workspace("r")
                .app("mail")
                .any(Level::Resource)
                .any(Level::Action),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Subject::person("owner"),
        );
        policy.grants.push(grant.clone());
        project_workspace_policy(&mut doc, &policy).expect("projected");
        let held = workspace_policy_of(&doc, "r");
        let recorded = held
            .grants
            .iter()
            .find(|held| held.id == grant.id)
            .expect("the workspace document carries the grant");
        assert_eq!(recorded, &grant);
        assert_eq!(recorded.granted_by, Subject::person("owner"));

        policy.grants.retain(|held| held.id != grant.id);
        project_workspace_policy(&mut doc, &policy).expect("projected");
        let revoked = workspace_policy_of(&doc, "r");
        assert!(
            revoked.grants.iter().all(|held| held.id != grant.id),
            "a revoked grant leaves the workspace document"
        );
    }

    #[test]
    fn a_bound_instance_survives_the_owner_re_sending_its_placeholder() {
        let placeholder = TileContent::AppPlaceholder {
            kind: AppKind::Other("so.tilo.notes".into()),
            label: "Notes".into(),
        };
        let bound = TileContent::Foreign {
            app_id: "so.tilo.notes".into(),
            surface_id: 7,
        };
        assert_eq!(
            localise_tile(placeholder.clone(), Some(&bound), "Notes"),
            bound
        );
        let waiting = TileContent::PeerWindow {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
            missing: None,
        };
        assert_eq!(
            localise_tile(placeholder.clone(), Some(&waiting), "Notes"),
            waiting
        );
        let other = TileContent::Foreign {
            app_id: "so.tilo.terminal".into(),
            surface_id: 8,
        };
        assert_eq!(
            localise_tile(placeholder.clone(), Some(&other), "Notes"),
            placeholder
        );
        assert_eq!(
            localise_tile(placeholder.clone(), None, "Notes"),
            placeholder
        );
    }

    #[test]
    fn a_hosted_window_arrives_as_itself_and_a_restoring_one_as_a_peer_window() {
        let hosted = TileContent::HostedWindow {
            app_id: "org.gimp.GIMP".into(),
            label: "Poster.xcf".into(),
            host: "ab".repeat(32),
            host_label: "Laptop".into(),
        };
        // Neither a fresh arrival nor a later round turns it into
        // something this device would launch.
        assert_eq!(localise_tile(hosted.clone(), None, "Poster.xcf"), hosted);
        assert_eq!(
            localise_tile(hosted.clone(), Some(&hosted), "Poster.xcf"),
            hosted
        );
        assert_eq!(
            crate::apps::canonical_app_id(&hosted).as_deref(),
            Some("org.gimp.GIMP")
        );

        let restoring = TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        };
        assert_eq!(
            localise_tile(restoring, None, "Groceries"),
            TileContent::PeerWindow {
                app_id: "so.tilo.notes".into(),
                label: "Groceries".into(),
                note: String::new(),
                missing: None,
            }
        );
    }

    fn held(entry: (&str, &str)) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::from([(entry.0.to_string(), entry.1.to_string())])
    }

    /// Two workspace documents that start empty (the legacy path) each mint the
    /// `records` map on first projection; the merge must show both indexes.
    #[test]
    fn concurrently_created_record_indexes_both_survive() {
        for _ in 0..8 {
            let mut base = AutoCommit::new();
            base.commit();
            let mut a = base.fork();
            let mut b = base.fork();
            project_workspace_records(&mut a, &held(("file:/a.md", "document"))).unwrap();
            project_workspace_records(&mut b, &held(("file:/b.md", "document"))).unwrap();
            a.merge_incoming(&mut b).unwrap();
            let ids: Vec<String> = workspace_records_of(&a)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            assert_eq!(ids, ["file:/a.md", "file:/b.md"]);
        }
    }
}
