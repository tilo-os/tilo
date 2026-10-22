//! Maximized windows stepped back for another window the person was taken
//! to, and given their place back when that window goes away.

use crate::shell::state::{GroupId, StripId, TabId, WorkspaceTree};

/// A window the person maximized, stepped back to its width so that the
/// window `for_tab` could come forward over it. Maximized is the person's
/// choice, and stepping back only lends the space: when `for_tab` goes away
/// and the person is back on `group`, it is maximized again, so a window
/// that opens and closes (a dialog, `wl-copy`'s one-pixel window, a viewer
/// opened from a terminal) never leaves the person's window restored. Held
/// in [`WorkspaceTree::stepped_back`]; the person maximizing or restoring
/// `group` themselves settles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SteppedBack {
    pub group: GroupId,
    pub for_tab: TabId,
}

/// `group` steps back from maximized for `for_tab`, which the person is
/// being taken to, until that window goes away ([`SteppedBack`]).
pub fn step_back_for(tree: &mut WorkspaceTree, group: GroupId, for_tab: TabId) {
    let Some(stepped) = tree
        .desktop
        .workspaces
        .iter_mut()
        .flat_map(|workspace| workspace.strips.iter_mut())
        .flat_map(|strip| strip.window_groups.iter_mut())
        .find(|candidate| candidate.id == group && candidate.maximized)
    else {
        return;
    };
    stepped.maximized = false;
    tree.stepped_back.retain(|held| held.group != group);
    tree.stepped_back.push(SteppedBack { group, for_tab });
}

/// The window `gone` has gone away: every window that stepped back for it
/// and that the person is back on (the focused window of its strip, with
/// nothing else maximized there) is maximized again. One the person has
/// since left keeps its width: covering where they went would move them.
pub(crate) fn give_back_place(tree: &mut WorkspaceTree, gone: TabId) {
    let lent: Vec<GroupId> = tree
        .stepped_back
        .iter()
        .filter(|held| held.for_tab == gone)
        .map(|held| held.group)
        .collect();
    if lent.is_empty() {
        return;
    }
    tree.stepped_back.retain(|held| held.for_tab != gone);
    let focused: Vec<(StripId, GroupId)> = tree
        .desktop
        .active_group
        .iter()
        .map(|(strip, group)| (*strip, *group))
        .collect();
    let mut changed = false;
    for strip in tree
        .desktop
        .workspaces
        .iter_mut()
        .flat_map(|workspace| workspace.strips.iter_mut())
    {
        let Some(&(_, front)) = focused.iter().find(|(id, _)| *id == strip.id) else {
            continue;
        };
        if !lent.contains(&front) || strip.window_groups.iter().any(|g| g.maximized) {
            continue;
        }
        if let Some(group) = strip.window_groups.iter_mut().find(|g| g.id == front) {
            group.maximized = true;
            changed = true;
        }
    }
    if changed {
        crate::shell::tab_binding::mark_tree_changed(tree);
        tree.queue_save();
    }
}

/// The person maximized or restored `group` themselves: nothing lent of it
/// is given back any more.
pub fn settle_stepped_back(tree: &mut WorkspaceTree, group: GroupId) {
    tree.stepped_back.retain(|held| held.group != group);
}
