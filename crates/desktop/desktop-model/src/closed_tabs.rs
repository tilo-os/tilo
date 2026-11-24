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
//! written to the workspace's document, not projected into the workspace and not
//! sent to a peer. A window reopens in this device's layout, with this
//! device's launch of the app, which is where the person closed it; reopening
//! it on another device would put a window where nobody is looking.

use std::collections::VecDeque;

use crate::apps::{app_ids_match, canonical_app_id};
use crate::ids::{GroupId, StripId, TabId, WorkspaceId};
use crate::layout::{Tab, TileContent, WindowGroup};
use crate::tree::Layout;
use crate::tree::Workspace;
use crate::width::WindowWidth;

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
    /// The monitor this device showed the window's group on, when it had
    /// several: a group made again goes back to it.
    pub display: Option<String>,
    /// Whether the window went away by itself ([`Leaving::WindowWent`]) rather
    /// than the person closing it.
    pub window_went: bool,
    /// The launch that opens the window again on what it showed, when the one
    /// it came from is known: an app that ended leaves it
    /// ([`Workspace::hold_ended`]). `None` asks the tile's app for its window.
    pub launch: Option<String>,
}

/// Where a reopen put a window back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reopened {
    pub strip: StripId,
    pub group: GroupId,
    pub tab: TabId,
    /// The monitor the group made again goes back to, when the close
    /// remembered one; `None` when the window went into a group still there.
    pub display: Option<String>,
    /// The launch that opens it again on what it showed, when the close knew it
    /// ([`ClosedTab::launch`]).
    pub launch: Option<String>,
}

/// Why a window left its group: a person closed it, it went away by itself,
/// or it only moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leaving {
    /// A person closed the window: the close is remembered, so it can be
    /// undone.
    Closed,
    /// The window went away by itself while its tab was on the shell: its
    /// app closed it, or ended. Remembered as a close the person can undo,
    /// and the place an app that stopped working says so in
    /// ([`Workspace::hold_ended`]).
    WindowWent,
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

    /// Whether the person closed the window `tab`, as far as the closes
    /// remembered go: its app's end after that is theirs, not a window that
    /// stopped working.
    #[must_use]
    pub fn closed_by_person(&self, tab: TabId) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.tab.id == tab && !entry.window_went)
    }

    /// The newest close `pick` takes, taken off the stack: a close is undone
    /// once, never twice.
    pub fn take_newest(&mut self, pick: impl Fn(&ClosedTab) -> bool) -> Option<ClosedTab> {
        let at = self.entries.iter().rposition(pick)?;
        self.entries.remove(at)
    }

    /// The newest close `pick` takes, left on the stack.
    fn newest_mut(&mut self, pick: impl Fn(&ClosedTab) -> bool) -> Option<&mut ClosedTab> {
        self.entries.iter_mut().rev().find(|entry| pick(entry))
    }

    /// Whether a close of the window `tab` is remembered.
    #[must_use]
    pub fn holds(&self, tab: TabId) -> bool {
        self.entries.iter().any(|entry| entry.tab.id == tab)
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

impl Leaving {
    /// Whether the window is gone from the desktop, so its leaving is
    /// remembered as a close.
    #[must_use]
    pub fn closes(self) -> bool {
        self != Self::Moved
    }
}

impl Workspace {
    /// Remember the window at `strip`, `group` and `tab` (indices) as closed
    /// for the reason `leaving` gives,
    /// shown on monitor `display`, so [`Self::reopen_closed`] puts it back,
    /// and end the shares it showed
    /// ([`Self::end_window_shares`]): a window that closes stops being shared.
    /// Called by the removal path every close goes through, so whatever closed
    /// a window — its ✕, a chord, a menu row, a window closed whole — the
    /// close can be undone and the workspace stops showing it.
    /// Returns whether there was such a window.
    pub fn note_close(
        &mut self,
        strip: usize,
        group: usize,
        tab: usize,
        display: Option<String>,
        leaving: Leaving,
    ) -> bool {
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
        let closing_id = closing.id;
        // The card an ended app left closes: the close remembered is the window's own
        // ([`Self::hold_ended`]), now the person's, newest, so a reopen brings back the window on
        // what it showed rather than the card.
        if matches!(closing.tile, TileContent::AppPlaceholder { .. })
            && let Some(mut window) = self
                .closed_tabs
                .take_newest(|entry| entry.tab.id == closing_id)
        {
            window.window_went = leaving == Leaving::WindowWent;
            self.closed_tabs.push(window);
            self.end_window_shares(closing_id);
            return true;
        }
        let entry = ClosedTab {
            strip: strip_id,
            group: holding.id,
            group_index: group,
            tab_index: tab,
            group_width: holding.width,
            whole_group: holding.tabs.len() == 1,
            tab: closing.relaunchable(),
            display,
            window_went: leaving == Leaving::WindowWent,
            launch: None,
        };
        self.closed_tabs.push(entry);
        self.end_window_shares(closing_id);
        true
    }

    /// The closes this workspace remembers, oldest first.
    #[must_use]
    pub fn closed_tabs(&self) -> &ClosedTabs {
        &self.closed_tabs
    }

    /// Hold `from`'s closes in place of this workspace's: how a workspace
    /// rebuilt from the workspace's layout keeps the closes this device made in
    /// it, which the workspace never carried.
    pub fn keep_closed_tabs(&mut self, from: &Self) {
        self.closed_tabs = from.closed_tabs.clone();
    }

    /// Put the newest close back where it was — the newest of `app`'s windows
    /// when an app asks, which reopens only its own — and return where it landed:
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
    pub fn reopen_closed(
        &mut self,
        focused_strip: Option<StripId>,
        app: Option<&str>,
    ) -> Option<Reopened> {
        self.reopen_closed_where(focused_strip, |entry| {
            app.is_none_or(|app| {
                canonical_app_id(&entry.tab.tile).is_some_and(|id| app_ids_match(id.as_str(), app))
            })
        })
    }

    /// Put the window `tab` back where it was, as [`Self::reopen_closed`] does: exactly that
    /// close, whatever closed after it. A window whose app ended stands in its place as the card
    /// that says so: the window takes its place again over the card.
    pub fn reopen_closed_window(&mut self, tab: TabId) -> Option<Reopened> {
        self.reopen_closed_where(None, |entry| entry.tab.id == tab)
    }

    /// Keep the place of the window `tab`, whose app ended by itself, to say so in, and remember
    /// `launch` as what opens it again on what it showed: the window put back where it stood when
    /// it went away by itself (never a window the person closed), or kept where it stands. Its
    /// close stays remembered while the card stands in its place, so the person's reopen of it
    /// brings back the window ([`Self::reopen_closed_window`]). False when it is neither.
    pub fn hold_ended(&mut self, tab: TabId, launch: &str) -> bool {
        if let Some(entry) = self
            .closed_tabs
            .newest_mut(|entry| entry.tab.id == tab && entry.window_went)
        {
            entry.launch = Some(launch.to_owned());
            let entry = entry.clone();
            return self.holds(tab) || self.put_back(entry, None).is_some();
        }
        let Some((strip, group, at)) = self.position_of(tab) else {
            return false;
        };
        let Some(holding) = self
            .strips
            .get(strip)
            .and_then(|at| at.window_groups.get(group).map(|holding| (at.id, holding)))
        else {
            return false;
        };
        let (strip_id, holding) = holding;
        let Some(standing) = holding.tabs.get(at) else {
            return false;
        };
        let entry = ClosedTab {
            strip: strip_id,
            group: holding.id,
            group_index: group,
            tab_index: at,
            group_width: holding.width,
            whole_group: holding.tabs.len() == 1,
            tab: standing.relaunchable(),
            display: None,
            window_went: true,
            launch: Some(launch.to_owned()),
        };
        self.closed_tabs.push(entry);
        true
    }

    fn reopen_closed_where(
        &mut self,
        focused_strip: Option<StripId>,
        pick: impl Fn(&ClosedTab) -> bool,
    ) -> Option<Reopened> {
        loop {
            let entry = self.closed_tabs.take_newest(&pick)?;
            let tab = entry.tab.id;
            if let Some((strip, group, at)) = self.position_of(tab) {
                // The card an ended app left holds the window's place: the window takes it again.
                // Any other window standing under this id was brought back already, by a peer's
                // tree or a document: its close is undone, and it is not opened twice.
                let holding = self.strips.get_mut(strip)?;
                let strip_id = holding.id;
                let held = holding.window_groups.get_mut(group)?;
                let standing = held.tabs.get_mut(at)?;
                if !matches!(standing.tile, TileContent::AppPlaceholder { .. }) {
                    continue;
                }
                *standing = entry.tab;
                held.activate_tab(tab);
                return Some(Reopened {
                    strip: strip_id,
                    group: held.id,
                    tab,
                    display: None,
                    launch: entry.launch,
                });
            }
            return self.put_back(entry, focused_strip);
        }
    }

    /// Put `entry`'s window back where it stood: into its group at its old index when that group
    /// is still there, else into the group made again at its old place in its strip, or in
    /// `focused_strip` when that strip is gone too, or the first strip.
    fn put_back(&mut self, entry: ClosedTab, focused_strip: Option<StripId>) -> Option<Reopened> {
        let tab = entry.tab.id;
        let launch = entry.launch.clone();
        let mut display = None;
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
            display = entry.display;
            id
        };
        Some(Reopened {
            strip: strip_id,
            group: entry.group,
            tab,
            display,
            launch,
        })
    }

    /// The strip, group and tab indices of window `tab`, wherever it stands.
    fn position_of(&self, tab: TabId) -> Option<(usize, usize, usize)> {
        self.strips.iter().enumerate().find_map(|(strip, at)| {
            at.window_groups
                .iter()
                .enumerate()
                .find_map(|(group, holding)| {
                    holding
                        .tabs
                        .iter()
                        .position(|held| held.id == tab)
                        .map(|index| (strip, group, index))
                })
        })
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

impl Layout {
    /// Put the window closed most recently in `workspace` — of `app`, when an
    /// app asks — back where it was and focus it, as the person's reopen does: the workspace shows the
    /// strip it lands in, and that strip its group, on the display the group
    /// was shown on while that display is still here. `None`, changing
    /// nothing, when the workspace is gone or remembers no close.
    pub fn reopen_closed(&mut self, workspace: WorkspaceId, app: Option<&str>) -> Option<Reopened> {
        let focused = self
            .workspaces
            .iter()
            .find(|candidate| candidate.id() == workspace)
            .and_then(|candidate| self.active_strip_for(candidate));
        let reopened = self
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id() == workspace)?
            .reopen_closed(focused, app)?;
        self.active = workspace;
        self.active_strip.insert(workspace, reopened.strip);
        self.set_active_group(reopened.strip, reopened.group);
        if let Some(monitor) = &reopened.display {
            self.outputs.return_home(reopened.group, monitor);
        }
        self.outputs.note_focus(reopened.strip, reopened.group);
        Some(reopened)
    }

    /// Put the window `tab` back where it was and focus it, as [`Self::reopen_closed`] does, in
    /// whichever workspace remembers its close: exactly that window, whatever closed after it.
    /// `None`, changing nothing, when no workspace remembers it.
    pub fn reopen_closed_window(&mut self, tab: TabId) -> Option<Reopened> {
        let workspace = self
            .workspaces
            .iter()
            .find(|candidate| candidate.closed_tabs().holds(tab))?
            .id();
        let reopened = self
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id() == workspace)?
            .reopen_closed_window(tab)?;
        self.active = workspace;
        self.active_strip.insert(workspace, reopened.strip);
        self.set_active_group(reopened.strip, reopened.group);
        if let Some(monitor) = &reopened.display {
            self.outputs.return_home(reopened.group, monitor);
        }
        self.outputs.note_focus(reopened.strip, reopened.group);
        Some(reopened)
    }
}
