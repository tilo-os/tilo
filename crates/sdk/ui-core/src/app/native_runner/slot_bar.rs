//! The app's window-bar slots in its own window, outside a Tilo session: the
//! shared [`SlotRow`] across the top of the native window, under whatever
//! frame the host gives it ([`crate::slot_row`] says what the row is).

use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;
use winit::window::Window;

use super::super::App;
use crate::search::SearchRequest;
use crate::slot_row::{RowInput, RowPlace, SlotRow, SlotRowHost};
use crate::widget::{HistoryControl, InPlace, Widget};
use crate::window_toolbar::{SearchOpening, ToolbarEvent};

impl SlotRowHost for App {
    fn search_root(&mut self) -> &mut dyn Widget {
        self.root_widget.as_mut()
    }

    fn search_offer(&mut self) -> InPlace {
        self.window_search_offer().into()
    }

    fn open_app_search(&mut self, request: SearchRequest) {
        self.open_search(request);
    }

    fn search_drove(&mut self) {
        self.mark_semantic_tree_stale();
        self.layout.layout_dirty = true;
    }

    fn takes_search(&mut self, opening: SearchOpening) -> bool {
        self.window_bar_takes(opening)
    }

    fn history_control(&self) -> Option<HistoryControl> {
        self.window_history_control()
    }

    fn press_history(&mut self, control: HistoryControl) {
        let _ = self.invoke_window_history_control(control);
    }

    fn deliver(&mut self, event: ToolbarEvent) {
        self.deliver_app_slot_event(event);
    }

    fn slots_acted(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
    }
}

pub(super) struct SlotBar {
    row: SlotRow,
}

impl SlotBar {
    pub(super) fn new() -> Self {
        Self {
            row: SlotRow::new(),
        }
    }

    /// Keep the row current: shown while nothing hosts the app's slots and
    /// it declared some, under the host's frame (`app.layout.safe_area`).
    /// Call every frame before layout: the row's height is part of the app's
    /// top inset.
    pub(super) fn refresh(&mut self, app: &mut App, window: &Window) {
        // Outside a session the row is the window's bar: it hosts the app's
        // slots and presents its search and History, as the desktop's bar
        // does in one. A window the shell hears is hosted by the shell from
        // its first frame: the row never shows there, not even until the
        // shell says so.
        let standalone = app.floating.client.is_none();
        let shown = standalone && window.fullscreen().is_none();
        if standalone {
            if crate::window_toolbar::set_hosted(shown) {
                app.needs_sync = true;
                app.layout.layout_dirty = true;
            }
            if app.floating.window_bar.presents() != shown {
                app.set_window_presents_history(shown);
            }
            self.row.serve_app_requests(app);
        }
        let physical = window.inner_size();
        let size =
            crate::scale::logical_size((physical.width, physical.height), window.scale_factor());
        let area = app.layout.safe_area;
        let title = window.title();
        let history = app.window_history_control();
        let height = self.row.refresh(
            &app.theme,
            shown.then_some(RowPlace {
                left: area.left,
                top: area.top,
                right: area.right,
                window: size,
                app_name: &title,
            }),
            history,
        );
        self.row.drive_search(app);
        if (app.layout.slot_bar_inset - height).abs() > f64::EPSILON {
            app.layout.slot_bar_inset = height;
            app.layout.layout_dirty = true;
            if height > 0.0 {
                self.row.mark_repaint();
            }
        }
    }

    /// The input evidence of the window's search, beside the app's own
    /// ([`App::native_input_snapshot`]): what the field holds and where the
    /// control stands, while the row shows.
    pub(super) fn with_search_evidence(
        &self,
        snapshot: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        self.row.with_search_evidence(snapshot)
    }

    /// Whether the row changed since the last frame, which must then
    /// repaint the whole window rather than only the app's own damage.
    pub(super) fn take_repaint(&mut self) -> bool {
        self.row.take_repaint()
    }

    /// Whether the row needs another frame: a menu arriving, the field's
    /// caret.
    pub(super) fn needs_frame(&self) -> bool {
        self.row.needs_frame()
    }

    pub(super) fn draw(&mut self, app: &mut App, scene: &mut Scene, transform: Affine) {
        let left = app.layout.safe_area.left;
        self.row.draw(
            scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
            left,
        );
    }

    /// Hand `event` to the row when it is the row's: a pointer over a slot,
    /// over one of its menus, or anything while its location is edited.
    /// Returns whether the row took it; what the person did reaches the app
    /// as the desktop's bar delivers it.
    pub(super) fn route(&mut self, app: &mut App, window: &Window, event: &WindowEvent) -> bool {
        if !self.row.is_shown() {
            return false;
        }
        let modifiers = app.input.modifiers;
        let input = RowInput {
            scale: window.scale_factor(),
            modifiers: &modifiers,
            platform: app.input.platform,
        };
        let (took, redraw) = self.row.route(app, event, &input);
        if redraw {
            window.request_redraw();
        }
        took
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::MotionSource;
    use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
    use crate::widget::{NavItem, NavList, NavSection};
    use vello::kurbo::Rect;

    fn settings_like_list() -> NavList {
        NavList::new(vec![
            NavSection::new(
                "personal",
                "Personal",
                vec![
                    NavItem::new("appearance", "Appearance"),
                    NavItem::new("sound", "Sound"),
                    NavItem::new("time", "Time & date"),
                ],
            )
            .filterable(),
        ])
    }

    fn filter(app: &App) -> String {
        app.root_widget
            .as_any()
            .and_then(|root| root.downcast_ref::<NavList>())
            .expect("the app is its list")
            .filter()
            .to_owned()
    }

    /// The row as shown in a window 800 wide, without a platform window.
    fn shown(bar: &mut SlotBar) {
        bar.row
            .show_for_test(Rect::new(8.0, 0.0, 792.0, TOOLBAR_ROW_HEIGHT));
    }

    // Outside a session the runner's row is the window's bar, and its search is
    // the window's one search entry: Find opens it on the app's list, what is
    // typed narrows the list in the next frame, and the evidence says so.
    #[test]
    fn the_standalone_bar_searches_the_apps_list_in_place() {
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        let mut bar = SlotBar::new();
        shown(&mut bar);
        bar.row
            .open_search(&mut app, MotionSource::Keyboard, SearchOpening::Find);
        assert!(bar.row.search_mut().is_open() && bar.row.search_mut().has_keyboard());
        assert_eq!(
            bar.row.search_mut().in_place(),
            InPlace::List,
            "a list narrows in place"
        );

        bar.row.type_into_search("s");
        let typed = bar
            .with_search_evidence(Some(serde_json::json!({})))
            .expect("evidence");
        assert_eq!(typed["window_search"]["text"], "s");
        assert_eq!(typed["window_search"]["focused"], true);
        assert_eq!(typed["window_search"]["caret_byte"], 1);
        assert_eq!(
            filter(&app),
            "",
            "the list narrows in the frame, not the keystroke"
        );

        bar.row.drive_search(&mut app);
        assert_eq!(filter(&app), "s", "the frame narrows the list");

        bar.row.search_mut().close(MotionSource::Keyboard);
        bar.row.drive_search(&mut app);
        assert_eq!(filter(&app), "", "closing the search shows the whole list");
    }

    #[test]
    fn evidence_is_only_the_apps_while_the_row_is_hidden() {
        let bar = SlotBar::new();
        let snapshot = bar
            .with_search_evidence(Some(serde_json::json!({ "app": "x" })))
            .expect("evidence");
        assert!(snapshot.get("window_search").is_none());
    }
}
