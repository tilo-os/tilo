//! A room's shared desktop as one Automerge record, entry by entry.
//!
//! Every workspace, strip, window group and window is its own map under its
//! id, holding where it is (its parent and an order among its siblings) and
//! who made it. Concurrent changes to different entries merge: two people
//! opening, moving or closing their own windows at once each keep their
//! change, and a page that reconnects with an old desktop only writes the
//! entries it changed itself, never over what others did meanwhile. Who may
//! change which entry is [`crate::ownership`]'s rule.
//!
//! Shape, at the record's root:
//!
//! ```text
//! workspaces: { <id>: { order: f64, workspace: <Workspace JSON, no strips> } }
//! strips:     { <id>: { workspace: <id>, order: f64, name, author? } }
//! groups:     { <id>: { strip: <id>, order: f64 } }
//! windows:    { <id>: { group: <id>, order: f64, tab: <Tab JSON>, author?, record? } }
//! ```
//!
//! What only one device decides (which window is focused, widths,
//! maximization) is never written. [`read`] tolerates what concurrency
//! leaves behind: a window whose group was removed meanwhile gets a group
//! of its own in the first strip, and a group or strip whose parent was
//! removed is placed in the first one, so nothing anyone opened vanishes.

use std::collections::{BTreeMap, HashMap};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::ids::{GroupId, StripId, TabId, WorkspaceId};
use crate::layout::{Strip, Tab, WindowGroup};
use crate::ownership::SharedDesktop;
use crate::tree::DesktopTree;
use crate::width::WindowWidth;
use crate::workspace::Workspace;

const WORKSPACES: &str = "workspaces";
const STRIPS: &str = "strips";
const GROUPS: &str = "groups";
const WINDOWS: &str = "windows";
const ORDER: &str = "order";
const WORKSPACE: &str = "workspace";
const STRIP: &str = "strip";
const GROUP: &str = "group";
const NAME: &str = "name";
const AUTHOR: &str = "author";
const TAB: &str = "tab";
const RECORD: &str = "record";

/// A document read now or as of some heads.
struct At<'a, D: ReadDoc> {
    doc: &'a D,
    heads: Option<&'a [ChangeHash]>,
}

impl<D: ReadDoc> At<'_, D> {
    fn get(&self, obj: &ObjId, key: &str) -> Option<(Value<'_>, ObjId)> {
        match self.heads {
            Some(heads) => self.doc.get_at(obj, key, heads),
            None => self.doc.get(obj, key),
        }
        .ok()
        .flatten()
    }

    /// Every map at `ROOT[key]`: the winner first, then concurrent losers.
    fn containers(&self, key: &str) -> Vec<ObjId> {
        match self.heads {
            Some(heads) => self.doc.get_all_at(ROOT, key, heads),
            None => self.doc.get_all(ROOT, key),
        }
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(value, id)| matches!(value, Value::Object(ObjType::Map)).then_some(id))
        .collect()
    }

    fn keys(&self, obj: &ObjId) -> Vec<String> {
        match self.heads {
            Some(heads) => self.doc.keys_at(obj, heads).collect(),
            None => self.doc.keys(obj).collect(),
        }
    }

    fn text(&self, obj: &ObjId, key: &str) -> Option<String> {
        match self.get(obj, key)? {
            (Value::Scalar(scalar), _) => match scalar.as_ref() {
                ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn number(&self, obj: &ObjId, key: &str) -> Option<f64> {
        match self.get(obj, key)? {
            (Value::Scalar(scalar), _) => match scalar.as_ref() {
                ScalarValue::F64(value) => Some(*value),
                _ => None,
            },
            _ => None,
        }
    }

    /// Every entry of the containers at `ROOT[key]`, each id once: the
    /// entry's map, in whichever container holds it first.
    fn entries(&self, key: &str) -> BTreeMap<u64, ObjId> {
        let mut entries = BTreeMap::new();
        for container in self.containers(key) {
            for id in self.keys(&container) {
                let Ok(parsed) = id.parse::<u64>() else {
                    continue;
                };
                if entries.contains_key(&parsed) {
                    continue;
                }
                if let Some((Value::Object(ObjType::Map), entry)) = self.get(&container, &id) {
                    entries.insert(parsed, entry);
                }
            }
        }
        entries
    }
}

fn parse_id(text: Option<String>) -> Option<u64> {
    text?.parse().ok()
}

fn sorted<T>(mut items: Vec<(f64, u64, T)>) -> Vec<T> {
    items.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    items.into_iter().map(|(_, _, item)| item).collect()
}

/// What a record holds, read entry by entry and not yet put together.
#[derive(Default)]
struct Parts {
    authors: BTreeMap<u64, String>,
    records: Vec<(TabId, String)>,
    workspaces: Vec<Workspace>,
    /// Strips by the workspace they name.
    strips: HashMap<Option<u64>, Vec<(f64, u64, Strip)>>,
    /// Groups by the strip they name.
    groups: HashMap<Option<u64>, Vec<(f64, u64, GroupId)>>,
    /// Windows by the group they name.
    windows: HashMap<u64, Vec<(f64, u64, Tab)>>,
}

impl Parts {
    fn of<D: ReadDoc>(at: &At<'_, D>) -> Self {
        let mut parts = Self::default();
        let mut workspaces = Vec::new();
        for (id, entry) in at.entries(WORKSPACES) {
            let Some(mut workspace) = at
                .text(&entry, WORKSPACE)
                .and_then(|json| serde_json::from_str::<Workspace>(&json).ok())
            else {
                continue;
            };
            workspace.id = WorkspaceId(id);
            workspace.strips.clear();
            workspaces.push((at.number(&entry, ORDER).unwrap_or(0.0), id, workspace));
        }
        parts.workspaces = sorted(workspaces);
        for (id, entry) in at.entries(STRIPS) {
            if let Some(author) = at.text(&entry, AUTHOR) {
                parts.authors.insert(id, author);
            }
            let name = at.text(&entry, NAME).unwrap_or_default();
            parts
                .strips
                .entry(parse_id(at.text(&entry, WORKSPACE)))
                .or_default()
                .push((
                    at.number(&entry, ORDER).unwrap_or(0.0),
                    id,
                    Strip::new(StripId(id), name),
                ));
        }
        for (id, entry) in at.entries(GROUPS) {
            parts
                .groups
                .entry(parse_id(at.text(&entry, STRIP)))
                .or_default()
                .push((at.number(&entry, ORDER).unwrap_or(0.0), id, id));
        }
        for (id, entry) in at.entries(WINDOWS) {
            let Some(mut tab) = at
                .text(&entry, TAB)
                .and_then(|json| serde_json::from_str::<Tab>(&json).ok())
            else {
                continue;
            };
            tab.id = id;
            if let Some(author) = at.text(&entry, AUTHOR) {
                parts.authors.insert(id, author);
            }
            if let Some(record) = at.text(&entry, RECORD) {
                parts.records.push((id, record));
            }
            parts
                .windows
                .entry(parse_id(at.text(&entry, GROUP)).unwrap_or(0))
                .or_default()
                .push((at.number(&entry, ORDER).unwrap_or(0.0), id, tab));
        }
        parts
    }

    /// Put the parts together, adopting what lost its parent meanwhile
    /// into the first workspace or strip.
    fn assemble(mut self) -> Option<SharedDesktop> {
        let first_workspace = self.workspaces.first()?.id.0;
        let workspace_ids: Vec<u64> = self.workspaces.iter().map(|w| w.id.0).collect();
        let mut strips: HashMap<u64, Vec<(f64, u64, Strip)>> = HashMap::new();
        for (parent, list) in self.strips.drain() {
            let parent = parent
                .filter(|id| workspace_ids.contains(id))
                .unwrap_or(first_workspace);
            strips.entry(parent).or_default().extend(list);
        }
        for workspace in &mut self.workspaces {
            workspace.strips = sorted(strips.remove(&workspace.id.0).unwrap_or_default());
        }
        let strip_ids: Vec<u64> = self
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter().map(|s| s.id.0))
            .collect();
        if let Some(&first_strip) = strip_ids.first() {
            let mut groups: HashMap<u64, Vec<(f64, u64, GroupId)>> = HashMap::new();
            for (parent, list) in self.groups.drain() {
                let parent = parent
                    .filter(|id| strip_ids.contains(id))
                    .unwrap_or(first_strip);
                groups.entry(parent).or_default().extend(list);
            }
            for (parent, list) in &self.windows {
                if !groups.values().flatten().any(|group| group.2 == *parent) {
                    // Its group went away while a window was opened or moved
                    // into it: the window keeps a group of that id.
                    let order = list.iter().map(|w| w.0).fold(f64::MAX, f64::min);
                    groups
                        .entry(first_strip)
                        .or_default()
                        .push((order, *parent, *parent));
                }
            }
            for strip in self.workspaces.iter_mut().flat_map(|w| w.strips.iter_mut()) {
                let list = sorted(groups.remove(&strip.id.0).unwrap_or_default());
                strip.window_groups = list
                    .into_iter()
                    .filter_map(|group| {
                        let tabs = sorted(self.windows.remove(&group).unwrap_or_default());
                        let first = tabs.first()?.id;
                        Some(WindowGroup {
                            id: group,
                            tabs,
                            active_tab: first,
                            width: WindowWidth::Normal,
                            maximized: false,
                            focus_history: Vec::new(),
                        })
                    })
                    .collect();
            }
        }
        self.records.sort_unstable();
        let active = WorkspaceId(first_workspace);
        Some(SharedDesktop {
            tree: DesktopTree {
                workspaces: self.workspaces,
                active,
                ..DesktopTree::default()
            },
            records: self.records,
            authors: self.authors,
        })
    }
}

/// The shared desktop `doc` holds, now (`None`) or as of `heads`; `None`
/// when it holds no workspace there.
#[must_use]
pub fn read(doc: &impl ReadDoc, heads: Option<&[ChangeHash]>) -> Option<SharedDesktop> {
    Parts::of(&At { doc, heads }).assemble()
}

/// `desktop` as [`read`] gives it back: only what the record keeps, so a
/// local desktop and the room's compare equal when nothing shared differs.
#[must_use]
pub fn canonical(desktop: &SharedDesktop) -> SharedDesktop {
    let mut doc = AutoCommit::new();
    if write(&mut doc, desktop).is_err() {
        return desktop.clone();
    }
    read(&doc, None).unwrap_or_default()
}

/// One entry as written: its parent and order, and its text fields.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    parent: Option<(&'static str, u64)>,
    order: f64,
    texts: Vec<(&'static str, String)>,
}

/// `n` as an order step.
fn float(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The positions of the longest strictly rising run among `values`
/// (ignoring the missing ones).
fn rising(values: &[Option<f64>]) -> Vec<usize> {
    // tails[k]: the position ending the best run of length k + 1.
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; values.len()];
    let value_at = |index: usize| values.get(index).copied().flatten().unwrap_or(f64::MIN);
    for (index, value) in values.iter().enumerate() {
        let Some(value) = value else { continue };
        let at = tails.partition_point(|&t| value_at(t) < *value);
        if let Some(slot) = previous.get_mut(index) {
            *slot = at
                .checked_sub(1)
                .and_then(|before| tails.get(before).copied());
        }
        match tails.get_mut(at) {
            Some(tail) => *tail = index,
            None => tails.push(index),
        }
    }
    let mut run = Vec::new();
    let mut cursor = tails.last().copied();
    while let Some(index) = cursor {
        run.push(index);
        cursor = previous.get(index).copied().flatten();
    }
    run.reverse();
    run
}

/// Orders for `ids`, in order, keeping each `held` order that still rises
/// (the longest run of them that does) and placing the rest between their
/// neighbours: a move writes only what moved.
fn orders(ids: &[u64], held: &HashMap<u64, f64>) -> Vec<f64> {
    let values: Vec<Option<f64>> = ids.iter().map(|id| held.get(id).copied()).collect();
    let kept = rising(&values);
    let mut out: Vec<Option<f64>> = values
        .iter()
        .enumerate()
        .map(|(index, value)| value.filter(|_| kept.contains(&index)))
        .collect();
    // Fill each run of gaps evenly between the kept orders around it.
    let mut start = 0;
    while start < out.len() {
        if out.get(start).copied().flatten().is_some() {
            start = start.saturating_add(1);
            continue;
        }
        let end = (start..out.len())
            .find(|&i| out.get(i).copied().flatten().is_some())
            .unwrap_or(out.len());
        let before = start
            .checked_sub(1)
            .and_then(|i| out.get(i).copied().flatten());
        let after = out.get(end).copied().flatten();
        let count = float(end.saturating_sub(start));
        let (low, high) = match (before, after) {
            (Some(low), Some(high)) => (low, high),
            (Some(low), None) => (low, low + count + 1.0),
            (None, Some(high)) => (high - count - 1.0, high),
            (None, None) => (0.0, count + 1.0),
        };
        let step = (high - low) / (count + 1.0);
        for (offset, slot) in out
            .iter_mut()
            .skip(start)
            .take(end.saturating_sub(start))
            .enumerate()
        {
            *slot = Some(low + step * (float(offset) + 1.0));
        }
        start = end;
    }
    out.into_iter().map(|order| order.unwrap_or(0.0)).collect()
}

/// The entries of each kind a desktop is written as, or a document holds.
struct Levels<T> {
    workspaces: BTreeMap<u64, T>,
    strips: BTreeMap<u64, T>,
    groups: BTreeMap<u64, T>,
    windows: BTreeMap<u64, T>,
}

impl<T> Default for Levels<T> {
    fn default() -> Self {
        Self {
            workspaces: BTreeMap::new(),
            strips: BTreeMap::new(),
            groups: BTreeMap::new(),
            windows: BTreeMap::new(),
        }
    }
}

/// Write `desktop` into `doc`, changing only the entries that differ from
/// what `doc` holds: an unchanged desktop writes nothing. Returns whether it
/// changed anything.
///
/// # Errors
/// The document refused a write.
pub fn write(
    doc: &mut AutoCommit,
    desktop: &SharedDesktop,
) -> Result<bool, automerge::AutomergeError> {
    let held = held(doc);
    let want = wanted(desktop, &held);
    let mut changed = false;
    changed |= reconcile(doc, WORKSPACES, &held.workspaces, &want.workspaces)?;
    changed |= reconcile(doc, STRIPS, &held.strips, &want.strips)?;
    changed |= reconcile(doc, GROUPS, &held.groups, &want.groups)?;
    changed |= reconcile(doc, WINDOWS, &held.windows, &want.windows)?;
    Ok(changed)
}

/// The held orders of the entries in `level` under `parent`.
fn held_orders(level: &BTreeMap<u64, HeldEntry>, parent: Option<u64>) -> HashMap<u64, f64> {
    level
        .iter()
        .filter(|(_, held)| held.entry.parent.map(|(_, id)| id) == parent)
        .filter(|(_, held)| held.entry.order.is_finite())
        .map(|(id, held)| (*id, held.entry.order))
        .collect()
}

/// The entries `desktop` is written as, its orders kept from `held`'s.
fn wanted(desktop: &SharedDesktop, held: &Levels<HeldEntry>) -> Levels<Entry> {
    let records: HashMap<u64, &String> = desktop.records.iter().map(|(t, r)| (*t, r)).collect();
    let author = |id: u64| {
        desktop
            .authors
            .get(&id)
            .map(|author| (AUTHOR, author.clone()))
    };
    let mut want = Levels::default();
    let workspaces = &desktop.tree.workspaces;
    let workspace_ids: Vec<u64> = workspaces.iter().map(|w| w.id.0).collect();
    let workspace_orders = orders(&workspace_ids, &held_orders(&held.workspaces, None));
    for (workspace, order) in workspaces.iter().zip(workspace_orders) {
        let mut bare = workspace.clone();
        bare.strips.clear();
        let json = serde_json::to_string(&bare).unwrap_or_default();
        let texts = vec![(WORKSPACE, json)];
        want.workspaces.insert(
            workspace.id.0,
            Entry {
                parent: None,
                order,
                texts,
            },
        );
        let ids: Vec<u64> = workspace.strips.iter().map(|s| s.id.0).collect();
        let strip_orders = orders(&ids, &held_orders(&held.strips, Some(workspace.id.0)));
        for (strip, order) in workspace.strips.iter().zip(strip_orders) {
            let mut texts = vec![(NAME, strip.name.clone())];
            texts.extend(author(strip.id.0));
            let parent = Some((WORKSPACE, workspace.id.0));
            want.strips.insert(
                strip.id.0,
                Entry {
                    parent,
                    order,
                    texts,
                },
            );
            let ids: Vec<u64> = strip.window_groups.iter().map(|g| g.id).collect();
            let group_orders = orders(&ids, &held_orders(&held.groups, Some(strip.id.0)));
            for (group, order) in strip.window_groups.iter().zip(group_orders) {
                let parent = Some((STRIP, strip.id.0));
                want.groups.insert(
                    group.id,
                    Entry {
                        parent,
                        order,
                        texts: Vec::new(),
                    },
                );
                let ids: Vec<u64> = group.tabs.iter().map(|t| t.id).collect();
                let tab_orders = orders(&ids, &held_orders(&held.windows, Some(group.id)));
                for (tab, order) in group.tabs.iter().zip(tab_orders) {
                    let mut texts = vec![(TAB, serde_json::to_string(tab).unwrap_or_default())];
                    texts.extend(author(tab.id));
                    if let Some(record) = records.get(&tab.id) {
                        texts.push((RECORD, (*record).clone()));
                    }
                    let parent = Some((GROUP, group.id));
                    want.windows.insert(
                        tab.id,
                        Entry {
                            parent,
                            order,
                            texts,
                        },
                    );
                }
            }
        }
    }
    want
}

/// An entry a document holds: its map, and what it says.
struct HeldEntry {
    obj: ObjId,
    entry: Entry,
}

/// What `doc` holds of each kind of entry, by id.
fn held(doc: &AutoCommit) -> Levels<HeldEntry> {
    let at = At { doc, heads: None };
    let level = |key: &str, parent: Option<&'static str>, fields: &[&'static str]| {
        at.entries(key)
            .into_iter()
            .map(|(id, obj)| {
                let parent =
                    parent.and_then(|field| parse_id(at.text(&obj, field)).map(|id| (field, id)));
                let order = at.number(&obj, ORDER).unwrap_or(f64::NAN);
                let texts = fields
                    .iter()
                    .filter_map(|field| at.text(&obj, field).map(|text| (*field, text)))
                    .collect();
                (
                    id,
                    HeldEntry {
                        obj,
                        entry: Entry {
                            parent,
                            order,
                            texts,
                        },
                    },
                )
            })
            .collect()
    };
    Levels {
        workspaces: level(WORKSPACES, None, &[WORKSPACE]),
        strips: level(STRIPS, Some(WORKSPACE), &[NAME, AUTHOR]),
        groups: level(GROUPS, Some(STRIP), &[]),
        windows: level(WINDOWS, Some(GROUP), &[TAB, AUTHOR, RECORD]),
    }
}

/// Make the entries at `ROOT[key]` exactly `want`, writing only what differs.
fn reconcile(
    doc: &mut AutoCommit,
    key: &str,
    held: &BTreeMap<u64, HeldEntry>,
    want: &BTreeMap<u64, Entry>,
) -> Result<bool, automerge::AutomergeError> {
    let mut changed = false;
    let containers = At {
        doc: &*doc,
        heads: None,
    }
    .containers(key);
    // Removed entries, from every container that holds them.
    for id in held.keys().filter(|id| !want.contains_key(id)) {
        let name = id.to_string();
        for container in &containers {
            if doc.get(container, name.as_str())?.is_some() {
                doc.delete(container, name.as_str())?;
                changed = true;
            }
        }
    }
    let container = match containers.first() {
        Some(container) => container.clone(),
        None if want.is_empty() => return Ok(changed),
        None => {
            changed = true;
            doc.put_object(ROOT, key, ObjType::Map)?
        }
    };
    for (id, entry) in want {
        let before = held.get(id).map(|existing| &existing.entry);
        if before == Some(entry) {
            continue;
        }
        let obj = match held.get(id) {
            Some(existing) => existing.obj.clone(),
            None => doc.put_object(&container, id.to_string(), ObjType::Map)?,
        };
        changed = true;
        if let Some((field, parent)) = entry.parent
            && before.and_then(|b| b.parent) != Some((field, parent))
        {
            doc.put(&obj, field, parent.to_string())?;
        }
        if before.is_none_or(|b| b.order.to_bits() != entry.order.to_bits()) {
            doc.put(&obj, ORDER, entry.order)?;
        }
        for (field, text) in &entry.texts {
            let same = before.is_some_and(|b| b.texts.iter().any(|(f, t)| f == field && t == text));
            if !same {
                doc.put(&obj, *field, text.as_str())?;
            }
        }
        for (field, _) in before.map(|b| b.texts.as_slice()).unwrap_or_default() {
            if !entry.texts.iter().any(|(f, _)| f == field) {
                doc.delete(&obj, *field)?;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;
