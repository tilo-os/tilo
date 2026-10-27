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

use std::collections::{BTreeMap, HashMap};

use automerge::{AutoCommit, ReadDoc};

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

/// Write `workspace`, the layout of `room` as other devices are to see it,
/// into the record `doc`, changing only what differs from what it holds.
///
/// `workspace` is this device's, with its own legacy windows already turned
/// into [`TileContent::HostedWindow`]s naming it. A window whose content is
/// what the record holds to every device keeps the record's value; one this
/// device made, or changed, is written by `person` (the person id of whoever
/// sits at this device). Returns whether it changed the record.
///
/// # Errors
/// The document refused a write.
pub fn write(
    doc: &mut AutoCommit,
    room: RoomId,
    workspace: &Workspace,
    person: Option<&str>,
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
    let id = held
        .as_ref()
        .map_or_else(|| WorkspaceId::of_room(room), |(record, _)| record.id);
    let mut shared = Workspace::in_room(id, room, workspace.name.clone());
    if let Some((record, _)) = &held {
        shared.name.clone_from(&record.name);
    }
    shared.strips = workspace
        .strips
        .iter()
        .map(|strip| Strip {
            id: strip.id,
            name: strip.name.clone(),
            window_groups: strip
                .window_groups
                .iter()
                .filter(|group| !group.tabs.is_empty())
                .map(|group| WindowGroup {
                    tabs: group
                        .tabs
                        .iter()
                        .map(|tab| match held_tabs.get(&tab.id) {
                            Some(held) if same_content(held, tab) => (*held).clone(),
                            _ => Tab {
                                tile: written(&tab.tile, &tab.title),
                                ..tab.clone()
                            },
                        })
                        .collect(),
                    ..group.clone()
                })
                .collect(),
        })
        .collect();
    let live: std::collections::HashSet<u64> = shared
        .strips
        .iter()
        .flat_map(|strip| {
            std::iter::once(strip.id.0).chain(
                strip
                    .window_groups
                    .iter()
                    .flat_map(|group| group.tabs.iter().map(|tab| tab.id)),
            )
        })
        .collect();
    if let Some(person) = person {
        for id in &live {
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

/// Fold the record `doc`'s layout of `workspace`'s room into `workspace`:
/// strips, groups and windows are where the record says, and what this
/// device decides alone is kept by id. `device` is this device's key.
/// Returns whether anything changed; `false` with nothing done while the
/// record holds no layout yet.
pub fn fold(doc: &impl ReadDoc, workspace: &mut Workspace, device: &str) -> bool {
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
    let strips: Vec<Strip> = record
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
                            WindowGroup {
                                id: group.id,
                                tabs,
                                active_tab,
                                width: local.width,
                                maximized: local.maximized,
                                focus_history: local.focus_history.clone(),
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
    let changed =
        serde_json::to_value(&workspace.strips).ok() != serde_json::to_value(&strips).ok();
    if changed {
        workspace.strips = strips;
    }
    changed
}

#[cfg(test)]
mod tests;
