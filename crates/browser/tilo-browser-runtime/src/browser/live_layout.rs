//! Where this person's windows are, for the room's presence: the windows of
//! the strip shown, as the desktop surface laid them out, each named by its
//! app and the short digest of its record only
//! ([`tilo_relay_protocol::live::LiveLayout`], placed by the rule native
//! shells use too). Others draw a mini view of this screen from what they
//! hold themselves; a window on something not shared with them tells them
//! only its app.

use tilo_desktop_model::TabId;
use tilo_relay_protocol::live::{LiveLayout, LiveRecord};
use ui::kurbo::Rect;

use super::Mount;

impl Mount {
    /// Where this person is on the desktop, for the room's presence:
    /// `{focus, place, layout}`, the record of the focused window (the
    /// desktop's own record where no window is focused or it has none yet),
    /// the strip shown and where its windows are
    /// ([`layout_value`]). `None` for a single application.
    pub(super) fn live_place_value(&self) -> Option<serde_json::Value> {
        let desktop = self.desktop.as_ref()?;
        let shown = desktop.handle.shown_tabs();
        let area = desktop.handle.shown_strip().map(|(_, area, _)| area);
        let (focus, place, layout) = desktop.handle.read(|tree| {
            let place = tree.active_workspace().and_then(|workspace| {
                tilo_desktop_surface::navigation::active_strip(tree, workspace)
            });
            let route = tilo_desktop_surface::navigation::active_route(tree);
            let focus = route.and_then(|route| desktop.shared.live_record_of(tree, route.tab));
            // Where the windows are, by app and record digest only.
            let layout = area.and_then(|area| {
                layout_value(
                    area,
                    &shown,
                    |tab| desktop.shared.live_record_of(tree, tab),
                    route.map(|route| route.tab),
                )
            });
            (focus, place, layout)
        });
        let (app, record) = focus.unwrap_or_else(|| {
            (
                crate::desktop::DESKTOP_PACKAGE.to_owned(),
                crate::replication::DESKTOP_RECORD.to_owned(),
            )
        });
        Some(serde_json::json!({
            "focus": {"app": app, "record": record},
            "place": place.map(|strip| strip.0.to_string()),
            "layout": layout,
        }))
    }
}

/// The layout's JSON as the page's presence carries it: `{size: [w, h],
/// windows: [{app, record, rect, focused}]}`, `record` the 16-hex short
/// digest, laid out in the content `area` (the strip's, under the bar) by
/// [`LiveLayout::place`]. `record_of` names a tab's live record; a window
/// without one is left out. `None` for an area of no size.
pub(super) fn layout_value(
    area: Rect,
    shown: &[(TabId, Rect)],
    record_of: impl Fn(TabId) -> Option<(String, String)>,
    focused: Option<TabId>,
) -> Option<serde_json::Value> {
    let windows: Vec<_> = shown
        .iter()
        .filter_map(|&(tab, rect)| {
            let (app, record) = record_of(tab)?;
            Some((
                LiveRecord { app, record },
                [rect.x0, rect.y0, rect.x1, rect.y1],
                focused == Some(tab),
            ))
        })
        .collect();
    let layout = LiveLayout::place([area.x0, area.y0, area.x1, area.y1], &windows)?;
    Some(serde_json::json!({
        "size": layout.size,
        "windows": layout.windows.iter().map(|window| serde_json::json!({
            "app": window.app,
            "record": hex::encode(window.record),
            "rect": window.rect,
            "focused": window.focused,
        })).collect::<Vec<_>>(),
    }))
}
