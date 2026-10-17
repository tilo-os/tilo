//! Recently closed tabs, so a close can be undone.
//!
//! Every user close (the tab `✕`, Super+W, the tab menu, `tab.close`, or
//! closing a whole window group from the palette) records where the tab lived
//! and what it showed. `tab.reopen` (Super+Shift+T by default, and Ctrl+Shift+T where the focused app leaves it unhandled) puts it back at the same group and index.
//!
//! A live client window cannot be revived once asked to exit, so a foreign
//! tile is kept in its durable relaunchable form: the tab comes back as the
//! same app, relaunched and bound into the restored tab, exactly as a session
//! restore does. Hosted apps keep their tab id and therefore their document.

use std::collections::VecDeque;
use std::time::Duration;

use crate::shell::state::{
    GroupId, LocalAppRecord, RoomId, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    WorkspaceId, WorkspaceTree,
};

/// How many closes each workspace remembers.
pub const CLOSED_TABS_PER_WORKSPACE: usize = 25;
/// How long a reversible workspace deletion offers Undo.
pub const UNDO_WINDOW: Duration = Duration::from_secs(8);

/// One tab's content and the local state keyed by its id.
#[derive(Clone, Debug)]
pub struct ClosedTabContent {
    pub tab: Tab,
    pub local_app_record: Option<LocalAppRecord>,
    /// The personal-continuity room this device owned the tab in, so a
    /// reopen withdraws the pending close instead of publishing it.
    pub continuation_room: Option<RoomId>,
}

/// A close that can be undone: one tab, or a whole window group.
#[derive(Clone, Debug)]
pub struct ClosedTab {
    /// Identifies this remembered close.
    pub serial: u64,
    pub workspace: WorkspaceId,
    pub strip: StripId,
    pub group: GroupId,
    /// The group's position in its strip when it was closed.
    pub group_index: usize,
    /// The first tab's position in its group.
    pub tab_index: usize,
    pub group_width: WindowWidth,
    /// Whether the close took the group with it; reopening recreates it.
    pub whole_group: bool,
    pub active_tab: TabId,
    pub tabs: Vec<ClosedTabContent>,
}

impl ClosedTab {
    /// The active title of this remembered close.
    pub fn title(&self) -> &str {
        self.tabs
            .iter()
            .find(|content| content.tab.id == self.active_tab)
            .or_else(|| self.tabs.first())
            .map_or("", |content| content.tab.title.as_str())
    }
}

/// A foreign tile's surface dies with the close; keep what relaunches it.
fn relaunchable(tab: &Tab) -> Tab {
    let tile = match &tab.tile {
        TileContent::Foreign { app_id, .. } => TileContent::LocalApp {
            app_id: app_id.clone(),
            label: tab.title.clone(),
            note: String::new(),
        },
        TileContent::LocalApp { app_id, label, .. } => TileContent::LocalApp {
            app_id: app_id.clone(),
            label: label.clone(),
            note: String::new(),
        },
        other => other.clone(),
    };
    Tab {
        id: tab.id,
        title: tab.title.clone(),
        tile,
    }
}

fn content(tree: &WorkspaceTree, tab: &Tab) -> ClosedTabContent {
    ClosedTabContent {
        tab: relaunchable(tab),
        local_app_record: tree.local_app_records.get(&tab.id).cloned(),
        continuation_room: tree.continuation_owned_tabs.get(&tab.id).copied(),
    }
}

/// Record `tab_id` as it is now, before it is removed. A tab that is its
/// group's only tab is recorded as closing the group.
pub fn capture_tab(tree: &WorkspaceTree, tab_id: TabId) -> Option<ClosedTab> {
    for workspace in &tree.desktop.workspaces {
        for strip in &workspace.strips {
            for (group_index, group) in strip.window_groups.iter().enumerate() {
                let Some(tab_index) = group.tabs.iter().position(|tab| tab.id == tab_id) else {
                    continue;
                };
                let tab = group.tabs.get(tab_index)?;
                return Some(ClosedTab {
                    serial: 0,
                    workspace: workspace.id,
                    strip: strip.id,
                    group: group.id,
                    group_index,
                    tab_index,
                    group_width: group.width,
                    whole_group: group.tabs.len() == 1,
                    active_tab: tab_id,
                    tabs: vec![content(tree, tab)],
                });
            }
        }
    }
    None
}

/// Record a whole window group before it is closed.
pub fn capture_group(tree: &WorkspaceTree, group_id: GroupId) -> Option<ClosedTab> {
    for workspace in &tree.desktop.workspaces {
        for strip in &workspace.strips {
            let Some(group_index) = strip
                .window_groups
                .iter()
                .position(|group| group.id == group_id)
            else {
                continue;
            };
            let group = strip.window_groups.get(group_index)?;
            return Some(ClosedTab {
                serial: 0,
                workspace: workspace.id,
                strip: strip.id,
                group: group.id,
                group_index,
                tab_index: 0,
                group_width: group.width,
                whole_group: true,
                active_tab: group.active_tab,
                tabs: group.tabs.iter().map(|tab| content(tree, tab)).collect(),
            });
        }
    }
    None
}

/// The closes that can still be undone, newest last.
#[derive(Debug, Default)]
pub struct ClosedTabStack {
    entries: VecDeque<ClosedTab>,
    next_serial: u64,
}

impl ClosedTabStack {
    /// Remember a close, forgetting that workspace's oldest beyond
    /// [`CLOSED_TABS_PER_WORKSPACE`]. Returns the entry's serial.
    pub fn push(&mut self, mut entry: ClosedTab) -> u64 {
        self.next_serial = self.next_serial.saturating_add(1);
        entry.serial = self.next_serial;
        let workspace = entry.workspace;
        self.entries.push_back(entry);
        while self.len_for(workspace) > CLOSED_TABS_PER_WORKSPACE {
            if let Some(oldest) = self
                .entries
                .iter()
                .position(|candidate| candidate.workspace == workspace)
            {
                self.entries.remove(oldest);
            }
        }
        self.next_serial
    }

    pub fn len_for(&self, workspace: WorkspaceId) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.workspace == workspace)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Take the newest close in `workspace`. Closes from workspaces that no
    /// longer exist are dropped along the way; they have nowhere to go.
    pub fn pop_for(&mut self, tree: &WorkspaceTree, workspace: WorkspaceId) -> Option<ClosedTab> {
        self.entries.retain(|entry| {
            tree.desktop
                .workspaces
                .iter()
                .any(|candidate| candidate.id == entry.workspace)
        });
        let newest = self
            .entries
            .iter()
            .rposition(|entry| entry.workspace == workspace)?;
        self.entries.remove(newest)
    }

    /// Take one exact remembered close.
    pub fn take(&mut self, serial: u64) -> Option<ClosedTab> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.serial == serial)?;
        self.entries.remove(index)
    }
}

/// Put a closed tab (or group) back where it was and focus it. Returns the
/// focused tab, or `None` when its workspace no longer exists.
///
/// The tab goes back into its group at its old index when the group is still
/// there; otherwise the group is recreated at its old position in the strip
/// (or the workspace's focused strip, if that strip is gone too).
pub fn restore(tree: &mut WorkspaceTree, entry: ClosedTab) -> Option<TabId> {
    let workspace_index = tree
        .desktop
        .workspaces
        .iter()
        .position(|workspace| workspace.id == entry.workspace)?;
    let focused_strip = tree.desktop.active_strip.get(&entry.workspace).copied();
    let workspace = tree.desktop.workspaces.get_mut(workspace_index)?;
    let existing_group = (!entry.whole_group)
        .then(|| {
            workspace.strips.iter().find_map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .position(|group| group.id == entry.group)
                    .map(|index| (strip.id, index))
            })
        })
        .flatten();
    let mut tabs = Vec::with_capacity(entry.tabs.len());
    let mut local_state = Vec::with_capacity(entry.tabs.len());
    for content in entry.tabs {
        local_state.push((
            content.tab.id,
            content.local_app_record,
            content.continuation_room,
        ));
        tabs.push(content.tab);
    }
    let (strip_id, group_id) = if let Some((strip_id, group_index)) = existing_group {
        let strip = workspace
            .strips
            .iter_mut()
            .find(|strip| strip.id == strip_id)?;
        let group = strip.window_groups.get_mut(group_index)?;
        let at = entry.tab_index.min(group.tabs.len());
        for (offset, tab) in tabs.into_iter().enumerate() {
            group
                .tabs
                .insert(at.saturating_add(offset).min(group.tabs.len()), tab);
        }
        group.activate_tab(entry.active_tab);
        (strip_id, group.id)
    } else {
        if workspace.strips.is_empty() {
            return None;
        }
        let strip_index = workspace
            .strips
            .iter()
            .position(|strip| strip.id == entry.strip)
            .or_else(|| {
                focused_strip
                    .and_then(|focused| workspace.strips.iter().position(|s| s.id == focused))
            })
            .unwrap_or(0);
        let strip = workspace.strips.get_mut(strip_index)?;
        let at = entry.group_index.min(strip.window_groups.len());
        strip.window_groups.insert(
            at,
            WindowGroup {
                id: entry.group,
                tabs,
                active_tab: entry.active_tab,
                width: entry.group_width,
                maximized: false,
                focus_history: Vec::new(),
            },
        );
        (strip.id, entry.group)
    };
    for (id, record, continuation_room) in local_state {
        if let Some(record) = record {
            tree.local_app_records.insert(id, record);
        }
        if let Some(room) = continuation_room {
            tree.continuation_closed_tabs.remove(&id);
            tree.continuation_owned_tabs.insert(id, room);
        }
    }
    tree.desktop.active = entry.workspace;
    tree.desktop.active_strip.insert(entry.workspace, strip_id);
    tree.desktop.set_active_group(strip_id, group_id);
    tree.seed_doc_from_cache();
    tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
    Some(entry.active_tab)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::FreshId as _;

    fn tab(id: TabId, title: &str) -> Tab {
        Tab {
            id,
            title: title.into(),
            tile: TileContent::Foreign {
                app_id: "org.example.Editor".into(),
                surface_id: id.saturating_add(900),
            },
        }
    }

    fn tree() -> WorkspaceTree {
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.desktop.workspaces[0].strips[0].window_groups = vec![WindowGroup {
            id: 5,
            tabs: vec![tab(1, "One"), tab(2, "Two")],
            active_tab: 2,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        tree
    }

    #[test]
    fn a_foreign_tab_is_remembered_as_its_relaunchable_app() {
        let tree = tree();
        let entry = capture_tab(&tree, 2).unwrap();
        assert!(!entry.whole_group, "its group keeps another tab");
        assert_eq!((entry.group, entry.tab_index), (5, 1));
        assert!(matches!(
            &entry.tabs[0].tab.tile,
            TileContent::LocalApp { app_id, note, .. }
                if app_id == "org.example.Editor" && note.is_empty()
        ));
    }

    #[test]
    fn each_workspace_remembers_a_bounded_number_of_closes() {
        let tree = tree();
        let workspace = tree.desktop.workspaces[0].id;
        let other = WorkspaceId::new();
        let mut stack = ClosedTabStack::default();
        let mut elsewhere = capture_tab(&tree, 1).unwrap();
        elsewhere.workspace = other;
        stack.push(elsewhere);
        for _ in 0..CLOSED_TABS_PER_WORKSPACE + 5 {
            stack.push(capture_tab(&tree, 2).unwrap());
        }
        assert_eq!(stack.len_for(workspace), CLOSED_TABS_PER_WORKSPACE);
        assert_eq!(stack.len_for(other), 1, "another workspace keeps its own");
    }

    #[test]
    fn closes_from_a_vanished_workspace_are_skipped() {
        let mut tree = tree();
        let workspace = tree.desktop.workspaces[0].id;
        let mut stack = ClosedTabStack::default();
        let mut gone = capture_tab(&tree, 1).unwrap();
        gone.workspace = WorkspaceId::new();
        let gone_serial = stack.push(gone.clone());
        assert!(restore(&mut tree, gone).is_none(), "nowhere to put it back");
        assert!(stack.pop_for(&tree, workspace).is_none());
        assert!(stack.take(gone_serial).is_none(), "pruned by the pop");
        assert!(stack.is_empty());
    }
}
