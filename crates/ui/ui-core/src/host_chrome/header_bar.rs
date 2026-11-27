//! The header bar an SDK app draws where the window system leaves decorations to the client (GNOME, and any
//! Wayland compositor without server-side `xdg-decoration`): History as an icon menu button, a search
//! toggle, the title and the window controls, in one bar that replaces the fallback frame. It presents
//! the host command model and nothing else.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use tilo_actions::ControlInvocation;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::model::{CommandGroup, HostCommand};
use tilo_ui_base::i18n::I18nManager;
use tilo_ui_base::icons::Icon;

use crate::widget::ControlSize;
use crate::{widget::Button, widget::ContextMenu, widget::MenuItem, widget::Row, widget::Text};
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::{widget::Spacer, widget::Widget};

/// What the person asked of the window or the app through the bar.
#[derive(Clone, Debug, PartialEq)]
pub enum HeaderRequest {
    Command(ControlInvocation),
    /// A press on the bar's empty area: the compositor moves the window.
    Move,
    ToggleMaximize,
    Minimize,
    Close,
}

type Requests = Rc<RefCell<VecDeque<HeaderRequest>>>;

/// What a bar shows and does: its title, and each command's label, enabled state, place and invocation.
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

#[tilo_ui_base::component::not_a_component(
    shell,
    "the host operating system chrome a window outside the Tilo shell carries"
)]
pub struct HeaderBar {
    pub presented: Presented,
    pub row: Row,
    pub menu: ContextMenu,
    pub history_button: Rect,
    pub requests: Requests,
    pub menu_requested: Rc<RefCell<bool>>,
    pub viewport: Rect,
    pub rect: Rect,
    pub last_press: Option<crate::time::Instant>,
    pub swallow_release: bool,

    /// Input or a new document value changed what this widget paints: it repaints once, and rests after
    /// the frame draws it.
    pub dirty: bool,
}

impl HeaderBar {
    pub fn new(title: &str, commands: &[HostCommand]) -> Self {
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
        let mut previous = None;
        for command in &history {
            if previous.is_some_and(|group| group != command.group) {
                menu.push(MenuItem::separator());
            }
            previous = Some(command.group);
            let press = tilo_ui_base::widget::Press::of(tilo_ui_base::component::Event::Action(
                command.binding.clone(),
            ));
            menu.push(MenuItem::bound(command.label.clone(), press).with_enabled(command.enabled));
        }

        let mut row = Row::new().no_wrap();
        // A window with no History (none has one without Tilo's model) shows no History control.
        if !history.is_empty() {
            let open_menu = menu_requested.clone();
            let history_button = control("host-header-history", Icon::History, "host-menu-history")
                .on_open(move || *open_menu.borrow_mut() = true);
            row = row.push(Box::new(history_button));
        }
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
            dirty: true,
            presented: presented(title, commands),
            row,
            menu,
            history_button: Rect::ZERO,
            requests,
            menu_requested,
            viewport: Rect::ZERO,
            rect: Rect::ZERO,
            last_press: None,
            swallow_release: false,
        }
    }

    /// Whether this bar already shows `title` and `commands`, so a refresh can keep it, and the pointer
    /// and press state it holds.
    pub fn presents(&self, title: &str, commands: &[HostCommand]) -> bool {
        self.presented == presented(title, commands)
    }

    /// The window area the History menu may open into.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.dirty = true;
        self.viewport = viewport;
    }

    /// Lay the bar out `width` wide in `tree`, an arena of its own that `nodes` maps back into this
    /// bar.
    pub fn solve_layout(
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
        let mut bindings = tilo_ui_base::widget::bound_nodes::BoundNodeMap::new();
        // SAFETY: the arena and node map were just built from this bar alone,
        // which stays in place and unaliased for both calls.
        let solved = unsafe {
            bindings.rebuild(self, tree, node, nodes);
            tilo_ui_base::widget::compute_widget_layout(
                tree, node, available, nodes, &bindings, text,
            )
        };
        solved.ok()?;
        self.apply_layout(tree, node, Point::ORIGIN);
        Some(node)
    }

    /// Requests in the order the person made them.
    pub fn take_requests(&self) -> Vec<HeaderRequest> {
        self.requests.borrow_mut().drain(..).collect()
    }

    /// Whether `event` belongs to the bar: its own area, or anywhere while its menu is open.
    pub fn claims(&self, point: Point) -> bool {
        self.menu.is_open() || self.rect.contains(point)
    }

    fn open_menu(&mut self) {
        self.menu.open_anchored(self.history_button, self.viewport);
    }

    /// Open the menu the History control asked for, once.
    fn take_menu_request(&mut self) {
        if std::mem::take(&mut *self.menu_requested.borrow_mut()) {
            self.open_menu();
        }
    }

    /// Hand the host the command a chosen row pressed.
    fn drain_menu(&mut self) {
        while let Some(invocation) =
            tilo_ui_base::widget::controls::ActionControls::take_control_invocation(&mut self.menu)
        {
            self.requests
                .borrow_mut()
                .push_back(HeaderRequest::Command(invocation));
        }
    }

    /// A press on the bar that no control took: a double press maximizes, a single one lets the
    /// compositor move the window.
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

const DOUBLE_PRESS: std::time::Duration = std::time::Duration::from_millis(400);

fn control(id: &str, icon: Icon, key: &str) -> Button {
    let label = I18nManager::lookup_plain(key);
    Button::icon_only(id, icon, &label).control_size(ControlSize::Small)
}

impl Widget for HeaderBar {
    fn sync(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) {
        self.dirty = true;
        self.take_menu_request();
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

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

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        crate::widget::toolbar::paint_band(scene, transform, theme, self.rect);
        self.row.draw_child(scene, transform, text, theme, &env);
        self.menu.layout_within(text, theme);
    }

    fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.menu
            .draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
                scene,
                transform,
                text,
                theme,
                env.clone(),
            ));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.dirty = true;
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
        // The row's controls publish their press; the bar is drawn apart
        // from the app's tree, so it answers the press here, before the
        // menu it asks for opens.
        let path = ctx.path;
        crate::widget::resources::start_pending_control_invocations(self, &mut ctx.edits(), path);
        self.take_menu_request();
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

    fn needs_redraw(&self) -> bool {
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
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

/// Which window edge a pointer at `point` grabs for resizing, for a window that draws its own frame.
pub fn resize_edge(
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
