//! Session-only experiment: navigate spatial groups, then gather focus.
//! Uses the retained desktop's framing rather than a second window manager.
use super::ShellOverlayHost;
use crate::shell::state::{GroupId, StripId, WorkspaceId};
use ui::widget::Widget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Gather {
    workspace: WorkspaceId,
    strip: StripId,
    group: GroupId,
    display: Option<u32>,
}

impl ShellOverlayHost {
    fn axis_focus(&self) -> Option<Gather> {
        let chrome = self.chrome.as_ref()?.borrow();
        if !chrome.axis_navigation_enabled() || chrome.session_locked() || chrome.is_fullscreen() {
            return None;
        }
        let tree = chrome.tree();
        let workspace = tree.desktop.active_workspace()?;
        let strip = tree
            .desktop
            .active_strip
            .get(&workspace.id)
            .copied()
            .filter(|id| workspace.strips.iter().any(|strip| strip.id == *id))
            .or_else(|| workspace.strips.first().map(|strip| strip.id))?;
        let group = tree.desktop.active_group_for(strip)?;
        Some(Gather {
            workspace: workspace.id,
            strip,
            group,
            display: tree.desktop.outputs.focused_display(),
        })
    }

    /// Opt-in changes the unit of spatial navigation to a group. A group's
    /// active tab stays active; tab cycling remains the separate tab action.
    pub(super) fn step_axis_navigation(&mut self, steps: i32) -> bool {
        let Some(at) = self.axis_focus() else {
            return false;
        };
        let target = self.desktop_surface_mut().and_then(|surface| {
            let row = surface.row(at.strip)?;
            if row.wants_pointer_capture() || row.group_drag_active() {
                return None;
            }
            let ids = row
                .groups
                .iter()
                .filter(|group| !group.is_closing())
                .filter(|group| {
                    !row.is_partitioned() || row.assigned_output_of_group(group.id) == at.display
                })
                .map(|group| group.id)
                .collect::<Vec<_>>();
            let index = ids.iter().position(|id| *id == at.group)?;
            let amount = usize::try_from(steps.unsigned_abs()).ok()?;
            let next = if steps < 0 {
                index.saturating_sub(amount)
            } else {
                index
                    .saturating_add(amount)
                    .min(ids.len().saturating_sub(1))
            };
            ids.get(next).copied()
        });
        let Some(group) = target else { return false };
        let changed = self.focus_group_on_strip(at.strip, group);
        self.axis_gather_pending = self.axis_focus();
        changed
    }

    pub(super) fn request_axis_gather(&mut self) {
        self.axis_gather_pending = self.axis_focus();
    }

    /// A pending gather is bound to local workspace, strip, focus and output.
    /// Input interruption cannot gather a stale window into a different context.
    /// Jump framing cancels any old snap and also works with reduced motion.
    pub(super) fn finish_axis_gather(&mut self) {
        let Some(pending) = self.axis_gather_pending else {
            return;
        };
        if self.axis_focus() != Some(pending) {
            self.axis_gather_pending = None;
            return;
        }
        let Some(surface) = self.desktop_surface_mut() else {
            return;
        };
        let Some(row) = surface.row_mut(pending.strip) else {
            return;
        };
        if row.wants_pointer_capture() || row.group_drag_active() {
            self.axis_gather_pending = None;
            return;
        }
        if !row.group_layout_is_current() || !row.group_is_laid_out(pending.group) {
            return;
        }
        if !row.scroll_group_into_view(pending.group) {
            // Already visible still means stop: a previous snap may target
            // another group even though this focus needs no displacement.
            let offset = pending
                .display
                .and_then(|display| row.output_pan(display))
                .map_or_else(|| row.scroll_x(), ui::widget::Pan::scroll_x);
            row.pan_to(offset);
        }
        surface.keep_clicked_group_in_place(pending.strip, pending.group);
        self.axis_gather_pending = None;
    }
}
