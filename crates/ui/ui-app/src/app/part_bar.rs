//! The app's window-bar parts in its own window, outside a Tilo session: the
//! shared [`PartRow`] across the top of the window, under whatever frame the
//! host gives it ([`crate::part_row`] says what the row is). The native runner
//! shows it over a platform window and the display-less driver
//! ([`super::driver::Driver`]) over its own surface, both through
//! [`BarWindow`].
//!
//! The row is in the window's accessibility tree as one toolbar, first in
//! reading order ([`window_tree_update`]), and an assistive technology's
//! action on one of its nodes runs the part as a press does
//! ([`PartBar::perform_accessibility`]): a screen reader, a script and an
//! agent reach the app's parts, the window's History and its search.

use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;

use super::super::App;
use crate::part_row::{PartRow, PartRowHost, RowInput, RowPlace, WindowAction};
use crate::widget::search_request::SearchRequest;
use crate::window_toolbar::{SearchOpening, ToolbarEvent};
use crate::{widget::HistoryControl, widget::InPlace};
use tilo_ui_base::widget::Widget;

/// What the row needs of the window it stands in: how the window stands, and
/// the window actions its controls ask for.
pub(super) trait BarWindow {
    /// The window fills the screen: it wears no bar then.
    fn fullscreen(&self) -> bool;
    fn maximized(&self) -> bool;
    /// The window's size in device pixels.
    fn device_size(&self) -> (u32, u32);
    fn scale_factor(&self) -> f64;
    /// The window's title, which names the row.
    fn title(&self) -> String;
    /// Whether the app draws the window's frame itself on `platform`: the
    /// one case the row carries the window's controls, since the host draws
    /// none.
    fn app_draws_frame(&self, platform: tilo_ui_base::platform::PlatformConventions) -> bool;
    /// Run a press on the window's own controls or the row's ground. Close
    /// never comes here: the host's own close path runs it
    /// ([`PartBar::take_close`]).
    fn act(&self, action: WindowAction);
    fn request_redraw(&self);
}

#[cfg(feature = "native")]
impl BarWindow for winit::window::Window {
    fn fullscreen(&self) -> bool {
        winit::window::Window::fullscreen(self).is_some()
    }

    fn maximized(&self) -> bool {
        self.is_maximized()
    }

    fn device_size(&self) -> (u32, u32) {
        let size = self.inner_size();
        (size.width, size.height)
    }

    fn scale_factor(&self) -> f64 {
        winit::window::Window::scale_factor(self)
    }

    fn title(&self) -> String {
        winit::window::Window::title(self)
    }

    fn app_draws_frame(&self, platform: tilo_ui_base::platform::PlatformConventions) -> bool {
        crate::host_chrome::frame::app_draws_frame(
            crate::host_chrome::HostChrome::current(platform),
            tilo_presentation::desktop_draws_frame(self),
        ) == Some(true)
    }

    fn act(&self, action: WindowAction) {
        match action {
            WindowAction::Move => {
                let _ = self.drag_window();
            }
            WindowAction::ToggleMaximize => self.set_maximized(!self.is_maximized()),
            WindowAction::Minimize => self.set_minimized(true),
            WindowAction::Close => {}
        }
    }

    fn request_redraw(&self) {
        winit::window::Window::request_redraw(self);
    }
}

impl PartRowHost for App {
    fn search_root(&mut self) -> &mut dyn Widget {
        self.root_widget.as_mut()
    }

    fn search_offer(&mut self) -> InPlace {
        self.window_search_offer().into()
    }

    fn open_app_search(&mut self, request: SearchRequest) {
        // The window's search is Tilo's model's action palette: without it there is none to open.
        #[cfg(feature = "model")]
        self.open_search(request);
        #[cfg(not(feature = "model"))]
        let _ = request;
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
        // The window's own bar reports the same refusal the same way
        // ([`crate::app::window_bar`]): History is not open yet, or the API
        // refused, and the row is offered again.
        if let Err(error) = self.invoke_window_history_control(control) {
            tracing::debug!(%error, ?control, "window parts: History control refused");
        }
    }

    fn deliver(&mut self, event: ToolbarEvent) {
        self.deliver_app_part_event(event);
    }

    fn parts_acted(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
    }
}

/// The key of the row's toolbar node under the window's root: no widget's
/// node hangs there, so the row's nodes keep their own ids.
const WINDOW_PARTS_KEY: u64 = u64::from_le_bytes(*b"winparts");

/// The window's accessibility tree, at `scale`: the app's, with the row of
/// parts first while it shows.
#[cfg(feature = "native")]
pub(super) fn window_tree_update(
    app: &mut App,
    bar: &PartBar,
    scale: f64,
) -> accesskit::TreeUpdate {
    let mut update = app.build_a11y_tree_update(scale);
    if let Some(row) = bar.accessibility(app) {
        tilo_ui_base::accesskit_bridge::publish_host_part(&mut update, &row, scale);
    }
    tilo_ui_base::accesskit_bridge::native_tree_update(update)
}

pub(super) struct PartBar {
    row: PartRow,
    /// The window's title, which names the row's toolbar.
    title: String,
    /// The row's Close control asked for the window: the runner closes it.
    close: bool,
    /// Where the app's own header row holds the window's bar: the window's controls and search
    /// stand at its trailing end, and the row of parts stands aside.
    held: Option<crate::widget::WindowBarPart>,
}

impl PartBar {
    pub(super) fn new() -> Self {
        Self {
            row: PartRow::new(),
            title: String::new(),
            close: false,
            held: None,
        }
    }

    /// Keep the row current: shown while nothing hosts the app's parts and
    /// it declared some, under the host's frame (`app.layout.safe_area`).
    /// Call every frame before layout: the row's height is part of the app's
    /// top inset. Answers whether the row stands: while it does it is the
    /// window's bar, so the header the app draws stands aside.
    #[must_use]
    pub(super) fn refresh(&mut self, app: &mut App, window: &dyn BarWindow) -> bool {
        // Outside a session the row is the window's bar: it hosts the app's
        // parts and presents its search and History, as the shell's bar
        // does in one. A window the shell hears is hosted by the shell from
        // its first frame: the row never shows there, not even until the
        // shell says so.
        let standalone = app.floating.client.is_none();
        // A window that stands on no ground is an overlay the session floats
        // over another window, such as a sheet or the keyboard: it wears no
        // bar of its own, since everything it paints shows what is under it.
        // A window whose root is a shell's window frame wears that frame's bar, its tabs, title
        // or the header row it hands the bar to: a row here would be a second bar.
        let may_hold = standalone
            && !window.fullscreen()
            && app.root_widget.paints_ground()
            && crate::widget::window_header::window_frame_of(app.root_widget.as_ref()).is_none();
        let size = tilo_ui_base::scale::logical_size(window.device_size(), window.scale_factor());
        // The app's own header row takes the window's bar where it stands as the bar's parts: the
        // row then fills the window's top and the window's controls and search stand at its
        // trailing end, and no row of parts stands above it.
        if may_hold {
            let bar = crate::widget::WindowBar {
                controls_width: crate::widget::window_header::merged_row_width_in(size.0),
                lists_actions: false,
            };
            crate::widget::window_header::offer_window_bar(app.root_widget.as_mut(), Some(bar));
        }
        self.held = if may_hold {
            crate::widget::window_header::content_bar(app.root_widget.as_ref())
        } else {
            None
        };
        if self.held.is_some() {
            crate::widget::window_header::offer_window_search(
                app.root_widget.as_mut(),
                crate::widget::window_header::merged_row_holds_search(size.0),
            );
        }
        let shown = may_hold && self.held.is_none();
        // Where the shell leaves the frame to the app, the row is the whole
        // bar: the window's minimize, maximize and close stand in their pill
        // at its trailing end, under the hand that also drags by it.
        self.row
            .set_window_controls(shown && window.app_draws_frame(app.input.platform));
        self.row.set_maximized(window.maximized());
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
        if (app.layout.part_bar_inset - height).abs() > f64::EPSILON {
            app.layout.part_bar_inset = height;
            app.layout.layout_dirty = true;
            if height > 0.0 {
                self.row.mark_repaint();
            }
        }
        shown
    }

    /// Whether the row stands across the window's top.
    pub(super) fn is_shown(&self) -> bool {
        self.row.is_shown()
    }

    /// Whether the row carries the window's own controls.
    pub(super) fn shows_window_controls(&self) -> bool {
        self.row.shows_window_controls()
    }

    /// The row's parts as assistive technology reaches them: each with the
    /// target [`Self::perform`] runs it by.
    pub(super) fn parts(&self) -> Vec<tilo_ui_base::a11y::AccessNode> {
        self.row.accessibility()
    }

    /// Run `action` on the row's part `target` (a `window-toolbar-*` part, a
    /// `window-search-*` part or one of the window's own controls), as
    /// automation does. Whether the part took it.
    pub(super) fn perform(
        &mut self,
        app: &mut App,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
    ) -> bool {
        self.row.perform(app, target, action)
    }

    /// Open the window's search on what the app offers, as its Find or
    /// Replace chord does.
    pub(super) fn open_search(&mut self, app: &mut App, opening: SearchOpening) {
        self.row
            .open_search(app, tilo_ui_base::tokens::MotionSource::Keyboard, opening);
    }

    pub(super) fn search(&self) -> &crate::widget::WindowSearch {
        self.row.search()
    }

    pub(super) fn search_mut(&mut self) -> &mut crate::widget::WindowSearch {
        self.row.search_mut()
    }

    /// The window actions the row's controls asked for since the last call.
    pub(super) fn take_window_actions(&mut self) -> Vec<WindowAction> {
        self.row.take_window_actions()
    }

    /// Whether the row's Close control asked to close the window.
    pub(super) fn take_close(&mut self) -> bool {
        std::mem::take(&mut self.close)
    }

    #[cfg(any(feature = "native", test))]
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
            &mut tilo_ui_base::draw::DrawCtx::new(
                scene,
                transform,
                &mut app.text_renderer,
                &app.theme,
                app.presence_ctx.draw_env(),
            ),
            left,
        );
        // A row of the app's holding the bar keeps the window's controls and search at its
        // trailing end.
        if let Some(part) = self.held {
            let furniture = crate::widget::window_header::RowFurniture::of(part);
            let search = self.row.search_mut();
            if furniture.search.area() > 0.0 {
                search.place_trailing(furniture.search, furniture.row.x0, furniture.row.x0);
            } else {
                search.place(vello::kurbo::Rect::ZERO, 0.0);
            }
            search.set_app_name(&self.title);
            search.set_frame_origin(vello::kurbo::Point::ZERO);
            furniture.paint(
                &mut tilo_ui_base::draw::DrawCtx::new(
                    scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                    app.presence_ctx.draw_env(),
                ),
                search,
                crate::widget::window_header::RowFurnitureState::default(),
            );
        }
    }

    /// Hand `event` to the row when it is the row's: a pointer over a part,
    /// over one of its menus, or anything while its location is edited.
    /// Returns whether the row took it; what the person did reaches the app
    /// as the shell's bar delivers it.
    pub(super) fn route(
        &mut self,
        app: &mut App,
        window: &dyn BarWindow,
        event: &WindowEvent,
    ) -> bool {
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
        for action in self.row.take_window_actions() {
            match action {
                WindowAction::Close => self.close = true,
                action => window.act(action),
            }
        }
        if redraw {
            window.request_redraw();
        }
        took
    }

    /// The row as one toolbar of the window's accessibility tree, named for
    /// the window: the app's parts, the window's History in its menu and
    /// the search. None while it is hidden or the palette covers the window.
    pub(super) fn accessibility(&self, app: &App) -> Option<tilo_ui_base::a11y::AccessNode> {
        if !self.row.is_shown() || app.palette_presented() {
            return None;
        }
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Toolbar)
                .with_key(WINDOW_PARTS_KEY)
                .with_name(self.title.clone())
                .with_bounds(self.row.band(app.layout.safe_area.left, &app.theme))
                .with_children(self.row.accessibility()),
        )
    }

    /// Run `action` on the row's node `target`, as assistive technology
    /// asks: the part runs as a press on it does, and what it runs reaches
    /// the app as the shell's bar delivers it. Returns whether `target`
    /// is the row's and it took the action; any other node is the app's.
    pub(super) fn perform_accessibility(
        &mut self,
        app: &mut App,
        target: accesskit::NodeId,
        action: &tilo_ui_base::a11y::Action,
    ) -> bool {
        let Some(row) = self.accessibility(app) else {
            return false;
        };
        let Some(part) = tilo_ui_base::accesskit_bridge::surface_action_target(&row, target) else {
            return false;
        };
        let part = part.to_owned();
        self.row.perform(app, &part, action)
    }
}

#[cfg(test)]
#[cfg(feature = "native")]
mod tests {
    use super::*;
    use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
    use crate::widget::{NavItem, NavList, NavSection};
    use crate::window_toolbar::{ToolbarAction, WindowToolbar};
    use tilo_ui_base::tokens::MotionSource;
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
    fn shown(bar: &mut PartBar) {
        bar.row
            .show_for_test(Rect::new(8.0, 0.0, 792.0, TOOLBAR_ROW_HEIGHT));
    }

    // Outside a session the runner's row is the window's bar, and its search is
    // the window's one search entry: Find opens it on the app's list, what is
    // typed narrows the list in the next frame, and the evidence says so.
    #[test]
    fn the_standalone_bar_searches_the_apps_list_in_place() {
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        let mut bar = PartBar::new();
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
    fn shown_declared(bar: &mut PartBar, app: &mut App) {
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
        app.layout.part_bar_inset = height;
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

    // A window of its own carries its row of parts in its accessibility
    // tree, first, as one toolbar named for the window, beside the app's
    // own nodes; a screen reader's or an agent's action on a part hands the
    // app the part's press, as a click on it does.
    #[test]
    fn the_windows_accessibility_tree_carries_its_parts_and_runs_them() {
        declare_call();
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        let mut bar = PartBar::new();
        shown_declared(&mut bar, &mut app);

        let update = window_tree_update(&mut app, &bar, 2.0);
        let (_, window) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == tilo_ui_base::accesskit_bridge::ROOT_ID)
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
        let (mute, mute_node) = labelled(&update, "Mute").expect("the microphone's part");
        assert!(toolbar.children().contains(mute));
        assert_eq!(
            tilo_ui_base::accesskit_bridge::surface_action_target(&row, *mute),
            Some("window-toolbar-action:call-mute")
        );
        assert!(
            mute_node.supports_action(accesskit::Action::Click),
            "a reader can press it"
        );
        let (leave, _) = labelled(&update, "Leave").expect("the leave part");
        assert_eq!(
            tilo_ui_base::accesskit_bridge::surface_action_target(&row, *leave),
            Some("window-toolbar-action:call-leave")
        );

        let request = accesskit::ActionRequest {
            action: accesskit::Action::Click,
            target_tree: accesskit::TreeId::ROOT,
            target_node: *mute,
            data: None,
        };
        let action = tilo_ui_base::accesskit_bridge::accesskit_action_to_ours(
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
            "the app hears the press as the shell's bar delivers it"
        );

        let (app_node, _) = labelled(&update, "Appearance").expect("the app's node");
        assert!(
            !bar.perform_accessibility(&mut app, *app_node, &action),
            "the app's own nodes stay the app's"
        );
        assert!(crate::window_toolbar::take_events().is_empty());
    }

    // The window's search is in the tree with the parts, and a reader opens
    // it as a press on its control does.
    #[test]
    fn a_reader_opens_the_windows_search_from_the_tree() {
        declare_call();
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        let mut bar = PartBar::new();
        shown_declared(&mut bar, &mut app);
        let row = bar.accessibility(&app).expect("the row is published");
        let update = window_tree_update(&mut app, &bar, 1.0);
        let (search, _) = update
            .nodes
            .iter()
            .find(|(id, _)| {
                tilo_ui_base::accesskit_bridge::surface_action_target(&row, *id)
                    == Some("window-search-open")
            })
            .expect("the window's search control");
        assert!(bar.perform_accessibility(
            &mut app,
            *search,
            &tilo_ui_base::a11y::Action::Activate
        ));
        assert!(bar.row.search_mut().is_open(), "the search opens");
    }

    // A hidden row (a window the shell hosts, or a full screen) adds
    // nothing to the window's tree and takes no action.
    #[test]
    fn a_hidden_row_publishes_nothing() {
        declare_call();
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        lay_out(&mut app);
        let bar = PartBar::new();
        let update = window_tree_update(&mut app, &bar, 1.0);
        assert!(labelled(&update, "Mute").is_none());
        assert!(bar.accessibility(&app).is_none());
    }

    #[test]
    fn evidence_is_only_the_apps_while_the_row_is_hidden() {
        let bar = PartBar::new();
        let snapshot = bar
            .with_search_evidence(Some(serde_json::json!({ "app": "x" })))
            .expect("evidence");
        assert!(snapshot.get("window_search").is_none());
    }
}
