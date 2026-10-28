//! Where a carried tab is let go of, resolved from the live tab bar.
use super::{Point, ShellDriver};

impl ShellDriver {
    /// Move the cursor onto group `group_idx`'s tab bar where a release
    /// drops a carried tab into that group's tab list, after its
    /// `after_tab`-th pill (or first, when it has no `after_tab`). Resolving
    /// from the live bar keeps a cross-group tab drag valid at any viewport,
    /// where a baked coordinate would land in a gap and split off a window.
    pub fn move_cursor_to_group_tab_bar(
        &mut self,
        group_idx: usize,
        after_tab: Option<usize>,
    ) -> bool {
        let strip_id = {
            let state = self.chrome();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .and_then(|workspace| workspace.strips.first())
                .map(|strip| strip.id)
        };
        let Some(strip_id) = strip_id else {
            return false;
        };
        let point = {
            let Some(group) = self.host_mut().group_at_for_tests(strip_id, group_idx) else {
                return false;
            };
            let bar = group.bar_rect();
            let y = f64::midpoint(bar.y0, bar.y1);
            let x = match after_tab {
                Some(index) => group.tab_rects().get(index).map_or(bar.x1, |pill| pill.x1),
                None => bar.x0,
            };
            // Inside the bar, just past the pill the tab lands after.
            Point::new(f64::min(x + 2.0, bar.x1 - 1.0).max(bar.x0), y)
        };
        self.move_cursor(point);
        true
    }
}
