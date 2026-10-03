//! The device's whole-desktop document: every workspace this machine holds,
//! as one automerge document the host keeps for local history and recovery.
//!
//! Unlike a room document (see [`crate::projection`]), it describes every
//! workspace, so it is never handed to a peer. This module is the only
//! definition of its shape: [`seed`] writes it from a [`DesktopTree`],
//! [`rebuild`] reads it back, and every other function is a narrow edit that
//! mirrors one change the tree already made. Topology is decided by
//! [`crate::topology`]; the edits here only record the outcome. The host owns
//! the document, decides when to commit, and stamps history.
//!
//! Shape: `{ active: uint, workspaces: [ { id, name, room, owner?,
//! capabilities: {id → json}, permission_profiles_v1: {key → json},
//! tasks_v1: {id → json}, effect_receipts_v1: {invocation → json},
//! automations_v1: {shortcut id → json},
//! strips: [ { id, window_groups: [ { id, tabs: [ { id, title, tile_json } ] } ] } ] } ] }`.
//! Geometry and maximization are presentation state and never written.

use std::collections::HashSet;

use automerge::{
    AutoCommit, ObjId, ObjType, Prop, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use collab_types::PermissionProfile;
use collab_types::caps::CapOverride;

use crate::app::AppKind;
use crate::ids::{GroupId, RoomId, StripId, TabId, WorkspaceId};
use crate::layout::{Strip, Tab, TileContent, WindowGroup};
use crate::projection::{ROOM_AUTOMATIONS_KEY, ROOM_EFFECT_RECEIPTS_KEY, ROOM_SCHEDULE_CLAIMS_KEY};
use crate::task::{WorkspaceEffectReceipt, WorkspaceTask};
use crate::topology::{DropTarget, TabMove};
use crate::tree::DesktopTree;
use crate::width::WindowWidth;
use crate::workspace::{UserId, Workspace};

const WORKSPACES_KEY: &str = "workspaces";
const ACTIVE_KEY: &str = "active";
const CAPABILITIES_KEY: &str = "capabilities";
const PERMISSION_PROFILES_KEY: &str = "permission_profiles_v1";
const TASKS_KEY: &str = "tasks_v1";
const STRIPS_KEY: &str = "strips";
const GROUPS_KEY: &str = "window_groups";
const TABS_KEY: &str = "tabs";

/// The map key a permission profile is stored under: a digest of its
/// case-folded name, so two spellings of one name share a slot.
pub fn permission_profile_key(name: &str) -> String {
    blake3::hash(name.trim().to_lowercase().as_bytes())
        .to_hex()
        .to_string()
}

fn obj(doc: &AutoCommit, parent: &ObjId, prop: impl Into<Prop>) -> Option<ObjId> {
    doc.get(parent, prop).ok().flatten().map(|(_, id)| id)
}

fn uint(doc: &AutoCommit, parent: &ObjId, prop: impl Into<Prop>) -> Option<u64> {
    match doc.get(parent, prop).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Uint(v) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}

fn text(doc: &AutoCommit, parent: &ObjId, prop: impl Into<Prop>) -> Option<String> {
    match doc.get(parent, prop).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn bytes(doc: &AutoCommit, parent: &ObjId, prop: impl Into<Prop>) -> Option<Vec<u8>> {
    match doc.get(parent, prop).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn map_or_create(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match obj(doc, parent, key) {
        Some(id) => Some(id),
        None => doc.put_object(parent, key, ObjType::Map).ok(),
    }
}

/// Every workspace entry with its id.
fn workspace_entries(doc: &AutoCommit) -> Vec<(u64, ObjId)> {
    let Some(list) = obj(doc, &ROOT, WORKSPACES_KEY) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|i| {
            let entry = obj(doc, &list, i)?;
            Some((uint(doc, &entry, "id")?, entry))
        })
        .collect()
}

/// Every strip entry `(strip id, strip entry, window_groups list)`.
fn strip_entries(doc: &AutoCommit) -> Vec<(u64, ObjId, Option<ObjId>)> {
    let mut out = Vec::new();
    for (_, ws) in workspace_entries(doc) {
        let Some(strips) = obj(doc, &ws, STRIPS_KEY) else {
            continue;
        };
        for i in 0..doc.length(&strips) {
            let Some(entry) = obj(doc, &strips, i) else {
                continue;
            };
            let Some(id) = uint(doc, &entry, "id") else {
                continue;
            };
            let groups = obj(doc, &entry, GROUPS_KEY);
            out.push((id, entry, groups));
        }
    }
    out
}

/// The `window_groups` list of `strip_id`, if the document holds it.
fn strip_groups(doc: &AutoCommit, strip_id: StripId) -> Option<ObjId> {
    strip_entries(doc)
        .into_iter()
        .find(|(id, _, _)| *id == strip_id.raw())
        .and_then(|(_, _, groups)| groups)
}

/// Position of the group `group_id` in a `window_groups` list.
fn group_index(doc: &AutoCommit, groups: &ObjId, group_id: GroupId) -> Option<usize> {
    (0..doc.length(groups)).find(|&i| {
        obj(doc, groups, i).is_some_and(|entry| uint(doc, &entry, "id") == Some(group_id))
    })
}

/// The tab list holding `tab_id` somewhere in `strip_id`, and its index.
fn tab_in_strip(doc: &AutoCommit, strip_id: StripId, tab_id: TabId) -> Option<(ObjId, usize)> {
    let groups = strip_groups(doc, strip_id)?;
    (0..doc.length(&groups)).find_map(|g| {
        let tabs = obj(doc, &obj(doc, &groups, g)?, TABS_KEY)?;
        let idx = (0..doc.length(&tabs))
            .find(|&i| obj(doc, &tabs, i).is_some_and(|t| uint(doc, &t, "id") == Some(tab_id)))?;
        Some((tabs, idx))
    })
}

/// The group list holding `group_id` in any strip, and its index.
fn group_anywhere(doc: &AutoCommit, group_id: GroupId) -> Option<(ObjId, usize)> {
    strip_entries(doc).into_iter().find_map(|(_, _, groups)| {
        let groups = groups?;
        let idx = group_index(doc, &groups, group_id)?;
        Some((groups, idx))
    })
}

fn put_tab(doc: &mut AutoCommit, tabs: &ObjId, index: usize, tab: &Tab) -> Option<()> {
    let entry = doc.insert_object(tabs, index, ObjType::Map).ok()?;
    doc.put(&entry, "id", ScalarValue::Uint(tab.id)).ok()?;
    doc.put(&entry, "title", tab.title.clone()).ok()?;
    let blob = serde_json::to_vec(&tab.tile).ok()?;
    doc.put(&entry, "tile_json", ScalarValue::Bytes(blob))
        .ok()?;
    Some(())
}

/// Serialize one group into a `window_groups` list. Every topology writer
/// goes through here, so seeding, reordering and splitting cannot drift in
/// the fields they persist.
fn insert_group(doc: &mut AutoCommit, groups: &ObjId, index: usize, group: &WindowGroup) {
    let Ok(entry) = doc.insert_object(groups, index, ObjType::Map) else {
        return;
    };
    let _ = doc.put(&entry, "id", ScalarValue::Uint(group.id));
    let Ok(tabs) = doc.put_object(&entry, TABS_KEY, ObjType::List) else {
        return;
    };
    for (i, tab) in group.tabs.iter().enumerate() {
        let _ = put_tab(doc, &tabs, i, tab);
    }
}

/// The child `key` of `parent` as an object of type `kind`: the existing one
/// when it already has that type, otherwise a fresh one in its place.
fn child_object(doc: &mut AutoCommit, parent: &ObjId, key: &str, kind: ObjType) -> Option<ObjId> {
    match obj(doc, parent, key) {
        Some(id) if doc.object_type(&id).ok() == Some(kind) => Some(id),
        _ => doc.put_object(parent, key, kind).ok(),
    }
}

/// Writing an unchanged value is still an operation, and every operation is
/// a history row; these write only a value that differs.
fn put_text_if_changed(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: &str) {
    if text(doc, parent, key).as_deref() != Some(value) {
        let _ = doc.put(parent, key, value.to_string());
    }
}

/// Compared as JSON values rather than bytes: a map inside `value`
/// serialises in a different order once it has been read back, and that is
/// not a change. `false` when `value` does not serialise.
fn put_json_if_changed<T: serde::Serialize>(
    doc: &mut AutoCommit,
    parent: &ObjId,
    key: &str,
    value: &T,
) -> bool {
    let Ok(wanted) = serde_json::to_value(value) else {
        return false;
    };
    let current = bytes(doc, parent, key)
        .and_then(|blob| serde_json::from_slice::<serde_json::Value>(&blob).ok());
    if current.as_ref() != Some(&wanted)
        && let Ok(blob) = serde_json::to_vec(value)
    {
        let _ = doc.put(parent, key, ScalarValue::Bytes(blob));
    }
    true
}

fn delete_if_present(doc: &mut AutoCommit, parent: &ObjId, key: &str) {
    if doc.get(parent, key).ok().flatten().is_some() {
        let _ = doc.delete(parent, key);
    }
}

/// Make the map `key` hold exactly `items`, as JSON blobs: changed blobs are
/// rewritten and keys no longer present are deleted.
fn sync_blob_map<T: serde::Serialize>(
    doc: &mut AutoCommit,
    parent: &ObjId,
    key: &str,
    items: impl IntoIterator<Item = (String, T)>,
) {
    let Some(map) = child_object(doc, parent, key, ObjType::Map) else {
        return;
    };
    let mut wanted = HashSet::new();
    for (item_key, item) in items {
        if put_json_if_changed(doc, &map, &item_key, &item) {
            wanted.insert(item_key);
        }
    }
    let stale: Vec<String> = doc.keys(&map).filter(|k| !wanted.contains(k)).collect();
    for item_key in stale {
        let _ = doc.delete(&map, item_key.as_str());
    }
}

fn entry_id(doc: &AutoCommit, list: &ObjId, index: usize) -> Option<u64> {
    obj(doc, list, index).and_then(|entry| uint(doc, &entry, "id"))
}

/// Make the list of id-keyed map entries `list` match `items`, in order.
/// An entry whose id is still wanted is updated in place by `write`; a
/// missing one is inserted; a stale or duplicate one is deleted. Only a
/// reorder rewrites an entry whole, because an automerge list has no move.
fn sync_list<T>(
    doc: &mut AutoCommit,
    list: &ObjId,
    items: &[T],
    id_of: impl Fn(&T) -> u64,
    mut write: impl FnMut(&mut AutoCommit, &ObjId, &T),
) {
    let wanted: HashSet<u64> = items.iter().map(&id_of).collect();
    let mut seen = HashSet::new();
    let mut doomed = Vec::new();
    for index in 0..doc.length(list) {
        match entry_id(doc, list, index) {
            Some(id) if wanted.contains(&id) && seen.insert(id) => {}
            _ => doomed.push(index),
        }
    }
    for index in doomed.into_iter().rev() {
        let _ = doc.delete(list, index);
    }
    for (index, item) in items.iter().enumerate() {
        let id = id_of(item);
        if entry_id(doc, list, index) == Some(id) {
            if let Some(entry) = obj(doc, list, index) {
                write(doc, &entry, item);
            }
            continue;
        }
        let later = (index.saturating_add(1)..doc.length(list))
            .find(|&j| entry_id(doc, list, j) == Some(id));
        if let Some(later) = later {
            let _ = doc.delete(list, later);
        }
        let at = index.min(doc.length(list));
        let Ok(entry) = doc.insert_object(list, at, ObjType::Map) else {
            continue;
        };
        let _ = doc.put(&entry, "id", ScalarValue::Uint(id));
        write(doc, &entry, item);
    }
    for index in (items.len()..doc.length(list)).rev() {
        let _ = doc.delete(list, index);
    }
}

fn write_tab(doc: &mut AutoCommit, entry: &ObjId, tab: &Tab) {
    put_text_if_changed(doc, entry, "title", &tab.title);
    put_json_if_changed(doc, entry, "tile_json", &tab.tile);
}

fn write_group(doc: &mut AutoCommit, entry: &ObjId, group: &WindowGroup) {
    if let Some(tabs) = child_object(doc, entry, TABS_KEY, ObjType::List) {
        sync_list(doc, &tabs, &group.tabs, |tab| tab.id, write_tab);
    }
}

fn write_strip(doc: &mut AutoCommit, entry: &ObjId, strip: &Strip) {
    if let Some(groups) = child_object(doc, entry, GROUPS_KEY, ObjType::List) {
        sync_list(
            doc,
            &groups,
            &strip.window_groups,
            |group| group.id,
            write_group,
        );
    }
}

fn write_workspace(doc: &mut AutoCommit, entry: &ObjId, ws: &Workspace) {
    put_text_if_changed(doc, entry, "name", &ws.name);
    // Room identity travels with the workspace: a reader hydrating from
    // this document must land in this room, not mint its own.
    put_text_if_changed(doc, entry, "room", &ws.room.to_string());
    match &ws.owner {
        Some(owner) => put_text_if_changed(doc, entry, "owner", owner),
        None => delete_if_present(doc, entry, "owner"),
    }
    sync_blob_map(
        doc,
        entry,
        CAPABILITIES_KEY,
        ws.capabilities.iter().map(|cap| (cap.id.clone(), cap)),
    );
    sync_blob_map(
        doc,
        entry,
        PERMISSION_PROFILES_KEY,
        ws.permission_profiles
            .iter()
            .map(|profile| (permission_profile_key(&profile.name), profile)),
    );
    sync_blob_map(
        doc,
        entry,
        TASKS_KEY,
        ws.tasks.iter().map(|task| (task.id.clone(), task)),
    );
    sync_blob_map(
        doc,
        entry,
        ROOM_EFFECT_RECEIPTS_KEY,
        ws.effect_receipts
            .iter()
            .map(|receipt| (receipt.invocation.0.to_string(), receipt)),
    );
    sync_blob_map(
        doc,
        entry,
        ROOM_AUTOMATIONS_KEY,
        ws.shortcuts
            .iter()
            .map(|shortcut| (shortcut.id.clone(), shortcut)),
    );
    sync_blob_map(
        doc,
        entry,
        ROOM_SCHEDULE_CLAIMS_KEY,
        ws.schedule_claims.iter().map(|claim| (claim.key(), claim)),
    );
    if let Some(strips) = child_object(doc, entry, STRIPS_KEY, ObjType::List) {
        sync_list(
            doc,
            &strips,
            &ws.strips,
            |strip| strip.id.raw(),
            write_strip,
        );
    }
}

/// Make `doc` describe exactly `desktop`, writing only the difference.
/// Workspaces, strips, groups and tabs are matched by id, so an unchanged
/// entry keeps its object, its history and any concurrent edit made to it,
/// and a rename is one write to `name`. The tree is the source of truth:
/// an entry the tree no longer holds is deleted. Best-effort: a write that
/// fails skips the entries nested under it rather than aborting.
pub fn seed(doc: &mut AutoCommit, desktop: &DesktopTree) {
    if uint(doc, &ROOT, ACTIVE_KEY) != Some(desktop.active.raw()) {
        let _ = doc.put(&ROOT, ACTIVE_KEY, ScalarValue::Uint(desktop.active.raw()));
    }
    let Some(list) = child_object(doc, &ROOT, WORKSPACES_KEY, ObjType::List) else {
        return;
    };
    sync_list(
        doc,
        &list,
        &desktop.workspaces,
        |ws| ws.id.raw(),
        write_workspace,
    );
}

fn read_blob_map<T: serde::de::DeserializeOwned>(
    doc: &AutoCommit,
    parent: &ObjId,
    key: &str,
) -> Option<Vec<T>> {
    let map = obj(doc, parent, key)?;
    Some(
        doc.keys(&map)
            .filter_map(|k| serde_json::from_slice(&bytes(doc, &map, k.as_str())?).ok())
            .collect(),
    )
}

/// A workspace's owner and capabilities as the document records them.
/// Capabilities absent from the document are absent from the answer — what
/// makes a revocation take effect.
fn read_capabilities(
    doc: &AutoCommit,
    entry: &ObjId,
) -> Option<(Option<UserId>, Vec<CapOverride>)> {
    let owner = text(doc, entry, "owner");
    let caps = read_blob_map(doc, entry, CAPABILITIES_KEY)?;
    Some((owner, caps))
}

fn read_permission_profiles(doc: &AutoCommit, entry: &ObjId) -> Option<Vec<PermissionProfile>> {
    let mut profiles: Vec<PermissionProfile> = read_blob_map(doc, entry, PERMISSION_PROFILES_KEY)?;
    profiles.retain(|profile| profile.validate().is_ok());
    profiles.sort_by_key(|profile| profile.name.to_lowercase());
    Some(profiles)
}

fn read_tasks(doc: &AutoCommit, entry: &ObjId) -> Vec<WorkspaceTask> {
    read_blob_map(doc, entry, TASKS_KEY).unwrap_or_default()
}

fn read_shortcuts(doc: &AutoCommit, entry: &ObjId) -> Vec<tilo_automation::Shortcut> {
    let mut shortcuts: Vec<tilo_automation::Shortcut> =
        read_blob_map(doc, entry, ROOM_AUTOMATIONS_KEY).unwrap_or_default();
    shortcuts.sort_by(|left, right| left.id.cmp(&right.id));
    shortcuts
}

fn read_schedule_claims(doc: &AutoCommit, entry: &ObjId) -> Vec<tilo_automation::ScheduleClaim> {
    let mut claims: Vec<tilo_automation::ScheduleClaim> =
        read_blob_map(doc, entry, ROOM_SCHEDULE_CLAIMS_KEY).unwrap_or_default();
    claims.sort_by_key(tilo_automation::ScheduleClaim::key);
    claims
}

fn read_effect_receipts(doc: &AutoCommit, entry: &ObjId) -> Vec<WorkspaceEffectReceipt> {
    let mut receipts: Vec<WorkspaceEffectReceipt> =
        read_blob_map(doc, entry, ROOM_EFFECT_RECEIPTS_KEY).unwrap_or_default();
    receipts.sort_by_key(|receipt| receipt.completed_at);
    receipts
}

/// A tile from a `tile_json` blob, or a bare stub carrying just the title
/// when the blob is absent or does not decode.
fn tile_from_blob(blob: &[u8], title: &str) -> TileContent {
    serde_json::from_slice::<TileContent>(blob).unwrap_or_else(|_| TileContent::Stub {
        kind: AppKind::Other(String::new()),
        label: title.to_string(),
    })
}

/// Refresh `desktop` from `doc`. The active workspace, names, rooms,
/// ownership, capabilities, profiles, tasks, receipts and shortcuts come from the
/// document; `active_strip`, `active_group` and presence stay as the tree
/// had them. Each workspace keeps its existing strips by id, then the
/// document's topology and tabs are folded in: width and maximization stay
/// on the matching local group, and a group seen for the first time gets
/// local defaults.
pub fn rebuild(doc: &AutoCommit, desktop: &mut DesktopTree) {
    if let Some(raw) = uint(doc, &ROOT, ACTIVE_KEY) {
        desktop.active = WorkspaceId::from(raw);
    }
    if obj(doc, &ROOT, WORKSPACES_KEY).is_none() {
        return;
    }
    let entries = workspace_entries(doc);
    let mut rebuilt: Vec<Workspace> = Vec::with_capacity(entries.len());
    for (id_raw, entry) in &entries {
        let name = text(doc, entry, "name").unwrap_or_default();
        let ws_id = WorkspaceId::from(*id_raw);
        let mut ws = match desktop.workspaces.iter().position(|w| w.id == ws_id) {
            Some(existing) => {
                let mut existing = desktop.workspaces.remove(existing);
                existing.name = name;
                existing
            }
            None => Workspace::new(ws_id, name),
        };
        // Absent or unparseable keeps the tree's.
        if let Some(room) = text(doc, entry, "room")
            .and_then(|v| RoomId::parse(v.as_str()))
            .filter(|r| !r.is_nil())
        {
            ws.room = room;
        }
        if let Some((owner, caps)) = read_capabilities(doc, entry) {
            if owner.is_some() {
                ws.owner = owner;
            }
            ws.capabilities = caps;
        }
        if let Some(profiles) = read_permission_profiles(doc, entry) {
            ws.permission_profiles = profiles;
        }
        ws.tasks = read_tasks(doc, entry);
        ws.effect_receipts = read_effect_receipts(doc, entry);
        ws.shortcuts = read_shortcuts(doc, entry);
        ws.schedule_claims = read_schedule_claims(doc, entry);
        rebuilt.push(ws);
    }
    desktop.workspaces = rebuilt;

    for (id_raw, entry) in entries {
        let ws_id = WorkspaceId::from(id_raw);
        let Some(strips) = obj(doc, &entry, STRIPS_KEY) else {
            continue;
        };
        for strip_pos in 0..doc.length(&strips) {
            let Some(strip_entry) = obj(doc, &strips, strip_pos) else {
                continue;
            };
            let Some(strip_id) = uint(doc, &strip_entry, "id").map(StripId::from) else {
                continue;
            };
            let Some(groups) = obj(doc, &strip_entry, GROUPS_KEY) else {
                continue;
            };
            for group_pos in 0..doc.length(&groups) {
                let Some(group_entry) = obj(doc, &groups, group_pos) else {
                    continue;
                };
                let Some(group_id) = uint(doc, &group_entry, "id") else {
                    continue;
                };
                let Some(ws) = desktop.workspaces.iter_mut().find(|w| w.id == ws_id) else {
                    continue;
                };
                if !ws.strips.iter().any(|s| s.id == strip_id) {
                    ws.strips.push(Strip::new(strip_id, "Main"));
                }
                let strip = ws.strips.iter_mut().find(|s| s.id == strip_id);
                let Some(strip) = strip else {
                    continue;
                };
                let doc_tabs = group_tabs(doc, &group_entry);
                fold_group(strip, group_id, &doc_tabs);
            }
        }
    }
}

/// The tabs under a group entry, as `(id, title, tile_json)`.
fn group_tabs(doc: &AutoCommit, group_entry: &ObjId) -> Vec<(u64, String, Vec<u8>)> {
    let Some(tabs) = obj(doc, group_entry, TABS_KEY) else {
        return Vec::new();
    };
    (0..doc.length(&tabs))
        .filter_map(|i| {
            let entry = obj(doc, &tabs, i)?;
            let id = uint(doc, &entry, "id")?;
            let title = text(doc, &entry, "title").unwrap_or_default();
            let blob = bytes(doc, &entry, "tile_json").unwrap_or_default();
            Some((id, title, blob))
        })
        .collect()
}

/// Fold a document group's tabs into `strip`'s group `group_id`, creating
/// it with local presentation defaults when the strip does not have it.
fn fold_group(strip: &mut Strip, group_id: GroupId, doc_tabs: &[(u64, String, Vec<u8>)]) {
    let hydrate = |(id, title, blob): &(u64, String, Vec<u8>)| Tab {
        id: *id,
        title: title.clone(),
        tile: tile_from_blob(blob, title),
    };
    let Some(group) = strip.window_groups.iter_mut().find(|g| g.id == group_id) else {
        let tabs: Vec<Tab> = doc_tabs.iter().map(hydrate).collect();
        let active_tab = tabs.first().map_or(0, |t| t.id);
        strip.window_groups.push(WindowGroup {
            id: group_id,
            tabs,
            active_tab,
            width: WindowWidth::Normal,
            maximized: false,
        });
        return;
    };
    if doc_tabs.is_empty() {
        return;
    }
    let tabs: Vec<Tab> = doc_tabs
        .iter()
        .map(|entry| {
            group
                .tabs
                .iter()
                .find(|t| t.id == entry.0)
                .cloned()
                .unwrap_or_else(|| hydrate(entry))
        })
        .collect();
    if group.tabs.is_empty()
        || group
            .tabs
            .iter()
            .all(|t| !doc_tabs.iter().any(|(id, _, _)| *id == t.id))
    {
        group.active_tab = tabs.first().map_or(0, |t| t.id);
    }
    group.tabs = tabs;
}

fn workspace_entry(doc: &AutoCommit, workspace_id: WorkspaceId) -> Option<ObjId> {
    workspace_entries(doc)
        .into_iter()
        .find(|(id, _)| *id == workspace_id.raw())
        .map(|(_, entry)| entry)
}

/// Append a new workspace entry — id, name and room only; its strips reach
/// the document through later edits or the next [`seed`]. The room goes in
/// with the workspace, so a reader learns the identity in the same change
/// that tells it the workspace exists. Creates the list for an unseeded
/// document.
pub fn append_workspace(
    doc: &mut AutoCommit,
    workspace_id: WorkspaceId,
    name: &str,
    room: RoomId,
) -> bool {
    let Some(list) = obj(doc, &ROOT, WORKSPACES_KEY)
        .or_else(|| doc.put_object(&ROOT, WORKSPACES_KEY, ObjType::List).ok())
    else {
        return false;
    };
    let index = doc.length(&list);
    let Ok(entry) = doc.insert_object(&list, index, ObjType::Map) else {
        return false;
    };
    let _ = doc.put(&entry, "id", ScalarValue::Uint(workspace_id.raw()));
    let _ = doc.put(&entry, "name", name.to_string());
    let _ = doc.put(&entry, "room", room.to_string());
    true
}

/// Record which workspace is showing.
pub fn set_active(doc: &mut AutoCommit, workspace_id: WorkspaceId) {
    let _ = doc.put(&ROOT, ACTIVE_KEY, ScalarValue::Uint(workspace_id.raw()));
}

/// Record a workspace's room. `false` when the document does not hold the
/// workspace.
pub fn set_room(doc: &mut AutoCommit, workspace_id: WorkspaceId, room: RoomId) -> bool {
    workspace_entry(doc, workspace_id)
        .is_some_and(|entry| doc.put(&entry, "room", room.to_string()).is_ok())
}

/// Record a workspace's owner.
pub fn set_owner(doc: &mut AutoCommit, workspace_id: WorkspaceId, owner: &str) -> bool {
    workspace_entry(doc, workspace_id).is_some_and(|entry| doc.put(&entry, "owner", owner).is_ok())
}

fn upsert_blob(
    doc: &mut AutoCommit,
    workspace_id: WorkspaceId,
    map_key: &str,
    key: &str,
    value: &impl serde::Serialize,
) -> bool {
    let Some(entry) = workspace_entry(doc, workspace_id) else {
        return false;
    };
    let Some(map) = map_or_create(doc, &entry, map_key) else {
        return false;
    };
    let Ok(blob) = serde_json::to_vec(value) else {
        return false;
    };
    doc.put(&map, key, ScalarValue::Bytes(blob)).is_ok()
}

/// Record a capability. Capabilities are a map keyed by id rather than a
/// list: deleting a map key merges concurrently, while deleting list index
/// 3 on two peers removes two different capabilities.
pub fn insert_capability(
    doc: &mut AutoCommit,
    workspace_id: WorkspaceId,
    cap: &CapOverride,
) -> bool {
    upsert_blob(doc, workspace_id, CAPABILITIES_KEY, &cap.id, cap)
}

/// Delete a capability, so a revocation is not only local.
pub fn revoke_capability(doc: &mut AutoCommit, workspace_id: WorkspaceId, cap_id: &str) -> bool {
    let Some(entry) = workspace_entry(doc, workspace_id) else {
        return false;
    };
    let Some(caps) = obj(doc, &entry, CAPABILITIES_KEY) else {
        return false;
    };
    doc.delete(&caps, cap_id).is_ok()
}

/// Record a reusable permission profile under its name's key.
pub fn upsert_permission_profile(
    doc: &mut AutoCommit,
    workspace_id: WorkspaceId,
    profile: &PermissionProfile,
) -> bool {
    let key = permission_profile_key(&profile.name);
    upsert_blob(doc, workspace_id, PERMISSION_PROFILES_KEY, &key, profile)
}

/// Record a task under its id.
pub fn upsert_task(doc: &mut AutoCommit, workspace_id: WorkspaceId, task: &WorkspaceTask) -> bool {
    upsert_blob(doc, workspace_id, TASKS_KEY, &task.id, task)
}

/// Record a shortcut under its id, replacing any earlier version of it.
pub fn upsert_shortcut(
    doc: &mut AutoCommit,
    workspace_id: WorkspaceId,
    shortcut: &tilo_automation::Shortcut,
) -> bool {
    upsert_blob(
        doc,
        workspace_id,
        ROOM_AUTOMATIONS_KEY,
        &shortcut.id,
        shortcut,
    )
}

/// Delete a shortcut, so the deletion is not only local.
pub fn remove_shortcut(doc: &mut AutoCommit, workspace_id: WorkspaceId, shortcut_id: &str) -> bool {
    let Some(entry) = workspace_entry(doc, workspace_id) else {
        return false;
    };
    let Some(shortcuts) = obj(doc, &entry, ROOM_AUTOMATIONS_KEY) else {
        return false;
    };
    doc.get(&shortcuts, shortcut_id).ok().flatten().is_some()
        && doc.delete(&shortcuts, shortcut_id).is_ok()
}

/// Record a schedule claim under its schedule, replacing the earlier one.
pub fn upsert_schedule_claim(
    doc: &mut AutoCommit,
    workspace_id: WorkspaceId,
    claim: &tilo_automation::ScheduleClaim,
) -> bool {
    upsert_blob(
        doc,
        workspace_id,
        ROOM_SCHEDULE_CLAIMS_KEY,
        &claim.key(),
        claim,
    )
}

/// Record an effect receipt under its invocation.
pub fn upsert_effect_receipt(
    doc: &mut AutoCommit,
    workspace_id: WorkspaceId,
    receipt: &WorkspaceEffectReceipt,
) -> bool {
    let key = receipt.invocation.0.to_string();
    upsert_blob(doc, workspace_id, ROOM_EFFECT_RECEIPTS_KEY, &key, receipt)
}

/// Delete one tab, or its whole group when the tab was the group's last.
/// The narrow mirror of a user-driven close, so a later merge cannot
/// resurrect the tab.
pub fn remove_tab(
    doc: &mut AutoCommit,
    strip_id: StripId,
    group_id: GroupId,
    tab_id: TabId,
    remove_group: bool,
) -> bool {
    if remove_group {
        return group_anywhere(doc, group_id)
            .is_some_and(|(groups, idx)| doc.delete(&groups, idx).is_ok());
    }
    tab_in_strip(doc, strip_id, tab_id).is_some_and(|(tabs, idx)| doc.delete(&tabs, idx).is_ok())
}

/// Record a tab move the tree made (see
/// [`DesktopTree::move_tab_across_strips`] and
/// [`DesktopTree::split_tab_into_new_group`]), at the exact index or seam
/// the tree used. `src_strip` and `dst_strip` may be the same strip.
/// All-or-nothing: `false` leaves `doc` as it was.
pub fn apply_tab_move(
    doc: &mut AutoCommit,
    src_strip: StripId,
    dst_strip: StripId,
    moved: &TabMove,
) -> bool {
    let before = doc.clone();
    if try_apply_tab_move(doc, src_strip, dst_strip, moved).is_some() {
        true
    } else {
        *doc = before;
        false
    }
}

fn try_apply_tab_move(
    doc: &mut AutoCommit,
    src_strip: StripId,
    dst_strip: StripId,
    moved: &TabMove,
) -> Option<()> {
    let tab = moved.new_group.tabs.first()?;
    let (src_tabs, src_tab_idx) = tab_in_strip(doc, src_strip, tab.id)?;
    let src_groups = strip_groups(doc, src_strip)?;
    let dst_groups = strip_groups(doc, dst_strip)?;
    let src_group_idx = group_index(doc, &src_groups, moved.source_group)?;
    let dst_existing_tabs = match moved.target {
        DropTarget::ExistingGroup { group_id, .. } => {
            let idx = group_index(doc, &dst_groups, group_id)?;
            Some(obj(doc, &obj(doc, &dst_groups, idx)?, TABS_KEY)?)
        }
        DropTarget::GroupGap { .. } => None,
    };
    if moved.source_group_removed {
        doc.delete(&src_groups, src_group_idx).ok()?;
    } else {
        doc.delete(&src_tabs, src_tab_idx).ok()?;
    }
    match moved.target {
        DropTarget::GroupGap { gap_index } => {
            let index = gap_index.min(doc.length(&dst_groups));
            let group_entry = doc.insert_object(&dst_groups, index, ObjType::Map).ok()?;
            doc.put(&group_entry, "id", ScalarValue::Uint(moved.new_group.id))
                .ok()?;
            let tabs = doc.put_object(&group_entry, TABS_KEY, ObjType::List).ok()?;
            put_tab(doc, &tabs, 0, tab)
        }
        DropTarget::ExistingGroup { tab_index, .. } => {
            let tabs = dst_existing_tabs?;
            let index = tab_index.min(doc.length(&tabs));
            put_tab(doc, &tabs, index, tab)
        }
    }
}

/// Record a group reorder: the group moves to `new_index` as a unit,
/// rewritten from the tree's copy so it cannot drift.
pub fn reorder_group(
    doc: &mut AutoCommit,
    strip_id: StripId,
    group: &WindowGroup,
    new_index: usize,
) -> bool {
    let Some(groups) = strip_groups(doc, strip_id) else {
        return false;
    };
    let Some(current) = group_index(doc, &groups, group.id) else {
        return false;
    };
    if doc.delete(&groups, current).is_err() {
        return false;
    }
    let index = new_index.min(doc.length(&groups));
    insert_group(doc, &groups, index, group);
    true
}

/// Record a group moved whole into `strip_id` at `index`, out of whichever
/// strip held it, rewritten from the tree's copy so it cannot drift.
/// All-or-nothing: `false` leaves `doc` as it was.
pub fn move_group(
    doc: &mut AutoCommit,
    strip_id: StripId,
    group: &WindowGroup,
    index: usize,
) -> bool {
    let before = doc.clone();
    let moved = (|| {
        let (_, strip_entry, _) = strip_entries(doc)
            .into_iter()
            .find(|(id, _, _)| *id == strip_id.raw())?;
        let (source, at) = group_anywhere(doc, group.id)?;
        doc.delete(&source, at).ok()?;
        let groups = child_object(doc, &strip_entry, GROUPS_KEY, ObjType::List)?;
        let index = index.min(doc.length(&groups));
        insert_group(doc, &groups, index, group);
        Some(())
    })();
    if moved.is_none() {
        *doc = before;
    }
    moved.is_some()
}

/// Replace one strip's group list with `groups`, leaving the rest of the
/// document — and concurrent changes to it — alone.
pub fn rewrite_strip_groups(
    doc: &mut AutoCommit,
    strip_id: StripId,
    groups: &[WindowGroup],
) -> bool {
    let Some((_, strip_entry, _)) = strip_entries(doc)
        .into_iter()
        .find(|(id, _, _)| *id == strip_id.raw())
    else {
        return false;
    };
    let Ok(list) = doc.put_object(&strip_entry, GROUPS_KEY, ObjType::List) else {
        return false;
    };
    for (i, group) in groups.iter().enumerate() {
        insert_group(doc, &list, i, group);
    }
    true
}

/// Replace one group's tab list with `tabs`, in order.
pub fn rewrite_group_tabs(doc: &mut AutoCommit, group_id: GroupId, tabs: &[Tab]) -> bool {
    let Some(group_entry) =
        group_anywhere(doc, group_id).and_then(|(groups, idx)| obj(doc, &groups, idx))
    else {
        return false;
    };
    let Ok(list) = doc.put_object(&group_entry, TABS_KEY, ObjType::List) else {
        return false;
    };
    for (i, tab) in tabs.iter().enumerate() {
        let _ = put_tab(doc, &list, i, tab);
    }
    true
}

/// Append a tab to a group of `workspace_id`: the group `group_id`, or with
/// `None` the first group of the first strip. An empty `tile_blob` leaves
/// the tile unset.
pub fn append_tab(
    doc: &mut AutoCommit,
    workspace_id: WorkspaceId,
    group_id: Option<GroupId>,
    tab_id: TabId,
    title: String,
    tile_blob: Vec<u8>,
) -> bool {
    let Some(entry) = workspace_entry(doc, workspace_id) else {
        return false;
    };
    let Some(strips) = obj(doc, &entry, STRIPS_KEY) else {
        return false;
    };
    let group_entry = match group_id {
        None => obj(doc, &strips, 0_usize)
            .and_then(|strip| obj(doc, &strip, GROUPS_KEY))
            .and_then(|groups| obj(doc, &groups, 0_usize)),
        Some(group_id) => (0..doc.length(&strips)).find_map(|i| {
            let groups = obj(doc, &obj(doc, &strips, i)?, GROUPS_KEY)?;
            let idx = group_index(doc, &groups, group_id)?;
            obj(doc, &groups, idx)
        }),
    };
    let Some(group_entry) = group_entry else {
        return false;
    };
    let Some(tabs) = obj(doc, &group_entry, TABS_KEY)
        .or_else(|| doc.put_object(&group_entry, TABS_KEY, ObjType::List).ok())
    else {
        return false;
    };
    let index = doc.length(&tabs);
    let Ok(tab_entry) = doc.insert_object(&tabs, index, ObjType::Map) else {
        return false;
    };
    let _ = doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
    let _ = doc.put(&tab_entry, "title", title);
    if !tile_blob.is_empty() {
        let _ = doc.put(&tab_entry, "tile_json", ScalarValue::Bytes(tile_blob));
    }
    true
}
