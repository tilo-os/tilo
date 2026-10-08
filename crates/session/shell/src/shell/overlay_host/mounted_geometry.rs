//! Content-free geometry from the current mounted desktop, through the
//! existing owner state door. Never reconstruct rectangles from model widths.
use super::ShellOverlayHost;
use ui::widget::Widget as _;
use vello::kurbo::Rect;

const MAX_GROUPS: usize = 128;
const MAX_TABS: usize = 128;
const MAX_OUTPUTS: usize = 16;

fn bounds(rect: Rect) -> [f64; 4] {
    [rect.x0, rect.y0, rect.x1, rect.y1]
}

impl ShellOverlayHost {
    /// Actual logical rectangles; stale/unmounted trees refuse the projection.
    pub fn mounted_window_geometry_json(&self) -> serde_json::Value {
        let unavailable = |reason| serde_json::json!({"available": false, "reason": reason});
        let Some(chrome) = &self.chrome else {
            return unavailable("unmounted_desktop");
        };
        let state = chrome.borrow();
        let tree = state.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return unavailable("no_active_workspace");
        };
        if self.content_built_for_ws != Some(workspace.id.raw())
            || self.workspace_content_layout_dirty
            || !self.rows_cover_active_strips(&tree)
        {
            return unavailable("stale_workspace_layout");
        }
        if self.outputs.is_empty() {
            return unavailable("no_mounted_output");
        }
        if self.outputs.len() > MAX_OUTPUTS
            || workspace
                .strips
                .iter()
                .map(|strip| strip.window_groups.len())
                .sum::<usize>()
                > MAX_GROUPS
            || workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .map(|group| group.tabs.len())
                .sum::<usize>()
                > MAX_TABS
        {
            return unavailable("geometry_limit");
        }
        let mut groups = Vec::new();
        for strip in &workspace.strips {
            let Some(&pointer) = self.group_row_ptrs.get(&strip.id) else {
                return unavailable("unmounted_strip");
            };
            // SAFETY: the pointer belongs to this host's retained content;
            // see `group_row_ptrs`. No mutable host borrow can replace it here.
            let row = unsafe { &*pointer };
            if row.group_ids()
                != strip
                    .window_groups
                    .iter()
                    .map(|group| group.id)
                    .collect::<Vec<_>>()
            {
                return unavailable("stale_group_membership");
            }
            let visible = row.onscreen_group_ids();
            for expected in &strip.window_groups {
                match group_geometry(row, expected, strip.id, visible.contains(&expected.id)) {
                    Ok(group) => groups.push(group),
                    Err(reason) => return unavailable(reason),
                }
            }
        }
        let (workspace_id, room) = (workspace.id, workspace.room);
        drop(tree);
        drop(state);
        let topbars: Vec<_> = self.top_bar_rects().into_iter().map(|(id, rect, primary)|
            serde_json::json!({"output_id": id, "rect": bounds(rect), "primary": primary})).collect();
        let outputs: Vec<_> = self
            .outputs
            .iter()
            .map(
                |output| serde_json::json!({"output_id": output.id, "rect": bounds(output.rect())}),
            )
            .collect();
        serde_json::json!({"available": true, "workspace_id": workspace_id,
            "room": room, "coordinate_space": "logical", "groups": groups,
            "topbars": topbars, "outputs": outputs})
    }
}

fn group_geometry(
    row: &ui::widget::GroupRow,
    expected: &crate::shell::state::WindowGroup,
    strip: crate::shell::state::StripId,
    onscreen: bool,
) -> Result<serde_json::Value, &'static str> {
    let group = row
        .laid_out_group(expected.id)
        .ok_or("stale_group_layout")?;
    if group.width() != expected.width || group.maximized != expected.maximized {
        return Err("stale_group_presentation");
    }
    if group.active_tab_id() != expected.active_tab
        || !group
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .eq(expected.tabs.iter().map(|tab| tab.id))
    {
        return Err("stale_tab_membership");
    }
    let mut tabs = Vec::new();
    for tab in &expected.tabs {
        let content = group.content_rect_for_tab(tab.id).ok_or("unmounted_tab")?;
        tabs.push(serde_json::json!({"tab_id": tab.id, "content_rect": bounds(content)}));
    }
    Ok(serde_json::json!({
        "strip_id": strip, "group_id": group.id,
        "active_tab_id": group.active_tab_id(), "maximized": group.maximized,
        "onscreen": onscreen,
        "group_rect": bounds(group.get_rect()), "bar_rect": bounds(group.bar_rect()),
        "tabs": tabs,
    }))
}
