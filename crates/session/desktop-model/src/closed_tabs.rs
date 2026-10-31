//! Recently closed windows, so a close can be undone.
//!
//! A close is the one thing a person does to a desktop that leaves nothing to
//! look at: the window and its place are gone. Each workspace therefore
//! remembers its last [`CLOSED_TABS_PER_WORKSPACE`] closes, with where the
//! window's group stood in its strip, the window's place in that group, the
//! group's width and whether the group went with it, so
//! [`Workspace::reopen_closed`] puts the newest one back exactly where it was.
//!
//! This is this device's own history, never the workspace's state: it is not
//! written to the workspace's document, not projected into the room and not
//! sent to a peer. A window reopens in this device's layout, with this
//! device's launch of the app, which is where the person closed it; reopening
//! it on another device would put a window where nobody is looking.

use std::collections::VecDeque;

use crate::ids::{GroupId, StripId, TabId, WorkspaceId};
use crate::layout::{Tab, WindowGroup};
use crate::tree::DesktopTree;
use crate::width::WindowWidth;
use crate::workspace::Workspace;

/// How many closes each workspace remembers.
pub const CLOSED_TABS_PER_WORKSPACE: usize = 25;

/// One close that can be undone: the window, and where it stood.
#[derive(Clone, Debug)]
pub struct ClosedTab {
    /// The strip the window's group was in, and the group's place in it.
    pub strip: StripId,
    pub group: GroupId,
    pub group_index: usize,
    /// The window's place in its group.
    pub tab_index: usize,
    /// The width the group had.
    pub group_width: WindowWidth,
    /// Whether the close took the group with it, so a reopen makes it again.
    pub whole_group: bool,
    /// The window's content, in the form that opens again
    /// ([`Tab::relaunchable`]).
    pub tab: Tab,
}

/// Where a reopen put a window back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reopened {
    pub strip: StripId,
    pub group: GroupId,
    pub tab: TabId,
}

/// Why a window left its group: a person closed it, or it only moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leaving {
    /// A person closed the window: the close is remembered, so it can be
    /// undone.
    Closed,
    /// The window moves elsewhere in the desktop, or leaves it as a whole
    /// workspace or strip goes: nothing was closed, and nothing is
    /// remembered.
    Moved,
}

/// A workspace's closes that can still be undone, oldest first.
#[derive(Clone, Debug, Default)]
pub struct ClosedTabs {
    entries: VecDeque<ClosedTab>,
}

impl ClosedTabs {
    /// Remember `entry`, forgetting the oldest beyond
    /// [`CLOSED_TABS_PER_WORKSPACE`].
    pub fn push(&mut self, entry: ClosedTab) {
        if self.entries.len() == CLOSED_TABS_PER_WORKSPACE {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// The newest close, taken off the stack: a close is undone once, never
    /// twice.
    pub fn pop(&mut self) -> Option<ClosedTab> {
        self.entries.pop_back()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Workspace {
    /// Remember the window at `strip`, `group` and `tab` (indices) as closed,
    /// so [`Self::reopen_closed`] puts it back. Called by the removal path
    /// every close goes through, so whatever closed a window — its ✕, a
    /// chord, a menu row, a window closed whole — the close can be undone.
    /// Returns whether there was such a window.
    pub fn note_close(&mut self, strip: usize, group: usize, tab: usize) -> bool {
        let Some((strip_id, holding)) = self
            .strips
            .get(strip)
            .and_then(|at| at.window_groups.get(group).map(|holding| (at.id, holding)))
        else {
            return false;
        };
        let Some(closing) = holding.tabs.get(tab) else {
            return false;
        };
        let entry = ClosedTab {
            strip: strip_id,
            group: holding.id,
            group_index: group,
            tab_index: tab,
            group_width: holding.width,
            whole_group: holding.tabs.len() == 1,
            tab: closing.relaunchable(),
        };
        self.closed_tabs.push(entry);
        true
    }

    /// The closes this workspace remembers, oldest first.
    #[must_use]
    pub fn closed_tabs(&self) -> &ClosedTabs {
        &self.closed_tabs
    }

    /// Hold `from`'s closes in place of this workspace's: how a workspace
    /// rebuilt from the room's layout keeps the closes this device made in
    /// it, which the room never carried.
    pub fn keep_closed_tabs(&mut self, from: &Self) {
        self.closed_tabs = from.closed_tabs.clone();
    }

    /// Put the newest close back where it was and return where it landed:
    /// into its group at its old index when that group is still there, else
    /// into the group made again at its old place in its strip — or in the
    /// strip this workspace shows (`focused_strip`) when that strip is gone
    /// too, or its first strip when it shows none.
    ///
    /// The newest close is taken either way: a window whose workspace holds
    /// no strip any more has nowhere to go back to, and is not offered
    /// again. A remembered window that is open again already — a peer's tree
    /// or a document brought it back — has had its close undone, and is
    /// skipped rather than reopened as a second window under one identity.
    pub fn reopen_closed(&mut self, focused_strip: Option<StripId>) -> Option<Reopened> {
        loop {
            let entry = self.closed_tabs.pop()?;
            let tab = entry.tab.id;
            if self.holds(tab) {
                continue;
            }
            let strip_id = if let Some((strip, group)) = self.group_position(entry.group) {
                let id = self.strips.get(strip)?.id;
                let holding = self.strips.get_mut(strip)?.window_groups.get_mut(group)?;
                let at = entry.tab_index.min(holding.tabs.len());
                holding.tabs.insert(at, entry.tab);
                holding.activate_tab(tab);
                id
            } else {
                let strip = self
                    .strips
                    .iter()
                    .position(|strip| strip.id == entry.strip)
                    .or_else(|| {
                        focused_strip.and_then(|focused| {
                            self.strips.iter().position(|strip| strip.id == focused)
                        })
                    })
                    .unwrap_or(0);
                let at = entry
                    .group_index
                    .min(self.strips.get(strip)?.window_groups.len());
                let holding = self.strips.get_mut(strip)?;
                let id = holding.id;
                holding.window_groups.insert(
                    at,
                    WindowGroup {
                        id: entry.group,
                        active_tab: tab,
                        tabs: vec![entry.tab],
                        width: entry.group_width,
                        maximized: false,
                        focus_history: Vec::new(),
                    },
                );
                id
            };
            return Some(Reopened {
                strip: strip_id,
                group: entry.group,
                tab,
            });
        }
    }

    /// Whether this workspace holds window `tab`.
    fn holds(&self, tab: TabId) -> bool {
        self.strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .any(|group| group.tabs.iter().any(|held| held.id == tab))
    }

    /// The strip and group indices of group `group`, wherever it stands.
    fn group_position(&self, group: GroupId) -> Option<(usize, usize)> {
        self.strips.iter().enumerate().find_map(|(strip, at)| {
            at.window_groups
                .iter()
                .position(|held| held.id == group)
                .map(|holding| (strip, holding))
        })
    }
}

impl DesktopTree {
    /// Put the window closed most recently in `workspace` back where it was
    /// and focus it, as the person's reopen does: the workspace shows the
    /// strip it lands in, and that strip its group. `None`, changing
    /// nothing, when the workspace is gone or remembers no close.
    pub fn reopen_closed(&mut self, workspace: WorkspaceId) -> Option<Reopened> {
        let focused = self.active_strip.get(&workspace).copied();
        let reopened = self
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)?
            .reopen_closed(focused)?;
        self.active = workspace;
        self.active_strip.insert(workspace, reopened.strip);
        self.set_active_group(reopened.strip, reopened.group);
        Some(reopened)
    }
}
