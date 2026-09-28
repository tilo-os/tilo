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

use crate::app::AppKind;
use crate::ids::{GroupId, RoomId, StripId, TabId, WorkspaceId, counter_of, peer_salt_of};
use crate::layout::{Strip, Tab, TileContent, WindowGroup};
use crate::task::{WorkspaceEffectReceipt, WorkspaceTask};
use crate::tree::DesktopTree;
use crate::width::WindowWidth;
use crate::workspace::Workspace;

/// Key naming the room a document describes (a UUID string).
pub const ROOM_ID_KEY: &str = "room";
/// Key of the room's display name.
pub const ROOM_NAME_KEY: &str = "name";
/// Key of the room owner's user id.
pub const ROOM_OWNER_KEY: &str = "owner";
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
/// Key a room document stores its durable tasks under.
pub const ROOM_TASKS_KEY: &str = "tasks_v1";
/// Key a room document stores its effect receipts under.
pub const ROOM_EFFECT_RECEIPTS_KEY: &str = "effect_receipts_v1";
/// Key a room document stores its saved shortcuts under.
pub const ROOM_AUTOMATIONS_KEY: &str = "automations_v1";
/// Key a room document stores its schedule claims under.
pub const ROOM_SCHEDULE_CLAIMS_KEY: &str = "schedule_claims_v1";

/// Key a room document stores its [`collab_types::ChannelClass`] under.
/// Governance: only the room's authority writes it, and a member refuses
/// any batch that would change a class it already holds.
pub const ROOM_CHANNEL_CLASS_KEY: &str = "channel_class";

/// Record `class` as the room's channel class. Written by the governing
/// machine on every projection, so a room document always states its class
/// rather than leaving a reader to assume one.
pub fn project_channel_class(doc: &mut AutoCommit, class: collab_types::ChannelClass) {
    put_str_if_changed(
        doc,
        &automerge::ROOT,
        ROOM_CHANNEL_CLASS_KEY,
        class.as_str(),
    );
}

/// The channel class a room document records. `None` when the document
/// states none (governance not yet received) or states a value this build
/// does not know, which a reader treats as "not yet governed" rather than
/// guessing.
pub fn channel_class_of(doc: &AutoCommit) -> Option<collab_types::ChannelClass> {
    use automerge::{ROOT, ScalarValue, Value};
    match doc.get(&ROOT, ROOM_CHANNEL_CLASS_KEY).ok().flatten() {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Str(name) => collab_types::ChannelClass::parse(name),
            _ => None,
        },
        _ => None,
    }
}

/// One tab as a room's replication document carries it. Deliberately not
/// a [`Tab`]: no field here for anything a person's own session decided.
#[derive(Clone, Debug)]
pub struct ReplicatedTab {
    pub id: TabId,
    pub order: u64,
    pub title: String,
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

/// Write `value` at `key` only when the document does not already say
/// it, so a document accumulates a change per changed value rather than
/// one per export — otherwise each sync round manufactures ops out of
/// untouched state and two peers exporting to each other never fall
/// quiet.
pub fn put_str_if_changed(doc: &mut AutoCommit, obj: &automerge::ObjId, key: &str, value: &str) {
    use automerge::{ReadDoc, ScalarValue, Value, transaction::Transactable};
    let current = match doc.get(obj, key).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    };
    if current.as_deref() != Some(value) {
        let _ = doc.put(obj, key, value.to_string());
    }
}

/// [`put_str_if_changed`] for an unsigned integer.
pub fn put_uint_if_changed(doc: &mut AutoCommit, obj: &automerge::ObjId, key: &str, value: u64) {
    use automerge::{ReadDoc, ScalarValue, Value, transaction::Transactable};
    let current = match doc.get(obj, key).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Uint(v) => Some(*v),
            _ => None,
        },
        _ => None,
    };
    if current != Some(value) {
        let _ = doc.put(obj, key, ScalarValue::Uint(value));
    }
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
) {
    use automerge::transaction::Transactable;
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

/// Project durable tasks as independently mergeable values. Task ids are
/// stable, so two collaborators creating work concurrently add two map
/// keys instead of racing over a positional list.
///
/// # Errors
///
/// The document's error when a map this projection needs cannot be
/// created.
pub fn project_workspace_tasks(
    doc: &mut AutoCommit,
    tasks: &[WorkspaceTask],
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    let tasks_id = match doc.get(&ROOT, ROOM_TASKS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None if tasks.is_empty() => return Ok(()),
        None => doc.put_object(&ROOT, ROOM_TASKS_KEY, ObjType::Map)?,
    };
    for task in tasks {
        let Ok(blob) = serde_json::to_vec(task) else {
            continue;
        };
        let unchanged = matches!(
            doc.get(&tasks_id, task.id.as_str()).ok().flatten(),
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(current) if current == &blob)
        );
        if !unchanged {
            let _ = doc.put(&tasks_id, task.id.as_str(), ScalarValue::Bytes(blob));
        }
    }
    Ok(())
}

pub fn workspace_tasks_of(doc: &AutoCommit) -> Vec<WorkspaceTask> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, tasks_id)) = doc.get(&ROOT, ROOM_TASKS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut tasks = Vec::new();
    for key in doc.keys(&tasks_id) {
        if let Some((Value::Scalar(value), _)) = doc.get(&tasks_id, key.as_str()).ok().flatten()
            && let ScalarValue::Bytes(blob) = value.as_ref()
            && let Ok(task) = serde_json::from_slice::<WorkspaceTask>(blob)
        {
            tasks.push(task);
        }
    }
    tasks.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    tasks
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
    shortcuts.sort_by(|left, right| left.id.cmp(&right.id));
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
    claims.sort_by_key(tilo_automation::ScheduleClaim::key);
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
    receipts.sort_by_key(|receipt: &WorkspaceEffectReceipt| receipt.completed_at);
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
/// Device-local tiles ([`TileContent::is_device_local`]) are never
/// published.
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
        let has_owned = strip.window_groups.iter().any(|g| {
            g.tabs.iter().any(|t| !t.tile.is_device_local())
                && (mine(g.id)
                    || g.tabs
                        .iter()
                        .any(|t| !t.tile.is_device_local() && mine(t.id)))
        });
        if !owns_strip && !has_owned {
            continue;
        }

        let Ok(strip_entry) = winner_or_create(doc, &strip_all, &contents_id, skey.as_str()) else {
            continue;
        };
        if owns_strip {
            live_strips.insert(skey.clone());
            if empty == EmptyStrips::Retain {
                put_uint_if_changed(doc, &strip_entry, STRIP_RETAINED_EMPTY_KEY, 1);
            }
            put_str_if_changed(doc, &strip_entry, "name", &strip.name);
            put_uint_if_changed(doc, &strip_entry, "order", si as u64);
        }

        // A strip whose groups cannot be written is skipped; the sweep
        // below still runs.
        let strip_all = or_created(strip_all, &strip_entry);
        let _ = project_owned_groups_in(doc, &strip_all, strip, &mine);
    }
    sweep_stale(doc, &contents_all, &live_strips, &mine);
    Ok(())
}

/// [`project_owned_contents`]'s per-strip pass: get-or-create
/// `strip_entry`'s `groups` map, project every group this machine owns
/// (or owns a tab within), then sweep group keys this machine owns
/// that no longer appear among `strip`'s groups.
///
/// # Errors
///
/// The document's error when a map this projection needs cannot be
/// created.
pub fn project_owned_groups(
    doc: &mut AutoCommit,
    strip_entry: &automerge::ObjId,
    strip: &Strip,
    mine: &impl Fn(u64) -> bool,
) -> Result<(), automerge::AutomergeError> {
    project_owned_groups_in(doc, std::slice::from_ref(strip_entry), strip, mine)
}

/// [`project_owned_groups`] over every candidate entry of one strip (the
/// first is the one written to).
fn project_owned_groups_in(
    doc: &mut AutoCommit,
    strip_entries: &[automerge::ObjId],
    strip: &Strip,
    mine: &impl Fn(u64) -> bool,
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
        if group.tabs.iter().all(|tab| tab.tile.is_device_local()) {
            continue;
        }
        let owns_group = mine(group.id);
        let has_owned_tabs = group
            .tabs
            .iter()
            .any(|t| !t.tile.is_device_local() && mine(t.id));
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
            put_uint_if_changed(doc, &group_entry, "order", gi as u64);
            // Width, maximization and active-tab selection are local
            // presentation preferences. Old room documents may still
            // contain the first two fields, but no current peer writes
            // or reads them.
        }

        let group_copies = or_created(group_copies, &group_entry);
        let _ = project_owned_tabs_in(doc, &group_copies, group, mine);
    }
    sweep_stale(doc, &groups_all, &live_groups, mine);
    Ok(())
}

/// [`project_owned_groups`]'s per-group pass: get-or-create
/// `group_entry`'s `tabs` map, project every tab this machine owns,
/// then sweep tab keys this machine owns that no longer appear among
/// `group`'s tabs.
///
/// # Errors
///
/// The document's error when a map this projection needs cannot be
/// created.
pub fn project_owned_tabs(
    doc: &mut AutoCommit,
    group_entry: &automerge::ObjId,
    group: &WindowGroup,
    mine: &impl Fn(u64) -> bool,
) -> Result<(), automerge::AutomergeError> {
    project_owned_tabs_in(doc, std::slice::from_ref(group_entry), group, mine)
}

/// [`project_owned_tabs`] over every candidate entry of one group (the
/// first is the one written to).
fn project_owned_tabs_in(
    doc: &mut AutoCommit,
    group_entries: &[automerge::ObjId],
    group: &WindowGroup,
    mine: &impl Fn(u64) -> bool,
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
        if tab.tile.is_device_local() || !mine(tab.id) {
            continue;
        }
        let tkey = contents_key(tab.id);
        live_tabs.insert(tkey.clone());
        let tab_copies = child_candidates(doc, &tabs_all, tkey.as_str());
        let Ok(tab_entry) = winner_or_create(doc, &tab_copies, &tabs_id, tkey.as_str()) else {
            continue;
        };
        put_uint_if_changed(doc, &tab_entry, "order", ti as u64);
        put_str_if_changed(doc, &tab_entry, "title", &tab.title);
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
    sweep_stale(doc, &tabs_all, &live_tabs, mine);
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
    let text = |obj: &automerge::ObjId, key: &str| -> Option<String> {
        match doc.get(obj, key).ok().flatten() {
            Some((Value::Scalar(v), _)) => match v.as_ref() {
                ScalarValue::Str(t) => Some(t.to_string()),
                _ => None,
            },
            _ => None,
        }
    };

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
                    tile,
                });
            }
            tabs.sort_by_key(|t| (t.order, t.id));
            groups.push(ReplicatedGroup {
                id: gid,
                order: uint(group_entry, "order").unwrap_or(u64::MAX),
                tabs,
            });
        }
        groups.sort_by_key(|g| (g.order, g.id));
        strips.push(ReplicatedStrip {
            id: sid,
            order: uint(strip_entry, "order").unwrap_or(u64::MAX),
            name: text(strip_entry, "name"),
            groups,
        });
    }
    strips.sort_by_key(|s| (s.order, s.id));
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
                        format!("t|{}|{}|{:?}", tab.order, tab.title, tab.tile),
                    ));
                }
            }
        }
    }
    out.sort();
    out
}

/// Reconcile one strip against what the room document says about it.
pub fn merge_strip(
    mut local: Strip,
    rep: ReplicatedStrip,
    is_local: &dyn Fn(u64) -> bool,
    changed: &mut bool,
) -> Strip {
    // A name is only taken from the document for a strip we do not
    // own; ours is ours to write.
    if !is_local(local.id.raw())
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
                let order = if is_local(gid) { i as u64 } else { rep.order };
                out.push((order, gid, merge_group(group, rep, is_local, changed)));
            }
            None if is_local(gid) => out.push((i as u64, gid, group)),
            None => *changed = true,
        }
    }
    for (gid, rep) in by_group {
        out.push((rep.order, gid, hydrate_group(rep)));
        *changed = true;
    }
    out.sort_by_key(|(order, id, _)| (*order, *id));
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
    is_local: &dyn Fn(u64) -> bool,
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
        match by_tab.remove(&tid) {
            Some(rep) => {
                let order = if is_local(tid) { i as u64 } else { rep.order };
                let mut tab = tab;
                if !is_local(tid) {
                    if tab.title != rep.title {
                        tab.title.clone_from(&rep.title);
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
            None if is_local(tid) => out.push((i as u64, tid, tab)),
            None => *changed = true,
        }
    }
    for (tid, rep) in by_tab {
        out.push((rep.order, tid, hydrate_tab(rep)));
        *changed = true;
    }
    out.sort_by_key(|(order, id, _)| (*order, *id));
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
    }
}

/// Make a peer's tile mean something on this machine: a
/// [`TileContent::Foreign`] `wl_surface` id only resolves on the
/// compositor hosting that client, so the surface is dropped and the
/// app id survives as [`TileContent::PeerWindow`]. `current` is
/// preferred once a local instance is bound, or the peer's
/// re-projection would take the window away again every round.
pub fn localise_tile(
    tile: TileContent,
    current: Option<&TileContent>,
    tab_title: &str,
) -> TileContent {
    match tile {
        TileContent::Foreign { app_id, .. } => {
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
            }
        }
        other => other,
    }
}

/// Key the room document stores a room's record index under, mirroring
/// the record index's shape (id -> `kind`) in the document that
/// crosses the wire.
pub const ROOM_RECORDS_KEY: &str = "records";

/// Key of the withdrawal mark on a room record entry.
pub const ROOM_RECORD_REMOVED_KEY: &str = "removed";

/// Key of a room record entry's bringing-in: `"<nonce> <by> <signature>"`,
/// or just `"<nonce>"` from a machine with nothing to sign with.
pub const ROOM_RECORD_ADDED_KEY: &str = "added";

/// Key of a room record entry's withdrawal seal: `"<by> <signature>"`.
pub const ROOM_RECORD_WITHDRAWN_KEY: &str = "withdrawn";

/// What a machine did to a room record, for its host to seal: who acted,
/// signed over the act. The nonce names one bringing-in, so a withdrawal
/// seals the bringing-in it takes back and cannot be replayed on a later
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomRecordAct<'a> {
    Added { id: &'a str, nonce: &'a str },
    Withdrawn { id: &'a str, nonce: &'a str },
}

/// A seal a host makes for an act: `"<by> <signature>"`, or `None` when it
/// has no identity to sign with.
pub type RoomRecordSeal<'s> = &'s dyn Fn(RoomRecordAct<'_>) -> Option<String>;

/// A room record's bringing-in, as a room document holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomRecordAdded {
    pub nonce: String,
    /// `(by, signature)`, when the machine that brought it in sealed it.
    pub seal: Option<(String, String)>,
}

/// The actor that makes every room document's record map. No machine
/// writes as it; it makes only the change in [`room_records_genesis`].
const ROOM_RECORDS_GENESIS_ACTOR: [u8; 16] = *b"tilo-room-recs\0\0";

/// The change that makes a room document's record map — byte for byte the
/// same on every machine, so it hashes the same. Two members who each bring
/// the first record into a room apply this one change and hold one map;
/// had each made its own under the same key, one map would win and the
/// other's references would be hidden behind it.
fn room_records_genesis() -> Vec<automerge::Change> {
    use automerge::{ObjType, ROOT, transaction::Transactable};
    thread_local! {
        static GENESIS: Vec<automerge::Change> = {
            let mut genesis = AutoCommit::new()
                .with_actor(automerge::ActorId::from(ROOM_RECORDS_GENESIS_ACTOR.as_slice()));
            let made = genesis.put_object(&ROOT, ROOM_RECORDS_KEY, ObjType::Map);
            debug_assert!(made.is_ok(), "an empty document takes a root map");
            genesis.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
            genesis.get_changes(&[])
        };
    }
    GENESIS.with(Clone::clone)
}

/// The change that makes record `id`'s entry, of type `kind`, in a room
/// document's record map — like [`room_records_genesis`], byte for byte the
/// same on every machine: its actor is derived from the entry, it depends
/// only on the map's genesis and it carries no time. Two members bringing the
/// same record in at once make one entry rather than two maps under one key,
/// one hiding the other and whatever was sealed in it.
fn room_record_entry_genesis(id: &str, kind: &str) -> Vec<automerge::Change> {
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    let mut doc = AutoCommit::new();
    if tilo_automerge::ApplyIncoming::apply_incoming(&mut doc, room_records_genesis()).is_err() {
        return Vec::new();
    }
    let base = doc.get_heads();
    let mut actor = blake3::Hasher::new();
    actor.update(b"tilo.room-records.entry.v1\0");
    actor.update(id.as_bytes());
    actor.update(b"\0");
    actor.update(kind.as_bytes());
    let actor = actor.finalize();
    let mut doc = doc.with_actor(automerge::ActorId::from(&actor.as_bytes()[..16]));
    let Some((_, records)) = doc.get(&ROOT, ROOM_RECORDS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let Ok(entry) = doc.put_object(&records, id, ObjType::Map) else {
        return Vec::new();
    };
    if doc.put(&entry, "kind", kind).is_err() {
        return Vec::new();
    }
    doc.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
    doc.get_changes(&base)
}

fn made_by_records_genesis(records: &automerge::ObjId) -> bool {
    matches!(records, automerge::ObjId::Id(_, actor, _)
        if actor.to_bytes() == ROOM_RECORDS_GENESIS_ACTOR.as_slice())
}

/// The room document's record map, made by [`room_records_genesis`] when
/// the room has none yet.
fn room_records_map(doc: &mut AutoCommit) -> Result<automerge::ObjId, automerge::AutomergeError> {
    use automerge::{ROOT, ReadDoc};
    if let Some((_, id)) = doc.get(&ROOT, ROOM_RECORDS_KEY)? {
        return Ok(id);
    }
    tilo_automerge::ApplyIncoming::apply_incoming(doc, room_records_genesis())?;
    doc.get(&ROOT, ROOM_RECORDS_KEY)?.map(|(_, id)| id).ok_or(
        automerge::AutomergeError::InvalidObjId(ROOM_RECORDS_KEY.into()),
    )
}

/// Project a room's record index into that room's replication
/// document. A room holds a *reference* (id and type), never the
/// record itself — documents travel per-record over the broker.
///
/// What is written is what this machine *did* since `previous` (the index
/// it projected last): a record it brought in is written live, one it took
/// out is marked removed. A record neither brought in nor taken out is left
/// alone, so a peer's reference that has not reached our index yet is never
/// swept and a peer's withdrawal is never undone by our stale copy. With no
/// `previous` (the first projection of the session) nothing is marked
/// removed and no withdrawal is cleared.
///
/// Removal is a mark, not a delete, so it reaches a member who synced the
/// reference; and a withdrawal wins over a concurrent re-reference (see
/// [`room_record_removed`]).
///
/// # Errors
///
/// The document's error when a map this projection needs cannot be
/// created.
pub fn project_room_records(
    doc: &mut AutoCommit,
    held: &std::collections::BTreeMap<String, String>,
    previous: Option<&std::collections::BTreeMap<String, String>>,
    seal: RoomRecordSeal<'_>,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ReadDoc, transaction::Transactable};

    let withdrawn: Vec<&String> = previous
        .map(|previous| {
            previous
                .keys()
                .filter(|id| !held.contains_key(*id))
                .collect()
        })
        .unwrap_or_default();
    if held.is_empty() && withdrawn.is_empty() {
        return Ok(());
    }
    let records_id = room_records_map(doc)?;
    for (id, kind) in held {
        let (entry, brought_in) = match doc.get(&records_id, id.as_str()).ok().flatten() {
            Some((_, obj)) => (obj, false),
            None if made_by_records_genesis(&records_id) => {
                if tilo_automerge::ApplyIncoming::apply_incoming(
                    doc,
                    room_record_entry_genesis(id, kind),
                )
                .is_err()
                {
                    continue;
                }
                match doc.get(&records_id, id.as_str()).ok().flatten() {
                    Some((_, obj)) => (obj, true),
                    None => continue,
                }
            }
            None => match doc.put_object(&records_id, id.as_str(), ObjType::Map) {
                Ok(obj) => (obj, true),
                Err(_) => continue,
            },
        };
        // Compared before writing, like every other value here — a
        // re-projection of unchanged state must not manufacture one.
        let current = match doc.get(&entry, "kind").ok().flatten() {
            Some((automerge::Value::Scalar(v), _)) => match v.as_ref() {
                automerge::ScalarValue::Str(k) => Some(k.to_string()),
                _ => None,
            },
            _ => None,
        };
        if current.as_deref() != Some(kind.as_str()) {
            let _ = doc.put(&entry, "kind", kind.clone());
        }
        // Brought in again during this session after being taken out: the
        // later act clears the mark. A record merely still held does not.
        let re_referenced = previous.is_some_and(|previous| !previous.contains_key(id));
        let brought_back = re_referenced && entry_removed(doc, &entry);
        if brought_back {
            let _ = doc.put(&entry, ROOM_RECORD_REMOVED_KEY, false);
        }
        // Who brought it in, sealed: whom a withdrawal is checked against.
        if brought_in || brought_back {
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            let added = match seal(RoomRecordAct::Added { id, nonce: &nonce }) {
                Some(sealed) => format!("{nonce} {sealed}"),
                None => nonce,
            };
            let _ = doc.put(&entry, ROOM_RECORD_ADDED_KEY, added);
        }
    }
    for id in withdrawn {
        if let Some((_, entry)) = doc.get(&records_id, id.as_str()).ok().flatten()
            && !entry_removed(doc, &entry)
        {
            let _ = doc.put(&entry, ROOM_RECORD_REMOVED_KEY, true);
            // Sealed over the bringing-in it takes back.
            if let Some(nonce) = added_values(doc, &entry)
                .first()
                .map(|added| added.nonce.clone())
                && let Some(sealed) = seal(RoomRecordAct::Withdrawn { id, nonce: &nonce })
            {
                let _ = doc.put(&entry, ROOM_RECORD_WITHDRAWN_KEY, sealed);
            }
        }
    }
    Ok(())
}

fn strings_at(doc: &AutoCommit, entry: &automerge::ObjId, key: &str) -> Vec<String> {
    use automerge::{ScalarValue, Value};
    doc.get_all(entry, key)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(value, _)| match value {
            Value::Scalar(v) => match v.as_ref() {
                ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            Value::Object(_) => None,
        })
        .collect()
}

fn added_values(doc: &AutoCommit, entry: &automerge::ObjId) -> Vec<RoomRecordAdded> {
    strings_at(doc, entry, ROOM_RECORD_ADDED_KEY)
        .into_iter()
        .filter_map(|value| {
            let mut parts = value.split(' ');
            let nonce = parts.next().filter(|nonce| !nonce.is_empty())?.to_string();
            let seal = match (parts.next(), parts.next()) {
                (Some(by), Some(sig)) => Some((by.to_string(), sig.to_string())),
                _ => None,
            };
            Some(RoomRecordAdded { nonce, seal })
        })
        .collect()
}

fn record_entry(doc: &AutoCommit, id: &str) -> Option<automerge::ObjId> {
    use automerge::{ROOT, ReadDoc};
    let (_, records) = doc.get(&ROOT, ROOM_RECORDS_KEY).ok().flatten()?;
    doc.get(&records, id).ok().flatten().map(|(_, entry)| entry)
}

/// Every bringing-in of record `id` the room document holds — more than one
/// only when members brought it in concurrently.
pub fn room_record_added(doc: &AutoCommit, id: &str) -> Vec<RoomRecordAdded> {
    record_entry(doc, id).map_or_else(Vec::new, |entry| added_values(doc, &entry))
}

/// Every withdrawal seal on record `id`, as `(by, signature)`.
pub fn room_record_withdrawals(doc: &AutoCommit, id: &str) -> Vec<(String, String)> {
    record_entry(doc, id).map_or_else(Vec::new, |entry| {
        strings_at(doc, &entry, ROOM_RECORD_WITHDRAWN_KEY)
            .into_iter()
            .filter_map(|value| {
                let (by, sig) = value.split_once(' ')?;
                Some((by.to_string(), sig.to_string()))
            })
            .collect()
    })
}

/// Whether an entry is marked removed. Every concurrent value counts, so a
/// withdrawal is never lost to a re-reference made without seeing it.
fn entry_removed(doc: &AutoCommit, entry: &automerge::ObjId) -> bool {
    use automerge::{ReadDoc, ScalarValue, Value};
    doc.get_all(entry, ROOM_RECORD_REMOVED_KEY)
        .unwrap_or_default()
        .iter()
        .any(|(value, _)| matches!(value, Value::Scalar(v) if matches!(v.as_ref(), ScalarValue::Boolean(true))))
}

/// Whether the room document marks record `id` removed from the room.
pub fn room_record_removed(doc: &AutoCommit, id: &str) -> bool {
    use automerge::{ROOT, ReadDoc};
    doc.get(&ROOT, ROOM_RECORDS_KEY)
        .ok()
        .flatten()
        .and_then(|(_, records)| doc.get(&records, id).ok().flatten())
        .is_some_and(|(_, entry)| entry_removed(doc, &entry))
}

/// Read a room document's record index back as `(record id, kind)`
/// pairs, sorted by id so two peers read one document the same way.
/// Records marked removed are not in the room.
pub fn room_records_of(doc: &AutoCommit) -> Vec<(String, String)> {
    room_record_entries(doc)
        .into_iter()
        .filter(|(_, _, removed)| !removed)
        .map(|(id, kind, _)| (id, kind))
        .collect()
}

/// Ids the room document marks removed, sorted.
pub fn room_records_removed_of(doc: &AutoCommit) -> Vec<String> {
    room_record_entries(doc)
        .into_iter()
        .filter(|(_, _, removed)| *removed)
        .map(|(id, _, _)| id)
        .collect()
}

fn room_record_entries(doc: &AutoCommit) -> Vec<(String, String, bool)> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, records_id)) = doc.get(&ROOT, ROOM_RECORDS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in doc.keys(&records_id) {
        let Some((_, entry)) = doc.get(&records_id, key.as_str()).ok().flatten() else {
            continue;
        };
        let kind = match doc.get(&entry, "kind").ok().flatten() {
            Some((Value::Scalar(v), _)) => match v.as_ref() {
                ScalarValue::Str(k) => k.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let removed = entry_removed(doc, &entry);
        out.push((key, kind, removed));
    }
    out.sort();
    out
}

/// Who is folding a room document into their tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    /// A peer that publishes into the room under this salt: entries carrying
    /// it are its own, never overwritten or dropped by the document. Salt `0`
    /// has no id space, so an owner under it merges nothing.
    Owner(u16),
    /// A read-only viewer owns no topology namespace (not even salt `0`):
    /// the document is the whole truth about the room's windows.
    Viewer,
}

/// Fold the room document's `contents` into the cached workspace
/// addressing `room`, reporting whether anything changed. Entries
/// this machine owns are never overwritten or dropped — the document
/// is downstream of the cache, so reading one back could only revert
/// an unprojected local change; a peer's entries are taken from the
/// document and dropped when absent.
///
/// `active_tab` is never read from the document: it stays put unless
/// gone, and a group arriving from a peer opens on its
/// first tab. Vanishing strips take their `active_strip`/
/// `active_group` with them so resolvers fall back rather than point
/// at nothing.
pub fn merge_room_contents(
    desktop: &mut DesktopTree,
    room: RoomId,
    replicated: Vec<ReplicatedStrip>,
    reader: Reader,
) -> bool {
    let local_salt = match reader {
        Reader::Owner(0) => return false,
        Reader::Owner(salt) => Some(salt),
        Reader::Viewer => None,
    };
    let is_local = |id: u64| local_salt == Some(peer_salt_of(id));

    let Some(ws) = desktop.workspaces.iter_mut().find(|w| w.room == room) else {
        return false;
    };
    let mut changed = false;
    let prior_strip_order: Vec<StripId> = ws.strips.iter().map(|strip| strip.id).collect();
    let mut by_strip: std::collections::BTreeMap<u64, ReplicatedStrip> =
        replicated.into_iter().map(|s| (s.id, s)).collect();

    // `(order, id, strip)` so local-only strips keep a position too.
    let mut out: Vec<(u64, u64, Strip)> = Vec::new();
    // Old strip ids that don't make it into `out` below — collected
    // as the loop drops them rather than diffed afterward against a
    // snapshot, since `ws.strips` is about to be overwritten with the
    // merged result.
    let mut vanished: Vec<StripId> = Vec::new();
    for (i, strip) in std::mem::take(&mut ws.strips).into_iter().enumerate() {
        let sid = strip.id.raw();
        let published = by_strip.remove(&sid);
        if published.is_none() {
            if is_local(sid) {
                // Ours and simply not published; the document has
                // no say over it.
                out.push((i as u64, sid, strip));
            } else {
                // A foreign strip no longer being merged in — drop it.
                changed = true;
                vanished.push(StripId::from(sid));
            }
            continue;
        }
        let rep = published.unwrap_or_else(|| ReplicatedStrip {
            id: sid,
            order: i as u64,
            name: None,
            groups: Vec::new(),
        });
        // Ours keeps our cache's position — taking it from the
        // document would undo a local reorder until re-projected.
        let order = if is_local(sid) { i as u64 } else { rep.order };
        out.push((order, sid, merge_strip(strip, rep, &is_local, &mut changed)));
    }
    for (sid, rep) in by_strip {
        out.push((rep.order, sid, hydrate_strip(rep)));
        changed = true;
    }
    out.sort_by_key(|(order, id, _)| (*order, *id));
    if !prior_strip_order
        .iter()
        .copied()
        .eq(out.iter().map(|(_, _, strip)| strip.id))
    {
        changed = true;
    }
    ws.strips = out.into_iter().map(|(_, _, s)| s).collect();

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
    use automerge::{ROOT, ScalarValue, Value};
    match candidate.get(&ROOT, ROOM_ID_KEY).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => RoomId::parse(v.as_str()),
            _ => None,
        },
        _ => None,
    }
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
/// workspace if the tree has none (its local id derived from the room id),
/// take the room's name and owner, and fold its contents in as a
/// [`Reader::Viewer`]. Existing local workspace identity and local selection
/// remain untouched. Returns whether anything changed.
///
/// # Errors
///
/// A description when the document does not declare exactly `room`, or no
/// free local workspace id remains.
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
    if !tree.workspaces.iter().any(|w| w.room == room) {
        let bytes = room.as_uuid().into_bytes();
        let mut head = [0_u8; 8];
        head.copy_from_slice(bytes.get(..8).ok_or("room identity")?);
        let mut raw = u64::from_be_bytes(head).max(1);
        while tree.workspaces.iter().any(|w| w.id.raw() == raw) {
            raw = raw.checked_add(1).ok_or("workspace identities exhausted")?;
        }
        let mut workspace = Workspace::new(WorkspaceId(raw), &name);
        workspace.room = room;
        if tree.workspaces.is_empty() {
            tree.active = workspace.id;
        }
        tree.workspaces.push(workspace);
    }
    let owner = text(ROOM_OWNER_KEY)?;
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|w| w.room == room)
        .ok_or("workspace missing")?;
    let renamed = workspace.name != name;
    workspace.name = name;
    workspace.owner = owner;
    let shortcuts = workspace_shortcuts_of(doc);
    let reshortcut = workspace.shortcuts != shortcuts;
    workspace.shortcuts = shortcuts;
    let changed = merge_room_contents(tree, room, contents_of(doc), Reader::Viewer);
    Ok(changed || renamed || reshortcut)
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

/// The next counter a durable topology owner mints from, when the room
/// records one.
pub fn desktop_counter_of(doc: &AutoCommit) -> Option<u64> {
    doc.get(&automerge::ROOT, ROOM_DESKTOP_COUNTER_KEY)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;

    fn held(entry: (&str, &str)) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::from([(entry.0.to_string(), entry.1.to_string())])
    }

    /// Two room documents forked from one empty history each bring the
    /// `records` map in on first projection; the merge must show both indexes.
    #[test]
    fn concurrently_created_record_indexes_both_survive() {
        for _ in 0..8 {
            let mut base = AutoCommit::new();
            base.commit();
            let mut a = base.fork();
            let mut b = base.fork();
            project_room_records(&mut a, &held(("file:/a.md", "document")), None, &|_| None)
                .unwrap();
            project_room_records(&mut b, &held(("file:/b.md", "document")), None, &|_| None)
                .unwrap();
            a.merge_incoming(&mut b).unwrap();
            let ids: Vec<String> = room_records_of(&a).into_iter().map(|(id, _)| id).collect();
            assert_eq!(ids, ["file:/a.md", "file:/b.md"]);
        }
    }
}
