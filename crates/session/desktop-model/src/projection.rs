//! How a room's replication document carries a workspace, and how it is
//! read back.
//!
//! Pure functions over an [`AutoCommit`] the host owns: the room's
//! governance, people, shortcuts, receipts and record index. Its layout
//! (strips, groups and windows) is the workspace's layout record, kept at
//! the document's root in [`crate::shared_record`]'s shape, which every
//! device writes and folds ([`crate::shared_record::device`]). Admission —
//! deciding whether a peer's batch may land at all — and the documents'
//! lifetimes stay with the host.
//!
//! This is the one room schema every host reads and writes. Versioning is
//! additive and per key: a key a build does not know is left alone, and a
//! key's shape never changes under the same name.

use automerge::{AutoCommit, ReadDoc};
use tilo_automerge::{ReadTyped as _, WriteTyped as _};
use tilo_sort::CompactSort as _;

use crate::ids::{RoomId, StripId, WorkspaceId, peer_salt_of};
use crate::layout::{Tab, TileContent};
use crate::receipt::WorkspaceEffectReceipt;
use crate::tree::DesktopTree;
use crate::workspace::Workspace;

/// Key naming the room a document describes (a UUID string).
pub const ROOM_ID_KEY: &str = "room";
/// Key of the room's display name.
pub const ROOM_NAME_KEY: &str = "name";
/// Key of the room owner's user id.
pub const ROOM_OWNER_KEY: &str = "owner";
/// Key of the next id counter a durable topology owner mints from, so a
/// restarted owner never reuses an id a closed tab once had.
pub const ROOM_DESKTOP_COUNTER_KEY: &str = "desktop_counter_v1";
/// Key a room document stores its effect receipts under: one slice per
/// member, `{person → {invocation → JSON receipt}}`, which only that
/// member's devices write.
pub const ROOM_EFFECT_RECEIPTS_KEY: &str = "effect_receipts_v1";
/// Key a room document anchors each member's archived effect receipts
/// under: `{person → {device → history root}}`, each device's own.
pub const ROOM_EFFECT_HISTORY_KEY: &str = "effect_history_root_v1";
/// Key a room document stores its saved shortcuts under.
pub const ROOM_AUTOMATIONS_KEY: &str = "automations_v1";
/// Key a room document stores its named grant sets under, by lowercase name.
pub const ROOM_PRESETS_KEY: &str = "presets_v1";
/// Key of the map from member person id to the role preset they hold.
pub const ROOM_MEMBERS_KEY: &str = "members";
/// Key of the map from member person id to when the room admitted them,
/// Unix milliseconds (JSON bytes). Written once by the admitting authority.
pub const ROOM_MEMBERS_JOINED_KEY: &str = "members_joined_v1";
/// Key of the map from grant id to the grant (JSON bytes).
pub const ROOM_GRANTS_KEY: &str = "grants";
/// Key of the map from sandbox capability id to the capability (JSON bytes).
pub const ROOM_SANDBOX_KEY: &str = "sandbox";
/// Key a room document stores its schedule claims under.
pub const ROOM_SCHEDULE_CLAIMS_KEY: &str = "schedule_claims_v1";

/// Project a workspace's shortcuts as one map value per shortcut id, so two
/// collaborators saving different shortcuts concurrently merge as two keys.
/// A key this workspace no longer holds is deleted, which is how deleting a
/// shortcut reaches the room: a peer's addition lands in the local list when
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

    let map = match doc.get(&ROOT, ROOM_AUTOMATIONS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None if shortcuts.is_empty() => return Ok(()),
        None => doc.put_object(&ROOT, ROOM_AUTOMATIONS_KEY, ObjType::Map)?,
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
/// travel with the room's policy.
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
    project_blob_map(doc, ROOM_PRESETS_KEY, entries)
}

/// The named grant sets a room document carries, by name. A value that does
/// not decode or validate is skipped.
pub fn workspace_presets_of(doc: &AutoCommit) -> Vec<tilo_action_policy::Preset> {
    let mut presets: Vec<tilo_action_policy::Preset> = blob_map_of(doc, ROOM_PRESETS_KEY)
        .into_iter()
        .map(|(_, preset)| preset)
        .filter(|preset: &tilo_action_policy::Preset| preset.validate().is_ok())
        .collect();
    crate::grants::sort_presets(&mut presets);
    presets
}

/// Project a room's authority: its owner, each member's role and every grant.
///
/// # Errors
///
/// The document's error when a value cannot be written.
pub fn project_room_policy(
    doc: &mut AutoCommit,
    policy: &tilo_action_policy::RoomPolicy,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};

    match &policy.owner {
        Some(owner) => {
            doc.put_str_if_changed(&ROOT, ROOM_OWNER_KEY, owner)?;
        }
        None => {
            if doc.get(&ROOT, ROOM_OWNER_KEY).ok().flatten().is_some() {
                doc.delete(&ROOT, ROOM_OWNER_KEY)?;
            }
        }
    }
    let members = policy
        .members
        .iter()
        .filter_map(|(person, role)| Some((person.clone(), serde_json::to_vec(role).ok()?)))
        .collect();
    // The room's members and grants are always present, if empty: readers
    // of the room's security basis require both.
    ensure_map(doc, ROOM_MEMBERS_KEY)?;
    ensure_map(doc, ROOM_GRANTS_KEY)?;
    project_blob_map(doc, ROOM_MEMBERS_KEY, members)?;
    let joined = policy
        .joined
        .iter()
        .filter_map(|(person, at)| Some((person.clone(), serde_json::to_vec(at).ok()?)))
        .collect();
    project_blob_map(doc, ROOM_MEMBERS_JOINED_KEY, joined)?;
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
    project_blob_map(doc, ROOM_GRANTS_KEY, grants)
}

/// The authority a room document records for `room`. Values that do not
/// decode are skipped; grants on another room's nodes are dropped.
pub fn room_policy_of(doc: &AutoCommit, room: &str) -> tilo_action_policy::RoomPolicy {
    use automerge::{ROOT, ReadDoc};

    let owner = doc
        .get(&ROOT, ROOM_OWNER_KEY)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok());
    let members = blob_map_of(doc, ROOM_MEMBERS_KEY).into_iter().collect();
    let joined = blob_map_of(doc, ROOM_MEMBERS_JOINED_KEY)
        .into_iter()
        .collect();
    let grants = blob_map_of(doc, ROOM_GRANTS_KEY)
        .into_iter()
        .map(|(_, grant): (String, tilo_action_policy::Grant)| grant)
        .filter(|grant| grant.node.room_id() == Some(room))
        .collect();
    tilo_action_policy::RoomPolicy {
        room: room.to_owned(),
        owner,
        members,
        joined,
        grants,
    }
    .with_current_roles()
}

/// Project a room's sandbox capabilities, by id.
///
/// # Errors
///
/// The document's error when the map cannot be written.
pub fn project_room_sandbox(
    doc: &mut AutoCommit,
    sandbox: &[collab_types::caps::CapOverride],
) -> Result<(), automerge::AutomergeError> {
    let entries = sandbox
        .iter()
        .filter_map(|cap| Some((cap.id.clone(), serde_json::to_vec(cap).ok()?)))
        .collect();
    project_blob_map(doc, ROOM_SANDBOX_KEY, entries)
}

/// The sandbox capabilities a room document carries.
pub fn room_sandbox_of(doc: &AutoCommit) -> Vec<collab_types::caps::CapOverride> {
    blob_map_of(doc, ROOM_SANDBOX_KEY)
        .into_iter()
        .map(|(_, cap)| cap)
        .collect()
}

/// The shortcuts a room document carries, by id. A value that does not
/// decode is skipped.
pub fn workspace_shortcuts_of(doc: &AutoCommit) -> Vec<tilo_automation::SavedShortcut> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, map)) = doc.get(&ROOT, ROOM_AUTOMATIONS_KEY).ok().flatten() else {
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

/// Project a workspace's schedule claims as one map value per schedule
/// ([`ScheduleClaim::key`](tilo_automation::ScheduleClaim::key)). A key this
/// workspace no longer holds is deleted, as for shortcuts.
///
/// # Errors
///
/// The document's error when the claims map cannot be created.
pub fn project_schedule_claims(
    doc: &mut AutoCommit,
    claims: &[tilo_automation::ScheduleClaim],
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    let map = match doc.get(&ROOT, ROOM_SCHEDULE_CLAIMS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None if claims.is_empty() => return Ok(()),
        None => doc.put_object(&ROOT, ROOM_SCHEDULE_CLAIMS_KEY, ObjType::Map)?,
    };
    let stale: Vec<String> = doc
        .keys(&map)
        .filter(|key| !claims.iter().any(|claim| claim.key() == *key))
        .collect();
    for key in stale {
        doc.delete(&map, key.as_str())?;
    }
    for claim in claims {
        let Ok(blob) = serde_json::to_vec(claim) else {
            continue;
        };
        let key = claim.key();
        let unchanged = matches!(
            doc.get(&map, key.as_str()).ok().flatten(),
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(current) if current == &blob)
        );
        if !unchanged {
            doc.put(&map, key.as_str(), ScalarValue::Bytes(blob))?;
        }
    }
    Ok(())
}

/// The schedule claims a room document carries. A value that does not
/// decode, or that is filed under another schedule's key, is skipped.
pub fn schedule_claims_of(doc: &AutoCommit) -> Vec<tilo_automation::ScheduleClaim> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, map)) = doc.get(&ROOT, ROOM_SCHEDULE_CLAIMS_KEY).ok().flatten() else {
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

/// Project `receipts` into `person`'s slice of the room's effect receipts,
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
    let ledger = match doc.get(&ROOT, ROOM_EFFECT_RECEIPTS_KEY)? {
        Some((Value::Object(ObjType::Map), id)) => id,
        _ => doc.put_object(&ROOT, ROOM_EFFECT_RECEIPTS_KEY, ObjType::Map)?,
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

/// Every effect receipt the room holds, each member's slice together,
/// oldest first. Receipts that do not decode are skipped.
pub fn workspace_effect_receipts_of(doc: &AutoCommit) -> Vec<WorkspaceEffectReceipt> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value};
    let Some((Value::Object(ObjType::Map), ledger)) =
        doc.get(&ROOT, ROOM_EFFECT_RECEIPTS_KEY).ok().flatten()
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
        // A governed room's owner places in-process kinds; a member that bound
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

/// Key the room document stores a room's record index under, mirroring
/// the record index's shape (id -> `kind`) in the document that
/// crosses the wire.
pub const ROOM_RECORDS_KEY: &str = "records";

/// Project a room's record index into that room's replication
/// document. A room holds a *reference* (id and type), never the
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
pub fn project_room_records(
    doc: &mut AutoCommit,
    held: &std::collections::BTreeMap<String, String>,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};

    if held.is_empty() {
        return Ok(());
    }
    let records_id = match doc.get(&ROOT, ROOM_RECORDS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None => doc.put_object(&ROOT, ROOM_RECORDS_KEY, ObjType::Map)?,
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

/// Read a room document's record index back as `(record id, kind)`
/// pairs, sorted by id so two peers read one document the same way.
pub fn room_records_of(doc: &AutoCommit) -> Vec<(String, String)> {
    use automerge::{ObjType, ROOT, ReadDoc, Value};

    // `records` is created on first projection, so two peers whose room
    // documents started empty may each have minted one: a key conflict
    // where a plain `get` hides one peer's index. Join every candidate
    // map (the `get` winner first; the first copy of a record wins).
    let mut candidates: Vec<automerge::ObjId> = Vec::new();
    if let Ok(Some((Value::Object(ObjType::Map), winner))) = doc.get(&ROOT, ROOM_RECORDS_KEY) {
        candidates.push(winner);
    }
    if let Ok(values) = doc.get_all(&ROOT, ROOM_RECORDS_KEY) {
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

/// The room a candidate document says it describes, when it names a valid
/// one. A payload naming a different room than the one it answers must be
/// refused before anything else reads it.
pub fn declared_room(candidate: &AutoCommit) -> Option<RoomId> {
    RoomId::parse(&candidate.str_at(&automerge::ROOT, ROOM_ID_KEY)?)
}

/// Apply one already-admitted room document to `tree`: create the room's
/// workspace if the tree has none (under [`WorkspaceId::of_room`]),
/// take the room's name and owner, and fold its layout in as a viewer that
/// shows every window as the room does. Existing local workspace identity
/// and local selection remain untouched. Returns whether anything changed.
///
/// # Errors
///
/// A description when the document does not declare exactly `room`, or
/// another workspace holds the room's workspace id.
pub fn apply_admitted_room(
    tree: &mut DesktopTree,
    room: RoomId,
    doc: &AutoCommit,
) -> Result<bool, String> {
    use automerge::ROOT;
    if declared_room(doc) != Some(room) {
        return Err("admitted room identity mismatch".into());
    }
    let text = |key: &str| -> Result<Option<String>, String> {
        Ok(doc
            .get(ROOT, key)
            .map_err(|e| e.to_string())?
            .and_then(|(v, _)| v.as_str().map(str::to_owned)))
    };
    let name = text(ROOM_NAME_KEY)?.unwrap_or_default();
    if !tree.workspaces.iter().any(|w| w.room() == room) {
        let id = WorkspaceId::of_room(room);
        if tree.workspaces.iter().any(|w| w.id == id) {
            return Err("another workspace holds the room's workspace id".into());
        }
        let workspace = Workspace::in_room(id, room, &name);
        if tree.workspaces.is_empty() {
            tree.active = workspace.id;
        }
        tree.workspaces.push(workspace);
    }
    let owner = text(ROOM_OWNER_KEY)?;
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|w| w.room() == room)
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

/// Key of the room's layout scope in its governance document
/// ([`RoomLayout`]), which only the room's governor writes.
pub const ROOM_LAYOUT_KEY: &str = "layout_v1";

/// Whether a room shares its members' strips and windows: a field of the
/// room's governance, written by its governor alone, that every member reads.
///
/// A room admitted through a public door (a community site's) or with open
/// admission (listed in a public directory) is [`Self::Personal`]; an
/// invite-only room is [`Self::Shared`]. The governor may change it.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RoomLayout {
    /// The namespaced layout model: each member's windows are published into
    /// the room, and every member shows everyone's.
    #[default]
    Shared,
    /// Each member's strips and windows stay on their device. Only the
    /// room's records (its apps' documents) and its people are shared: the
    /// governor takes in no member's layout, and devices send none.
    Personal,
}

impl RoomLayout {
    /// The word the governance document stores.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Personal => "personal",
        }
    }

    /// The layout `id` names; `None` for any other word.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "shared" => Some(Self::Shared),
            "personal" => Some(Self::Personal),
            _ => None,
        }
    }

    /// Whether each member's layout stays on their device.
    #[must_use]
    pub const fn is_personal(self) -> bool {
        matches!(self, Self::Personal)
    }
}

/// The layout scope `doc`'s governor chose, `None` when it chose none (a
/// room from before the field, or an invite-only room left at the
/// default). A value this build does not read is personal: what a room may
/// not share is never shared by a device that misreads it.
#[must_use]
pub fn room_layout_choice(doc: &AutoCommit) -> Option<RoomLayout> {
    use automerge::{ROOT, ScalarValue, Value};
    match doc.get(&ROOT, ROOM_LAYOUT_KEY).ok().flatten()? {
        (Value::Scalar(value), _) => match value.as_ref() {
            ScalarValue::Str(id) => Some(RoomLayout::from_id(id).unwrap_or(RoomLayout::Personal)),
            _ => Some(RoomLayout::Personal),
        },
        _ => Some(RoomLayout::Personal),
    }
}

/// The people the room `doc` governs lists as its owner and members, by
/// person id: who still holds what they made on its desktop
/// ([`crate::ownership::released`]). `None` when `doc` lists nobody (not
/// heard yet), so nothing is taken as released before the list arrives.
#[must_use]
pub fn room_members(doc: &AutoCommit) -> Option<std::collections::BTreeSet<String>> {
    use automerge::{ROOT, ReadDoc};
    let owner = doc
        .get(&ROOT, ROOM_OWNER_KEY)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok());
    let people: std::collections::BTreeSet<String> =
        blob_map_of::<serde_json::Value>(doc, ROOM_MEMBERS_KEY)
            .into_iter()
            .map(|(person, _)| person)
            .chain(owner)
            .collect();
    (!people.is_empty()).then_some(people)
}

/// The layout scope of the room `doc` governs: its governor's choice, else
/// [`RoomLayout::Shared`], an invite-only room's default.
#[must_use]
pub fn room_layout(doc: &AutoCommit) -> RoomLayout {
    room_layout_choice(doc).unwrap_or_default()
}

/// In the workspace addressing `room`, remove every window `foreign` names
/// (someone else's, mirrored here while the room shared its layout), then
/// the window groups left with none, and the strips left with none that this
/// machine (`local_salt`) did not make. Every window of this machine's own
/// stays where it is. Reports whether anything changed.
pub fn drop_windows_of_others(
    desktop: &mut DesktopTree,
    room: RoomId,
    local_salt: u16,
    foreign: &impl Fn(&Tab) -> bool,
) -> bool {
    let Some(ws) = desktop.workspaces.iter_mut().find(|w| w.room() == room) else {
        return false;
    };
    let mut changed = false;
    let mut vanished: Vec<StripId> = Vec::new();
    for strip in &mut ws.strips {
        for group in &mut strip.window_groups {
            let before = group.tabs.len();
            group.tabs.retain(|tab| !foreign(tab));
            if group.tabs.len() == before {
                continue;
            }
            changed = true;
            group
                .focus_history
                .retain(|id| group.tabs.iter().any(|tab| tab.id == *id));
            if !group.tabs.iter().any(|tab| tab.id == group.active_tab)
                && let Some(first) = group.tabs.first()
            {
                group.active_tab = first.id;
            }
        }
        strip.window_groups.retain(|group| !group.tabs.is_empty());
    }
    ws.strips.retain(|strip| {
        let keep = !strip.window_groups.is_empty() || peer_salt_of(strip.id.raw()) == local_salt;
        if !keep {
            vanished.push(strip.id);
        }
        keep
    });
    changed |= !vanished.is_empty();
    let ws_id = ws.id;
    if desktop
        .active_strip
        .get(&ws_id)
        .is_some_and(|active| vanished.contains(active))
    {
        desktop.active_strip.remove(&ws_id);
    }
    for strip in vanished {
        desktop.active_group.remove(&strip);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppKind;
    use crate::ids::compose_id;
    use crate::layout::{Strip, WindowGroup};
    use tilo_automerge::ApplyIncoming;

    /// A grant given and revoked in a room is what the room document holds:
    /// the grant is in it under its id while it stands, gone once revoked,
    /// signed by whoever gave it, and with it a member's role preset.
    #[test]
    fn a_grant_and_its_revocation_are_what_the_room_document_carries() {
        use tilo_action_policy::{Answer, Grant, KindSet, Level, Subject, TreePath};

        let mut policy = tilo_action_policy::RoomPolicy::new("r", "owner");
        let mut doc = AutoCommit::new();
        project_room_policy(&mut doc, &policy).expect("projected");
        let roles = room_policy_of(&doc, "r");
        assert_eq!(roles.owner.as_deref(), Some("owner"));
        assert!(
            roles
                .grants
                .iter()
                .any(|grant| grant.subject == Subject::role(tilo_action_policy::Role::Editor)),
            "a room document carries its roles' presets"
        );

        let grant = Grant::new(
            "bob-mail",
            Subject::person("bob"),
            TreePath::room("r")
                .app("mail")
                .any(Level::Resource)
                .any(Level::Action),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Subject::person("owner"),
        );
        policy.grants.push(grant.clone());
        project_room_policy(&mut doc, &policy).expect("projected");
        let held = room_policy_of(&doc, "r");
        let recorded = held
            .grants
            .iter()
            .find(|held| held.id == grant.id)
            .expect("the room document carries the grant");
        assert_eq!(recorded, &grant);
        assert_eq!(recorded.granted_by, Subject::person("owner"));

        policy.grants.retain(|held| held.id != grant.id);
        project_room_policy(&mut doc, &policy).expect("projected");
        let revoked = room_policy_of(&doc, "r");
        assert!(
            revoked.grants.iter().all(|held| held.id != grant.id),
            "a revoked grant leaves the room document"
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

    /// Two room documents that start empty (the legacy path) each mint the
    /// `records` map on first projection; the merge must show both indexes.
    #[test]
    fn concurrently_created_record_indexes_both_survive() {
        for _ in 0..8 {
            let mut base = AutoCommit::new();
            base.commit();
            let mut a = base.fork();
            let mut b = base.fork();
            project_room_records(&mut a, &held(("file:/a.md", "document"))).unwrap();
            project_room_records(&mut b, &held(("file:/b.md", "document"))).unwrap();
            a.merge_incoming(&mut b).unwrap();
            let ids: Vec<String> = room_records_of(&a).into_iter().map(|(id, _)| id).collect();
            assert_eq!(ids, ["file:/a.md", "file:/b.md"]);
        }
    }

    /// A test window group under `salt`, with one tab named `name`.
    fn canvas_group(salt: u16, counter: u64, name: &str) -> WindowGroup {
        let id = compose_id(salt, counter);
        WindowGroup {
            id,
            tabs: vec![Tab {
                id,
                title: name.into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Canvas".into()),
                    label: name.into(),
                },
                app_title: None,
            }],
            active_tab: id,
            width: crate::width::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// A desktop with one workspace in `room` holding `strips`.
    fn room_desktop(room: RoomId, workspace: u64, strips: Vec<Strip>) -> crate::tree::DesktopTree {
        let mut w =
            crate::workspace::Workspace::in_room(crate::ids::WorkspaceId(workspace), room, "Team");
        w.strips = strips;
        crate::tree::DesktopTree {
            active: w.id,
            workspaces: vec![w],
            ..crate::tree::DesktopTree::default()
        }
    }

    #[test]
    fn a_rooms_layout_is_what_its_governor_wrote_and_shared_by_default() {
        use automerge::transaction::Transactable as _;
        let mut doc = AutoCommit::new();
        assert_eq!(room_layout_choice(&doc), None);
        assert_eq!(room_layout(&doc), RoomLayout::Shared);
        doc.put(automerge::ROOT, ROOM_LAYOUT_KEY, "personal")
            .unwrap();
        assert_eq!(room_layout(&doc), RoomLayout::Personal);
        doc.put(automerge::ROOT, ROOM_LAYOUT_KEY, "shared").unwrap();
        assert_eq!(room_layout_choice(&doc), Some(RoomLayout::Shared));
        doc.put(automerge::ROOT, ROOM_LAYOUT_KEY, "by-invitation")
            .unwrap();
        assert_eq!(room_layout(&doc), RoomLayout::Personal);
        doc.put(automerge::ROOT, ROOM_LAYOUT_KEY, 3_u64).unwrap();
        assert_eq!(room_layout(&doc), RoomLayout::Personal);
        for layout in [RoomLayout::Shared, RoomLayout::Personal] {
            assert_eq!(RoomLayout::from_id(layout.id()), Some(layout));
        }
    }

    /// A device's local copy of a workspace whose room became personal loses
    /// the windows it mirrored of others, and keeps every window of its own,
    /// wherever it stands.
    #[test]
    fn dropping_others_windows_keeps_every_window_of_ones_own() {
        const MINE: u16 = 0x1111;
        const THEIRS: u16 = 0x2222;
        let room = RoomId::new();
        // My strip, holding my window and one of theirs side by side; their
        // strip, holding only theirs; their strip I moved a window of mine
        // into.
        let mut mine = Strip::new(StripId(compose_id(MINE, 1)), "Mine");
        mine.window_groups.push(canvas_group(MINE, 2, "Board"));
        mine.window_groups
            .push(canvas_group(THEIRS, 3, "ssh prod-db"));
        let mut theirs = Strip::new(StripId(compose_id(THEIRS, 4)), "Terminals");
        theirs
            .window_groups
            .push(canvas_group(THEIRS, 5, "~/private"));
        let mut shared = Strip::new(StripId(compose_id(THEIRS, 6)), "Shared");
        shared.window_groups.push(canvas_group(MINE, 7, "Notes"));
        let mut tree = room_desktop(room, 1, vec![mine, theirs, shared]);
        let ws = tree.workspaces[0].id;
        tree.active_strip.insert(ws, StripId(compose_id(THEIRS, 4)));

        let foreign = |tab: &Tab| peer_salt_of(tab.id) == THEIRS;
        assert!(drop_windows_of_others(&mut tree, room, MINE, &foreign));
        let titles: Vec<(&str, Vec<&str>)> = tree.workspaces[0]
            .strips
            .iter()
            .map(|strip| {
                (
                    strip.name.as_str(),
                    strip
                        .window_groups
                        .iter()
                        .flat_map(|group| group.tabs.iter().map(|tab| tab.title.as_str()))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            titles,
            [("Mine", vec!["Board"]), ("Shared", vec!["Notes"])],
            "every window of mine stays, none of theirs"
        );
        assert!(
            !tree.active_strip.contains_key(&ws),
            "no pointer at a strip that went"
        );
        assert!(
            !drop_windows_of_others(&mut tree, room, MINE, &foreign),
            "nothing more to drop"
        );
    }
}
