//! A workspace's shared desktop as one Automerge record, entry by entry.
//! Every workspace, strip and window is its own map under its id, holding who
//! made it, so concurrent changes to different entries merge and a page that
//! reconnects with an old desktop writes only the entries it changed itself. A
//! strip holds its window groups, and a group the order of its windows: putting
//! a window into a strip writes inside that strip, so taking back the change that
//! made the strip leaves it standing while it holds what another person put in
//! it: an undo keeps a map somebody wrote inside since. Who may change which
//! entry is [`crate::ownership`]'s rule.
//! ```text
//! workspaces: { <id>: { order: f64, workspace: <map, no strips>, arrangement? } }
//! strips:     { <id>: { workspace: <id>, order: f64, name, author?,
//!                       groups: { <id>: { order: f64, place?, float?, windows: { <id>: f64 } } } } }
//! windows:    { <id>: { tab: <map>, author?, record?, opened_by? } }
//! ```
//! A workspace, a window and a place are maps of their own fields, not one blob of
//! text each, so two people editing different fields of the same one keep both
//! changes. What only one device decides (focus, widths, maximization) is never
//! written, and [`read`] tolerates what concurrency leaves behind, so nothing
//! anyone opened vanishes.

use std::collections::{BTreeMap, HashMap, HashSet};
use tilo_automerge::json;
use tilo_sort::CompactSort as _;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::arrangement::{Arrangement, ArrangementKind, CanvasPlace, FloatPlace};
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
const ARRANGEMENT: &str = "arrangement";
const PLACE: &str = "place";
const FLOAT: &str = "float";
/// The agent run that opened a window ([`crate::ownership::OpenedBy`]). No
/// writer deletes it: it goes with its window's entry.
const OPENED_BY: &str = "opened_by";

/// A document read now or as of some heads.
struct At<'a, D: ReadDoc> {
    doc: &'a D,
    heads: Option<&'a [ChangeHash]>,
}

impl<D: ReadDoc> At<'_, D> {
    fn get(&self, obj: &ObjId, key: impl Into<automerge::Prop>) -> Option<(Value<'_>, ObjId)> {
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

    /// The JSON value at `key`, whole: a field that is a map of its own.
    fn json(&self, obj: &ObjId, key: &str) -> Option<serde_json::Value> {
        let (value, id) = self.get(obj, key)?;
        match (&value, self.heads) {
            (Value::Object(_), Some(heads)) => json::read_at(self.doc, &id, heads, None).ok(),
            (Value::Object(_), None) => json::read_value(self.doc, &value, &id).ok(),
            (Value::Scalar(scalar), _) => json::scalar_json(scalar).ok(),
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
    /// entry's map, in whichever container holds it first, and whether it was
    /// written blind: held by a container concurrent with the winning one. Only a document
    /// that did not hold the workspace's record yet makes a container of its
    /// own, so what such a container holds was written without seeing the
    /// workspace's desktop.
    fn placed_entries(&self, key: &str) -> BTreeMap<String, (ObjId, bool)> {
        let mut entries = BTreeMap::new();
        for (index, container) in self.containers(key).into_iter().enumerate() {
            for id in self.keys(&container) {
                if entries.contains_key(&id) {
                    continue;
                }
                if let Some((Value::Object(ObjType::Map), entry)) = self.get(&container, &id) {
                    entries.insert(id, (entry, index > 0));
                }
            }
        }
        entries
    }

    /// What each of `parents` holds in its map `key`, in turn: a group a strip holds is a map of
    /// its own, a window a group holds is its order. An id two writers placed at once is listed
    /// under each parent that holds it.
    fn placed(&self, parents: impl IntoIterator<Item = (u64, ObjId)>, key: &str) -> Vec<Placed> {
        let mut out = Vec::new();
        for (parent, obj) in parents {
            let Some((Value::Object(ObjType::Map), within)) = self.get(&obj, key) else {
                continue;
            };
            for name in self.keys(&within) {
                let Ok(id) = name.parse::<u64>() else {
                    continue;
                };
                let (order, obj) = match self.get(&within, name.as_str()) {
                    Some((Value::Object(ObjType::Map), entry)) => {
                        (self.number(&entry, ORDER), Some(entry))
                    }
                    Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
                        ScalarValue::F64(order) => (Some(*order), None),
                        _ => (None, None),
                    },
                    _ => continue,
                };
                out.push(Placed {
                    parent,
                    id,
                    within: within.clone(),
                    obj,
                    order,
                });
            }
        }
        out
    }
}

/// An entry kept inside its parent's map ([`At::placed`]).
struct Placed {
    parent: u64,
    id: u64,
    /// The map it is kept in.
    within: ObjId,
    /// Its own map, for a group.
    obj: Option<ObjId>,
    order: Option<f64>,
}

// Through `serde_json::Value`, the input `DesktopTree::from_json` reads: one
// copy of the workspace and window deserializers serves both. Not generic, so
// every reader of the record shares these instead of instantiating its own.
fn workspace_from(value: serde_json::Value) -> Option<Workspace> {
    serde_json::from_value(value).ok()
}

fn tab_from(value: serde_json::Value) -> Option<Tab> {
    serde_json::from_value(value).ok()
}

fn sorted<K: Ord, T>(mut items: Vec<(f64, K, T)>) -> Vec<T> {
    items.compact_sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    items.into_iter().map(|(_, _, item)| item).collect()
}

/// What a record holds, read entry by entry and not yet put together.
#[derive(Default)]
struct Parts {
    authors: BTreeMap<u64, String>,
    opened_by: BTreeMap<TabId, crate::ownership::OpenedBy>,
    records: Vec<(TabId, String)>,
    workspaces: Vec<Workspace>,
    strips: HashMap<Option<WorkspaceId>, Vec<(f64, u64, Strip)>>,
    /// Groups by the strip that holds them.
    groups: HashMap<u64, Vec<(f64, u64, GroupId)>>,
    /// Windows by the group that holds them.
    windows: HashMap<u64, Vec<(f64, u64, Tab)>>,
    /// Windows no group holds: one moved into a group that was closed at the same time.
    loose: Vec<(u64, Tab)>,
    /// Where groups stand on their workspace's canvas.
    places: HashMap<GroupId, CanvasPlace>,
    /// Which of those groups float, and where.
    floats: HashMap<GroupId, FloatPlace>,
    /// Strips written blind ([`At::placed_entries`]).
    blind: HashSet<u64>,
}

/// `items` with every blind one that shares its name with another folded
/// into that one: the first not blind of that name, else the first of it.
/// Returns what is kept, and where each folded id went.
fn fold<T>(
    items: Vec<T>,
    blind: &HashSet<u64>,
    key: impl Fn(&T) -> (u64, &str),
) -> (Vec<T>, HashMap<u64, u64>) {
    let into = |name: &str| {
        let named = || items.iter().map(&key).filter(move |(_, n)| *n == name);
        named()
            .find(|(id, _)| !blind.contains(id))
            .or_else(|| named().next())
            .map(|(id, _)| id)
    };
    let mut folded = HashMap::new();
    let mut keep = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let (id, name) = key(item);
        match into(name) {
            Some(target) if target != id && blind.contains(&id) => {
                folded.insert(id, target);
            }
            _ => keep.push(index),
        }
    }
    let kept = items
        .into_iter()
        .enumerate()
        .filter(|(index, _)| keep.contains(index))
        .map(|(_, item)| item)
        .collect();
    (kept, folded)
}

impl Parts {
    fn of(held: Levels<HeldEntry>) -> Self {
        let mut parts = Self::default();
        let mut workspaces = Vec::new();
        for (id, held) in held.workspaces {
            let entry = held.entry;
            let Some(mut workspace) = entry.json(WORKSPACE).and_then(workspace_from) else {
                continue;
            };
            workspace.strips.clear();
            workspace.arrangement = Arrangement {
                kind: entry
                    .text(ARRANGEMENT)
                    .map_or(ArrangementKind::Strips, ArrangementKind::parse),
                places: BTreeMap::new(),
                floats: BTreeMap::new(),
            };
            workspaces.push((entry.order(), id, workspace));
        }
        parts.workspaces = sorted(workspaces);
        for (id, held) in held.strips {
            if held.blind {
                parts.blind.insert(id);
            }
            let entry = held.entry;
            if let Some(author) = entry.text(AUTHOR) {
                parts.authors.insert(id, author.to_owned());
            }
            let strip = Strip::new(StripId(id), entry.text(NAME).unwrap_or_default());
            let workspace = entry.text(WORKSPACE).and_then(WorkspaceId::parse);
            let strips = parts.strips.entry(workspace).or_default();
            strips.push((entry.order(), id, strip));
        }
        for (id, held) in held.groups {
            let entry = held.entry;
            let place = entry
                .json(PLACE)
                .and_then(|value| serde_json::from_value(value).ok());
            if let Some(place) = place.map(|place: CanvasPlace| place.window()) {
                // A window's place is a window's size, whoever wrote a smaller one.
                parts.places.insert(id, place);
            }
            let float = entry
                .json(FLOAT)
                .and_then(|value| serde_json::from_value(value).ok());
            if let Some(float) = float {
                parts.floats.insert(id, float);
            }
            if let Some((_, strip)) = entry.parent {
                parts
                    .groups
                    .entry(strip)
                    .or_default()
                    .push((entry.order(), id, id));
            }
        }
        for (id, held) in held.windows {
            let entry = held.entry;
            let Some(mut tab) = entry.json(TAB).and_then(tab_from) else {
                continue;
            };
            tab.id = id;
            if let Some(author) = entry.text(AUTHOR) {
                parts.authors.insert(id, author.to_owned());
            }
            if let Some(record) = entry.text(RECORD) {
                parts.records.push((id, record.to_owned()));
            }
            let opened = entry
                .json(OPENED_BY)
                .and_then(|value| serde_json::from_value(value).ok());
            if let Some(opened) = opened {
                parts.opened_by.insert(id, opened);
            }
            match entry.parent {
                Some((_, group)) => {
                    let windows = parts.windows.entry(group).or_default();
                    windows.push((entry.order(), id, tab));
                }
                None => parts.loose.push((id, tab)),
            }
        }
        parts
    }

    /// Put the parts together, adopting what lost its parent meanwhile
    /// into the first workspace or strip.
    ///
    /// A page that publishes before the workspace's desktop reached it writes
    /// its own fresh "Main" strip into a map of its own, concurrent with the
    /// workspace's. A strip written blind like that folds into the one of its
    /// name the workspace has, and what it held moves there.
    fn assemble(mut self) -> Option<SharedDesktop> {
        let first_workspace = self.workspaces.first()?.id();
        let held: Vec<WorkspaceId> = self.workspaces.iter().map(Workspace::id).collect();
        let mut strips: HashMap<WorkspaceId, Vec<(f64, u64, Strip)>> = HashMap::new();
        for (parent, list) in self.strips.drain() {
            let parent = parent
                .filter(|id| held.contains(id))
                .unwrap_or(first_workspace);
            strips.entry(parent).or_default().extend(list);
        }
        let mut folded_strips = HashMap::new();
        for workspace in &mut self.workspaces {
            let list = sorted(strips.remove(&workspace.id()).unwrap_or_default());
            let (list, folded) = fold(list, &self.blind, |s| (s.id.0, s.name.as_str()));
            workspace.strips = list;
            folded_strips.extend(folded);
        }
        let mut groups: HashMap<u64, Vec<(f64, u64, GroupId)>> = HashMap::new();
        for (parent, list) in self.groups.drain() {
            let parent = folded_strips.get(&parent).copied().unwrap_or(parent);
            groups.entry(parent).or_default().extend(list);
        }
        let first_strip = self
            .workspaces
            .iter()
            .find_map(|w| w.strips.first())
            .map(|s| s.id.0);
        if let Some(first_strip) = first_strip {
            for (id, tab) in std::mem::take(&mut self.loose) {
                // A window no group holds stays, in a group of its own.
                groups
                    .entry(first_strip)
                    .or_default()
                    .push((f64::MAX, id, id));
                self.windows.entry(id).or_default().push((0.0, id, tab));
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
        for workspace in &mut self.workspaces {
            let groups: Vec<GroupId> = workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .map(|group| group.id)
                .collect();
            for group in groups {
                if let Some(place) = self.places.get(&group) {
                    workspace.arrangement.places.insert(group, *place);
                }
                if let Some(float) = self.floats.get(&group) {
                    workspace.arrangement.floats.insert(group, *float);
                }
            }
        }
        self.records.compact_sort();
        Some(SharedDesktop {
            tree: DesktopTree {
                workspaces: self.workspaces,
                active: first_workspace,
                ..DesktopTree::default()
            },
            records: self.records,
            authors: self.authors,
            opened_by: self.opened_by,
        })
    }
}

/// The shared desktop `doc` holds, now (`None`) or as of `heads`; `None`
/// when it holds no workspace there.
#[must_use]
pub fn read(doc: &AutoCommit, heads: Option<&[ChangeHash]>) -> Option<SharedDesktop> {
    let mut desktop = read_as_kept(doc, heads)?;
    // A person's own window is never the workspace's ([`crate::personal`]): one an older build
    // placed here shows nobody, and the next write takes it out.
    crate::personal::leave_out(&mut desktop);
    Some(desktop)
}

/// The desk `doc` holds, every window as kept, a person's own windows included: for the copy of a
/// desk no workspace shares, which is that person's alone. A workspace's desk is read with
/// [`read`].
#[must_use]
pub fn read_as_kept(doc: &AutoCommit, heads: Option<&[ChangeHash]>) -> Option<SharedDesktop> {
    let mut desktop = if crate::desktop_doc::holds(doc) {
        desk_of(doc, heads)?
    } else {
        Parts::of(held(doc, heads)).assemble()?
    };
    for (_, record) in &mut desktop.records {
        if record == LEGACY_PRIMARY_RECORD {
            // The same record as the primary's route, which is empty: one
            // window has one record, so its route is what the desk holds.
            record.clear();
        }
    }
    Some(desktop)
}

/// The custody key of a package's primary record, as a page that predates
/// routes wrote it where a window's route belongs: not a route, so a desk
/// holding it holds the primary under a spelling that names nothing.
const LEGACY_PRIMARY_RECORD: &str = "@tilo:package-primary";

/// The desk the desk's own document ([`crate::desktop_doc`]) holds at
/// `heads`, as [`read`] gives a record: read with the document's own reader,
/// so a record that became the desk's document loses nothing of the desk.
///
/// A workspace's record is the record this module writes until a desk kept as one
/// document is published over it, and every change after that builds on the
/// document.
fn desk_of(doc: &AutoCommit, heads: Option<&[ChangeHash]>) -> Option<SharedDesktop> {
    let desk = match heads {
        Some(heads) => as_of(doc, heads),
        None => doc.clone(),
    };
    let mut tree = DesktopTree::default();
    crate::desktop_doc::rebuild(&desk, &mut tree);
    if tree.workspaces.is_empty() {
        return None;
    }
    Some(SharedDesktop {
        tree,
        records: crate::desktop_doc::records(&desk),
        authors: crate::desktop_doc::window_authors(&desk),
        opened_by: crate::desktop_doc::opened_by(&desk),
    })
}

/// `doc`'s changes up to `heads` as a document of their own, so that the
/// desk's own reader can read the desk as it was then.
fn as_of(doc: &AutoCommit, heads: &[ChangeHash]) -> AutoCommit {
    let mut wanted: Vec<automerge::Change> = Vec::new();
    let mut pending: Vec<ChangeHash> = heads.to_vec();
    let mut seen: HashSet<ChangeHash> = HashSet::new();
    while let Some(hash) = pending.pop() {
        if !seen.insert(hash) {
            continue;
        }
        let Some(change) = doc.get_change_by_hash(&hash) else {
            continue;
        };
        pending.extend(change.deps().iter().copied());
        wanted.push(change);
    }
    let mut out = AutoCommit::new();
    let _ = tilo_automerge::ApplyIncoming::apply_incoming(&mut out, wanted);
    out
}

/// What a field of an entry holds: text of its own, or a value made of
/// fields of its own (a workspace, a window, a place on the canvas).
#[derive(Clone, Debug, PartialEq)]
enum Field {
    Text(String),
    Json(serde_json::Value),
}

/// One entry as written: its parent and order, and its fields.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    parent: Option<(&'static str, u64)>,
    order: f64,
    fields: Vec<(&'static str, Field)>,
}

impl Entry {
    /// Its order, where one is kept.
    fn order(&self) -> f64 {
        if self.order.is_nan() { 0.0 } else { self.order }
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.fields.iter().find_map(|(field, value)| match value {
            Field::Text(text) if *field == key => Some(text.as_str()),
            _ => None,
        })
    }

    fn json(&self, key: &str) -> Option<serde_json::Value> {
        self.fields.iter().find_map(|(field, value)| match value {
            Field::Json(value) if *field == key => Some(value.clone()),
            _ => None,
        })
    }
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
fn orders<K: Eq + std::hash::Hash>(ids: &[K], held: &HashMap<K, f64>) -> Vec<f64> {
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
    workspaces: BTreeMap<WorkspaceId, T>,
    strips: BTreeMap<u64, T>,
    groups: BTreeMap<u64, T>,
    windows: BTreeMap<u64, T>,
    /// Second copies of a group or a window placed twice at once, by the map each is in.
    twice: Vec<(ObjId, u64)>,
}

impl<T> Default for Levels<T> {
    fn default() -> Self {
        Self {
            workspaces: BTreeMap::new(),
            strips: BTreeMap::new(),
            groups: BTreeMap::new(),
            windows: BTreeMap::new(),
            twice: Vec::new(),
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
    // A person's own windows stay on their desk ([`crate::personal`]), and one the record holds
    // from an older build is taken out.
    let mut shared = desktop.clone();
    crate::personal::leave_out(&mut shared);
    let desktop = &shared;
    // A record that became the desk's own document
    // ([`crate::desktop_doc`]) keeps the same desk, so a writer that knows
    // only this shape — a build from before the desk was one document —
    // writes into it rather than beside it.
    if crate::desktop_doc::holds(doc) {
        let wrote = crate::desktop_doc::write_shared(
            doc,
            &desktop.tree,
            &desktop.records,
            &desktop.authors,
        );
        return Ok(crate::desktop_doc::note_opened_by(doc, &desktop.opened_by) || wrote);
    }
    let held = held(doc, None);
    let want = wanted(desktop, &held);
    let ops = doc.pending_ops();
    // What leaves a group or a strip that stays is taken out first, while every map still stands.
    for (within, id) in &held.twice {
        doc.delete(within, id.to_string())?;
    }
    let stays = |held: &HeldEntry, want: &BTreeMap<u64, Entry>, id: &u64| {
        want.get(id)
            .is_some_and(|want| want.parent == held.entry.parent)
    };
    let groups_staying: HashSet<u64> = held
        .groups
        .iter()
        .filter(|(id, group)| stays(group, &want.groups, id))
        .map(|(id, _)| *id)
        .collect();
    for (id, group) in &held.groups {
        let strip_stays = group
            .entry
            .parent
            .is_some_and(|(_, strip)| want.strips.contains_key(&strip));
        if !groups_staying.contains(id) && strip_stays {
            delete_within(doc, group, *id)?;
        }
    }
    for (id, window) in &held.windows {
        let group_stays = window
            .entry
            .parent
            .is_some_and(|(_, group)| groups_staying.contains(&group));
        if group_stays && !stays(window, &want.windows, id) {
            delete_within(doc, window, *id)?;
        }
    }
    reconcile(
        doc,
        WORKSPACES,
        &held.workspaces,
        &want.workspaces,
        Keeps::Both,
    )?;
    let strips = reconcile(doc, STRIPS, &held.strips, &want.strips, Keeps::Both)?;
    let mut group_windows = HashMap::new();
    for (id, entry) in &want.groups {
        let Some(strip) = entry.parent.and_then(|(_, strip)| strips.get(&strip)) else {
            continue;
        };
        let held_here = held.groups.get(id).filter(|_| groups_staying.contains(id));
        let obj = if let Some(group) = held_here {
            group.obj.clone()
        } else {
            let within = map_at(doc, strip, GROUPS)?;
            let obj = doc.put_object(&within, id.to_string(), ObjType::Map)?;
            doc.put_object(&obj, WINDOWS, ObjType::Map)?;
            obj
        };
        update(
            doc,
            &obj,
            held_here.map(|group| &group.entry),
            entry,
            Keeps::Order,
        )?;
        group_windows.insert(*id, map_at(doc, &obj, WINDOWS)?);
    }
    reconcile(doc, WINDOWS, &held.windows, &want.windows, Keeps::Neither)?;
    for (id, entry) in &want.windows {
        let Some(within) = entry
            .parent
            .and_then(|(_, group)| group_windows.get(&group))
        else {
            continue;
        };
        let placed = held.windows.get(id).is_some_and(|window| {
            window.entry.parent == entry.parent
                && window.entry.order.to_bits() == entry.order.to_bits()
                && entry
                    .parent
                    .is_some_and(|(_, group)| groups_staying.contains(&group))
        });
        if !placed {
            doc.put(within, id.to_string(), entry.order)?;
        }
    }
    Ok(doc.pending_ops() > ops)
}

/// Take `held`, kept as `id` in its parent's map, out of that map.
fn delete_within(
    doc: &mut AutoCommit,
    held: &HeldEntry,
    id: u64,
) -> Result<(), automerge::AutomergeError> {
    match &held.within {
        Some(within) => doc.delete(within, id.to_string()),
        None => Ok(()),
    }
}

/// The map at `obj[key]`, stood up where `obj` holds none.
fn map_at(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: &'static str,
) -> Result<ObjId, automerge::AutomergeError> {
    match doc.get(obj, key)? {
        Some((Value::Object(ObjType::Map), map)) => Ok(map),
        _ => doc.put_object(obj, key, ObjType::Map),
    }
}

/// Stand the containers every entry of the record is kept in, empty, where `doc` holds none yet: a
/// change that takes back the entries a later change wrote leaves them standing.
///
/// # Errors
/// The document refused a write.
pub fn found(doc: &mut AutoCommit) -> Result<(), automerge::AutomergeError> {
    for key in [WORKSPACES, STRIPS, WINDOWS] {
        let held = At {
            doc: &*doc,
            heads: None,
        }
        .containers(key);
        if held.is_empty() {
            doc.put_object(ROOT, key, ObjType::Map)?;
        }
    }
    Ok(())
}

/// `value` written as the field `key` of `obj`: the document is the only
/// thing that can refuse a value made of JSON.
fn write_json(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: &'static str,
    value: &serde_json::Value,
) -> Result<(), automerge::AutomergeError> {
    json::write(doc, obj, key, value).map_err(|error| match error {
        json::JsonError::Automerge(error) => error,
        shape @ json::JsonError::Shape { .. } => automerge::AutomergeError::InvalidValueType {
            expected: "a JSON value".to_owned(),
            unexpected: shape.to_string(),
        },
    })
}

/// The held orders of the entries in `level` under `parent`.
fn held_orders<K: Copy + Eq + std::hash::Hash>(
    level: &BTreeMap<K, HeldEntry>,
    parent: Option<u64>,
) -> HashMap<K, f64> {
    level
        .iter()
        .filter(|(_, held)| held.entry.parent.map(|(_, id)| id) == parent)
        .filter(|(_, held)| held.entry.order.is_finite())
        .map(|(id, held)| (*id, held.entry.order))
        .collect()
}

/// Where a window group stands on the canvas and how it floats, as its entry keeps them.
fn group_fields(arrangement: &Arrangement, group: u64) -> Vec<(&'static str, Field)> {
    let place = arrangement
        .place_of(group)
        .and_then(|place| serde_json::to_value(place).ok())
        .map(|place| (PLACE, Field::Json(place)));
    let float = arrangement
        .float_of(group)
        .and_then(|float| serde_json::to_value(float).ok())
        .map(|float| (FLOAT, Field::Json(float)));
    place.into_iter().chain(float).collect()
}

/// The entries `desktop` is written as, its orders kept from `held`'s.
fn wanted(desktop: &SharedDesktop, held: &Levels<HeldEntry>) -> Levels<Entry> {
    let records: HashMap<u64, &String> = desktop.records.iter().map(|(t, r)| (*t, r)).collect();
    let author = |id: u64| {
        desktop
            .authors
            .get(&id)
            .map(|author| (AUTHOR, Field::Text(author.clone())))
    };
    let mut want = Levels::default();
    let workspaces = &desktop.tree.workspaces;
    let ids: Vec<WorkspaceId> = workspaces.iter().map(Workspace::id).collect();
    let workspace_orders = orders(&ids, &held_orders(&held.workspaces, None));
    for (workspace, order) in workspaces.iter().zip(workspace_orders) {
        let mut bare = workspace.clone();
        bare.strips.clear();
        bare.arrangement = Arrangement::default();
        let bare = serde_json::to_value(&bare).unwrap_or_default();
        let mut fields = vec![(WORKSPACE, Field::Json(bare))];
        if workspace.arrangement.kind != ArrangementKind::Strips {
            fields.push((
                ARRANGEMENT,
                Field::Text(workspace.arrangement.kind.as_str().to_owned()),
            ));
        }
        want.workspaces.insert(
            workspace.id(),
            Entry {
                parent: None,
                order,
                fields,
            },
        );
        let ids: Vec<u64> = workspace.strips.iter().map(|s| s.id.0).collect();
        let strip_orders = orders(&ids, &held_orders(&held.strips, None));
        for (strip, order) in workspace.strips.iter().zip(strip_orders) {
            let mut fields = vec![
                (WORKSPACE, Field::Text(workspace.id().to_string())),
                (NAME, Field::Text(strip.name.clone())),
            ];
            fields.extend(author(strip.id.0));
            want.strips.insert(
                strip.id.0,
                Entry {
                    parent: None,
                    order,
                    fields,
                },
            );
            let ids: Vec<u64> = strip.window_groups.iter().map(|g| g.id).collect();
            let group_orders = orders(&ids, &held_orders(&held.groups, Some(strip.id.0)));
            for (group, order) in strip.window_groups.iter().zip(group_orders) {
                want.groups.insert(
                    group.id,
                    Entry {
                        parent: Some((STRIP, strip.id.0)),
                        order,
                        fields: group_fields(&workspace.arrangement, group.id),
                    },
                );
                let ids: Vec<u64> = group.tabs.iter().map(|t| t.id).collect();
                let tab_orders = orders(&ids, &held_orders(&held.windows, Some(group.id)));
                for (tab, order) in group.tabs.iter().zip(tab_orders) {
                    let written = serde_json::to_value(tab).unwrap_or_default();
                    let mut fields = vec![(TAB, Field::Json(written))];
                    fields.extend(author(tab.id));
                    if let Some(record) = records.get(&tab.id) {
                        fields.push((RECORD, Field::Text((*record).clone())));
                    }
                    if let Some(opened) = desktop
                        .opened_by
                        .get(&tab.id)
                        .and_then(|opened| serde_json::to_value(opened).ok())
                    {
                        fields.push((OPENED_BY, Field::Json(opened)));
                    }
                    let parent = Some((GROUP, group.id));
                    want.windows.insert(
                        tab.id,
                        Entry {
                            parent,
                            order,
                            fields,
                        },
                    );
                }
            }
        }
    }
    want
}

/// An entry a document holds: its map, the map it is kept in when that is its parent's, and what
/// it says.
struct HeldEntry {
    obj: ObjId,
    within: Option<ObjId>,
    /// Written without seeing the workspace's record ([`At::placed_entries`]).
    blind: bool,
    entry: Entry,
}

/// What a field of an entry holds, as the record keeps it.
#[derive(Clone, Copy)]
enum Shape {
    Text,
    Json,
}
use Shape::{Json, Text};

/// Each level's ids are its own kind: a workspace's id, a strip's or a window's number.
fn keyed<K: std::str::FromStr + Ord>(entries: Vec<(String, HeldEntry)>) -> BTreeMap<K, HeldEntry> {
    entries
        .into_iter()
        .filter_map(|(id, held)| Some((id.parse().ok()?, held)))
        .collect()
}

/// What `doc` holds of each kind of entry, by id, now (`None`) or as of `heads`.
fn held(doc: &impl ReadDoc, heads: Option<&[ChangeHash]>) -> Levels<HeldEntry> {
    let at = At { doc, heads };
    let fields_of = |obj: &ObjId, fields: &[(&'static str, Shape)]| {
        fields
            .iter()
            .filter_map(|(field, shape)| {
                let value = match shape {
                    Shape::Text => Field::Text(at.text(obj, field)?),
                    Shape::Json => Field::Json(at.json(obj, field)?),
                };
                Some((*field, value))
            })
            .collect()
    };
    let level = |key: &str, fields: &[(&'static str, Shape)]| {
        at.placed_entries(key)
            .into_iter()
            .map(|(id, (obj, blind))| {
                let entry = Entry {
                    parent: None,
                    order: at.number(&obj, ORDER).unwrap_or(f64::NAN),
                    fields: fields_of(&obj, fields),
                };
                let held = HeldEntry {
                    obj,
                    within: None,
                    blind,
                    entry,
                };
                (id, held)
            })
            .collect::<Vec<_>>()
    };
    let mut held = Levels {
        workspaces: keyed(level(WORKSPACES, &[(WORKSPACE, Json), (ARRANGEMENT, Text)])),
        strips: keyed(level(
            STRIPS,
            &[(WORKSPACE, Text), (NAME, Text), (AUTHOR, Text)],
        )),
        windows: keyed(level(
            WINDOWS,
            &[
                (TAB, Json),
                (AUTHOR, Text),
                (RECORD, Text),
                (OPENED_BY, Json),
            ],
        )),
        ..Levels::default()
    };
    let strips = held
        .strips
        .iter()
        .map(|(id, strip)| (*id, strip.obj.clone()));
    for group in at.placed(strips, GROUPS) {
        let Some(obj) = group.obj else { continue };
        if held.groups.contains_key(&group.id) {
            held.twice.push((group.within, group.id));
            continue;
        }
        let entry = Entry {
            parent: Some((STRIP, group.parent)),
            order: group.order.unwrap_or(f64::NAN),
            fields: fields_of(&obj, &[(PLACE, Json), (FLOAT, Json)]),
        };
        let held_group = HeldEntry {
            obj,
            within: Some(group.within),
            blind: false,
            entry,
        };
        held.groups.insert(group.id, held_group);
    }
    let groups = held
        .groups
        .iter()
        .map(|(id, group)| (*id, group.obj.clone()));
    for window in at.placed(groups, WINDOWS) {
        match held.windows.get_mut(&window.id) {
            Some(held) if held.within.is_none() => {
                held.entry.parent = Some((GROUP, window.parent));
                held.entry.order = window.order.unwrap_or(f64::NAN);
                held.within = Some(window.within);
            }
            _ => held.twice.push((window.within, window.id)),
        }
    }
    held
}

/// What of where an entry is its own map keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keeps {
    /// Its parent and its order.
    Both,
    Order,
    /// A window: the group that holds it keeps both.
    Neither,
}

/// Make the entries at `ROOT[key]` exactly `want`, writing only what differs. Returns the map of
/// each.
fn reconcile<K: Copy + Ord + ToString>(
    doc: &mut AutoCommit,
    key: &str,
    held: &BTreeMap<K, HeldEntry>,
    want: &BTreeMap<K, Entry>,
    keeps: Keeps,
) -> Result<BTreeMap<K, ObjId>, automerge::AutomergeError> {
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
            }
        }
    }
    let container = match containers.first() {
        Some(container) => container.clone(),
        None if want.is_empty() => return Ok(BTreeMap::new()),
        None => doc.put_object(ROOT, key, ObjType::Map)?,
    };
    let mut objs = BTreeMap::new();
    for (id, entry) in want {
        let before = held.get(id).map(|existing| &existing.entry);
        let obj = if let Some(existing) = held.get(id) {
            existing.obj.clone()
        } else {
            let obj = doc.put_object(&container, id.to_string(), ObjType::Map)?;
            if key == STRIPS {
                // Every group the strip will hold goes into this one map, however many people
                // place one at once.
                doc.put_object(&obj, GROUPS, ObjType::Map)?;
            }
            obj
        };
        update(doc, &obj, before, entry, keeps)?;
        objs.insert(*id, obj);
    }
    Ok(objs)
}

/// Write what of `entry` differs from what `obj` holds (`before`).
fn update(
    doc: &mut AutoCommit,
    obj: &ObjId,
    before: Option<&Entry>,
    entry: &Entry,
    keeps: Keeps,
) -> Result<(), automerge::AutomergeError> {
    if before == Some(entry) {
        return Ok(());
    }
    if keeps == Keeps::Both
        && let Some((field, parent)) = entry.parent
        && before.and_then(|b| b.parent) != Some((field, parent))
    {
        doc.put(obj, field, parent.to_string())?;
    }
    if keeps != Keeps::Neither && before.is_none_or(|b| b.order.to_bits() != entry.order.to_bits())
    {
        doc.put(obj, ORDER, entry.order)?;
    }
    for (field, value) in &entry.fields {
        let same = before.is_some_and(|b| b.fields.iter().any(|(f, v)| f == field && v == value));
        if !same {
            match value {
                Field::Text(text) => doc.put(obj, *field, text.as_str())?,
                Field::Json(value) => write_json(doc, obj, field, value)?,
            }
        }
    }
    for (field, _) in before.map(|b| b.fields.as_slice()).unwrap_or_default() {
        // Who opened a window goes with the window: a writer that does
        // not say it knows nothing of it.
        if *field != OPENED_BY && !entry.fields.iter().any(|(f, _)| f == field) {
            doc.delete(obj, *field)?;
        }
    }
    Ok(())
}

/// The message a change of the record is committed with: an edit of the
/// shared desktop, by `person` when one made it. It names its action, as
/// every change a device signs must, and who made it, as the desktop's
/// history tells it ([`change_author`]).
#[must_use]
pub fn change_message(person: Option<&str>) -> String {
    let mut context = tilo_actions::ActionContext::for_resource(
        crate::ownership::DESKTOP_RECORD,
        crate::ownership::DESKTOP_KIND,
        "Desktop",
    );
    if let Some(person) = person.filter(|person| !person.is_empty()) {
        context = context.with_invoker(tilo_actions::ActionInvoker::person(person));
    }
    context
        .change_message()
        .unwrap_or_else(|| tilo_actions::FrameworkChange::Native.change_message())
}

/// `person`'s change of the record as one step of theirs: [`change_message`],
/// with the gesture it is, which also orders it among their steps of other
/// records.
#[must_use]
pub fn step_message(person: &str) -> String {
    tilo_actions::ActionContext {
        gesture: Some(tilo_actions::ActionId::new()),
        ..tilo_actions::ActionContext::for_resource(
            crate::ownership::DESKTOP_RECORD,
            crate::ownership::DESKTOP_KIND,
            "Desktop",
        )
        .with_invoker(tilo_actions::ActionInvoker::person(person))
    }
    .change_message()
    .unwrap_or_else(|| tilo_actions::FrameworkChange::Native.change_message())
}

/// `person`'s change of the record that is nobody's step: what it shows, or the desk it was
/// founded on.
#[must_use]
pub fn housekeeping_message(person: &str) -> String {
    tilo_actions::ActionContext::default()
        .with_invoker(tilo_actions::ActionInvoker::person(person))
        .with_change(tilo_actions::ChangeKind::Housekeeping)
        .change_message()
        .unwrap_or_else(|| change_message(Some(person)))
}

/// Who made a change of the record, as its message ([`change_message`])
/// names them.
#[must_use]
pub fn change_author(message: Option<&str>) -> Option<String> {
    tilo_actions::ActionContext::from_change_message(message?)?
        .invoker
        .map(|invoker| invoker.key())
        .filter(|actor| !actor.is_empty())
}

/// Commit what `doc` holds as one change, by `author` at `time` (seconds since the epoch), so the
/// desk's history says who did it and when. An `action` names what they did, as the change's own
/// entry; without one the change is the author's edit of the desk.
pub fn commit_change(
    doc: &mut AutoCommit,
    author: &str,
    time: i64,
    action: Option<&tilo_actions::ActionContext>,
) {
    let message = action
        .and_then(tilo_actions::ActionContext::change_message)
        .unwrap_or_else(|| change_message(Some(author)));
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(message)
            .with_time(time),
    );
}

pub mod device;
pub mod guard;

#[cfg(test)]
mod tests;
