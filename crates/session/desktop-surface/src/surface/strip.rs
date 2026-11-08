//! The strip's own box: the bands of wallpaper above and below a tiled row,
//! which keep its windows' borders and corners showing, and the gutter that
//! stands before its first window.

use tilo_sort::CompactSort as _;

use super::tile_content::{build_tile, descriptor, placeholder};
use super::{
    GroupId, GroupRow, HashMap, STRIP_EDGE_GUTTER_PX, Style, TabId, TileHost, TileSource,
    WindowGroup, Workspace,
};
use ui::widget::{STRIP_BOTTOM_GUTTER_PX, STRIP_TOP_GAP_PX};

/// The strip's node style over the row's `own` : every strip stands on the whole surface, its
/// displays' lanes placing its windows, so the strips the other displays show lie over it rather
/// than beside it in a share of the height. Tiled windows keep wallpaper above and below them, so
/// the strip reads as its own row under the bar and their borders and corners show; a window
/// filling the strip or taken across the display takes both bands instead.
pub(super) fn strip_style(row: &GroupRow, own: Style) -> Style {
    // A window taken across the display stands on no wallpaper, from the bar's band down to the
    // screen's bottom edge.
    let fills = row.is_fullscreen() || row.groups.iter().any(|group| group.maximized);
    let bottom = if fills { 0.0 } else { STRIP_BOTTOM_GUTTER_PX };
    // The bar keeps the strip a step clear of itself, so a window's own top edge and rounded
    // corners stand on wallpaper.
    let top = if fills { 0.0 } else { STRIP_TOP_GAP_PX };
    let mut style = own;
    style.position = ui::taffy::Position::Absolute;
    let edge = || ui::taffy::LengthPercentageAuto::length(0.0);
    style.inset = ui::taffy::Rect {
        left: edge(),
        top: edge(),
        right: edge(),
        bottom: edge(),
    };
    style.size = ui::taffy::Size {
        width: ui::taffy::Dimension::auto(),
        height: ui::taffy::Dimension::auto(),
    };
    style.margin.top = ui::taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(top));
    style.margin.bottom = ui::taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(bottom));
    style
}

/// A strip's row with the wallpaper gutter before its first window, as the native desktop keeps it.
pub(super) fn strip_row(groups: Vec<WindowGroup>) -> GroupRow {
    GroupRow::new(groups).with_leading_inset(STRIP_EDGE_GUTTER_PX)
}

/// A strip's row with only the focused window's active tab built; the other tabs are placeholders,
/// returned in the order to build them: each group's active tab by distance from the focused
/// window, then the inactive tabs.
pub(super) fn build_row_lazily(
    tiles: &mut dyn TileSource,
    controls: Option<&ui::ControlDomain>,
    workspace: &Workspace,
    strip: &tilo_desktop_model::Strip,
    focused: Option<GroupId>,
) -> (GroupRow, Vec<TabId>) {
    let focus = focused
        .and_then(|id| strip.window_groups.iter().position(|g| g.id == id))
        .unwrap_or(0);
    let mut deferred: Vec<(bool, usize, TabId)> = Vec::new();
    let groups = strip
        .window_groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let tabs = group
                .tabs
                .iter()
                .map(|tab| (descriptor(tab), placeholder()))
                .collect();
            let mut built = WindowGroup::new(group.id, tabs, group.active_tab, group.width)
                .with_maximized(group.maximized);
            for tab in &group.tabs {
                let active = tab.id == group.active_tab;
                if active && index == focus {
                    if let Some(host) = built.host_for_tab_mut(tab.id) {
                        *host = build_tile(tiles, controls, workspace, tab);
                    }
                } else {
                    deferred.push((!active, index.abs_diff(focus), tab.id));
                }
            }
            built
        })
        .collect();
    deferred.compact_sort_by_key(|&(inactive, distance, _)| (inactive, distance));
    let order = deferred.into_iter().map(|(_, _, tab)| tab).collect();
    (strip_row(groups), order)
}

/// A strip's row as the tree describes it. Each tab takes its content from `live` when present
/// there, else from `tiles` .
pub(super) fn build_row(
    tiles: &mut dyn TileSource,
    controls: Option<&ui::ControlDomain>,
    workspace: &Workspace,
    strip: &tilo_desktop_model::Strip,
    live: &mut HashMap<TabId, TileHost>,
) -> GroupRow {
    let groups = strip
        .window_groups
        .iter()
        .map(|group| {
            let tabs = group
                .tabs
                .iter()
                .map(|tab| (descriptor(tab), placeholder()))
                .collect();
            let mut built = WindowGroup::new(group.id, tabs, group.active_tab, group.width)
                .with_maximized(group.maximized);
            for tab in &group.tabs {
                if let Some(host) = built.host_for_tab_mut(tab.id) {
                    *host = live
                        .remove(&tab.id)
                        .unwrap_or_else(|| build_tile(tiles, controls, workspace, tab));
                }
            }
            built
        })
        .collect();
    strip_row(groups)
}
