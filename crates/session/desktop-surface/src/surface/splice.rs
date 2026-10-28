//! A changed tree applied to a strip's live row in place.
//!
//! A tree changed elsewhere (a peer's edit arriving from the room, an undo)
//! moves the windows that changed and nothing else: the row keeps its pan,
//! its windows' motion and every untouched window as it is. Removing,
//! inserting and reordering go through the row's own operations, so the
//! remaining windows slide over to close or open a gap and the pan eases
//! back into its legal range, as when the person did it here.

use tilo_desktop_model::{Strip, Tab};
use ui::widget::{GroupRow, TileHost, WindowGroup};

use super::{descriptor, placeholder};

/// Bring `row` to `strip`'s windows. A window still fading out after a
/// close stays until its fade ends. `host_for` gives the content of a tab
/// the row does not show yet. Tabs that left their window were taken out of
/// it beforehand ([`super::DesktopSurface::reconcile`]).
pub(super) fn splice_row(
    row: &mut GroupRow,
    strip: &Strip,
    host_for: &mut dyn FnMut(&Tab) -> TileHost,
) {
    let wanted = |id: u64| strip.window_groups.iter().any(|g| g.id == id);
    let gone: Vec<u64> = row
        .groups
        .iter()
        .filter(|g| !g.is_closing() && !wanted(g.id))
        .map(|g| g.id)
        .collect();
    for id in gone {
        row.remove_group_by_id(id);
    }
    for model in &strip.window_groups {
        if let Some(group) = row.groups.iter_mut().find(|g| g.id == model.id)
            && !group.is_closing()
        {
            update_group(group, model, host_for);
        }
    }
    let order: Vec<u64> = strip.window_groups.iter().map(|g| g.id).collect();
    let shown: Vec<u64> = row
        .groups
        .iter()
        .map(|g| g.id)
        .filter(|&id| wanted(id))
        .collect();
    let in_order = order.iter().filter(|id| shown.contains(id));
    if !in_order.eq(shown.iter()) {
        // Each window right after the one before it in the tree; windows
        // fading out stay where they are.
        let mut cursor = 0usize;
        for &id in &order {
            let Some(at) = row.groups.iter().position(|g| g.id == id) else {
                continue;
            };
            let to = if at < cursor {
                cursor.saturating_sub(1)
            } else {
                cursor
            };
            if at != to {
                row.reorder_group_by_id(id, to);
            }
            cursor = row
                .groups
                .iter()
                .position(|g| g.id == id)
                .map_or(cursor, |at| at.saturating_add(1));
        }
    }
    // New windows open right after the window before them in the tree.
    let mut after: Option<u64> = None;
    for model in &strip.window_groups {
        if !row.groups.iter().any(|g| g.id == model.id) {
            let tabs = model
                .tabs
                .iter()
                .map(|tab| (descriptor(tab), placeholder()))
                .collect();
            let mut built = WindowGroup::new(model.id, tabs, model.active_tab, model.width)
                .with_maximized(model.maximized);
            for tab in &model.tabs {
                if let Some(host) = built.host_for_tab_mut(tab.id) {
                    *host = host_for(tab);
                }
            }
            let index = after
                .and_then(|id| row.groups.iter().position(|g| g.id == id))
                .map_or(0, |at| at.saturating_add(1));
            row.insert_group(built, index);
        }
        after = Some(model.id);
    }
}

/// Bring a shown window to the tree's tabs, order, width and state.
fn update_group(
    group: &mut WindowGroup,
    model: &tilo_desktop_model::WindowGroup,
    host_for: &mut dyn FnMut(&Tab) -> TileHost,
) {
    for (index, tab) in model.tabs.iter().enumerate() {
        match group.tabs().iter().position(|t| t.id == tab.id) {
            Some(from) => {
                group.reorder_tab(from, index);
                group.update_tab_descriptor(descriptor(tab));
            }
            // A tab still fading out after its close keeps its host.
            None if group.host_for_tab_mut(tab.id).is_some() => {}
            None => group.insert_detached_tab(index, descriptor(tab), host_for(tab)),
        }
    }
    group.set_active_tab(model.active_tab);
    // A width the person is dragging, or set here and not yet saved, is
    // newer than the tree's.
    if !(group.edge_resize_in_flight() || group.has_pending_width_change())
        && group.width() != model.width
    {
        group.set_width(model.width);
    }
    if group.maximized != model.maximized {
        group.toggle_maximized();
        // The tree already holds the new state.
        let _ = group.take_pending_maximize_change();
    }
}
