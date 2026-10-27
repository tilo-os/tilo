//! The app's window-bar slots in its own window, outside a Tilo session: the
//! shared [`SlotRow`] across the top of the native window, under whatever
//! frame the host gives it ([`crate::slot_row`] says what the row is).
//!
//! The row is in the window's accessibility tree as one toolbar, first in
//! reading order ([`window_tree_update`]), and an assistive technology's
//! action on one of its nodes runs the slot as a press does
//! ([`SlotBar::perform_accessibility`]): a screen reader, a script and an
//! agent reach the app's slots, the window's History and its search.

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

/// The key of the row's toolbar node under the window's root: no widget's
/// node hangs there, so the row's nodes keep their own ids.
const WINDOW_SLOTS_KEY: u64 = u64::from_le_bytes(*b"winslots");

/// The window's accessibility tree, at `scale`: the app's, with the row of
/// slots first while it shows.
pub(super) fn window_tree_update(
    app: &mut App,
    bar: &SlotBar,
    scale: f64,
) -> accesskit::TreeUpdate {
    let mut update = app.build_a11y_tree_update(scale);
    if let Some(row) = bar.accessibility(app) {
        crate::accesskit_bridge::publish_host_part(&mut update, &row, scale);
    }
    crate::accesskit_bridge::native_tree_update(update)
}

pub(super) struct SlotBar {
    row: SlotRow,
    /// The window's title, which names the row's toolbar.
    title: String,
}

impl SlotBar {
    pub(super) fn new() -> Self {
        Self {
            row: SlotRow::new(),
            title: String::new(),
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
        self.title = window.title();
        let history = app.window_history_control();
        let height = self.row.refresh(
            &app.theme,
            shown.then_some(RowPlace {
                left: area.left,
                top: area.top,
                right: area.right,
                window: size,
                app_name: &self.title,
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

    /// The row as one toolbar of the window's accessibility tree, named for
    /// the window: the app's slots, the window's History in its menu and
    /// the search. None while it is hidden or the palette covers the window.
    fn accessibility(&self, app: &App) -> Option<crate::a11y::AccessNode> {
        if !self.row.is_shown() || app.palette_presented() {
            return None;
        }
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Toolbar)
                .with_key(WINDOW_SLOTS_KEY)
                .with_name(self.title.clone())
                .with_bounds(self.row.band(app.layout.safe_area.left, &app.theme))
                .with_children(self.row.accessibility()),
        )
    }

    /// Run `action` on the row's node `target`, as assistive technology
    /// asks: the slot runs as a press on it does, and what it runs reaches
    /// the app as the desktop's bar delivers it. Returns whether `target`
    /// is the row's and it took the action; any other node is the app's.
    pub(super) fn perform_accessibility(
        &mut self,
        app: &mut App,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> bool {
        let Some(row) = self.accessibility(app) else {
            return false;
        };
        let Some(slot) = crate::accesskit_bridge::surface_action_target(&row, target) else {
            return false;
        };
        let slot = slot.to_owned();
        self.row.perform(app, &slot, action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::MotionSource;
    use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
    use crate::widget::{NavItem, NavList, NavSection};
    use crate::window_toolbar::{ToolbarAction, WindowToolbar};
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

    /// Lay the app out and draw its frame as the runner does, under the row
    /// the bar shows.
    fn lay_out(app: &mut App) {
        app.layout.structure_dirty |= app.layout.root_node.is_none()
            || app.layout.node_map.is_empty()
            || app.take_root_structure_dirty();
        let rebuilt = app.layout.structure_dirty;
        if rebuilt {
            app.rebuild_structure();
        }
        let node = app.layout.root_node.expect("the app's root is laid out");
        crate::app::run_layout_pass(app, node, 800, 600, 1.0, rebuilt, None)
            .expect("the frame lays out");
        crate::app::draw_frame_content(app, &mut Scene::new(), 800, 600, 1.0, None);
        crate::app::complete_native_frame_content(app, |_| {});
    }

    /// The row in a window 800 wide titled "Call", showing what the app
    /// declared, as the runner refreshes it each frame without a platform
    /// window.
    fn shown_declared(bar: &mut SlotBar, app: &mut App) {
        bar.title = "Call".into();
        let height = bar.row.refresh(
            &app.theme,
            Some(RowPlace {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                window: (800.0, 600.0),
                app_name: "Call",
            }),
            None,
        );
        app.layout.slot_bar_inset = height;
        app.layout.layout_dirty = true;
        lay_out(app);
    }

    /// The id and node of the window's tree labelled `label`.
    fn labelled<'a>(
        update: &'a accesskit::TreeUpdate,
        label: &str,
    ) -> Option<&'a (accesskit::NodeId, accesskit::Node)> {
        update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
    }

    fn declare_call() {
        crate::window_toolbar::declare(Some(WindowToolbar {
            actions: vec![
                ToolbarAction::new("call-mute", "mic", "Mute"),
                ToolbarAction::new("call-leave", "phone-off", "Leave"),
            ],
            ..WindowToolbar::default()
        }));
    }

    // A window of its own carries its row of slots in its accessibility
    // tree, first, as one toolbar named for the window, beside the app's
    // own nodes; a screen reader's or an agent's action on a slot hands the
    // app the slot's press, as a click on it does.
    #[test]
    fn the_windows_accessibility_tree_carries_its_slots_and_runs_them() {
        declare_call();
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        let mut bar = SlotBar::new();
        shown_declared(&mut bar, &mut app);

        let update = window_tree_update(&mut app, &bar, 2.0);
        let (_, window) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == crate::accesskit_bridge::ROOT_ID)
            .expect("the window's root");
        let (toolbar_id, toolbar) = labelled(&update, "Call").expect("the row's toolbar");
        assert_eq!(toolbar.role(), accesskit::Role::Toolbar);
        assert_eq!(
            window.children().first(),
            Some(toolbar_id),
            "the row is first in reading order"
        );
        assert!(
            labelled(&update, "Appearance").is_some(),
            "the app's own nodes stay in the window's tree"
        );

        let row = bar.accessibility(&app).expect("the row is published");
        let (mute, mute_node) = labelled(&update, "Mute").expect("the microphone's slot");
        assert!(toolbar.children().contains(mute));
        assert_eq!(
            crate::accesskit_bridge::surface_action_target(&row, *mute),
            Some("window-toolbar-action:call-mute")
        );
        assert!(
            mute_node.supports_action(accesskit::Action::Click),
            "a reader can press it"
        );
        let (leave, _) = labelled(&update, "Leave").expect("the leave slot");
        assert_eq!(
            crate::accesskit_bridge::surface_action_target(&row, *leave),
            Some("window-toolbar-action:call-leave")
        );

        let request = accesskit::ActionRequest {
            action: accesskit::Action::Click,
            target_tree: accesskit::TreeId::ROOT,
            target_node: *mute,
            data: None,
        };
        let action = crate::accesskit_bridge::accesskit_action_to_ours(
            request.action,
            request.data.as_ref(),
        )
        .expect("a click is an activation");
        assert!(bar.perform_accessibility(&mut app, request.target_node, &action));
        assert_eq!(
            crate::window_toolbar::take_events(),
            [ToolbarEvent::Action {
                id: "call-mute".into()
            }],
            "the app hears the press as the desktop's bar delivers it"
        );

        let (app_node, _) = labelled(&update, "Appearance").expect("the app's node");
        assert!(
            !bar.perform_accessibility(&mut app, *app_node, &action),
            "the app's own nodes stay the app's"
        );
        assert!(crate::window_toolbar::take_events().is_empty());
    }

    // The window's search is in the tree with the slots, and a reader opens
    // it as a press on its control does.
    #[test]
    fn a_reader_opens_the_windows_search_from_the_tree() {
        declare_call();
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        let mut bar = SlotBar::new();
        shown_declared(&mut bar, &mut app);
        let row = bar.accessibility(&app).expect("the row is published");
        let update = window_tree_update(&mut app, &bar, 1.0);
        let (search, _) = update
            .nodes
            .iter()
            .find(|(id, _)| {
                crate::accesskit_bridge::surface_action_target(&row, *id)
                    == Some("window-search-open")
            })
            .expect("the window's search control");
        assert!(bar.perform_accessibility(&mut app, *search, &crate::a11y::Action::Activate));
        assert!(bar.row.search_mut().is_open(), "the search opens");
    }

    // A hidden row (a window the desktop hosts, or a full screen) adds
    // nothing to the window's tree and takes no action.
    #[test]
    fn a_hidden_row_publishes_nothing() {
        declare_call();
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        lay_out(&mut app);
        let bar = SlotBar::new();
        let update = window_tree_update(&mut app, &bar, 1.0);
        assert!(labelled(&update, "Mute").is_none());
        assert!(bar.accessibility(&app).is_none());
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
