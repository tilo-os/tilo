//! Moving a window out of the strip it is in.
//!
//! A window's bar reports a drag that left the strip. Only one strip is on
//! screen at a time, so "another strip" is the one that way: below the strip
//! when the window was let go under it, above when over it. The window lands
//! at the end of that strip and the desk follows it there, so the person sees
//! it arrive; let go where no strip lies, it stays where it was.

use super::Shared;
use tilo_desktop_model::{DesktopTree, DropTarget, StripId, WorkspaceId};
use ui::widget::GroupRow;

/// The strip a window dragged out of `strip` lands in: the next one below it
/// when `down`, else the one above, appended at its end. `None` when the
/// workspace has no strip that way.
fn strip_that_way(
    tree: &DesktopTree,
    strip: StripId,
    down: bool,
) -> Option<(WorkspaceId, StripId, usize)> {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.strips.iter().any(|s| s.id == strip))?;
    let at = workspace.strips.iter().position(|s| s.id == strip)?;
    let next = if down {
        workspace.strips.get(at.saturating_add(1))
    } else {
        at.checked_sub(1)
            .and_then(|before| workspace.strips.get(before))
    }?;
    Some((workspace.id, next.id, next.window_groups.len()))
}

/// Carry out the drag `row` reports out of `strip`: the window goes to the
/// strip that way, its live content with it, and the desk shows it there.
/// Returns whether the tree changed.
pub(super) fn carry_out(
    shared: &mut Shared,
    strip: StripId,
    row: &mut GroupRow,
    payload: ui::widget::CrossStripDetach,
    down: bool,
) -> bool {
    // Its live content goes back into its own row first: the pass that
    // follows carries it to the window it moved to, rather than building
    // that window's application anew.
    let tab = payload.tab_id;
    row.restore_cross_strip_detach(payload);
    let Some((workspace, dest, gap_index)) = strip_that_way(&shared.tree, strip, down) else {
        return false;
    };
    let group = shared.minter.mint();
    if shared
        .tree
        .move_tab_across_strips(strip, dest, tab, group, DropTarget::GroupGap { gap_index })
        .is_none()
    {
        return false;
    }
    shared.tree.active_strip.insert(workspace, dest);
    shared.tree.set_active_group(dest, group);
    shared.replaced = true;
    true
}
