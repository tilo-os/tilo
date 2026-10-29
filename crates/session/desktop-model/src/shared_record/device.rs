//! One device's copy of a workspace's layout record.
//!
//! The record ([`super`]) is the workspace's layout, shared by every device
//! that shows the workspace. Each device applies its own changes to the record
//! when they happen, online or offline, and folds in what the others did once
//! their changes arrive. Nothing waits for another device, and nothing is sent
//! to the device a window runs on to be applied there.
//!
//! Merge rules, which automerge gives per field of an entry:
//!
//! - Where an entry is (its parent and its order among its siblings) is one
//!   last-writer-wins value per field. Concurrent moves of one window leave it
//!   in the one place every device picks the same way; moves of different
//!   windows all land.
//! - A close deletes the entry, so a move made concurrently, which writes
//!   only inside it, does not bring it back.
//! - A window moved into a group closed meanwhile keeps a group of its own
//!   ([`super::read`]).
//!
//! What the record never holds is decided by each device alone, and folding
//! keeps it by id: group widths, maximization, the active tab, focus history,
//! which of this device's displays shows a group, and how this device shows a
//! window. A window that runs only on one device is written as a
//! [`TileContent::HostedWindow`] naming that device; the host shows its
//! live surface where the record places it, every other device a stream of it.
//! A device's own way of showing a window (a live surface, a peer's window
//! before a local instance is bound to it) is never written back: a window's
//! content is written only when it changed in what every device sees.

use std::collections::{BTreeMap, HashMap, HashSet};

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::ids::{RoomId, TabId, WorkspaceId};
use crate::layout::{Strip, Tab, TileContent, WindowGroup};
use crate::ownership::SharedDesktop;
use crate::projection::localise_tile;
use crate::tree::DesktopTree;
use crate::workspace::Workspace;

/// What every device sees of a tile: device-local handles and labels left
/// out, and the windows that open as an app on each device alike.
#[derive(PartialEq, Eq)]
enum Seen<'a> {
    /// A window every device opens its own instance of.
    App(String),
    /// A window that runs only on `host`.
    Hosted { app: &'a str, host: &'a str },
    /// A placeholder or stub, as written.
    Other(&'a TileContent),
}

fn seen(tile: &TileContent) -> Seen<'_> {
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. } => Seen::App(app_id.to_lowercase()),
        TileContent::HostedWindow { app_id, host, .. } => Seen::Hosted { app: app_id, host },
        other => Seen::Other(other),
    }
}

/// Whether `a` and `b` are the same window to every device.
fn same_content(a: &Tab, b: &Tab) -> bool {
    a.title == b.title && a.app_title == b.app_title && seen(&a.tile) == seen(&b.tile)
}

/// `tile` as the record holds it: no surface handle, which means nothing on
/// another device, and a peer's window as the app window it is.
fn written(tile: &TileContent, tab_title: &str) -> TileContent {
    match tile {
        TileContent::Foreign { app_id, .. } => TileContent::Foreign {
            app_id: app_id.clone(),
            surface_id: 0,
        },
        TileContent::PeerWindow { app_id, label, .. } => TileContent::LocalApp {
            app_id: app_id.clone(),
            label: if label.is_empty() {
                tab_title.to_owned()
            } else {
                label.clone()
            },
            note: String::new(),
        },
        TileContent::LocalApp { app_id, label, .. } => TileContent::LocalApp {
            app_id: app_id.clone(),
            label: label.clone(),
            note: String::new(),
        },
        other => other.clone(),
    }
}

/// The record's workspace for `room`: the one under the room's own id, else
/// the first.
fn record_workspace(desktop: SharedDesktop, room: RoomId) -> Option<(Workspace, SharedDesktop)> {
    let mut desktop = desktop;
    let id = WorkspaceId::of_room(room);
    let index = desktop
        .tree
        .workspaces
        .iter()
        .position(|w| w.id == id)
        .unwrap_or(0);
    if index >= desktop.tree.workspaces.len() {
        return None;
    }
    let workspace = desktop.tree.workspaces.remove(index);
    Some((workspace, desktop))
}

/// Who writes a device's layout: the person at it, and which ids it mints.
pub struct Writer<'a> {
    /// The person id of whoever sits at this device; what it makes is
    /// theirs.
    pub person: Option<&'a str>,
    /// Whether this device minted `id`: only a strip, group or window it
    /// minted is new to the record when it writes it. Anything else the
    /// record lacks is not this device's to add: its maker adds it, or
    /// someone closed it.
    pub mine: &'a dyn Fn(u64) -> bool,
    /// Whether a strip this device made is kept while it holds no window. A
    /// device's ordinary empty strip stays on it: an empty row arriving on
    /// every other device is noise, not news. One a person made to put
    /// windows in is a destination, and the others see it at once.
    pub retained: &'a dyn Fn(u64) -> bool,
    /// Whether the room laid `id` out before its layout was this record
    /// ([`former_ids`]): what the first device to write the record still
    /// shows of it comes over, whoever made it.
    pub carried: &'a dyn Fn(u64) -> bool,
}

impl Writer<'_> {
    /// Nothing laid out before the record.
    pub const NOTHING_CARRIED: &'static dyn Fn(u64) -> bool = &|_| false;
}

/// Write `workspace`, the layout of `room` as other devices are to see it,
/// into the record `doc`, changing only what differs from what it holds.
///
/// `workspace` is this device's, with its own legacy windows already turned
/// into [`TileContent::HostedWindow`]s naming it. A window whose content is
/// what the record holds to every device keeps the record's value; what this
/// device made is written as made by `writer.person`. Returns whether it
/// changed the record.
///
/// # Errors
/// The document refused a write.
pub fn write(
    doc: &mut AutoCommit,
    room: RoomId,
    workspace: &Workspace,
    writer: &Writer<'_>,
) -> Result<bool, automerge::AutomergeError> {
    let held = super::read(&*doc, None).and_then(|desktop| record_workspace(desktop, room));
    let (held_tabs, mut authors, held_records) = match &held {
        Some((record, desktop)) => (
            tabs_of(record),
            desktop.authors.clone(),
            desktop.records.clone(),
        ),
        None => (HashMap::new(), BTreeMap::new(), Vec::new()),
    };
    let held_ids: HashSet<u64> = held
        .as_ref()
        .map(|(record, _)| ids_of(record))
        .unwrap_or_default();
    // What the room laid out before the record comes over the first time a
    // device writes the record.
    let carried = |id: u64| held.is_none() && (writer.carried)(id);
    let known = |id: u64| held_ids.contains(&id) || carried(id) || (writer.mine)(id);
    // The workspace's own entry is the record's as it stands: written once,
    // by whichever device writes first, and never again from a device's
    // copy, so devices do not take turns rewriting it.
    let mut shared = held.as_ref().map_or_else(
        || Workspace::in_room(WorkspaceId::of_room(room), room, workspace.name.clone()),
        |(record, _)| record.clone(),
    );
    shared.strips = workspace
        .strips
        .iter()
        .filter_map(|strip| {
            let window_groups: Vec<WindowGroup> = strip
                .window_groups
                .iter()
                .filter_map(|group| {
                    let tabs: Vec<Tab> = group
                        .tabs
                        .iter()
                        .filter(|tab| known(tab.id))
                        .map(|tab| match held_tabs.get(&tab.id) {
                            Some(held) if same_content(held, tab) => (*held).clone(),
                            _ => Tab {
                                tile: written(&tab.tile, &tab.title),
                                ..tab.clone()
                            },
                        })
                        .collect();
                    (!tabs.is_empty()).then(|| WindowGroup {
                        tabs,
                        ..group.clone()
                    })
                })
                .collect();
            let id = strip.id.0;
            let kept = held_ids.contains(&id)
                || carried(id)
                || ((writer.mine)(id) && (writer.retained)(id));
            (kept || !window_groups.is_empty()).then(|| Strip {
                id: strip.id,
                name: strip.name.clone(),
                window_groups,
            })
        })
        .collect();
    // How the workspace is arranged is this device's to say like its
    // strips: the kind, and the places of the groups it writes.
    shared.arrangement.kind = workspace.arrangement.kind;
    let written: HashSet<u64> = shared
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .map(|group| group.id)
        .collect();
    shared.arrangement.places = workspace
        .arrangement
        .places
        .iter()
        .filter(|(group, _)| written.contains(group))
        .map(|(group, place)| (*group, *place))
        .collect();
    let live = ids_of(&shared);
    if let Some(person) = writer.person {
        for id in live.iter().filter(|id| (writer.mine)(**id)) {
            authors.entry(*id).or_insert_with(|| person.to_owned());
        }
    }
    authors.retain(|id, _| live.contains(id));
    let records = held_records
        .into_iter()
        .filter(|(tab, _)| live.contains(tab))
        .collect();
    let desktop = SharedDesktop {
        tree: DesktopTree {
            active: shared.id,
            workspaces: vec![shared],
            ..DesktopTree::default()
        },
        records,
        authors,
    };
    super::write(doc, &desktop)
}

/// Take the windows `take` names out of the record `doc`, and the groups and
/// strips `take` names once no window is left in them: what a device made,
/// out of a room that keeps each member's layout on their device. Returns
/// whether it changed the record.
///
/// # Errors
/// The document refused a write.
pub fn withdraw(
    doc: &mut AutoCommit,
    take: &dyn Fn(u64) -> bool,
) -> Result<bool, automerge::AutomergeError> {
    let Some(mut desktop) = super::read(&*doc, None) else {
        return Ok(false);
    };
    for workspace in &mut desktop.tree.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                group.tabs.retain(|tab| !take(tab.id));
            }
            strip.window_groups.retain(|group| !group.tabs.is_empty());
        }
        workspace
            .strips
            .retain(|strip| !strip.window_groups.is_empty() || !take(strip.id.0));
    }
    let live: HashSet<u64> = desktop.tree.workspaces.iter().flat_map(ids_of).collect();
    desktop.authors.retain(|id, _| live.contains(id));
    desktop.records.retain(|(tab, _)| live.contains(tab));
    super::write(doc, &desktop)
}

/// Every strip and window id `workspace` holds.
fn ids_of(workspace: &Workspace) -> HashSet<u64> {
    workspace
        .strips
        .iter()
        .flat_map(|strip| {
            std::iter::once(strip.id.0).chain(strip.window_groups.iter().flat_map(|group| {
                std::iter::once(group.id).chain(group.tabs.iter().map(|tab| tab.id))
            }))
        })
        .collect()
}

/// The strip, group and window ids a room document laid out before the
/// layout was a record of its own: in its `contents` map, and in a copy of
/// the record kept at the room document's root.
#[must_use]
pub fn former_ids(doc: &impl ReadDoc) -> HashSet<u64> {
    let mut ids = legacy_ids(doc);
    if let Some(desktop) = super::read(doc, None) {
        ids.extend(desktop.tree.workspaces.iter().flat_map(ids_of));
    }
    ids
}

/// The strip, group and window ids a room document laid out before the
/// record names under its `contents` map: hex keys, groups under a strip's
/// `groups`, windows under a group's `tabs`, in every concurrent copy.
fn legacy_ids(doc: &impl ReadDoc) -> HashSet<u64> {
    fn maps(doc: &impl ReadDoc, parent: &ObjId, key: &str) -> Vec<ObjId> {
        doc.get_all(parent, key)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(value, id)| matches!(value, Value::Object(ObjType::Map)).then_some(id))
            .collect()
    }
    fn walk(doc: &impl ReadDoc, containers: &[ObjId], depth: usize, out: &mut HashSet<u64>) {
        const CHILDREN: [&str; 2] = ["groups", "tabs"];
        for container in containers {
            for key in doc.keys(container) {
                let Ok(id) = u64::from_str_radix(&key, 16) else {
                    continue;
                };
                out.insert(id);
                if let Some(child) = CHILDREN.get(depth) {
                    for entry in maps(doc, container, &key) {
                        walk(doc, &maps(doc, &entry, child), depth.saturating_add(1), out);
                    }
                }
            }
        }
    }
    let mut out = HashSet::new();
    walk(doc, &maps(doc, &ROOT, "contents"), 0, &mut out);
    out
}

fn tabs_of(workspace: &Workspace) -> HashMap<TabId, &Tab> {
    workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| (tab.id, tab))
        .collect()
}

/// How this device shows `record`, a window as the record holds it, given
/// how it showed it before (`local`). `device` is this device's key.
fn shown(record: &Tab, local: Option<&Tab>, device: &str) -> Tab {
    let tile = match local {
        // The same window to every device: this device keeps its own way of
        // showing it (a live surface, a bound local instance).
        Some(local) if seen(&local.tile) == seen(&record.tile) => local.tile.clone(),
        _ => match &record.tile {
            // A window that runs here, shown here by its live surface.
            TileContent::HostedWindow { host, .. } if host == device => match local {
                Some(Tab {
                    tile: tile @ (TileContent::Foreign { .. } | TileContent::LocalApp { .. }),
                    ..
                }) => tile.clone(),
                _ => record.tile.clone(),
            },
            tile => localise_tile(tile.clone(), local.map(|tab| &tab.tile), &record.title),
        },
    };
    Tab {
        id: record.id,
        title: record.title.clone(),
        tile,
        app_title: record.app_title.clone(),
    }
}

/// What of a device's own layout the record never holds, so folding the
/// record keeps it.
pub struct Kept<'a> {
    /// Whether this device minted `id`: a strip it made that holds no
    /// window was never written ([`Writer::retained`]), and stays.
    pub mine: &'a dyn Fn(u64) -> bool,
    /// Whether `id` is this device's to keep to itself: a window it had
    /// open before it joined the room is not the room's.
    pub private: &'a dyn Fn(u64) -> bool,
}

impl Kept<'_> {
    /// Nothing of this device's is kept apart: a viewer's.
    pub const NOTHING: Kept<'static> = Kept {
        mine: &|_| false,
        private: &|_| false,
    };
}

/// Fold the record `doc`'s layout of `workspace`'s room into `workspace`:
/// strips, groups and windows are where the record says, and what this
/// device decides alone is kept by id. `device` is this device's key; what
/// `kept` names that the record does not hold stays where it is. Returns
/// whether anything changed; `false` with nothing done while the record
/// holds no layout yet.
pub fn fold(doc: &impl ReadDoc, workspace: &mut Workspace, device: &str, kept: &Kept<'_>) -> bool {
    let Some((record, _)) =
        super::read(doc, None).and_then(|desktop| record_workspace(desktop, workspace.room()))
    else {
        return false;
    };
    let local_groups: HashMap<u64, &WindowGroup> = workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .map(|group| (group.id, group))
        .collect();
    let local_tabs = tabs_of(workspace);
    let mut strips: Vec<Strip> = record
        .strips
        .iter()
        .map(|strip| Strip {
            id: strip.id,
            name: strip.name.clone(),
            window_groups: strip
                .window_groups
                .iter()
                .map(|group| {
                    let tabs: Vec<Tab> = group
                        .tabs
                        .iter()
                        .map(|tab| shown(tab, local_tabs.get(&tab.id).copied(), device))
                        .collect();
                    match local_groups.get(&group.id) {
                        Some(local) => {
                            let active_tab = if tabs.iter().any(|tab| tab.id == local.active_tab) {
                                local.active_tab
                            } else {
                                group.active_tab
                            };
                            let focus_history = local
                                .focus_history
                                .iter()
                                .copied()
                                .filter(|id| tabs.iter().any(|tab| tab.id == *id))
                                .collect();
                            WindowGroup {
                                id: group.id,
                                tabs,
                                active_tab,
                                width: local.width,
                                maximized: local.maximized,
                                focus_history,
                            }
                        }
                        None => WindowGroup {
                            tabs,
                            ..group.clone()
                        },
                    }
                })
                .collect(),
        })
        .collect();
    keep_apart(&mut strips, &workspace.strips, &ids_of(&record), kept);
    // The record's arrangement, with the places of what this device keeps
    // apart from it kept where this device has them.
    let mut arrangement = record.arrangement;
    let shown: HashSet<u64> = strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .map(|group| group.id)
        .collect();
    for (group, place) in &workspace.arrangement.places {
        if shown.contains(group) {
            arrangement.places.entry(*group).or_insert(*place);
        }
    }
    let mut changed =
        serde_json::to_value(&workspace.strips).ok() != serde_json::to_value(&strips).ok();
    if changed {
        workspace.strips = strips;
    }
    if workspace.arrangement != arrangement {
        workspace.arrangement = arrangement;
        changed = true;
    }
    changed
}

/// Put back into `strips`, the record's layout as this device shows it, what
/// of `local` the record does not hold and `kept` keeps on this device.
fn keep_apart(strips: &mut Vec<Strip>, local: &[Strip], held: &HashSet<u64>, kept: &Kept<'_>) {
    // In ascending index order, so each lands where it stood (or at the end,
    // when fewer strips precede it now) without moving one before it.
    for (index, strip) in local.iter().enumerate() {
        let private: Vec<WindowGroup> = strip
            .window_groups
            .iter()
            .filter_map(|group| {
                let tabs: Vec<Tab> = group
                    .tabs
                    .iter()
                    .filter(|tab| (kept.private)(tab.id) && !held.contains(&tab.id))
                    .cloned()
                    .collect();
                (!tabs.is_empty()).then(|| WindowGroup {
                    tabs,
                    ..group.clone()
                })
            })
            .collect();
        if strips.iter().any(|shown| shown.id == strip.id) {
            for group in private {
                let shown = strips
                    .iter_mut()
                    .flat_map(|shown| &mut shown.window_groups)
                    .find(|shown| shown.id == group.id);
                match shown {
                    Some(shown) => shown.tabs.extend(group.tabs),
                    None => {
                        if let Some(target) = strips.iter_mut().find(|shown| shown.id == strip.id) {
                            target.window_groups.push(group);
                        }
                    }
                }
            }
        } else if !private.is_empty() || (strip.window_groups.is_empty() && (kept.mine)(strip.id.0))
        {
            strips.insert(
                index.min(strips.len()),
                Strip {
                    window_groups: private,
                    ..strip.clone()
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
