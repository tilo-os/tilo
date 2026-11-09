//! Where this person's windows are, for the workspace's presence: the windows of
//! the strip shown, as the desktop surface laid them out, each named by its
//! app and the short digest of its record only
//! ([`tilo_relay_protocol::live::LiveLayout`], placed by
//! [`tilo_session::sync::presence::placed`], the rule every host places by). Others draw a mini view of this screen from what they
//! hold themselves; a window on something not shared with them tells them
//! only its app.

use tilo_desktop_model::TabId;
use tilo_relay_protocol::live::LiveRecord;
use ui::kurbo::Rect;

use super::Mount;

impl Mount {
    /// Where this person is on the desktop, for the workspace's presence:
    /// `{focus, place, layout}`, the focused window named by its app and the
    /// short digest of its record (the desktop's own record where no window
    /// is focused or it has none yet), the strip shown and where its windows
    /// are ([`layout_value`]). A single application, full tab, is its one
    /// window's focus alone: it has no strip and no layout.
    pub(super) fn live_place_value(&self) -> Option<serde_json::Value> {
        let Some(desktop) = self.desktop.as_ref() else {
            let (app, checkout) = self.app.as_ref().zip(self.checkout.as_ref())?;
            let record = tilo_desktop_surface::host::records::wire_record(&checkout.id);
            return Some(serde_json::json!({
                "focus": named(app.clone(), record.to_owned()),
                "place": null,
                "layout": null,
            }));
        };
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
            "focus": named(app, record),
            "place": place.map(|strip| strip.0.to_string()),
            "layout": layout,
        }))
    }
}

/// `{app, record}` naming one window the way the rest of the presence does:
/// the app and the 16-hex [`LiveRecord::short_digest`] of its record, never
/// the record id, so that a receiver matches it against what it holds and
/// learns nothing more than the app of a window on something not shared with
/// it.
fn named(app: String, record: String) -> serde_json::Value {
    let named = LiveRecord { app, record };
    let short = hex::encode(named.short_digest());
    serde_json::json!({"app": named.app, "record": short})
}

/// The layout's JSON as the page's presence carries it: `{size: [w, h],
/// windows: [{app, record, rect, focused}]}`, `record` the 16-hex short
/// digest, laid out in the content `area` (the strip's, under the bar) by the
/// placement rule every host uses ([`tilo_session::sync::presence::placed`]).
/// `record_of` names a tab's live record; a window without one is left out.
/// `None` for an area of no size.
pub(super) fn layout_value(
    area: Rect,
    shown: &[(TabId, Rect)],
    record_of: impl Fn(TabId) -> Option<(String, String)>,
    focused: Option<TabId>,
) -> Option<serde_json::Value> {
    let layout = tilo_session::sync::presence::placed(Some(area), shown, record_of, focused)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The focused window is named the way the rest of the presence and every
    /// receiver's match against its own records reads it: the app and the
    /// 16-hex short digest of the record, never the record id.
    #[test]
    fn a_focused_window_is_named_by_its_short_digest() {
        let value = named("so.tilo.notes".into(), "a record id".into());
        let record = LiveRecord {
            app: "so.tilo.notes".into(),
            record: "a record id".into(),
        };
        assert_eq!(value["app"], "so.tilo.notes");
        assert_eq!(value["record"], hex::encode(record.short_digest()));
        assert_eq!(value["record"].as_str().map(str::len), Some(16));
    }
}
