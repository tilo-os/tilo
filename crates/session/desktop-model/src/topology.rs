//! Pure rearrangements of the window topology.
//!
//! Each operation edits the [`DesktopTree`] cache only and reports what it
//! did, so a host can mirror the same change into its replication document
//! and roll both back together if the mirror is refused.

use crate::ids::{GroupId, MintIds, StripId, TabId};
use crate::layout::{Tab, WindowGroup};
use crate::tree::DesktopTree;
use crate::width::WindowWidth;

/// Where a dragged tab lands within a strip: at an exact index in an
/// existing group, or as a new group at an exact seam between groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropTarget {
    ExistingGroup { group_id: GroupId, tab_index: usize },
    GroupGap { gap_index: usize },
}

/// What a tab move did to the tree, for the host's document mirror.
#[derive(Clone, Debug)]
pub struct TabMove {
    /// The group the tab left.
    pub source_group: GroupId,
    /// Whether that group emptied and was removed.
    pub source_group_removed: bool,
    /// The single-tab group a [`DropTarget::GroupGap`] placement creates.
    /// For an [`DropTarget::ExistingGroup`] placement it is only a carrier
    /// for the moved tab.
    pub new_group: WindowGroup,
    /// Where the tab landed; a gap index is clamped to the strip.
    pub target: DropTarget,
}

fn single_tab_group(id: GroupId, tab: Tab) -> WindowGroup {
    WindowGroup {
        id,
        active_tab: tab.id,
        tabs: vec![tab],
        width: WindowWidth::Normal,
        maximized: false,
    }
}

impl DesktopTree {
    /// Move `tab_id` out of `src_strip_id` and into `dest_strip_id` at
    /// `target` — the tree side of a cross-strip tab drop. The source group
    /// is removed if it empties; if the moved tab was its active tab, the
    /// first remaining sibling becomes active.
    ///
    /// `None`, with the tree unchanged, when the strips are the same, the
    /// destination (or the group `target` names) does not exist, or the tab
    /// is not in the source strip. Searches every workspace, since a drop
    /// can land outside the active one.
    pub fn move_tab_across_strips(
        &mut self,
        src_strip_id: StripId,
        dest_strip_id: StripId,
        tab_id: TabId,
        new_group_id: GroupId,
        target: DropTarget,
    ) -> Option<TabMove> {
        if src_strip_id == dest_strip_id {
            // Cross-strip routing never commits back into its source row;
            // that path restores the detached live payload exactly instead.
            return None;
        }
        // Validate the destination and any named group before detaching
        // anything. A stale preview must not make the source tab disappear.
        let valid_destination = self
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .find(|strip| strip.id == dest_strip_id)
            .is_some_and(|strip| match target {
                DropTarget::ExistingGroup { group_id, .. } => {
                    strip.window_groups.iter().any(|group| group.id == group_id)
                }
                DropTarget::GroupGap { .. } => true,
            });
        if !valid_destination {
            return None;
        }
        // Validated above, so the insertion cannot miss; the snapshot keeps
        // "unchanged on `None`" true even if that ever stops holding.
        let before = self.workspaces.clone();
        let (moved, source_group, source_group_removed) = self.detach_tab(src_strip_id, tab_id)?;
        let new_group = single_tab_group(new_group_id, moved.clone());
        let landed = self
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.strips.iter_mut())
            .find(|strip| strip.id == dest_strip_id)
            .and_then(|strip| match target {
                DropTarget::ExistingGroup {
                    group_id,
                    tab_index,
                } => strip
                    .window_groups
                    .iter_mut()
                    .find(|group| group.id == group_id)
                    .map(|group| {
                        let index = tab_index.min(group.tabs.len());
                        group.tabs.insert(index, moved.clone());
                        group.active_tab = moved.id;
                        DropTarget::ExistingGroup {
                            group_id,
                            tab_index: index,
                        }
                    }),
                DropTarget::GroupGap { gap_index } => {
                    let index = gap_index.min(strip.window_groups.len());
                    strip.window_groups.insert(index, new_group.clone());
                    Some(DropTarget::GroupGap { gap_index: index })
                }
            });
        let Some(target) = landed else {
            self.workspaces = before;
            return None;
        };
        Some(TabMove {
            source_group,
            source_group_removed,
            new_group,
            target,
        })
    }

    /// Split `tab_id` out of its group into a new single-tab group at
    /// `insert_at` in the same strip — the tree side of dragging a tab out
    /// into a seam, or "Split Right". The source group's active tab follows
    /// the tab bar's rule: the neighbour that slid into the slot, else the
    /// last tab.
    ///
    /// `None`, with the tree unchanged, when the tab is not in `strip_id`
    /// or is its group's only tab (splitting it would dissolve and
    /// re-create the group one slot over).
    pub fn split_tab_into_new_group(
        &mut self,
        strip_id: StripId,
        tab_id: TabId,
        insert_at: usize,
        ids: &mut impl MintIds,
    ) -> Option<TabMove> {
        let strip = self
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.strips.iter_mut())
            .find(|strip| strip.id == strip_id)?;
        let (group_idx, tab_idx) =
            strip
                .window_groups
                .iter()
                .enumerate()
                .find_map(|(group_idx, group)| {
                    group
                        .tabs
                        .iter()
                        .position(|tab| tab.id == tab_id)
                        .map(|tab_idx| (group_idx, tab_idx))
                })?;
        let group = strip.window_groups.get_mut(group_idx)?;
        if group.tabs.len() < 2 {
            return None;
        }
        let moved = group.tabs.remove(tab_idx);
        if group.active_tab == tab_id {
            group.active_tab = group
                .tabs
                .get(tab_idx)
                .or_else(|| group.tabs.last())
                .map_or(0, |tab| tab.id);
        }
        let source_group = group.id;
        let new_group = single_tab_group(ids.mint_id(), moved);
        let gap_index = insert_at.min(strip.window_groups.len());
        strip.window_groups.insert(gap_index, new_group.clone());
        Some(TabMove {
            source_group,
            source_group_removed: false,
            new_group,
            target: DropTarget::GroupGap { gap_index },
        })
    }

    /// Remove `tab_id` from `strip_id`, promoting a sibling to active and
    /// dropping the group if it empties. Returns the tab, its group, and
    /// whether that group was removed.
    fn detach_tab(&mut self, strip_id: StripId, tab_id: TabId) -> Option<(Tab, GroupId, bool)> {
        let strip = self
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.strips.iter_mut())
            .filter(|strip| strip.id == strip_id)
            .find(|strip| {
                strip
                    .window_groups
                    .iter()
                    .any(|group| group.tabs.iter().any(|t| t.id == tab_id))
            })?;
        let (group_idx, pos) = strip
            .window_groups
            .iter()
            .enumerate()
            .find_map(|(gi, group)| {
                group
                    .tabs
                    .iter()
                    .position(|t| t.id == tab_id)
                    .map(|pos| (gi, pos))
            })?;
        let group = strip.window_groups.get_mut(group_idx)?;
        let source_group = group.id;
        let removed = group.tabs.remove(pos);
        // If the moved tab was active, promote the first remaining sibling
        // so a later render never dereferences a stale id.
        if group.active_tab == tab_id
            && let Some(first) = group.tabs.first()
        {
            group.active_tab = first.id;
        }
        let emptied = group.tabs.is_empty();
        if emptied {
            strip.window_groups.remove(group_idx);
        }
        Some((removed, source_group, emptied))
    }

    /// Move the window group `group_id` to `new_index` within `strip_id`,
    /// as a unit. `new_index` is clamped to the post-removal index space,
    /// so a raw gap index (which may equal the length, "append") lands on
    /// the last slot. Returns the moved group and where it landed; `None`,
    /// with nothing changed, when the order would not change.
    pub fn reorder_group_within_strip(
        &mut self,
        strip_id: StripId,
        group_id: GroupId,
        new_index: usize,
    ) -> Option<(WindowGroup, usize)> {
        let groups = &mut self
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.strips.iter_mut())
            .find(|strip| strip.id == strip_id)?
            .window_groups;
        let current = groups.iter().position(|g| g.id == group_id)?;
        let dest = new_index.min(groups.len().saturating_sub(1));
        if dest == current {
            return None;
        }
        let moved = groups.remove(current);
        groups.insert(dest, moved.clone());
        Some((moved, dest))
    }

    /// Move the tab at `from` to `to` inside `group_id`'s tab list — an
    /// intra-group tab drag. `to` is clamped to the last slot. Returns the
    /// reordered tab list; `None`, with nothing changed, when the order
    /// would not change.
    pub fn reorder_tab_within_group(
        &mut self,
        group_id: GroupId,
        from: usize,
        to: usize,
    ) -> Option<Vec<Tab>> {
        let group = self
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.strips.iter_mut())
            .flat_map(|strip| strip.window_groups.iter_mut())
            .find(|group| group.id == group_id)?;
        if from >= group.tabs.len() {
            return None;
        }
        let dest = to.min(group.tabs.len().saturating_sub(1));
        if dest == from {
            return None;
        }
        let moved = group.tabs.remove(from);
        group.tabs.insert(dest, moved);
        Some(group.tabs.clone())
    }
}
