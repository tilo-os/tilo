//! How a room's replication document carries the desktop, and how it is
//! read back.
//!
//! Pure functions over an [`AutoCommit`] the host owns: projecting the
//! strips, groups and tabs this peer minted into a room document, reading a
//! document's contents back as plain values, and folding them into the
//! local tree under the one-writer-per-entry rule. Admission — deciding
//! whether a peer's batch may land at all — and the documents' lifetimes
//! stay with the host.
//!
//! This is the one room schema every host reads and writes. Versioning is
//! additive and per key: a key a build does not know is left alone, and a
//! key's shape never changes under the same name.

use automerge::{AutoCommit, ReadDoc};
use tilo_automerge::{ReadTyped as _, WriteTyped as _};
use tilo_sort::CompactSort as _;

use crate::app::AppKind;
use crate::ids::{GroupId, RoomId, StripId, TabId, WorkspaceId, counter_of, peer_salt_of};
use crate::layout::{Strip, Tab, TileContent, WindowGroup};
use crate::receipt::WorkspaceEffectReceipt;
use crate::tree::DesktopTree;
use crate::width::WindowWidth;
use crate::workspace::Workspace;

/// Key naming the room a document describes (a UUID string).
pub const ROOM_ID_KEY: &str = "room";
/// Key of the room's display name.
pub const ROOM_NAME_KEY: &str = "name";
/// Key of the room owner's user id.
pub const ROOM_OWNER_KEY: &str = "owner";
/// Key of the room's removal record: the contents key of every strip, group
/// or tab taken out of the workspace, written by whoever took it out. An
/// entry missing from `contents` is gone only when this records it; a
/// document that simply never carried it (a new room's first document, one
/// a window has not been published to yet) takes nothing away.
pub const ROOM_REMOVED_KEY: &str = "removed_v1";
/// Key of the map from topology namespace (a peer salt as four hex digits)
/// to the device key (64 hex digits) that owns it.
pub const ROOM_CONTENTS_OWNERS_KEY: &str = "contents_owners_v1";
/// Key of the next id counter a durable topology owner mints from, so a
/// restarted owner never reuses an id a closed tab once had.
pub const ROOM_DESKTOP_COUNTER_KEY: &str = "desktop_counter_v1";
/// Key on a strip entry marking it as an explicitly created strip that is
/// kept while empty (`1`), unlike an ordinary strip, which is published only
/// while it holds windows.
pub const STRIP_RETAINED_EMPTY_KEY: &str = "retained_empty_v1";
/// Key a room document stores its effect receipts under.
pub const ROOM_EFFECT_RECEIPTS_KEY: &str = "effect_receipts_v1";
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

/// One tab as a room's replication document carries it. Deliberately not
/// a [`Tab`]: no field here for anything a person's own session decided.
#[derive(Clone, Debug)]
pub struct ReplicatedTab {
    pub id: TabId,
    pub order: u64,
    pub title: String,
    /// The window's own title while a person's name stands in `title`
    /// ([`Tab::app_title`]): the hosting device keeps it so clearing the
    /// name anywhere shows the window's latest title.
    pub app_title: Option<String>,
    /// `None` when unparseable — hydrated as a stub rather than dropped,
    /// since a window in the room should still show up as one here.
    pub tile: Option<TileContent>,
}

/// One window group as a room's replication document carries it. Geometry,
/// maximization and active-tab selection are per-device presentation state,
/// so the shared record carries only its topology and tabs.
#[derive(Clone, Debug)]
pub struct ReplicatedGroup {
    pub id: GroupId,
    pub order: u64,
    pub tabs: Vec<ReplicatedTab>,
}

/// One strip as a room's replication document carries it.
#[derive(Clone, Debug)]
pub struct ReplicatedStrip {
    pub id: u64,
    pub order: u64,
    pub name: Option<String>,
    pub groups: Vec<ReplicatedGroup>,
}

// ---- concurrently created containers ---------------------------------------
//
// `contents` and every map beneath it are created on first projection. Two
// peers whose room documents start empty each mint their own root `contents`
// map (and a guest may mint a strip's `groups` / a group's `tabs` before the
// owner does): a map-key conflict where Automerge shows one winner and the
// other peer's windows vanish. Reads join every candidate map at every level;
// writes go to the winner; each peer's sweep removes its stale entries from
// every candidate, so a strip removed while it sat in a losing map cannot come
// back as a ghost.

/// Every map stored under `obj[key]`: the `get` winner first, then the
/// concurrent losers in Automerge's deterministic order.
fn map_candidates(doc: &AutoCommit, obj: &automerge::ObjId, key: &str) -> Vec<automerge::ObjId> {
    use automerge::{ObjType, Value};
    let mut out: Vec<automerge::ObjId> = Vec::new();
    if let Ok(Some((Value::Object(ObjType::Map), winner))) = doc.get(obj, key) {
        out.push(winner);
    }
    if let Ok(values) = doc.get_all(obj, key) {
        for (value, id) in values {
            if matches!(value, Value::Object(ObjType::Map)) && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// [`map_candidates`] of `key` under each of `parents`, in order.
fn child_candidates(
    doc: &AutoCommit,
    parents: &[automerge::ObjId],
    key: &str,
) -> Vec<automerge::ObjId> {
    let mut out: Vec<automerge::ObjId> = Vec::new();
    for parent in parents {
        for id in map_candidates(doc, parent, key) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// The winning candidate, or a new map at `parent[key]` when there is none.
fn winner_or_create(
    doc: &mut AutoCommit,
    candidates: &[automerge::ObjId],
    parent: &automerge::ObjId,
    key: &str,
) -> Result<automerge::ObjId, automerge::AutomergeError> {
    use automerge::{ObjType, transaction::Transactable};
    match candidates.first() {
        Some(first) => Ok(first.clone()),
        None => doc.put_object(parent, key, ObjType::Map),
    }
}

/// `candidates`, or just `created` when there were none before it was made.
fn or_created(
    candidates: Vec<automerge::ObjId>,
    created: &automerge::ObjId,
) -> Vec<automerge::ObjId> {
    if candidates.is_empty() {
        vec![created.clone()]
    } else {
        candidates
    }
}

/// Delete, from every candidate container, the keys this machine owns that
/// are not in `live`.
fn sweep_stale(
    doc: &mut AutoCommit,
    containers: &[automerge::ObjId],
    live: &std::collections::HashSet<String>,
    mine: &impl Fn(u64) -> bool,
    kept: &mut std::collections::HashSet<u64>,
) {
    use automerge::transaction::Transactable;
    kept.extend(live.iter().filter_map(|key| contents_key_id(key)));
    for container in containers {
        let stale: Vec<String> = doc
            .keys(container)
            .filter(|k| !live.contains(k.as_str()) && contents_key_id(k).is_some_and(mine))
            .collect();
        for key in stale {
            let _ = doc.delete(container, key.as_str());
        }
    }
}

/// Every `(id, copies)` entry across `containers`, each id once, its copies
/// in candidate order (the first supplies its fields).
fn joined_entries(
    doc: &AutoCommit,
    containers: &[automerge::ObjId],
) -> Vec<(u64, Vec<automerge::ObjId>)> {
    let mut out: Vec<(u64, Vec<automerge::ObjId>)> = Vec::new();
    for container in containers {
        for key in doc.keys(container) {
            let Some(id) = contents_key_id(&key) else {
                continue;
            };
            if out.iter().any(|(seen, _)| *seen == id) {
                continue;
            }
            let copies = child_candidates(doc, containers, key.as_str());
            if !copies.is_empty() {
                out.push((id, copies));
            }
        }
    }
    out
}

/// Every tab entry in a room document's `contents`, joined across concurrent
/// containers at every level: `(strip, group, tab, tab entry)`.
pub(crate) fn tab_entries(doc: &AutoCommit) -> Vec<(u64, u64, u64, automerge::ObjId)> {
    let mut out = Vec::new();
    let contents = map_candidates(doc, &automerge::ROOT, ROOM_CONTENTS_KEY);
    for (strip, strip_copies) in joined_entries(doc, &contents) {
        let groups = child_candidates(doc, &strip_copies, "groups");
        for (group, group_copies) in joined_entries(doc, &groups) {
            let tabs = child_candidates(doc, &group_copies, "tabs");
            for (tab, tab_copies) in joined_entries(doc, &tabs) {
                if let Some(entry) = tab_copies.into_iter().next() {
                    out.push((strip, group, tab, entry));
                }
            }
        }
    }
    out
}

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
    shortcuts: &[tilo_automation::Shortcut],
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    let map = match doc.get(&ROOT, ROOM_AUTOMATIONS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None if shortcuts.is_empty() => return Ok(()),
        None => doc.put_object(&ROOT, ROOM_AUTOMATIONS_KEY, ObjType::Map)?,
    };
    let stale: Vec<String> = doc
        .keys(&map)
        .filter(|key| !shortcuts.iter().any(|shortcut| shortcut.id == *key))
        .collect();
    for key in stale {
        doc.delete(&map, key.as_str())?;
    }
    for shortcut in shortcuts {
        let Ok(blob) = serde_json::to_vec(shortcut) else {
            continue;
        };
        let unchanged = matches!(
            doc.get(&map, shortcut.id.as_str()).ok().flatten(),
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(current) if current == &blob)
        );
        if !unchanged {
            doc.put(&map, shortcut.id.as_str(), ScalarValue::Bytes(blob))?;
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
pub fn workspace_shortcuts_of(doc: &AutoCommit) -> Vec<tilo_automation::Shortcut> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, map)) = doc.get(&ROOT, ROOM_AUTOMATIONS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut shortcuts: Vec<tilo_automation::Shortcut> = doc
        .keys(&map)
        .filter_map(|key| match doc.get(&map, key.as_str()).ok().flatten() {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(blob) => serde_json::from_slice(blob).ok(),
                _ => None,
            },
            _ => None,
        })
        .collect();
    shortcuts.compact_sort_by(|left, right| left.id.cmp(&right.id));
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

/// Project effect receipts keyed by invocation, so receipts from two
/// peers merge as separate map keys.
///
/// # Errors
///
/// The document's error when the receipts map cannot be created.
pub fn project_effect_receipts(
    doc: &mut AutoCommit,
    receipts: &[WorkspaceEffectReceipt],
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    let object = match doc.get(&ROOT, ROOM_EFFECT_RECEIPTS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None if receipts.is_empty() => return Ok(()),
        None => doc.put_object(&ROOT, ROOM_EFFECT_RECEIPTS_KEY, ObjType::Map)?,
    };
    for receipt in receipts {
        let key = receipt.invocation.0.to_string();
        let Ok(blob) = serde_json::to_vec(receipt) else {
            continue;
        };
        let unchanged = matches!(
            doc.get(&object, key.as_str()).ok().flatten(),
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(current) if current == &blob)
        );
        if !unchanged {
            let _ = doc.put(&object, key, ScalarValue::Bytes(blob));
        }
    }
    Ok(())
}

pub fn workspace_effect_receipts_of(doc: &AutoCommit) -> Vec<WorkspaceEffectReceipt> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};
    let Some((_, object)) = doc.get(&ROOT, ROOM_EFFECT_RECEIPTS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut receipts = doc
        .keys(&object)
        .filter_map(|key| {
            let (Value::Scalar(value), _) = doc.get(&object, key).ok().flatten()? else {
                return None;
            };
            let ScalarValue::Bytes(blob) = value.as_ref() else {
                return None;
            };
            serde_json::from_slice(blob).ok()
        })
        .collect::<Vec<_>>();
    receipts.compact_sort_by_key(|receipt: &WorkspaceEffectReceipt| receipt.completed_at);
    receipts
}

/// Key the room document stores the room's window contents under.
pub const ROOM_CONTENTS_KEY: &str = "contents";

/// A strip / group / tab id as a `contents` map key: fixed-width hex,
/// so the key parses back into the id it names and two peers spell
/// one id the same way.
pub fn contents_key(id: u64) -> String {
    format!("{id:016x}")
}

/// The id a `contents` key names, or `None` for a key this build did
/// not write.
pub fn contents_key_id(key: &str) -> Option<u64> {
    u64::from_str_radix(key, 16).ok()
}

/// Whether [`project_owned_contents`] publishes a strip with no windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyStrips {
    /// An ordinary strip holds no state while empty; an empty row arriving on
    /// someone else's desktop is noise, not news. It is published once it
    /// holds a window — unless it was retained earlier, which sticks.
    Skip,
    /// Explicitly created strips (an owner applying a `CreateStrip`
    /// command) are durable destinations: publish them while empty, and mark
    /// them with [`STRIP_RETAINED_EMPTY_KEY`] so later ordinary projections
    /// keep them until the strip is removed.
    Retain,
}

/// Project the strips, window groups and tabs this machine owns into
/// the room document's `contents` map. The `active_*` pointers are
/// excluded — projecting them would drag a colleague off the page
/// they were on whenever somebody else changed tabs.
///
/// Every entry is written only by the peer whose salt its id carries
/// ([`peer_salt_of`]) — a single-writer rule
/// the host's admission of peer changes enforces, so last-writer-wins has
/// nothing to arbitrate. Entries carry their index; readers sort by
/// `(order, id)` so peers appending at once still agree, and the
/// sweep at each level deletes only entries this machine owns. Salt
/// `0` has no id space, so nothing is projected or merged there.
///
/// # Errors
///
/// The document's error when the root `contents` map cannot be created.
pub fn project_owned_contents(
    doc: &mut AutoCommit,
    strips: &[Strip],
    local_salt: u16,
    share_floor: Option<u64>,
    empty: EmptyStrips,
) -> Result<(), automerge::AutomergeError> {
    use automerge::ROOT;
    use std::collections::HashSet;

    if local_salt == 0 {
        return Ok(());
    }
    // "Mine to publish": minted by this peer, and since it joined
    // (not before) if this is a room it joined rather than minted.
    let mine = |id: u64| -> bool {
        peer_salt_of(id) == local_salt && share_floor.is_none_or(|floor| counter_of(id) >= floor)
    };

    // What of ours the document held before: whatever of it this
    // projection no longer keeps has left the workspace, and is recorded as
    // removed.
    let mut gone: Vec<u64> = content_ids(doc);
    gone.retain(|id| mine(*id));
    let mut kept: HashSet<u64> = HashSet::new();

    let contents_all = map_candidates(doc, &ROOT, ROOM_CONTENTS_KEY);
    let contents_id = winner_or_create(doc, &contents_all, &ROOT, ROOM_CONTENTS_KEY)?;
    let contents_all = or_created(contents_all, &contents_id);

    let mut live_strips: HashSet<String> = HashSet::new();
    for (si, strip) in strips.iter().enumerate() {
        let skey = contents_key(strip.id.raw());
        let strip_all = child_candidates(doc, &contents_all, skey.as_str());
        let retained = strip_all.first().is_some_and(|entry| {
            doc.get(entry, STRIP_RETAINED_EMPTY_KEY)
                .ok()
                .flatten()
                .and_then(|(value, _)| value.as_u64())
                == Some(1)
        });
        if strip.window_groups.is_empty() && empty == EmptyStrips::Skip && !retained {
            continue;
        }
        let owns_strip = mine(strip.id.raw());
        // A peer's strip is still walked: a window we opened into it
        // is ours to publish even though the row isn't.
        let has_owned = strip
            .window_groups
            .iter()
            .any(|g| !g.tabs.is_empty() && (mine(g.id) || g.tabs.iter().any(|t| mine(t.id))));
        if !owns_strip && !has_owned {
            continue;
        }

        let Ok(strip_entry) = winner_or_create(doc, &strip_all, &contents_id, skey.as_str()) else {
            continue;
        };
        if owns_strip {
            live_strips.insert(skey.clone());
            if empty == EmptyStrips::Retain {
                let _ = doc.put_uint_if_changed(&strip_entry, STRIP_RETAINED_EMPTY_KEY, 1);
            }
            let _ = doc.put_str_if_changed(&strip_entry, "name", &strip.name);
            let _ = doc.put_uint_if_changed(&strip_entry, "order", si as u64);
        }

        // A strip whose groups cannot be written is skipped; the sweep
        // below still runs.
        let strip_all = or_created(strip_all, &strip_entry);
        let _ = project_owned_groups_in(doc, &strip_all, strip, &mine, &mut kept);
    }
    sweep_stale(doc, &contents_all, &live_strips, &mine, &mut kept);
    gone.retain(|id| !kept.contains(id));
    record_removals(doc, &gone, &kept)
}

/// Every strip, group and tab id in `doc`'s `contents`, joined across
/// concurrent containers at every level.
fn content_ids(doc: &AutoCommit) -> Vec<u64> {
    let mut out = Vec::new();
    let contents = map_candidates(doc, &automerge::ROOT, ROOM_CONTENTS_KEY);
    for (strip, strip_copies) in joined_entries(doc, &contents) {
        out.push(strip);
        let groups = child_candidates(doc, &strip_copies, "groups");
        for (group, group_copies) in joined_entries(doc, &groups) {
            out.push(group);
            let tabs = child_candidates(doc, &group_copies, "tabs");
            out.extend(joined_entries(doc, &tabs).into_iter().map(|(tab, _)| tab));
        }
    }
    out
}

/// Record `removed` in the room's removal record ([`ROOM_REMOVED_KEY`]),
/// and take every id in `live` out of it: an entry published again is in
/// the workspace again.
fn record_removals(
    doc: &mut AutoCommit,
    removed: &[u64],
    live: &std::collections::HashSet<u64>,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ROOT, transaction::Transactable as _};
    let all = map_candidates(doc, &ROOT, ROOM_REMOVED_KEY);
    if all.is_empty() && removed.is_empty() {
        return Ok(());
    }
    for map in &all {
        let revived: Vec<String> = doc
            .keys(map)
            .filter(|key| contents_key_id(key).is_some_and(|id| live.contains(&id)))
            .collect();
        for key in revived {
            doc.delete(map, key.as_str())?;
        }
    }
    if removed.is_empty() {
        return Ok(());
    }
    let map = winner_or_create(doc, &all, &ROOT, ROOM_REMOVED_KEY)?;
    for id in removed {
        let key = contents_key(*id);
        if doc.get(&map, key.as_str())?.is_none() {
            doc.put(&map, key.as_str(), 1_u64)?;
        }
    }
    Ok(())
}

/// Every id the room's removal record ([`ROOM_REMOVED_KEY`]) holds, joined
/// across concurrently created records.
pub fn removed_of(doc: &AutoCommit) -> std::collections::BTreeSet<u64> {
    map_candidates(doc, &automerge::ROOT, ROOM_REMOVED_KEY)
        .iter()
        .flat_map(|map| {
            doc.keys(map)
                .filter_map(|key| contents_key_id(&key))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Publish `strip`, at position `order`, as a strip kept while empty
/// ([`STRIP_RETAINED_EMPTY_KEY`]): a person made it to put windows in, so
/// it reaches the others now, not once the first window does. Ordinary
/// projections keep it from then on. Only a strip in `local_salt`'s
/// namespace is written.
///
/// # Errors
///
/// The document's error when the root `contents` map or the strip's entry
/// cannot be created.
pub fn retain_owned_strip(
    doc: &mut AutoCommit,
    strip: &Strip,
    order: u64,
    local_salt: u16,
) -> Result<bool, automerge::AutomergeError> {
    use automerge::ROOT;
    if local_salt == 0 || peer_salt_of(strip.id.raw()) != local_salt {
        return Ok(false);
    }
    let contents_all = map_candidates(doc, &ROOT, ROOM_CONTENTS_KEY);
    let contents_id = winner_or_create(doc, &contents_all, &ROOT, ROOM_CONTENTS_KEY)?;
    let contents_all = or_created(contents_all, &contents_id);
    let key = contents_key(strip.id.raw());
    let strip_all = child_candidates(doc, &contents_all, key.as_str());
    let entry = winner_or_create(doc, &strip_all, &contents_id, key.as_str())?;
    let _ = doc.put_uint_if_changed(&entry, STRIP_RETAINED_EMPTY_KEY, 1);
    let _ = doc.put_str_if_changed(&entry, "name", &strip.name);
    let _ = doc.put_uint_if_changed(&entry, "order", order);
    Ok(true)
}

/// [`project_owned_contents`]'s per-strip pass over every candidate entry
/// of one strip (the first is the one written to): get-or-create its
/// `groups` map, project every group this machine owns (or owns a tab
/// within), then sweep group keys this machine owns that no longer appear
/// among `strip`'s groups.
fn project_owned_groups_in(
    doc: &mut AutoCommit,
    strip_entries: &[automerge::ObjId],
    strip: &Strip,
    mine: &impl Fn(u64) -> bool,
    kept: &mut std::collections::HashSet<u64>,
) -> Result<(), automerge::AutomergeError> {
    use std::collections::HashSet;

    let Some(strip_entry) = strip_entries.first() else {
        return Ok(());
    };
    let groups_all = child_candidates(doc, strip_entries, "groups");
    let groups_id = winner_or_create(doc, &groups_all, strip_entry, "groups")?;
    let groups_all = or_created(groups_all, &groups_id);
    let mut live_groups: HashSet<String> = HashSet::new();
    for (gi, group) in strip.window_groups.iter().enumerate() {
        if group.tabs.is_empty() {
            continue;
        }
        let owns_group = mine(group.id);
        let has_owned_tabs = group.tabs.iter().any(|t| mine(t.id));
        if !owns_group && !has_owned_tabs {
            continue;
        }
        let gkey = contents_key(group.id);
        let group_copies = child_candidates(doc, &groups_all, gkey.as_str());
        let Ok(group_entry) = winner_or_create(doc, &group_copies, &groups_id, gkey.as_str())
        else {
            continue;
        };
        if owns_group {
            live_groups.insert(gkey.clone());
            let _ = doc.put_uint_if_changed(&group_entry, "order", gi as u64);
            // Width, maximization and active-tab selection are local
            // presentation preferences. Old room documents may still
            // contain the first two fields, but no current peer writes
            // or reads them.
        }

        let group_copies = or_created(group_copies, &group_entry);
        let _ = project_owned_tabs_in(doc, &group_copies, group, mine, kept);
    }
    sweep_stale(doc, &groups_all, &live_groups, mine, kept);
    Ok(())
}

/// [`project_owned_groups_in`]'s per-group pass over every candidate entry
/// of one group (the first is the one written to): get-or-create its `tabs`
/// map, project every tab this machine owns, then sweep tab keys this
/// machine owns that no longer appear among `group`'s tabs.
fn project_owned_tabs_in(
    doc: &mut AutoCommit,
    group_entries: &[automerge::ObjId],
    group: &WindowGroup,
    mine: &impl Fn(u64) -> bool,
    kept: &mut std::collections::HashSet<u64>,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ScalarValue, transaction::Transactable};
    use std::collections::HashSet;

    let Some(group_entry) = group_entries.first() else {
        return Ok(());
    };
    let tabs_all = child_candidates(doc, group_entries, "tabs");
    let tabs_id = winner_or_create(doc, &tabs_all, group_entry, "tabs")?;
    let tabs_all = or_created(tabs_all, &tabs_id);
    let mut live_tabs: HashSet<String> = HashSet::new();
    for (ti, tab) in group.tabs.iter().enumerate() {
        if !mine(tab.id) {
            continue;
        }
        let tkey = contents_key(tab.id);
        live_tabs.insert(tkey.clone());
        let tab_copies = child_candidates(doc, &tabs_all, tkey.as_str());
        let Ok(tab_entry) = winner_or_create(doc, &tab_copies, &tabs_id, tkey.as_str()) else {
            continue;
        };
        let _ = doc.put_uint_if_changed(&tab_entry, "order", ti as u64);
        let _ = doc.put_str_if_changed(&tab_entry, "title", &tab.title);
        match &tab.app_title {
            Some(app_title) => {
                let _ = doc.put_str_if_changed(&tab_entry, "app_title", app_title);
            }
            None => {
                if doc.get(&tab_entry, "app_title").ok().flatten().is_some() {
                    let _ = doc.delete(&tab_entry, "app_title");
                }
            }
        }
        if let Ok(blob) = serde_json::to_vec(&tab.tile) {
            let unchanged = matches!(
                doc.get(&tab_entry, "tile_json").ok().flatten(),
                Some((automerge::Value::Scalar(v), _))
                    if matches!(v.as_ref(), ScalarValue::Bytes(b) if b == &blob)
            );
            if !unchanged {
                let _ = doc.put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
            }
        }
    }
    sweep_stale(doc, &tabs_all, &live_tabs, mine, kept);
    Ok(())
}

/// Read a room document's `contents` back into plain values. Every
/// level sorts by `(order, id)`; an entry with no `order` sorts
/// last so it can't displace one whose owner did state a position.
/// Concurrently created containers are joined at every level; for an
/// entry present in several, the winning copy supplies its fields.
pub fn contents_of(doc: &AutoCommit) -> Vec<ReplicatedStrip> {
    use automerge::{ROOT, ScalarValue, Value};

    let uint = |obj: &automerge::ObjId, key: &str| -> Option<u64> {
        match doc.get(obj, key).ok().flatten() {
            Some((Value::Scalar(v), _)) => match v.as_ref() {
                ScalarValue::Uint(n) => Some(*n),
                // A negative `Int` has no sane `order`/id reading, so
                // treat it as absent rather than wrapping it into a
                // huge `u64` via a bit-reinterpreting cast.
                ScalarValue::Int(n) => u64::try_from(*n).ok(),
                _ => None,
            },
            _ => None,
        }
    };
    let text = |obj: &automerge::ObjId, key: &str| -> Option<String> { doc.str_at(obj, key) };

    let contents_all = map_candidates(doc, &ROOT, ROOM_CONTENTS_KEY);
    let mut strips: Vec<ReplicatedStrip> = Vec::new();
    for (sid, strip_copies) in joined_entries(doc, &contents_all) {
        let Some(strip_entry) = strip_copies.first() else {
            continue;
        };
        let mut groups: Vec<ReplicatedGroup> = Vec::new();
        let groups_all = child_candidates(doc, &strip_copies, "groups");
        for (gid, group_copies) in joined_entries(doc, &groups_all) {
            let Some(group_entry) = group_copies.first() else {
                continue;
            };
            let mut tabs: Vec<ReplicatedTab> = Vec::new();
            let tabs_all = child_candidates(doc, &group_copies, "tabs");
            for (tid, tab_copies) in joined_entries(doc, &tabs_all) {
                let Some(tab_entry) = tab_copies.first() else {
                    continue;
                };
                let tile = match doc.get(tab_entry, "tile_json").ok().flatten() {
                    Some((Value::Scalar(v), _)) => match v.as_ref() {
                        ScalarValue::Bytes(b) => serde_json::from_slice(b).ok(),
                        _ => None,
                    },
                    _ => None,
                };
                tabs.push(ReplicatedTab {
                    id: tid,
                    order: uint(tab_entry, "order").unwrap_or(u64::MAX),
                    title: text(tab_entry, "title").unwrap_or_default(),
                    app_title: text(tab_entry, "app_title"),
                    tile,
                });
            }
            tabs.compact_sort_by_key(|t| (t.order, t.id));
            groups.push(ReplicatedGroup {
                id: gid,
                order: uint(group_entry, "order").unwrap_or(u64::MAX),
                tabs,
            });
        }
        groups.compact_sort_by_key(|g| (g.order, g.id));
        strips.push(ReplicatedStrip {
            id: sid,
            order: uint(strip_entry, "order").unwrap_or(u64::MAX),
            name: text(strip_entry, "name"),
            groups,
        });
    }
    strips.compact_sort_by_key(|s| (s.order, s.id));
    strips
}

/// Every value in `doc`'s `contents` that this machine owns, as a
/// comparable digest. A host admitting a peer's batch diffs this
/// before/after a peer's batch and refuses it whole if any owned
/// entry changed — enforcing "one writer per entry".
pub fn owned_contents_digest(doc: Option<&AutoCommit>, local_salt: u16) -> Vec<(u64, String)> {
    let Some(doc) = doc else {
        return Vec::new();
    };
    if local_salt == 0 {
        return Vec::new();
    }
    let mut out: Vec<(u64, String)> = Vec::new();
    for strip in contents_of(doc) {
        if peer_salt_of(strip.id) == local_salt {
            out.push((
                strip.id,
                format!(
                    "s|{}|{}",
                    strip.order,
                    strip.name.clone().unwrap_or_default()
                ),
            ));
        }
        for group in &strip.groups {
            if peer_salt_of(group.id) == local_salt {
                out.push((group.id, format!("g|{}", group.order)));
            }
            for tab in &group.tabs {
                if peer_salt_of(tab.id) == local_salt {
                    out.push((
                        tab.id,
                        format!(
                            "t|{}|{}|{:?}|{:?}",
                            tab.order, tab.title, tab.app_title, tab.tile
                        ),
                    ));
                }
            }
        }
    }
    out.compact_sort();
    out
}

/// What folding a room document reads besides one entry's own record: which
/// entries this reader writes to the room itself, and where the document
/// says the others are.
pub struct Fold<'a> {
    /// Whether this reader publishes the entry `id` itself: its record in the
    /// document is downstream of this reader's tree, so the tree's copy wins
    /// over it, and the tree's absence of it is a removal not projected yet.
    pub authored: &'a dyn Fn(u64) -> bool,
    /// The document's removal record ([`removed_of`]).
    pub removed: &'a std::collections::BTreeSet<u64>,
    /// Every group the document's `contents` places, in any strip.
    pub groups: &'a std::collections::BTreeSet<u64>,
    /// Every tab the document's `contents` places, in any group.
    pub tabs: &'a std::collections::BTreeSet<u64>,
}

impl Fold<'_> {
    /// Whether an entry of the tree that is missing from where the document
    /// would hold it has left: the document records it removed, or places
    /// it elsewhere among `placed`, and this reader is not the one that
    /// writes it. An entry the document never carried stays, whoever minted
    /// it.
    fn gone(&self, id: u64, placed: &std::collections::BTreeSet<u64>) -> bool {
        !(self.authored)(id) && (self.removed.contains(&id) || placed.contains(&id))
    }

    /// [`Self::gone`] for a strip, which no document places anywhere else.
    fn strip_gone(&self, id: u64) -> bool {
        !(self.authored)(id) && self.removed.contains(&id)
    }

    /// [`Self::gone`] for a group.
    fn group_gone(&self, id: u64) -> bool {
        self.gone(id, self.groups)
    }

    /// [`Self::gone`] for a tab.
    fn tab_gone(&self, id: u64) -> bool {
        self.gone(id, self.tabs)
    }

    /// `group` without what has left ([`Self::gone`]): its tabs, and the
    /// group itself once it holds no other. `None` when nothing of it stays;
    /// `changed` is set when anything went.
    fn prune_group(&self, mut group: WindowGroup, changed: &mut bool) -> Option<WindowGroup> {
        let before = group.tabs.len();
        group.tabs.retain(|tab| !self.tab_gone(tab.id));
        if group.tabs.len() != before {
            *changed = true;
            group
                .focus_history
                .retain(|id| group.tabs.iter().any(|tab| tab.id == *id));
            if !group.tabs.iter().any(|tab| tab.id == group.active_tab)
                && let Some(first) = group.tabs.first()
            {
                group.active_tab = first.id;
            }
        } else if group.tabs.is_empty() && !self.group_gone(group.id) {
            return Some(group);
        }
        if group.tabs.is_empty() {
            *changed = true;
            return None;
        }
        Some(group)
    }

    /// [`Self::prune_group`] over every group of `strip`. Reports whether
    /// anything went.
    fn prune(&self, strip: &mut Strip) -> bool {
        let mut changed = false;
        strip.window_groups = std::mem::take(&mut strip.window_groups)
            .into_iter()
            .filter_map(|group| self.prune_group(group, &mut changed))
            .collect();
        changed
    }
}

/// Reconcile one strip against what the room document says about it.
pub fn merge_strip(
    mut local: Strip,
    rep: ReplicatedStrip,
    fold: &Fold<'_>,
    changed: &mut bool,
) -> Strip {
    // A name is only taken from the document for a strip we do not
    // write; ours is ours to write.
    if !(fold.authored)(local.id.raw())
        && let Some(name) = rep.name
        && local.name != name
    {
        local.name = name;
        *changed = true;
    }
    let mut by_group: std::collections::BTreeMap<u64, ReplicatedGroup> =
        rep.groups.into_iter().map(|g| (g.id, g)).collect();
    let prior_group_order: Vec<u64> = local.window_groups.iter().map(|group| group.id).collect();

    let mut out: Vec<(u64, u64, WindowGroup)> = Vec::new();
    for (i, group) in std::mem::take(&mut local.window_groups)
        .into_iter()
        .enumerate()
    {
        let gid = group.id;
        match by_group.remove(&gid) {
            Some(rep) => {
                let order = if (fold.authored)(gid) {
                    i as u64
                } else {
                    rep.order
                };
                out.push((order, gid, merge_group(group, rep, fold, changed)));
            }
            // Not here in the document: kept unless it has left, with
            // whatever of it has not.
            None => {
                if let Some(group) = fold.prune_group(group, changed) {
                    out.push((i as u64, gid, group));
                }
            }
        }
    }
    for (gid, rep) in by_group {
        // Ours and not here: closed or moved here, and not projected yet.
        if (fold.authored)(gid) {
            continue;
        }
        out.push((rep.order, gid, hydrate_group(rep)));
        *changed = true;
    }
    out.compact_sort_by_key(|(order, id, _)| (*order, *id));
    if !prior_group_order
        .iter()
        .copied()
        .eq(out.iter().map(|(_, id, _)| *id))
    {
        *changed = true;
    }
    local.window_groups = out.into_iter().map(|(_, _, g)| g).collect();
    local
}

/// Reconcile one window group. **This is where view state is kept.**
pub fn merge_group(
    mut local: WindowGroup,
    rep: ReplicatedGroup,
    fold: &Fold<'_>,
    changed: &mut bool,
) -> WindowGroup {
    // Geometry and maximization remain exactly as this device last set
    // them. A peer changing its own desktop must never resize, maximize,
    // or otherwise rearrange ours.

    let mut by_tab: std::collections::BTreeMap<u64, ReplicatedTab> =
        rep.tabs.into_iter().map(|t| (t.id, t)).collect();
    let prior_tab_order: Vec<u64> = local.tabs.iter().map(|tab| tab.id).collect();
    let mut out: Vec<(u64, u64, Tab)> = Vec::new();
    for (i, tab) in std::mem::take(&mut local.tabs).into_iter().enumerate() {
        let tid = tab.id;
        let authored = (fold.authored)(tid);
        match by_tab.remove(&tid) {
            Some(rep) => {
                let order = if authored { i as u64 } else { rep.order };
                let mut tab = tab;
                if !authored {
                    if tab.title != rep.title {
                        tab.title.clone_from(&rep.title);
                        *changed = true;
                    }
                    if tab.app_title != rep.app_title {
                        tab.app_title.clone_from(&rep.app_title);
                        *changed = true;
                    }
                    if let Some(tile) = rep.tile.clone() {
                        let tile = localise_tile(tile, Some(&tab.tile), &rep.title);
                        if serde_json::to_vec(&tab.tile).ok() != serde_json::to_vec(&tile).ok() {
                            tab.tile = tile;
                            *changed = true;
                        }
                    }
                }
                out.push((order, tid, tab));
            }
            None if fold.tab_gone(tid) => *changed = true,
            None => out.push((i as u64, tid, tab)),
        }
    }
    for (tid, rep) in by_tab {
        if (fold.authored)(tid) {
            continue;
        }
        out.push((rep.order, tid, hydrate_tab(rep)));
        *changed = true;
    }
    out.compact_sort_by_key(|(order, id, _)| (*order, *id));
    if !prior_tab_order
        .iter()
        .copied()
        .eq(out.iter().map(|(_, id, _)| *id))
    {
        *changed = true;
    }
    local.tabs = out.into_iter().map(|(_, _, t)| t).collect();

    // `active_tab` is not taken from `rep` — the document doesn't
    // carry one. Moves only when the tab is genuinely gone.
    if !local.tabs.iter().any(|t| t.id == local.active_tab)
        && let Some(first) = local.tabs.first()
    {
        local.active_tab = first.id;
    }
    local
}

/// A strip that arrived from a peer.
pub fn hydrate_strip(rep: ReplicatedStrip) -> Strip {
    Strip {
        id: StripId::from(rep.id),
        name: rep.name.unwrap_or_default(),
        window_groups: rep.groups.into_iter().map(hydrate_group).collect(),
    }
}

/// A window group that arrived from a peer. Its `active_tab` is
/// chosen **here** — a local decision about a local view, not the
/// opening peer's to make.
pub fn hydrate_group(rep: ReplicatedGroup) -> WindowGroup {
    let tabs: Vec<Tab> = rep.tabs.into_iter().map(hydrate_tab).collect();
    let active_tab = tabs.first().map(|t| t.id).unwrap_or_default();
    WindowGroup {
        id: rep.id,
        tabs,
        active_tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// A tab that arrived from a peer.
pub fn hydrate_tab(rep: ReplicatedTab) -> Tab {
    let title = rep.title;
    Tab {
        tile: rep.tile.map_or(
            TileContent::Stub {
                kind: AppKind::Other(String::new()),
                label: String::new(),
            },
            |tile| localise_tile(tile, None, &title),
        ),
        id: rep.id,
        title,
        app_title: rep.app_title,
    }
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

/// Who is folding a room document into their tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    /// A peer that publishes into the room under this salt: the entries it
    /// writes there are its own, never overwritten by the document, and an
    /// entry of its own the document holds but its tree does not was closed
    /// here and is not projected yet.
    Owner(u16),
    /// A read-only viewer publishes nothing: the document is the whole truth
    /// about the room's windows.
    Viewer,
}

/// Fold the room document `doc`'s contents into the cached workspace
/// addressing `room`, reporting whether anything changed.
///
/// What the workspace holds is the document's to say, whoever minted it: an
/// entry the document carries is taken in, and one it does not stays unless
/// the document records it removed ([`removed_of`]) or places it elsewhere.
/// A document that never carried an entry, such as a room's first one,
/// takes nothing away. Entries this reader writes itself
/// ([`Reader::Owner`]) are never overwritten or brought back: the document
/// is downstream of the cache for them, so reading one back could only
/// revert an unprojected local change.
///
/// `active_tab` is never read from the document: it stays put unless
/// gone, and a group arriving from a peer opens on its
/// first tab. Vanishing strips take their `active_strip`/
/// `active_group` with them so resolvers fall back rather than point
/// at nothing.
pub fn merge_room_contents(
    desktop: &mut DesktopTree,
    room: RoomId,
    doc: &AutoCommit,
    reader: Reader,
) -> bool {
    let authored = |id: u64| match reader {
        Reader::Owner(salt) => peer_salt_of(id) == salt,
        Reader::Viewer => false,
    };

    let Some(ws) = desktop.workspaces.iter_mut().find(|w| w.room() == room) else {
        return false;
    };
    let replicated = contents_of(doc);
    let removed = removed_of(doc);
    let groups: std::collections::BTreeSet<u64> = replicated
        .iter()
        .flat_map(|strip| strip.groups.iter().map(|group| group.id))
        .collect();
    let tabs: std::collections::BTreeSet<u64> = replicated
        .iter()
        .flat_map(|strip| &strip.groups)
        .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
        .collect();
    let fold = Fold {
        authored: &authored,
        removed: &removed,
        groups: &groups,
        tabs: &tabs,
    };
    let mut changed = false;
    let prior_strip_order: Vec<StripId> = ws.strips.iter().map(|strip| strip.id).collect();
    let mut by_strip: std::collections::BTreeMap<u64, ReplicatedStrip> =
        replicated.into_iter().map(|s| (s.id, s)).collect();

    // The order is canonical: every member computes the same one from the
    // same document, and merging a document again changes nothing. Every
    // strip the document carries stands at the position it was published
    // at, `(order, id)`, ours included (it is where our last projection put
    // it, and no command reorders strips); and a strip the document does not
    // carry goes back to the index it stood at. Mixing our cache's indices
    // with published positions instead re-sorted the strips on every merge
    // of an unchanged document, and each re-sort re-projected our positions
    // for the others to re-sort again.
    let mut published: Vec<(u64, u64, Strip)> = Vec::new();
    let mut kept: Vec<(usize, Strip)> = Vec::new();
    // Old strip ids that don't make it into the result below — collected
    // as the loop drops them rather than diffed afterward against a
    // snapshot, since `ws.strips` is about to be overwritten with the
    // merged result.
    let mut vanished: Vec<StripId> = Vec::new();
    for (i, mut strip) in std::mem::take(&mut ws.strips).into_iter().enumerate() {
        let sid = strip.id.raw();
        match by_strip.remove(&sid) {
            // Not in the document: it stays, with whatever of it has not
            // left; a strip that has left stays only while it still holds
            // windows that have not.
            None => {
                changed |= fold.prune(&mut strip);
                if fold.strip_gone(sid) && strip.window_groups.is_empty() {
                    changed = true;
                    vanished.push(StripId::from(sid));
                } else {
                    kept.push((i, strip));
                }
            }
            Some(rep) => {
                let order = rep.order;
                let merged = merge_strip(strip, rep, &fold, &mut changed);
                published.push((order, sid, merged));
            }
        }
    }
    for (sid, rep) in by_strip {
        // Ours and gone from the cache: closed here and not projected yet.
        if authored(sid) {
            continue;
        }
        let order = rep.order;
        published.push((order, sid, hydrate_strip(rep)));
        changed = true;
    }
    published.compact_sort_by_key(|(order, id, _)| (*order, *id));
    let mut strips: Vec<Strip> = published.into_iter().map(|(_, _, strip)| strip).collect();
    // In ascending index order, so each lands where it stood (or at the
    // end, when fewer strips precede it now) without moving one before it.
    for (index, strip) in kept {
        strips.insert(index.min(strips.len()), strip);
    }
    if !prior_strip_order
        .iter()
        .copied()
        .eq(strips.iter().map(|strip| strip.id))
    {
        changed = true;
    }
    ws.strips = strips;

    let ws_id = ws.id;
    let live: Vec<StripId> = ws.strips.iter().map(|s| s.id).collect();

    // Removed rather than repointed: resolvers already fall back to
    // the first strip/group with no explicit selection.
    if desktop
        .active_strip
        .get(&ws_id)
        .is_some_and(|active| !live.contains(active))
    {
        desktop.active_strip.remove(&ws_id);
    }
    for strip in vanished {
        desktop.active_group.remove(&strip);
    }
    changed
}

/// The room a candidate document says it describes, when it names a valid
/// one. A payload naming a different room than the one it answers must be
/// refused before anything else reads it.
pub fn declared_room(candidate: &AutoCommit) -> Option<RoomId> {
    RoomId::parse(&candidate.str_at(&automerge::ROOT, ROOM_ID_KEY)?)
}

/// How many entries this peer owns that `candidate` would rewrite,
/// compared only over entries both documents already hold: an entry of
/// ours the batch changes counts, one it merely carries that `current`
/// lacks does not. Room documents are not persisted, so comparing whole
/// sets would refuse a peer's own history back after a restart.
pub fn changed_owned_entries(
    current: Option<&AutoCommit>,
    candidate: &AutoCommit,
    local_salt: u16,
) -> usize {
    let before = owned_contents_digest(current, local_salt);
    let after = owned_contents_digest(Some(candidate), local_salt);
    let before_by_id: std::collections::BTreeMap<u64, &String> =
        before.iter().map(|(id, v)| (*id, v)).collect();
    after
        .iter()
        .filter(|(id, v)| before_by_id.get(id).is_some_and(|prev| *prev != v))
        .count()
}

/// Apply one already-admitted room document to `tree`: create the room's
/// workspace if the tree has none (under [`WorkspaceId::of_room`]),
/// take the room's name and owner, and fold its contents in as a
/// [`Reader::Viewer`]. Existing local workspace identity and local selection
/// remain untouched. Returns whether anything changed.
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
    let changed = merge_room_contents(tree, room, doc, Reader::Viewer);
    Ok(changed || renamed || reshortcut || reprofiled)
}

/// The room's topology namespaces: salt → the device key (64 lowercase hex
/// digits) that owns it. Malformed entries are skipped; salt `0` is never a
/// namespace.
pub fn contents_owners_of(doc: &AutoCommit) -> std::collections::BTreeMap<u16, String> {
    use automerge::ROOT;
    let Some((_, owners)) = doc.get(&ROOT, ROOM_CONTENTS_OWNERS_KEY).ok().flatten() else {
        return std::collections::BTreeMap::new();
    };
    doc.keys(&owners)
        .filter_map(|key| {
            let salt = u16::from_str_radix(&key, 16)
                .ok()
                .filter(|salt| *salt != 0)?;
            let (value, _) = doc.get(&owners, key.as_str()).ok().flatten()?;
            Some((salt, value.as_str()?.to_owned()))
        })
        .collect()
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

/// The people the room `doc` governs lists as its members, by person id:
/// who still holds what they made on its desktop
/// ([`crate::ownership::released`]). `None` when `doc` lists nobody (not
/// heard yet), so nothing is taken as released before the list arrives.
#[must_use]
pub fn room_members(doc: &AutoCommit) -> Option<std::collections::BTreeSet<String>> {
    let people: std::collections::BTreeSet<String> =
        blob_map_of::<serde_json::Value>(doc, ROOM_MEMBERS_KEY)
            .into_iter()
            .map(|(person, _)| person)
            .collect();
    (!people.is_empty()).then_some(people)
}

/// The layout scope of the room `doc` governs: its governor's choice, else
/// [`RoomLayout::Shared`], an invite-only room's default.
#[must_use]
pub fn room_layout(doc: &AutoCommit) -> RoomLayout {
    room_layout_choice(doc).unwrap_or_default()
}

/// Delete from `doc`'s `contents` every strip, group and tab whose id `keep`
/// refuses, with everything under it; what `keep` keeps is walked for what it
/// holds. Returns how many entries were deleted. Nothing else of the
/// document is touched.
pub fn drop_contents_except(doc: &mut AutoCommit, keep: &impl Fn(u64) -> bool) -> usize {
    let contents = map_candidates(doc, &automerge::ROOT, ROOM_CONTENTS_KEY);
    drop_contents_level(doc, &contents, 0, keep)
}

/// [`drop_contents_except`] over one level of `contents`: strips (`0`),
/// groups (`1`) or tabs (`2`).
fn drop_contents_level(
    doc: &mut AutoCommit,
    containers: &[automerge::ObjId],
    level: u8,
    keep: &impl Fn(u64) -> bool,
) -> usize {
    use automerge::transaction::Transactable as _;
    let child = match level {
        0 => Some("groups"),
        1 => Some("tabs"),
        _ => None,
    };
    let mut dropped: usize = 0;
    for container in containers {
        let keys: Vec<String> = doc.keys(container).collect();
        for key in keys {
            let Some(id) = contents_key_id(&key) else {
                continue;
            };
            if !keep(id) {
                if doc.delete(container, key.as_str()).is_ok() {
                    dropped = dropped.saturating_add(1);
                }
                continue;
            }
            if let Some(child) = child {
                let entries = map_candidates(doc, container, key.as_str());
                let nested = child_candidates(doc, &entries, child);
                dropped = dropped.saturating_add(drop_contents_level(
                    doc,
                    &nested,
                    level.saturating_add(1),
                    keep,
                ));
            }
        }
    }
    dropped
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
    use crate::ids::compose_id;
    use tilo_automerge::ApplyIncoming;

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
        let tab = hydrate_tab(ReplicatedTab {
            id: 9,
            order: 0,
            title: "Poster.xcf".into(),
            app_title: None,
            tile: Some(hosted.clone()),
        });
        assert_eq!(tab.tile, hosted);
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

    /// Every member orders the strips the same from the same document, and
    /// merging it again changes nothing: a sync of a tree that is already
    /// right leaves it alone instead of re-sorting it for the others to
    /// re-sort back.
    #[test]
    fn the_strip_order_is_canonical_and_settles() {
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let room = RoomId::new();
        let strip = |salt: u16, n: u64, name: &str| {
            let mut strip = Strip::new(StripId(compose_id(salt, n)), name);
            strip.window_groups.push(canvas_group(salt, n + 1, name));
            strip
        };
        let mut a = room_desktop(room, 1, vec![strip(A, 10, "a1"), strip(A, 20, "a2")]);
        let mut b = room_desktop(room, 2, vec![strip(B, 10, "b1")]);
        let mut doc = AutoCommit::new();
        for (tree, salt) in [(&a, A), (&b, B)] {
            project_owned_contents(
                &mut doc,
                &tree.workspaces[0].strips,
                salt,
                None,
                EmptyStrips::Skip,
            )
            .expect("projected");
        }
        let names = |tree: &crate::tree::DesktopTree| -> Vec<String> {
            tree.workspaces[0]
                .strips
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        assert!(merge_room_contents(&mut a, room, &doc, Reader::Owner(A)));
        assert!(merge_room_contents(&mut b, room, &doc, Reader::Owner(B)));
        assert_eq!(names(&a), names(&b), "one order for every member");
        assert_eq!(names(&a).len(), 3);
        for (tree, salt) in [(&mut a, A), (&mut b, B)] {
            assert!(
                !merge_room_contents(tree, room, &doc, Reader::Owner(salt)),
                "merging the same document again changes nothing"
            );
        }
    }

    /// A strip a person made reaches the others while still empty, and a
    /// window they put in another strip reaches them in that strip.
    #[test]
    fn a_made_strip_arrives_empty_and_windows_arrive_in_their_strip() {
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let room = RoomId::new();
        let mut first = Strip::new(StripId(compose_id(A, 1)), "Main");
        first.window_groups.push(canvas_group(A, 10, "Sketch"));
        let made = Strip::new(StripId(compose_id(A, 12)), "Research");
        let a = room_desktop(room, 1, vec![first.clone(), made.clone()]);
        let mut b = room_desktop(room, 2, vec![Strip::new(StripId(compose_id(B, 1)), "Mine")]);
        let mut doc = AutoCommit::new();
        assert!(retain_owned_strip(&mut doc, &made, 1, A).expect("retained"));
        project_owned_contents(
            &mut doc,
            &a.workspaces[0].strips,
            A,
            None,
            EmptyStrips::Skip,
        )
        .expect("projected");

        assert!(merge_room_contents(&mut b, room, &doc, Reader::Owner(B)));
        let strips = &b.workspaces[0].strips;
        assert!(
            strips
                .iter()
                .any(|s| s.id == made.id && s.window_groups.is_empty()),
            "a made strip arrives empty"
        );
        assert!(
            strips
                .iter()
                .any(|s| s.id == first.id
                    && s.window_groups.iter().any(|g| g.id == compose_id(A, 10))),
            "a window arrives in its strip"
        );
        assert!(
            strips.iter().any(|s| s.id == StripId(compose_id(B, 1))),
            "B's own strip stays"
        );
    }

    /// Every tab id in the room's workspace in `tree`.
    fn tab_ids(tree: &crate::tree::DesktopTree) -> Vec<u64> {
        tree.workspaces[0]
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect()
    }

    /// A workspace's windows are its own whoever minted them: a document
    /// that does not carry them (a room's first one, one a peer published
    /// other windows to) takes none away, for a member under any salt and
    /// for a viewer.
    #[test]
    fn windows_minted_under_any_salt_survive_a_merge() {
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        for minted in [0, 1, A, B, 0x0A51, 0xF276, crate::ids::RESERVED_SALT] {
            for reader in [Reader::Owner(B), Reader::Owner(0x0A51), Reader::Viewer] {
                let room = RoomId::new();
                let mut restored = Strip::new(StripId(compose_id(minted, 1)), "Main");
                restored
                    .window_groups
                    .push(canvas_group(minted, 4, "Notes"));
                restored
                    .window_groups
                    .push(canvas_group(minted, 5, "Canvas"));
                let mut tree = room_desktop(room, 1, vec![restored]);
                let held = tab_ids(&tree);

                let mut other = Strip::new(StripId(compose_id(A, 100)), "Theirs");
                other.window_groups.push(canvas_group(A, 110, "Sketch"));
                other.window_groups.push(canvas_group(A, 111, "Closed"));
                let mut published = AutoCommit::new();
                project_owned_contents(
                    &mut published,
                    std::slice::from_ref(&other),
                    A,
                    None,
                    EmptyStrips::Skip,
                )
                .expect("projected");
                // A takes one of its own windows out: the document records
                // a removal, of A's window only.
                other.window_groups.truncate(1);
                project_owned_contents(&mut published, &[other], A, None, EmptyStrips::Skip)
                    .expect("projected");

                for document in [AutoCommit::new(), published] {
                    merge_room_contents(&mut tree, room, &document, reader);
                    let now = tab_ids(&tree);
                    for id in &held {
                        assert!(
                            now.contains(id),
                            "window {id:#x} minted under {minted:#x} stays for {reader:?}: {now:x?}"
                        );
                    }
                }
            }
        }
    }

    /// Two devices that mint while apart never name two windows alike, and a
    /// room both publish into holds every window of both, for each of them.
    #[test]
    fn two_devices_minting_offline_never_collide() {
        let salts = [
            crate::ids::device_salt(0x0A51),
            crate::ids::device_salt(0xF276),
        ];
        let room = RoomId::new();
        let mut trees: Vec<crate::tree::DesktopTree> = Vec::new();
        let mut minted: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        let mut total = 0_usize;
        for (device, salt) in salts.into_iter().enumerate() {
            let mut ids = crate::ids::IdMinter::new(salt);
            let mut strip = Strip::new(StripId(ids.mint()), "Main");
            for n in 0..200 {
                let mut group = canvas_group(0, 0, &format!("window {n}"));
                group.id = ids.mint();
                group.tabs[0].id = ids.mint();
                group.active_tab = group.tabs[0].id;
                strip.window_groups.push(group);
            }
            let tree = room_desktop(room, device as u64 + 1, vec![strip]);
            for strip in &tree.workspaces[0].strips {
                minted.insert(strip.id.raw());
                for group in &strip.window_groups {
                    minted.insert(group.id);
                    minted.extend(group.tabs.iter().map(|tab| tab.id));
                }
            }
            total += 1 + 200 * 2;
            trees.push(tree);
        }
        assert_eq!(minted.len(), total, "no id was minted twice");

        // Each publishes into its own copy of the room while apart; the
        // copies then meet.
        let mut docs: Vec<AutoCommit> = trees
            .iter()
            .zip(salts)
            .map(|(tree, salt)| {
                let mut doc = AutoCommit::new();
                project_owned_contents(
                    &mut doc,
                    &tree.workspaces[0].strips,
                    salt,
                    None,
                    EmptyStrips::Skip,
                )
                .expect("projected");
                doc
            })
            .collect();
        let mut theirs = docs.pop().expect("two documents");
        let mut joined = docs.pop().expect("two documents");
        joined.merge_incoming(&mut theirs).expect("merged");
        let windows: std::collections::BTreeSet<u64> = trees.iter().flat_map(tab_ids).collect();
        for (tree, salt) in trees.iter_mut().zip(salts) {
            merge_room_contents(tree, room, &joined, Reader::Owner(salt));
            let held: std::collections::BTreeSet<u64> = tab_ids(tree).into_iter().collect();
            assert_eq!(held, windows, "device {salt:#x} holds every window of both");
        }
    }

    /// A window taken out of the workspace stays out: merging the document
    /// that records it gone, again and again, from a tree that still held
    /// it or from one restored from before, never brings it back.
    #[test]
    fn a_window_removed_in_the_workspace_stays_removed_after_a_merge() {
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let room = RoomId::new();
        let mut strip = Strip::new(StripId(compose_id(A, 1)), "Main");
        strip.window_groups.push(canvas_group(A, 10, "Notes"));
        strip.window_groups.push(canvas_group(A, 11, "Terminal"));
        let mut a = room_desktop(room, 1, vec![strip]);
        let mut doc = AutoCommit::new();
        project_owned_contents(
            &mut doc,
            &a.workspaces[0].strips,
            A,
            None,
            EmptyStrips::Skip,
        )
        .expect("projected");

        let mut b = room_desktop(room, 2, vec![Strip::new(StripId(compose_id(B, 1)), "Mine")]);
        merge_room_contents(&mut b, room, &doc, Reader::Owner(B));
        assert!(tab_ids(&b).contains(&compose_id(A, 10)));
        // A device restored from a state file saved now, and a viewer, both
        // still holding the window once it goes.
        let restored = b.clone();
        let mut viewer = a.clone();

        a.workspaces[0].strips[0]
            .window_groups
            .retain(|group| group.id != compose_id(A, 10));
        project_owned_contents(
            &mut doc,
            &a.workspaces[0].strips,
            A,
            None,
            EmptyStrips::Skip,
        )
        .expect("projected");
        assert!(removed_of(&doc).contains(&compose_id(A, 10)));

        for (tree, reader) in [
            (&mut b, Reader::Owner(B)),
            (&mut restored.clone(), Reader::Owner(B)),
            (&mut restored.clone(), Reader::Owner(0x0A51)),
            (&mut viewer, Reader::Viewer),
        ] {
            for _ in 0..2 {
                merge_room_contents(tree, room, &doc, reader);
                let held = tab_ids(tree);
                assert!(
                    !held.contains(&compose_id(A, 10)),
                    "the removed window stays removed for {reader:?}: {held:x?}"
                );
                assert!(held.contains(&compose_id(A, 11)), "the other one stays");
            }
        }

        // B publishing its own windows writes nothing of A's back.
        project_owned_contents(
            &mut doc,
            &b.workspaces[0].strips,
            B,
            None,
            EmptyStrips::Skip,
        )
        .expect("projected");
        let mut late = restored;
        merge_room_contents(&mut late, room, &doc, Reader::Owner(B));
        assert!(!tab_ids(&late).contains(&compose_id(A, 10)));
    }

    #[test]
    fn a_window_closed_here_stays_closed_while_the_room_still_shows_it() {
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let room = RoomId::new();
        let mut strip = Strip::new(StripId(compose_id(A, 1)), "Main");
        strip.window_groups.push(canvas_group(A, 10, "Notes"));
        strip.window_groups.push(canvas_group(B, 20, "Notes, B"));
        let mut own = Strip::new(StripId(compose_id(A, 2)), "Own");
        own.window_groups.push(canvas_group(A, 30, "Terminal"));
        let mut a = room_desktop(room, 1, vec![strip, own]);
        let mut doc = AutoCommit::new();
        project_owned_contents(
            &mut doc,
            &a.workspaces[0].strips,
            A,
            None,
            EmptyStrips::Skip,
        )
        .expect("projected");
        project_owned_contents(
            &mut doc,
            &a.workspaces[0].strips,
            B,
            None,
            EmptyStrips::Skip,
        )
        .expect("projected");

        // A closes its window and its own strip before projecting again.
        a.workspaces[0].strips[0]
            .window_groups
            .retain(|group| group.id != compose_id(A, 10));
        a.workspaces[0].strips.truncate(1);
        let windows = |tree: &crate::tree::DesktopTree| -> Vec<u64> {
            tree.workspaces[0]
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .map(|group| group.id)
                .collect()
        };
        merge_room_contents(&mut a, room, &doc, Reader::Owner(A));
        assert_eq!(
            windows(&a),
            vec![compose_id(B, 20)],
            "nothing of A's comes back"
        );
        assert_eq!(a.workspaces[0].strips.len(), 1, "nor its strip");
    }

    #[test]
    fn a_window_of_ours_in_a_peers_strip_stays_when_the_peer_takes_the_strip_away() {
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let room = RoomId::new();
        let mut strip = Strip::new(StripId(compose_id(A, 1)), "Main");
        strip.window_groups.push(canvas_group(A, 10, "Notes"));
        strip.window_groups.push(canvas_group(B, 20, "Notes, B"));
        let mut b = room_desktop(room, 2, vec![strip.clone()]);

        // A published the strip, then took it away with its own window.
        let mut doc = AutoCommit::new();
        project_owned_contents(&mut doc, &[strip], A, None, EmptyStrips::Skip).expect("projected");
        project_owned_contents(&mut doc, &[], A, None, EmptyStrips::Skip).expect("projected");
        merge_room_contents(&mut b, room, &doc, Reader::Owner(B));
        let strips = &b.workspaces[0].strips;
        assert_eq!(strips.len(), 1, "the strip stays while it holds B's window");
        assert_eq!(
            strips[0]
                .window_groups
                .iter()
                .map(|group| group.id)
                .collect::<Vec<_>>(),
            vec![compose_id(B, 20)],
            "only B's own window stays in it"
        );
    }

    /// A person's name for a host window travels with the window's own
    /// title behind it, and clearing it travels too: every member shows the
    /// name, and the window's latest title once it is cleared.
    #[test]
    fn a_named_host_window_carries_its_own_title_through_the_room() {
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let room = RoomId::new();
        let tab_id = compose_id(A, 10);
        let mut tab = Tab {
            id: tab_id,
            title: "~/src".into(),
            tile: TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: 4,
            },
            app_title: None,
        };
        tab.rename("Build");
        let mut group = canvas_group(A, 10, "unused");
        group.tabs = vec![tab];
        let mut strip = Strip::new(StripId(compose_id(A, 1)), "Main");
        strip.window_groups.push(group);
        let mut a = room_desktop(room, 1, vec![strip]);
        let mut b = room_desktop(room, 2, vec![Strip::new(StripId(compose_id(B, 1)), "Mine")]);
        let mut doc = AutoCommit::new();
        let project = |doc: &mut AutoCommit, a: &crate::tree::DesktopTree| {
            project_owned_contents(doc, &a.workspaces[0].strips, A, None, EmptyStrips::Skip)
                .expect("projected");
        };
        let find_tab = |tree: &crate::tree::DesktopTree| {
            tree.workspaces[0]
                .strips
                .iter()
                .flat_map(|s| &s.window_groups)
                .flat_map(|g| &g.tabs)
                .find(|t| t.id == tab_id)
                .cloned()
                .expect("the window is there")
        };
        project(&mut doc, &a);
        let replicated = contents_of(&doc)
            .into_iter()
            .flat_map(|s| s.groups)
            .flat_map(|g| g.tabs)
            .find(|t| t.id == tab_id)
            .expect("the tab is in the room");
        assert_eq!(replicated.title, "Build");
        assert_eq!(replicated.app_title.as_deref(), Some("~/src"));
        merge_room_contents(&mut b, room, &doc, Reader::Owner(B));
        let arrived = find_tab(&b);
        assert_eq!(arrived.title, "Build");
        assert_eq!(arrived.app_title.as_deref(), Some("~/src"));

        // The window keeps titling itself behind the name.
        let tab = &mut a.workspaces[0].strips[0].window_groups[0].tabs[0];
        tab.set_window_title("cargo test");
        project(&mut doc, &a);
        merge_room_contents(&mut b, room, &doc, Reader::Owner(B));
        assert_eq!(find_tab(&b).title, "Build");
        assert_eq!(find_tab(&b).app_title.as_deref(), Some("cargo test"));

        let tab = &mut a.workspaces[0].strips[0].window_groups[0].tabs[0];
        tab.rename("");
        project(&mut doc, &a);
        assert!(
            contents_of(&doc)
                .into_iter()
                .flat_map(|s| s.groups)
                .flat_map(|g| g.tabs)
                .all(|t| t.app_title.is_none()),
            "a cleared name leaves nothing behind in the room"
        );
        merge_room_contents(&mut b, room, &doc, Reader::Owner(B));
        let cleared = find_tab(&b);
        assert_eq!(cleared.title, "cargo test");
        assert_eq!(cleared.app_title, None);
    }

    /// The layout scope is the governor's word in the room document: absent,
    /// an invite-only room's default (shared); a word this build does not
    /// read is personal, never shared.
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

    /// What another namespace put into the room's contents goes, down to a
    /// window it opened in someone else's strip, and everything of the kept
    /// namespace stays.
    #[test]
    fn dropping_contents_keeps_only_what_keep_admits_at_every_level() {
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let mut a_strip = Strip::new(StripId(compose_id(A, 1)), "Main");
        a_strip.window_groups.push(canvas_group(A, 2, "Board"));
        let mut b_view = a_strip.clone();
        b_view
            .window_groups
            .push(canvas_group(B, 3, "Alacritty: ~/secret"));
        let b_strip = {
            let mut strip = Strip::new(StripId(compose_id(B, 4)), "Mine");
            strip.window_groups.push(canvas_group(B, 5, "htop"));
            strip
        };
        let mut doc = AutoCommit::new();
        project_owned_contents(&mut doc, &[a_strip], A, None, EmptyStrips::Skip).unwrap();
        project_owned_contents(&mut doc, &[b_view, b_strip], B, None, EmptyStrips::Skip).unwrap();
        assert_eq!(contents_of(&doc).len(), 2);

        let dropped = drop_contents_except(&mut doc, &|id| peer_salt_of(id) == A);
        assert_eq!(dropped, 2, "B's strip, and B's group in A's strip");
        let after = contents_of(&doc);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, compose_id(A, 1));
        let titles: Vec<&str> = after[0]
            .groups
            .iter()
            .flat_map(|group| group.tabs.iter().map(|tab| tab.title.as_str()))
            .collect();
        assert_eq!(titles, ["Board"]);
        assert_eq!(
            drop_contents_except(&mut doc, &|id| peer_salt_of(id) == A),
            0,
            "nothing left to drop"
        );
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
