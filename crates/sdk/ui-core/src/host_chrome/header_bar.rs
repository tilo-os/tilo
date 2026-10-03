//! The header bar an SDK app draws where the desktop leaves decorations to
//! the client (GNOME, and any Wayland compositor without server-side
//! `xdg-decoration`): History as a menu button, a search toggle, the title
//! and the window controls, in one bar that replaces the fallback frame.
//!
//! It presents the host command model and nothing else. A chosen History
//! item or the search toggle hands back its command's invocation, which the
//! app runs through the same door as the palette; window controls hand back
//! a [`HeaderRequest`] for the runner to apply to its window.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use tilo_actions::ControlInvocation;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::HostChrome;
use super::model::{CommandGroup, HostCommand};
use crate::i18n::I18nManager;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::ControlSize;
use crate::widget::{Button, ContextMenu, MenuItem, Row, Spacer, Text, Widget};

/// What the person asked of the window or the app through the bar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HeaderRequest {
    Command(ControlInvocation),
    /// A press on the bar's empty area: the compositor moves the window.
    Move,
    ToggleMaximize,
    Minimize,
    Close,
}

type Requests = Rc<RefCell<VecDeque<HeaderRequest>>>;

/// What a bar shows and does: its title, and each command's label, enabled
/// state, place and invocation.
type Presented = (
    String,
    Vec<(String, bool, CommandGroup, Option<ControlInvocation>)>,
);

fn presented(title: &str, commands: &[HostCommand]) -> Presented {
    (
        title.to_owned(),
        commands
            .iter()
            .map(|command| {
                (
                    command.label.clone(),
                    command.enabled,
                    command.group,
                    command.invocation(),
                )
            })
            .collect(),
    )
}

pub(crate) struct HeaderBar {
    presented: Presented,
    row: Row,
    menu: ContextMenu,
    /// The History menu's rows, in menu order; `None` for a separator.
    menu_commands: Vec<Option<ControlInvocation>>,
    history_button: Rect,
    requests: Requests,
    menu_requested: Rc<RefCell<bool>>,
    viewport: Rect,
    rect: Rect,
    last_press: Option<crate::time::Instant>,
    swallow_release: bool,
}

impl HeaderBar {
    pub(crate) fn new(title: &str, commands: &[HostCommand]) -> Self {
        let requests: Requests = Rc::default();
        let menu_requested = Rc::new(RefCell::new(false));
        let history: Vec<&HostCommand> = commands
            .iter()
            .filter(|command| {
                matches!(
                    command.group,
                    CommandGroup::History | CommandGroup::RecentChanges
                )
            })
            .collect();
        let search = commands
            .iter()
            .find(|command| command.group == CommandGroup::Search);

        let mut menu = ContextMenu::new();
        let mut menu_commands = Vec::new();
        let mut previous = None;
        for command in &history {
            if previous.is_some_and(|group| group != command.group) {
                menu.push(MenuItem::separator());
                menu_commands.push(None);
            }
            previous = Some(command.group);
            menu.push(MenuItem::selection(command.label.clone()).with_enabled(command.enabled));
            menu_commands.push(command.invocation());
        }

        let open_menu = menu_requested.clone();
        let history_button = Button::new("host-header-history", "host-menu-history", None)
            .with_icon(Icon::ChevronDown)
            .control_size(ControlSize::Small)
            .with_enabled(!history.is_empty())
            .on_click(move || *open_menu.borrow_mut() = true);
        let mut row = Row::new().no_wrap().push(Box::new(history_button));
        row = row
            .push(Box::new(Spacer::new()))
            .push(Box::new(Text::new(title)))
            .push(Box::new(Spacer::new()));
        if let Some(search) = search {
            let queue = requests.clone();
            let invocation = search.invocation();
            row = row.push(Box::new(
                control(
                    "host-header-search",
                    Icon::Search,
                    "host-search-placeholder",
                )
                .with_enabled(search.enabled && invocation.is_some())
                .on_click(move || {
                    if let Some(invocation) = invocation.clone() {
                        queue
                            .borrow_mut()
                            .push_back(HeaderRequest::Command(invocation));
                    }
                }),
            ));
        }
        for (id, icon, key, request) in [
            (
                "host-header-minimize",
                Icon::Minus,
                "host-window-minimize",
                HeaderRequest::Minimize,
            ),
            (
                "host-header-maximize",
                Icon::Square,
                "host-window-maximize",
                HeaderRequest::ToggleMaximize,
            ),
            (
                "host-header-close",
                Icon::X,
                "host-window-close",
                HeaderRequest::Close,
            ),
        ] {
            let queue = requests.clone();
            row = row
                .push(Box::new(control(id, icon, key).on_click(move || {
                    queue.borrow_mut().push_back(request.clone())
                })));
        }
        Self {
            presented: presented(title, commands),
            row,
            menu,
            menu_commands,
            history_button: Rect::ZERO,
            requests,
            menu_requested,
            viewport: Rect::ZERO,
            rect: Rect::ZERO,
            last_press: None,
            swallow_release: false,
        }
    }

    /// Whether this bar already shows `title` and `commands`, so a refresh
    /// can keep it, and the pointer and press state it holds.
    pub(crate) fn presents(&self, title: &str, commands: &[HostCommand]) -> bool {
        self.presented == presented(title, commands)
    }

    /// The window area the History menu may open into.
    pub(crate) fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    /// Lay the bar out `width` wide in `tree`, an arena of its own that
    /// `nodes` maps back into this bar. `None` when it did not settle.
    pub(crate) fn solve_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        theme: &Theme,
        text: &mut TextRenderer,
        width: f64,
    ) -> Option<NodeId> {
        *tree = TaffyTree::new();
        nodes.clear();
        let node = self.build_layout(tree, theme, nodes);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "logical window widths are far inside f32 range"
        )]
        let available = taffy::Size {
            width: taffy::AvailableSpace::Definite(width as f32),
            height: taffy::AvailableSpace::MaxContent,
        };
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        // SAFETY: the arena and node map were just built from this bar alone,
        // which stays in place and unaliased for both calls.
        let solved = unsafe {
            bindings.rebuild(self, tree, node, nodes);
            crate::widget::compute_widget_layout(tree, node, available, nodes, &bindings, text)
        };
        solved.ok()?;
        self.apply_layout(tree, node, Point::ORIGIN);
        Some(node)
    }

    /// Requests in the order the person made them. Rising-edge.
    pub(crate) fn take_requests(&self) -> Vec<HeaderRequest> {
        self.requests.borrow_mut().drain(..).collect()
    }

    /// Whether `event` belongs to the bar: its own area, or anywhere while
    /// its menu is open.
    pub(crate) fn claims(&self, point: Point) -> bool {
        self.menu.is_open() || self.rect.contains(point)
    }

    fn open_menu(&mut self) {
        self.menu.open_anchored(self.history_button, self.viewport);
    }

    fn drain_menu(&mut self) {
        let chosen = self
            .menu
            .take_selection()
            .and_then(|row| self.menu_commands.get(row).cloned().flatten());
        if let Some(invocation) = chosen {
            self.requests
                .borrow_mut()
                .push_back(HeaderRequest::Command(invocation));
        }
    }

    /// A press on the bar that no control took: a double press maximizes,
    /// a single one lets the compositor move the window.
    fn press_empty_area(&mut self) {
        let now = crate::time::now();
        let double = self
            .last_press
            .is_some_and(|last| now.duration_since(last) <= DOUBLE_PRESS);
        self.last_press = Some(now);
        self.requests.borrow_mut().push_back(if double {
            HeaderRequest::ToggleMaximize
        } else {
            HeaderRequest::Move
        });
    }
}

/// Within this, a second press on the bar is a double press.
const DOUBLE_PRESS: std::time::Duration = std::time::Duration::from_millis(400);

fn control(id: &str, icon: Icon, key: &str) -> Button {
    let label = I18nManager::lookup_plain(key);
    Button::icon_only(id, icon, &label).control_size(ControlSize::Small)
}

impl Widget for HeaderBar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.row.build_layout(taffy, theme, nodes);
        if let Ok(style) = taffy.style(node) {
            let mut style = style.clone();
            style.size.width = taffy::Dimension::percent(1.0);
            style.size.height = taffy::Dimension::length(theme.tokens.metrics.bar_height);
            style.padding = crate::widget::toolbar::band_padding(theme);
            let _ = taffy.set_style(node, style);
        }
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.row.apply_layout(taffy, node, origin);
        self.rect = self.row.get_rect();
        if let Some(button) = self.row.children().first() {
            self.history_button = button.get_rect();
        }
        if self.menu.is_open() {
            self.menu.reanchor(self.history_button, self.viewport);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        crate::widget::toolbar::paint_band(scene, transform, theme, self.rect);
        self.row.draw(scene, transform, text, theme, presence);
        self.menu.layout_within(text, theme);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.menu.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            self.swallow_release |= matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            );
            self.drain_menu();
            *ctx.handled = true;
            return;
        }
        if self.swallow_release
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                }
            )
        {
            self.swallow_release = false;
            *ctx.handled = true;
            return;
        }
        self.row.handle_event(event, ctx);
        if std::mem::take(&mut *self.menu_requested.borrow_mut()) {
            self.open_menu();
        }
        if !*ctx.handled
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            self.press_empty_area();
            *ctx.handled = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.row]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.row]
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open() || self.swallow_release
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu.modal_capture()
    }

    fn name(&self) -> &'static str {
        "HeaderBar"
    }

    fn test_id(&self) -> Option<&str> {
        Some("host-header-bar")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Whether the app draws its own frame, once the first configure has
/// settled `xdg-decoration` (`server_side`); `None` until then. Only a host
/// with no chrome of its own that also leaves decorations to the client
/// gets the bar.
pub(crate) fn app_draws_frame(chrome: HostChrome, server_side: Option<bool>) -> Option<bool> {
    match (chrome, server_side) {
        (HostChrome::InWindow, None) => None,
        (HostChrome::InWindow, Some(server_side)) => Some(!server_side),
        (HostChrome::TiloShell | HostChrome::Native, _) => Some(false),
    }
}

/// Which window edge a pointer at `point` grabs for resizing, for a window
/// that draws its own frame. The grab band is one related-row step wide.
pub(crate) fn resize_edge(
    point: Point,
    window: Rect,
    theme: &Theme,
) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection as Edge;
    let band = f64::from(theme.tokens.spacing.s2);
    let west = point.x - window.x0 < band;
    let east = window.x1 - point.x < band;
    let north = point.y - window.y0 < band;
    let south = window.y1 - point.y < band;
    match (north, south, west, east) {
        (true, _, true, _) => Some(Edge::NorthWest),
        (true, _, _, true) => Some(Edge::NorthEast),
        (_, true, true, _) => Some(Edge::SouthWest),
        (_, true, _, true) => Some(Edge::SouthEast),
        (true, ..) => Some(Edge::North),
        (_, true, ..) => Some(Edge::South),
        (_, _, true, _) => Some(Edge::West),
        (_, _, _, true) => Some(Edge::East),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use automerge::ROOT;
    use automerge::transaction::Transactable;

    use super::{HeaderBar, HeaderRequest, app_draws_frame, resize_edge};
    use crate::app::App;
    use crate::headless::Driver;
    use crate::host_chrome::HostChrome;
    use crate::host_chrome::model::{CommandGroup, HostCommand};
    use crate::widget::Widget;

    fn app_with_history() -> App {
        let mut state = crate::State::new();
        for title in ["first", "second"] {
            state.doc.put(ROOT, "title", title).unwrap();
            state.commit_action(None, None);
        }
        let mut app =
            App::new_with_controlled_inputs(state, Box::new(crate::widget::Text::new("Document")));
        app.enable_document_history_chrome();
        app.init_layout();
        app
    }

    fn driver(commands: &[HostCommand], theme: crate::Theme) -> Driver {
        let mut driver = Driver::for_root(
            crate::State::new(),
            HeaderBar::new("Notes", commands),
            (900, 120),
            theme,
        );
        driver
            .root_mut::<HeaderBar>()
            .set_viewport(vello::kurbo::Rect::new(0.0, 0.0, 900.0, 600.0));
        driver.relayout();
        driver
    }

    #[test]
    fn history_menu_lists_the_command_model_and_hands_back_the_chosen_command() {
        let mut app = app_with_history();
        let commands = app.host_commands(true);
        let mut driver = driver(&commands, crate::Theme::dark());
        let below = vello::kurbo::Point::new(600.0, 110.0);
        assert!(
            !driver.root_mut::<HeaderBar>().claims(below),
            "the app keeps its area"
        );
        driver.click_named("host-header-history").unwrap();
        let bar = driver.root_mut::<HeaderBar>();
        assert!(bar.menu.is_open());
        assert!(bar.claims(below), "an open menu takes every pointer event");
        let labels: Vec<_> = bar.menu.items().iter().map(|item| item.label()).collect();
        let history: Vec<_> = commands
            .iter()
            .filter(|command| {
                matches!(
                    command.group,
                    CommandGroup::History | CommandGroup::RecentChanges
                )
            })
            .collect();
        assert_eq!(labels[0], Some("Show History"));
        assert_eq!(labels[1], None, "recent changes follow a separator");
        assert_eq!(labels.len(), history.len() + 1);
        let show = bar.menu.item_rect(0).unwrap().center();
        driver.click_at(show.x, show.y);
        let requests = driver.root_mut::<HeaderBar>().take_requests();
        let [HeaderRequest::Command(invocation)] = requests.as_slice() else {
            panic!("choosing Show History hands back one command: {requests:?}");
        };
        assert_eq!(Some(invocation.clone()), history[0].invocation());
        assert!(
            app.invoke_host_command(invocation),
            "the palette's door opens it"
        );
        assert!(app.document_history_snapshot().unwrap().is_some());
    }

    #[test]
    fn a_live_window_sizes_every_control_to_its_content() {
        let mut app = app_with_history();
        let mut bar = HeaderBar::new("Notes", &app.host_commands(true));
        let (mut tree, mut nodes) = (taffy::TaffyTree::new(), std::collections::HashMap::new());
        let theme = crate::Theme::light();
        let mut text = crate::TextRenderer::new();
        bar.solve_layout(&mut tree, &mut nodes, &theme, &mut text, 800.0)
            .expect("the bar settles");
        let controls: Vec<_> = Widget::children(&bar.row)
            .into_iter()
            .filter_map(|child| Some((child.test_id()?.to_owned(), child.get_rect())))
            .collect();
        assert_eq!(controls.len(), 5, "{controls:?}");
        let height = controls[0].1.height();
        for (id, rect) in &controls {
            assert!(rect.width() >= height * 0.75, "{id} is squeezed: {rect:?}");
        }
        assert!(
            controls[0].1.width() > controls[1].1.width(),
            "History carries its label: {controls:?}"
        );
        assert!(
            controls.windows(2).all(|pair| pair[0].1.x1 <= pair[1].1.x0),
            "controls never overlap: {controls:?}"
        );
    }

    #[test]
    fn a_bar_is_kept_until_what_it_presents_changes() {
        let mut app = app_with_history();
        let commands = app.host_commands(true);
        let bar = HeaderBar::new("Notes", &commands);
        assert!(
            bar.presents("Notes", &app.host_commands(true)),
            "a refresh keeps it"
        );
        assert!(!bar.presents("Other", &commands), "a new title replaces it");
        let search = commands
            .iter()
            .find(|command| command.group == CommandGroup::Search)
            .and_then(HostCommand::invocation)
            .unwrap();
        assert!(app.invoke_host_command(&search));
        assert!(
            !bar.presents("Notes", &app.host_commands(true)),
            "Search stepping aside while the palette is open replaces it"
        );
    }

    #[test]
    fn framework_history_apps_search_from_the_header_bar() {
        let mut app = crate::app::framework_history_tests::native_chrome_app();
        let commands = app.host_commands(false);
        let mut driver = driver(&commands, crate::Theme::light());
        driver.click_named("host-header-search").unwrap();
        let requests = driver.root_mut::<HeaderBar>().take_requests();
        let [HeaderRequest::Command(invocation)] = requests.as_slice() else {
            panic!("the search toggle hands back its command: {requests:?}");
        };
        assert!(
            app.invoke_host_command(invocation),
            "the palette's door opens it"
        );
        let search = app
            .host_commands(false)
            .into_iter()
            .find(|command| command.group == CommandGroup::Search)
            .unwrap();
        assert!(!search.enabled, "the open palette answers its own shortcut");
    }

    #[test]
    fn framework_history_menu_opens_at_a_recent_change() {
        let mut app = crate::app::framework_history_tests::native_chrome_app();
        let commands = app.host_commands(true);
        let mut driver = driver(&commands, crate::Theme::light());
        driver.click_named("host-header-history").unwrap();
        let bar = driver.root_mut::<HeaderBar>();
        let labels: Vec<_> = bar.menu.items().iter().map(|item| item.label()).collect();
        assert_eq!(labels.len(), 4, "Show History, a separator and two changes");
        assert!(
            labels[2..]
                .iter()
                .all(|label| label.is_some_and(|label| label.contains(" · ")))
        );
        let newest = bar.menu.item_rect(2).unwrap().center();
        driver.click_at(newest.x, newest.y);
        let requests = driver.root_mut::<HeaderBar>().take_requests();
        let [HeaderRequest::Command(invocation)] = requests.as_slice() else {
            panic!("choosing a change hands back one command: {requests:?}");
        };
        assert!(app.invoke_host_command(invocation));
        crate::app::framework_history_tests::layout(&mut app);
        let show = app
            .host_commands(false)
            .into_iter()
            .find(|command| command.group == CommandGroup::History)
            .unwrap();
        assert_eq!(show.label, "Hide History", "History opened");
    }

    #[test]
    fn search_toggle_and_window_controls_ask_the_app_and_the_runner() {
        let mut app = app_with_history();
        let commands = app.host_commands(false);
        let mut driver = driver(&commands, crate::Theme::light());
        for id in [
            "host-header-search",
            "host-header-minimize",
            "host-header-maximize",
            "host-header-close",
        ] {
            driver.click_named(id).unwrap();
        }
        let search = commands
            .iter()
            .find(|command| command.group == CommandGroup::Search)
            .and_then(HostCommand::invocation)
            .unwrap();
        assert_eq!(
            driver.root_mut::<HeaderBar>().take_requests(),
            [
                HeaderRequest::Command(search),
                HeaderRequest::Minimize,
                HeaderRequest::ToggleMaximize,
                HeaderRequest::Close,
            ]
        );
        // The title is no control: a press there moves the window, a
        // second one soon after maximizes it.
        let bar = driver.targets()["host-header-bar"];
        let history = driver.targets()["host-header-history"];
        let empty = vello::kurbo::Point::new(history.x1 + 16.0, bar.center().y);
        driver.click_at(empty.x, empty.y);
        driver.click_at(empty.x, empty.y);
        assert_eq!(
            driver.root_mut::<HeaderBar>().take_requests(),
            [HeaderRequest::Move, HeaderRequest::ToggleMaximize]
        );
    }

    #[test]
    fn only_client_side_decoration_outside_tilo_gets_the_bar() {
        assert_eq!(
            app_draws_frame(HostChrome::InWindow, None),
            None,
            "wait for the first configure"
        );
        assert_eq!(
            app_draws_frame(HostChrome::InWindow, Some(false)),
            Some(true),
            "GNOME leaves the frame to the app"
        );
        assert_eq!(
            app_draws_frame(HostChrome::InWindow, Some(true)),
            Some(false),
            "KDE and X11 window managers draw it"
        );
        for server_side in [None, Some(false), Some(true)] {
            assert_eq!(
                app_draws_frame(HostChrome::TiloShell, server_side),
                Some(false),
                "the Tilo shell owns the chrome"
            );
        }
    }

    #[test]
    fn frame_edges_resize_and_the_inside_does_not() {
        use vello::kurbo::{Point, Rect};
        use winit::window::ResizeDirection as Edge;
        let theme = crate::Theme::dark();
        let window = Rect::new(0.0, 0.0, 800.0, 600.0);
        let at = |x, y| resize_edge(Point::new(x, y), window, &theme);
        assert_eq!(at(2.0, 2.0), Some(Edge::NorthWest));
        assert_eq!(at(798.0, 598.0), Some(Edge::SouthEast));
        assert_eq!(at(400.0, 1.0), Some(Edge::North));
        assert_eq!(at(1.0, 300.0), Some(Edge::West));
        assert_eq!(at(400.0, 300.0), None);
    }

    /// `TILO_HOST_CHROME_SHOTS=<dir>` writes the bar, menu open, per theme.
    #[test]
    fn header_bar_shots_in_every_theme() {
        let Ok(directory) = std::env::var("TILO_HOST_CHROME_SHOTS") else {
            return;
        };
        let mut app = app_with_history();
        let commands = app.host_commands(true);
        for (name, theme) in [
            ("light", crate::Theme::light()),
            ("dark", crate::Theme::dark()),
            ("high-contrast", crate::Theme::high_contrast()),
        ] {
            let mut driver = Driver::for_root(
                crate::State::new(),
                HeaderBar::new("Notes", &commands),
                (900, 260),
                theme,
            );
            driver
                .root_mut::<HeaderBar>()
                .set_viewport(vello::kurbo::Rect::new(0.0, 0.0, 900.0, 260.0));
            driver.relayout();
            driver.click_named("host-header-history").unwrap();
            driver.render_window_png(
                &std::path::Path::new(&directory).join(format!("header-bar-{name}.png")),
            );
        }
    }
}
