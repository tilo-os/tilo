//! How a room's replication document carries the desktop, and how it is
//! read back.
//!
//! Pure functions over an [`AutoCommit`] the host owns: projecting the
//! strips, groups and tabs this peer minted into a room document, reading a
//! document's contents back as plain values, and folding them into the
//! local tree under the one-writer-per-entry rule. Admission — deciding
//! whether a peer's batch may land at all — and the documents' lifetimes
//! stay with the host.

use automerge::{AutoCommit, ReadDoc};

use crate::app::AppKind;
use crate::ids::{GroupId, RoomId, StripId, TabId, counter_of, peer_salt_of};
use crate::layout::{Strip, Tab, TileContent, WindowGroup};
use crate::task::{WorkspaceEffectReceipt, WorkspaceTask};
use crate::tree::DesktopTree;
use crate::width::WindowWidth;

/// Key a room document stores its durable tasks under.
pub const ROOM_TASKS_KEY: &str = "tasks_v1";
/// Key a room document stores its effect receipts under.
pub const ROOM_EFFECT_RECEIPTS_KEY: &str = "effect_receipts_v1";

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
/// The document's error when a map this projection needs cannot be
/// created.
pub fn project_owned_contents(
    doc: &mut AutoCommit,
    strips: &[Strip],
    local_salt: u16,
    share_floor: Option<u64>,
) -> Result<(), automerge::AutomergeError> {
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    use std::collections::HashSet;

    if local_salt == 0 {
        return Ok(());
    }
    // "Mine to publish": minted by this peer, and since it joined
    // (not before) if this is a room it joined rather than minted.
    let mine = |id: u64| -> bool {
        peer_salt_of(id) == local_salt && share_floor.is_none_or(|floor| counter_of(id) >= floor)
    };

    let contents_id = match doc.get(&ROOT, ROOM_CONTENTS_KEY).ok().flatten() {
        Some((_, id)) => id,
        None => doc.put_object(&ROOT, ROOM_CONTENTS_KEY, ObjType::Map)?,
    };

    let mut live_strips: HashSet<String> = HashSet::new();
    for (si, strip) in strips.iter().enumerate() {
        // An empty strip holds no state; an empty row arriving on
        // someone else's desktop is noise, not news.
        if strip.window_groups.is_empty() {
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

        let skey = contents_key(strip.id.raw());
        let strip_entry = match doc.get(&contents_id, skey.as_str()).ok().flatten() {
            Some((_, id)) => id,
            None => match doc.put_object(&contents_id, skey.as_str(), ObjType::Map) {
                Ok(id) => id,
                Err(_) => continue,
            },
        };
        if owns_strip {
            live_strips.insert(skey.clone());
            put_str_if_changed(doc, &strip_entry, "name", &strip.name);
            put_uint_if_changed(doc, &strip_entry, "order", si as u64);
        }

        // A strip whose groups cannot be written is skipped; the sweep
        // below still runs.
        let _ = project_owned_groups(doc, &strip_entry, strip, &mine);
    }
    let stale: Vec<String> = doc
        .keys(&contents_id)
        .filter(|k| !live_strips.contains(k.as_str()) && contents_key_id(k).is_some_and(&mine))
        .collect();
    for key in stale {
        let _ = doc.delete(&contents_id, key.as_str());
    }
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
    use automerge::{ObjType, ReadDoc, transaction::Transactable};
    use std::collections::HashSet;

    let groups_id = match doc.get(strip_entry, "groups").ok().flatten() {
        Some((_, id)) => id,
        None => doc.put_object(strip_entry, "groups", ObjType::Map)?,
    };
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
        let group_entry = match doc.get(&groups_id, gkey.as_str()).ok().flatten() {
            Some((_, id)) => id,
            None => match doc.put_object(&groups_id, gkey.as_str(), ObjType::Map) {
                Ok(id) => id,
                Err(_) => continue,
            },
        };
        if owns_group {
            live_groups.insert(gkey.clone());
            put_uint_if_changed(doc, &group_entry, "order", gi as u64);
            // Width, maximization and active-tab selection are local
            // presentation preferences. Old room documents may still
            // contain the first two fields, but no current peer writes
            // or reads them.
        }

        let _ = project_owned_tabs(doc, &group_entry, group, mine);
    }
    let stale: Vec<String> = doc
        .keys(&groups_id)
        .filter(|k| !live_groups.contains(k.as_str()) && contents_key_id(k).is_some_and(mine))
        .collect();
    for key in stale {
        let _ = doc.delete(&groups_id, key.as_str());
    }
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
    use automerge::{ObjType, ReadDoc, ScalarValue, transaction::Transactable};
    use std::collections::HashSet;

    let tabs_id = match doc.get(group_entry, "tabs").ok().flatten() {
        Some((_, id)) => id,
        None => doc.put_object(group_entry, "tabs", ObjType::Map)?,
    };
    let mut live_tabs: HashSet<String> = HashSet::new();
    for (ti, tab) in group.tabs.iter().enumerate() {
        if tab.tile.is_device_local() || !mine(tab.id) {
            continue;
        }
        let tkey = contents_key(tab.id);
        live_tabs.insert(tkey.clone());
        let tab_entry = match doc.get(&tabs_id, tkey.as_str()).ok().flatten() {
            Some((_, id)) => id,
            None => match doc.put_object(&tabs_id, tkey.as_str(), ObjType::Map) {
                Ok(id) => id,
                Err(_) => continue,
            },
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
    let stale: Vec<String> = doc
        .keys(&tabs_id)
        .filter(|k| !live_tabs.contains(k.as_str()) && contents_key_id(k).is_some_and(mine))
        .collect();
    for key in stale {
        let _ = doc.delete(&tabs_id, key.as_str());
    }
    Ok(())
}

/// Read a room document's `contents` back into plain values. Every
/// level sorts by `(order, id)`; an entry with no `order` sorts
/// last so it can't displace one whose owner did state a position.
pub fn contents_of(doc: &AutoCommit) -> Vec<ReplicatedStrip> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

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

    let Some((_, contents_id)) = doc.get(&ROOT, ROOM_CONTENTS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut strips: Vec<ReplicatedStrip> = Vec::new();
    for skey in doc.keys(&contents_id) {
        let Some(sid) = contents_key_id(&skey) else {
            continue;
        };
        let Some((_, strip_entry)) = doc.get(&contents_id, skey.as_str()).ok().flatten() else {
            continue;
        };
        let mut groups: Vec<ReplicatedGroup> = Vec::new();
        if let Some((_, groups_id)) = doc.get(&strip_entry, "groups").ok().flatten() {
            for gkey in doc.keys(&groups_id) {
                let Some(gid) = contents_key_id(&gkey) else {
                    continue;
                };
                let Some((_, group_entry)) = doc.get(&groups_id, gkey.as_str()).ok().flatten()
                else {
                    continue;
                };
                let mut tabs: Vec<ReplicatedTab> = Vec::new();
                if let Some((_, tabs_id)) = doc.get(&group_entry, "tabs").ok().flatten() {
                    for tkey in doc.keys(&tabs_id) {
                        let Some(tid) = contents_key_id(&tkey) else {
                            continue;
                        };
                        let Some((_, tab_entry)) = doc.get(&tabs_id, tkey.as_str()).ok().flatten()
                        else {
                            continue;
                        };
                        let tile = match doc.get(&tab_entry, "tile_json").ok().flatten() {
                            Some((Value::Scalar(v), _)) => match v.as_ref() {
                                ScalarValue::Bytes(b) => serde_json::from_slice(b).ok(),
                                _ => None,
                            },
                            _ => None,
                        };
                        tabs.push(ReplicatedTab {
                            id: tid,
                            order: uint(&tab_entry, "order").unwrap_or(u64::MAX),
                            title: text(&tab_entry, "title").unwrap_or_default(),
                            tile,
                        });
                    }
                }
                tabs.sort_by_key(|t| (t.order, t.id));
                groups.push(ReplicatedGroup {
                    id: gid,
                    order: uint(&group_entry, "order").unwrap_or(u64::MAX),
                    tabs,
                });
            }
        }
        groups.sort_by_key(|g| (g.order, g.id));
        strips.push(ReplicatedStrip {
            id: sid,
            order: uint(&strip_entry, "order").unwrap_or(u64::MAX),
            name: text(&strip_entry, "name"),
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
    }
    Ok(())
}

/// Read a room document's record index back as `(record id, kind)`
/// pairs, sorted by id so two peers read one document the same way.
pub fn room_records_of(doc: &AutoCommit) -> Vec<(String, String)> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, records_id)) = doc.get(&ROOT, ROOM_RECORDS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
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
        out.push((key, kind));
    }
    out.sort();
    out
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
    local_salt: u16,
) -> bool {
    if local_salt == 0 {
        return false;
    }
    let is_local = |id: u64| peer_salt_of(id) == local_salt;

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
    match candidate.get(&ROOT, "room").ok().flatten() {
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
