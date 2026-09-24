//! Canonical native room topology projection. Input must already be admitted.
//! This module grants no authority and never merges transport changes.
use crate::identity::{counter_of, peer_salt_of};
use crate::*;
use automerge::AutoCommit;
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

fn put_str_if_changed(doc: &mut AutoCommit, obj: &automerge::ObjId, key: &str, value: &str) {
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
fn put_uint_if_changed(doc: &mut AutoCommit, obj: &automerge::ObjId, key: &str, value: u64) {
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
// `contents` and every map beneath it are created on first projection. On the
// legacy (unadmitted) room path two peers whose room documents start empty
// each mint their own root `contents` map (and a guest may mint a strip's
// `groups` / a group's `tabs` before the owner does): a map-key conflict where
// Automerge shows one winner and the other peer's windows vanish. Reads join
// every candidate map at every level; writes go to the winner; each peer's
// sweep removes its stale entries from every candidate, so a strip removed
// while it sat in a losing map cannot come back as a ghost.

/// Every map stored under `obj[key]`: the `get` winner first, then the
/// concurrent losers in Automerge's deterministic order.
fn map_candidates(doc: &AutoCommit, obj: &automerge::ObjId, key: &str) -> Vec<automerge::ObjId> {
    use automerge::{ObjType, ReadDoc, Value};
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
) -> Option<automerge::ObjId> {
    use automerge::{ObjType, transaction::Transactable};
    if let Some(first) = candidates.first() {
        return Some(first.clone());
    }
    match doc.put_object(parent, key, ObjType::Map) {
        Ok(id) => Some(id),
        Err(err) => {
            tracing::error!(?err, key, "room doc: map");
            None
        }
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
    use automerge::{ReadDoc, transaction::Transactable};
    for container in containers {
        let stale: Vec<String> = doc
            .keys(container)
            .filter(|k| {
                !live.contains(k.as_str()) && RoomProjection::contents_key_id(k).is_some_and(mine)
            })
            .collect();
        for key in stale {
            let _ = doc.delete(container, key.as_str());
        }
    }
}

pub struct RoomProjection;
impl RoomProjection {
    /// Key the room document stores the room's window contents under.
    const ROOM_CONTENTS_KEY: &'static str = "contents";

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
    /// [`Self::apply_room_changes`] enforces, so last-writer-wins has
    /// nothing to arbitrate. Entries carry their index; readers sort by
    /// `(order, id)` so peers appending at once still agree, and the
    /// sweep at each level deletes only entries this machine owns. Salt
    /// `0` has no id space, so nothing is projected or merged there.
    pub fn project_owned_contents(
        doc: &mut AutoCommit,
        strips: &[Strip],
        local_salt: u16,
        share_floor: Option<u64>,
    ) {
        Self::project_owned_contents_with_empty(doc, strips, local_salt, share_floor, false);
    }

    /// Explicit admitted strip lifecycle retains named empty destinations.
    /// Ordinary native window projection keeps its existing empty-row policy.
    pub fn project_owned_command_contents(doc: &mut AutoCommit, strips: &[Strip], local_salt: u16) {
        Self::project_owned_contents_with_empty(doc, strips, local_salt, None, true);
    }

    fn project_owned_contents_with_empty(
        doc: &mut AutoCommit,
        strips: &[Strip],
        local_salt: u16,
        share_floor: Option<u64>,
        retain_empty: bool,
    ) {
        use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
        use std::collections::HashSet;

        if local_salt == 0 {
            return;
        }
        // "Mine to publish": minted by this peer, and since it joined
        // (not before) if this is a room it joined rather than minted.
        let mine = |id: u64| -> bool {
            peer_salt_of(id) == local_salt
                && share_floor.is_none_or(|floor| counter_of(id) >= floor)
        };

        let contents_all = map_candidates(doc, &ROOT, Self::ROOM_CONTENTS_KEY);
        let Some(contents_id) =
            winner_or_create(doc, &contents_all, &ROOT, Self::ROOM_CONTENTS_KEY)
        else {
            return;
        };
        let contents_all = if contents_all.is_empty() {
            vec![contents_id.clone()]
        } else {
            contents_all
        };

        let mut live_strips: HashSet<String> = HashSet::new();
        for (si, strip) in strips.iter().enumerate() {
            let skey = Self::contents_key(strip.id.raw());
            let strip_all = child_candidates(doc, &contents_all, skey.as_str());
            // Unmarked empty native rows keep their historical local-only
            // policy. An admitted explicit strip has durable lifecycle state:
            // later ordinary projections preserve it until the strip is removed.
            let retained = strip_all.first().is_some_and(|entry| {
                doc.get(entry, "retained_empty_v1")
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.to_u64())
                    == Some(1)
            });
            if strip.window_groups.is_empty() && !retain_empty && !retained {
                continue;
            }
            let owns_strip = mine(strip.id.raw());
            // A peer's strip is still walked: a window we opened into it
            // is ours to publish even though the row isn't.
            let has_owned = strip
                .window_groups
                .iter()
                .any(|g| mine(g.id) || g.tabs.iter().any(|t| mine(t.id)));
            if !owns_strip && !has_owned {
                continue;
            }

            let Some(strip_entry) = winner_or_create(doc, &strip_all, &contents_id, skey.as_str())
            else {
                continue;
            };
            if owns_strip {
                live_strips.insert(skey.clone());
                if retain_empty {
                    put_uint_if_changed(doc, &strip_entry, "retained_empty_v1", 1);
                }
                put_str_if_changed(doc, &strip_entry, "name", &strip.name);
                put_uint_if_changed(doc, &strip_entry, "order", si as u64);
            }

            let strip_all = if strip_all.is_empty() {
                vec![strip_entry]
            } else {
                strip_all
            };
            Self::project_owned_groups_in(doc, &strip_all, strip, &mine);
        }
        sweep_stale(doc, &contents_all, &live_strips, &mine);
    }

    /// [`Self::project_owned_contents`]'s per-strip pass: get-or-create
    /// `strip_entry`'s `groups` map, project every group this machine owns
    /// (or owns a tab within), then sweep group keys this machine owns
    /// that no longer appear among `strip`'s groups.
    pub fn project_owned_groups(
        doc: &mut AutoCommit,
        strip_entry: &automerge::ObjId,
        strip: &Strip,
        mine: &impl Fn(u64) -> bool,
    ) {
        Self::project_owned_groups_in(doc, std::slice::from_ref(strip_entry), strip, mine);
    }

    /// [`Self::project_owned_groups`] over every candidate entry of one strip
    /// (the first is the one written to).
    fn project_owned_groups_in(
        doc: &mut AutoCommit,
        strip_entries: &[automerge::ObjId],
        strip: &Strip,
        mine: &impl Fn(u64) -> bool,
    ) {
        use std::collections::HashSet;

        let Some(strip_entry) = strip_entries.first() else {
            return;
        };
        let groups_all = child_candidates(doc, strip_entries, "groups");
        let Some(groups_id) = winner_or_create(doc, &groups_all, strip_entry, "groups") else {
            return;
        };
        let groups_all = if groups_all.is_empty() {
            vec![groups_id.clone()]
        } else {
            groups_all
        };
        let mut live_groups: HashSet<String> = HashSet::new();
        for (gi, group) in strip.window_groups.iter().enumerate() {
            let owns_group = mine(group.id);
            let has_owned_tabs = group.tabs.iter().any(|t| mine(t.id));
            if !owns_group && !has_owned_tabs {
                continue;
            }
            let gkey = Self::contents_key(group.id);
            let group_all = child_candidates(doc, &groups_all, gkey.as_str());
            let Some(group_entry) = winner_or_create(doc, &group_all, &groups_id, gkey.as_str())
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

            let group_all = if group_all.is_empty() {
                vec![group_entry]
            } else {
                group_all
            };
            Self::project_owned_tabs_in(doc, &group_all, group, mine);
        }
        sweep_stale(doc, &groups_all, &live_groups, mine);
    }

    /// [`Self::project_owned_groups`]'s per-group pass: get-or-create
    /// `group_entry`'s `tabs` map, project every tab this machine owns,
    /// then sweep tab keys this machine owns that no longer appear among
    /// `group`'s tabs.
    pub fn project_owned_tabs(
        doc: &mut AutoCommit,
        group_entry: &automerge::ObjId,
        group: &WindowGroup,
        mine: &impl Fn(u64) -> bool,
    ) {
        Self::project_owned_tabs_in(doc, std::slice::from_ref(group_entry), group, mine);
    }

    /// [`Self::project_owned_tabs`] over every candidate entry of one group
    /// (the first is the one written to).
    fn project_owned_tabs_in(
        doc: &mut AutoCommit,
        group_entries: &[automerge::ObjId],
        group: &WindowGroup,
        mine: &impl Fn(u64) -> bool,
    ) {
        use automerge::{ReadDoc, ScalarValue, transaction::Transactable};
        use std::collections::HashSet;

        let Some(group_entry) = group_entries.first() else {
            return;
        };
        let tabs_all = child_candidates(doc, group_entries, "tabs");
        let Some(tabs_id) = winner_or_create(doc, &tabs_all, group_entry, "tabs") else {
            return;
        };
        let tabs_all = if tabs_all.is_empty() {
            vec![tabs_id.clone()]
        } else {
            tabs_all
        };
        let mut live_tabs: HashSet<String> = HashSet::new();
        for (ti, tab) in group.tabs.iter().enumerate() {
            if !mine(tab.id) {
                continue;
            }
            let tkey = Self::contents_key(tab.id);
            live_tabs.insert(tkey.clone());
            let tab_all = child_candidates(doc, &tabs_all, tkey.as_str());
            let Some(tab_entry) = winner_or_create(doc, &tab_all, &tabs_id, tkey.as_str()) else {
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

        // Join every candidate map at every level (see "concurrently created
        // containers"); for an entry present in several candidates the first
        // (winning) copy supplies its fields.
        let entries = |containers: &[automerge::ObjId]| -> Vec<(u64, Vec<automerge::ObjId>)> {
            let mut out: Vec<(u64, Vec<automerge::ObjId>)> = Vec::new();
            for container in containers {
                for key in doc.keys(container) {
                    let Some(id) = Self::contents_key_id(&key) else {
                        continue;
                    };
                    if out.iter().any(|(seen, _)| *seen == id) {
                        continue;
                    }
                    out.push((id, child_candidates(doc, containers, key.as_str())));
                }
            }
            out.retain(|(_, copies)| !copies.is_empty());
            out
        };
        let contents_all = map_candidates(doc, &ROOT, Self::ROOM_CONTENTS_KEY);
        let mut strips: Vec<ReplicatedStrip> = Vec::new();
        for (sid, strip_copies) in entries(&contents_all) {
            let strip_entry = &strip_copies[0];
            let mut groups: Vec<ReplicatedGroup> = Vec::new();
            for (gid, group_copies) in entries(&child_candidates(doc, &strip_copies, "groups")) {
                let group_entry = &group_copies[0];
                let mut tabs: Vec<ReplicatedTab> = Vec::new();
                for (tid, tab_copies) in entries(&child_candidates(doc, &group_copies, "tabs")) {
                    let tab_entry = &tab_copies[0];
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
    /// comparable digest. Native room admission diffs this
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
        for strip in Self::contents_of(doc) {
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

    /// Fold the room document's `contents` into the cached workspace
    /// addressing `room`, reporting whether anything changed. Entries
    /// this machine owns are never overwritten or dropped — the document
    /// is downstream of the cache, so reading one back could only revert
    /// an unprojected local change; a peer's entries are taken from the
    /// document and dropped when absent.
    ///
    /// `active_tab` is never read from the document: it stays put unless
    /// genuinely gone, and a group arriving from a peer opens on its
    /// first tab. Vanishing strips take their `active_strip`/
    /// `active_group` with them so resolvers fall back rather than point
    /// at nothing.
    pub fn merge_room_contents(
        tree: &mut WorkspaceTree,
        room: RoomId,
        doc: &AutoCommit,
        local_salt: u16,
    ) -> bool {
        if local_salt == 0 {
            return false;
        }
        Self::merge_contents(tree, room, doc, Some(local_salt))
    }
    /// Read-only viewers own no topology namespace, including the zero salt.
    pub fn merge_viewer_contents(tree: &mut WorkspaceTree, room: RoomId, doc: &AutoCommit) -> bool {
        Self::merge_contents(tree, room, doc, None)
    }
    fn merge_contents(
        tree: &mut WorkspaceTree,
        room: RoomId,
        doc: &AutoCommit,
        local_salt: Option<u16>,
    ) -> bool {
        let replicated = Self::contents_of(doc);
        let is_local = |id: u64| local_salt == Some(peer_salt_of(id));

        let Some(ws) = tree.workspaces.iter_mut().find(|w| w.room == room) else {
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
            out.push((
                order,
                sid,
                Self::merge_strip(strip, rep, &is_local, &mut changed),
            ));
        }
        for (sid, rep) in by_strip {
            out.push((rep.order, sid, Self::hydrate_strip(rep)));
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
        if tree
            .active_strip
            .get(&ws_id)
            .is_some_and(|active| !live.contains(active))
        {
            tree.active_strip.remove(&ws_id);
        }
        for strip in vanished {
            tree.active_group.remove(&strip);
        }
        changed
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
        let prior_group_order: Vec<u64> =
            local.window_groups.iter().map(|group| group.id).collect();

        let mut out: Vec<(u64, u64, WindowGroup)> = Vec::new();
        for (i, group) in std::mem::take(&mut local.window_groups)
            .into_iter()
            .enumerate()
        {
            let gid = group.id;
            match by_group.remove(&gid) {
                Some(rep) => {
                    let order = if is_local(gid) { i as u64 } else { rep.order };
                    out.push((order, gid, Self::merge_group(group, rep, is_local, changed)));
                }
                None if is_local(gid) => out.push((i as u64, gid, group)),
                None => *changed = true,
            }
        }
        for (gid, rep) in by_group {
            out.push((rep.order, gid, Self::hydrate_group(rep)));
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
                            let tile = Self::localise_tile(tile, Some(&tab.tile), &rep.title);
                            if serde_json::to_vec(&tab.tile).ok() != serde_json::to_vec(&tile).ok()
                            {
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
            out.push((rep.order, tid, Self::hydrate_tab(rep)));
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
            window_groups: rep.groups.into_iter().map(Self::hydrate_group).collect(),
        }
    }

    /// A window group that arrived from a peer. Its `active_tab` is
    /// chosen **here** — a local decision about a local view, not the
    /// opening peer's to make.
    pub fn hydrate_group(rep: ReplicatedGroup) -> WindowGroup {
        let tabs: Vec<Tab> = rep.tabs.into_iter().map(Self::hydrate_tab).collect();
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
                |tile| Self::localise_tile(tile, None, &title),
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
}

impl RoomProjection {
    /// Explicit routes preserve imported ordinary records. Missing bindings use
    /// the existing canonical package-primary/per-tab policy.
    pub fn record_routes(doc: &AutoCommit) -> std::collections::BTreeMap<u64, String> {
        use automerge::{ROOT, ReadDoc};
        let mut routes = std::collections::BTreeMap::new();
        let Some((_, contents)) = doc.get(ROOT, "contents").ok().flatten() else {
            return routes;
        };
        for strip in doc.map_range(&contents, ..) {
            let Some((_, groups)) = doc.get(&strip.id(), "groups").ok().flatten() else {
                continue;
            };
            for group in doc.map_range(&groups, ..) {
                let Some((_, tabs)) = doc.get(&group.id(), "tabs").ok().flatten() else {
                    continue;
                };
                for tab in doc.map_range(&tabs, ..) {
                    if let (Some(id), Some((value, _))) = (
                        Self::contents_key_id(&tab.key),
                        doc.get(&tab.id(), "record_id").ok().flatten(),
                    ) {
                        if let Some(record) = value.to_str() {
                            routes.insert(id, record.to_owned());
                        }
                    }
                }
            }
        }
        routes
    }
    pub fn bind_record(
        doc: &mut AutoCommit,
        strip: u64,
        group: u64,
        tab: u64,
        record: &str,
    ) -> Result<(), String> {
        use automerge::{ROOT, ReadDoc, transaction::Transactable};
        let mut object = ROOT;
        for key in [
            "contents".to_owned(),
            Self::contents_key(strip),
            "groups".into(),
            Self::contents_key(group),
            "tabs".into(),
            Self::contents_key(tab),
        ] {
            object = doc
                .get(&object, key)
                .map_err(|e| e.to_string())?
                .ok_or("missing topology binding target")?
                .1;
        }
        if let Some((value, _)) = doc.get(&object, "record_id").map_err(|e| e.to_string())? {
            if value.to_str() != Some(record) {
                return Err("canonical record route cannot be rebound".into());
            }
            return Ok(());
        }
        doc.put(object, "record_id", record)
            .map_err(|e| e.to_string())
    }
    /// Apply one already-admitted room to the canonical tree. Existing local
    /// workspace identity and local selection remain untouched.
    pub fn apply_admitted_room(
        tree: &mut WorkspaceTree,
        room: RoomId,
        doc: &AutoCommit,
    ) -> Result<bool, String> {
        use automerge::{ROOT, ReadDoc};
        let declared = doc
            .get(ROOT, "room")
            .map_err(|e| e.to_string())?
            .and_then(|(v, _)| v.to_str().map(str::to_owned));
        if declared.as_deref().and_then(RoomId::parse) != Some(room) {
            return Err("admitted room identity mismatch".into());
        }
        let name = doc
            .get(ROOT, "name")
            .map_err(|e| e.to_string())?
            .and_then(|(v, _)| v.to_str().map(str::to_owned))
            .unwrap_or_default();
        if !tree.workspaces.iter().any(|w| w.room == room) {
            let bytes = room.as_uuid().into_bytes();
            let mut raw =
                u64::from_be_bytes(bytes[..8].try_into().map_err(|_| "room identity")?).max(1);
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
        let workspace = tree
            .workspaces
            .iter_mut()
            .find(|w| w.room == room)
            .ok_or("workspace missing")?;
        let renamed = workspace.name != name;
        workspace.name = name;
        workspace.owner = doc
            .get(ROOT, "owner")
            .map_err(|e| e.to_string())?
            .and_then(|(v, _)| v.to_str().map(str::to_owned));
        let changed = Self::merge_viewer_contents(tree, room, doc);
        Ok(changed || renamed)
    }
}

/// Canonical task projection, preserving native tolerant reads.
pub fn workspace_tasks_of(doc: &AutoCommit) -> Vec<crate::WorkspaceTask> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    let Some((_, tasks_id)) = doc.get(&ROOT, "tasks_v1").ok().flatten() else {
        return Vec::new();
    };
    let mut tasks = Vec::new();
    for key in doc.keys(&tasks_id) {
        if let Some((Value::Scalar(value), _)) = doc.get(&tasks_id, key.as_str()).ok().flatten()
            && let ScalarValue::Bytes(blob) = value.as_ref()
            && let Ok(task) = serde_json::from_slice::<crate::WorkspaceTask>(blob)
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

/// Canonical redacted effect receipts, preserving native tolerant reads.
pub fn workspace_effect_receipts_of(doc: &AutoCommit) -> Vec<crate::WorkspaceEffectReceipt> {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};
    let Some((_, object)) = doc.get(&ROOT, "effect_receipts_v1").ok().flatten() else {
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
    receipts.sort_by_key(|receipt: &crate::WorkspaceEffectReceipt| receipt.completed_at);
    receipts
}
