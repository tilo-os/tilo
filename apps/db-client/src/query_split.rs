//! Retained query panes with main's automatic, pointer, keyboard and menu sizing.
use crate::link::Link;
use crate::query::{EditorPane, ResultsPane};
use std::collections::HashMap;
use taffy::prelude::*;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::vello::peniko::Fill;
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::{
    AppModel, ContextMenu, MenuItem, ModelPath, ResizableSplit, TaffyTreeExt, Widget,
};
use ui::winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
const QUERY_DIVIDER_H: f64 = 10.0;
#[derive(Clone, Copy, Debug, PartialEq)]
enum QueryResizeCommand {
    Adjust(f64),
    Reset,
    OpenMenu,
}

struct QueryDivider {
    rect: Rect,
    focused: bool,
    automatic: bool,
    effective_height: f64,
    pending: Option<QueryResizeCommand>,
}

impl QueryDivider {
    fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            focused: false,
            automatic: true,
            effective_height: 0.0,
            pending: None,
        }
    }

    fn set_geometry(&mut self, rect: Rect, effective_height: f64, automatic: bool) {
        self.rect = rect;
        self.effective_height = effective_height;
        self.automatic = automatic;
    }

    fn take_command(&mut self) -> Option<QueryResizeCommand> {
        self.pending.take()
    }

    fn handle_key(&mut self, key: &Key) -> bool {
        if !self.focused {
            return false;
        }
        self.pending = match key {
            Key::Named(NamedKey::ArrowDown) => Some(QueryResizeCommand::Adjust(24.0)),
            Key::Named(NamedKey::ArrowUp) => Some(QueryResizeCommand::Adjust(-24.0)),
            Key::Named(NamedKey::Home) => Some(QueryResizeCommand::Reset),
            Key::Named(NamedKey::Enter | NamedKey::ContextMenu) => {
                Some(QueryResizeCommand::OpenMenu)
            }
            Key::Character(text) if text.as_str() == " " => Some(QueryResizeCommand::OpenMenu),
            _ => None,
        };
        self.pending.is_some()
    }
}

impl Widget for QueryDivider {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(QUERY_DIVIDER_H as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let center = self.rect.center();
        let track = Rect::new(
            self.rect.x0 + 8.0,
            center.y - 0.5,
            self.rect.x1 - 8.0,
            center.y + 0.5,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &track,
        );
        let grip = Rect::new(
            center.x - 24.0,
            center.y - 1.5,
            center.x + 24.0,
            center.y + 1.5,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            if self.focused {
                theme.border_focus
            } else {
                theme.text_dim
            },
            None,
            &grip.to_rounded_rect(1.5),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && self.handle_key(logical_key)
        {
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "QueryDivider"
    }

    fn test_id(&self) -> Option<&str> {
        Some("db.query-divider")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::RowResize)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Slider)
                .with_name("Query editor height")
                .with_description(
                    "Arrow keys resize. Home resets automatic height. Enter opens resize actions.",
                )
                .with_value(if self.automatic {
                    format!("Automatic, {:.0} pixels", self.effective_height)
                } else {
                    format!("Manual, {:.0} pixels", self.effective_height)
                })
                .with_state(
                    ui::a11y::AccessState::new()
                        .focusable(true)
                        .focused(self.focused),
                ),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        self.pending = match action {
            ui::a11y::Action::Increment => Some(QueryResizeCommand::Adjust(24.0)),
            ui::a11y::Action::Decrement => Some(QueryResizeCommand::Adjust(-24.0)),
            ui::a11y::Action::Activate | ui::a11y::Action::ShowContextMenu => {
                Some(QueryResizeCommand::OpenMenu)
            }
            _ => None,
        };
        self.pending.is_some()
    }
}

struct QueryResizeMenu {
    menu: ContextMenu,
}

impl QueryResizeMenu {
    fn new() -> Self {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Increase query height", || {}));
        menu.push(MenuItem::action("Decrease query height", || {}));
        menu.push(MenuItem::action("Reset automatic query height", || {}));
        Self { menu }
    }

    fn open(&mut self, anchor: Rect, viewport: Rect) {
        self.menu.open_anchored(anchor, viewport);
        self.menu.focus_first_action();
    }

    fn reanchor(&mut self, anchor: Rect, viewport: Rect) {
        self.menu.reanchor(anchor, viewport);
    }

    fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    fn close(&mut self) {
        self.menu.close();
    }

    fn take_selection(&mut self) -> Option<usize> {
        self.menu.take_selection()
    }
}

impl Widget for QueryResizeMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // ContextMenu prepares its text metrics here and paints in the
        // overlay pass, above both query panes.
        self.menu
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.menu
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.menu.handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "QueryResizeMenu"
    }

    fn test_id(&self) -> Option<&str> {
        Some("db.query-resize-menu")
    }

    fn get_rect(&self) -> Rect {
        self.menu.get_rect()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut node = self.menu.accessibility()?;
        for (index, child) in node.children.iter_mut().enumerate() {
            let target = match index {
                0 => "db.increase-query-height",
                1 => "db.decrease-query-height",
                2 => "db.reset-query-height",
                _ => continue,
            };
            *child = child
                .clone()
                .with_action_target(target)
                .with_bounds(self.menu.item_rect(index)?);
        }
        Some(node)
    }

    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if matches!(action, ui::a11y::Action::Activate)
            && let Some(index) = key_path.first().and_then(|key| usize::try_from(*key).ok())
            && index < 3
        {
            self.menu.click_item(index);
            return true;
        }
        false
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let index = match target {
            "db.increase-query-height" => 0,
            "db.decrease-query-height" => 1,
            "db.reset-query-height" => 2,
            _ => return false,
        };
        self.menu.click_item(index);
        true
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        [
            "db.increase-query-height",
            "db.decrease-query-height",
            "db.reset-query-height",
        ]
        .into_iter()
        .enumerate()
        .filter_map(|(index, id)| self.menu.item_rect(index).map(|rect| (id.into(), rect)))
        .collect()
    }
}

pub(crate) struct QuerySplit {
    split: ResizableSplit<EditorPane, ResultsPane>,
    divider: QueryDivider,
    menu: QueryResizeMenu,
    requested: Option<f64>,
    measured: f64,
    minimum: f64,
    dirty: bool,
    cursor: Point,
}
impl QuerySplit {
    pub(crate) fn new(link: &Link) -> Self {
        Self {
            split: ResizableSplit::new(
                EditorPane::new(link),
                ResultsPane::new(link),
                SplitOrientation::Vertical,
            )
            .with_split_position(0.3),
            divider: QueryDivider::new(),
            menu: QueryResizeMenu::new(),
            requested: None,
            measured: 100.0,
            minimum: 54.0,
            dirty: false,
            cursor: Point::ZERO,
        }
    }
    fn command(&mut self, command: QueryResizeCommand) {
        match command {
            QueryResizeCommand::Adjust(delta) => {
                self.requested = Some(self.divider.effective_height + delta)
            }
            QueryResizeCommand::Reset => self.requested = None,
            QueryResizeCommand::OpenMenu => {
                self.menu.open(self.divider.rect, self.split.get_rect());
                return;
            }
        }
        self.resolve_height();
    }
    fn resolve_height(&mut self) -> bool {
        let height = self.split.get_rect().height();
        if height <= 0.0 {
            return false;
        }
        let maximum = (height - 90.0).max(0.0);
        let wanted = self
            .requested
            .unwrap_or(self.measured)
            .clamp(self.minimum.min(maximum), maximum);
        let old = self.split.split_position();
        self.split.set_split_position((wanted / height) as f32);
        let changed = (old - self.split.split_position()).abs() > 0.0001;
        self.dirty |= changed;
        changed
    }
    fn drain(&mut self) {
        if let Some(command) = self.divider.take_command() {
            self.command(command);
        }
        if let Some(index) = self.menu.take_selection() {
            self.command(match index {
                0 => QueryResizeCommand::Adjust(24.0),
                1 => QueryResizeCommand::Adjust(-24.0),
                _ => QueryResizeCommand::Reset,
            });
            self.menu.close();
        }
    }
}
impl ui::widget::Decorator for QuerySplit {
    type Inner = ResizableSplit<EditorPane, ResultsPane>;
    fn inner(&self) -> &Self::Inner {
        &self.split
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.split
    }
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.drain();
        self.split.sync(state, path);
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let split = self.split.build_layout(taffy, theme, nodes);
        let divider = self.divider.build_layout(taffy, theme, nodes);
        let menu = self.menu.build_layout(taffy, theme, nodes);
        for node in [divider, menu] {
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            let _ = taffy.set_style(node, style);
        }
        let node = taffy.parent_node(
            Style {
                display: Display::Flex,
                size: Size {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            },
            &[split, divider, menu],
        );
        ui::widget::decorator::register_node(nodes, node, self);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let at = Point::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
        );
        if let Some(&child) = taffy.children(node).unwrap_or_default().first() {
            self.split.apply_layout(taffy, child, at);
        }
        let r = self.split.get_rect();
        let y = r.y0 + r.height() * f64::from(self.split.split_position());
        self.divider.set_geometry(
            Rect::new(r.x0, y - 5.0, r.x1, y + 5.0),
            y - r.y0,
            self.requested.is_none(),
        );
        if self.menu.is_open() {
            self.menu.reanchor(self.divider.rect, r);
        }
    }
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let Some(split_node) = taffy
            .children(node)
            .ok()
            .and_then(|nodes| nodes.first().copied())
        else {
            return false;
        };
        let Ok(layout) = taffy.layout(split_node).copied() else {
            return false;
        };
        let width = f64::from(layout.size.width);
        let height = f64::from(layout.size.height);
        if width <= 0.0 || height <= 0.0 {
            return false;
        }
        if self.requested.is_none() {
            if let Some(editor) = self.split.children_mut().first_mut() {
                let size = editor.measure(
                    Size {
                        width: Some(width as f32),
                        height: None,
                    },
                    Size {
                        width: AvailableSpace::Definite(width as f32),
                        height: AvailableSpace::MaxContent,
                    },
                    text,
                );
                let line = editor
                    .as_any()
                    .and_then(|any| any.downcast_ref::<EditorPane>())
                    .map_or(18.0, EditorPane::line_height);
                self.measured = f64::from(size.height) + 2.0 * line;
                self.minimum = 3.0 * line;
            }
        }
        let maximum = (height - 90.0).max(0.0);
        let wanted = self
            .requested
            .unwrap_or(self.measured)
            .clamp(self.minimum.min(maximum), maximum);
        self.split.set_split_position((wanted / height) as f32);
        // This runs before apply_layout, including the first frame. Update the
        // existing pane allocations so the shared layout solver can settle now.
        let mut changed = false;
        for (index, child) in taffy
            .children(split_node)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            let ratio = if index == 0 {
                self.split.split_position()
            } else {
                1.0 - self.split.split_position()
            };
            if let Ok(mut style) = taffy.style(child).cloned() {
                if (style.flex_grow - ratio).abs() > 0.0001 {
                    style.flex_grow = ratio;
                    let _ = taffy.set_style(child, style);
                    changed = true;
                }
            }
        }
        changed
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty) | self.split.take_structure_dirty()
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        if self.menu.is_open() {
            if let Some(&node) = nodes.get(2) {
                self.menu
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            self.drain();
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button,
            ..
        } = event
            && self.divider.rect.contains(self.cursor)
        {
            if *button == MouseButton::Right {
                self.command(QueryResizeCommand::OpenMenu);
                *ctx.handled = true;
                return;
            }
            if *button == MouseButton::Left {
                self.divider.set_focus(true);
                *ctx.focus_target = nodes.get(1).copied();
            }
        }
        let before = self.split.split_position();
        if let Some(&node) = nodes.first() {
            self.split
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
        }
        if before != self.split.split_position() {
            self.requested =
                Some(self.split.get_rect().height() * f64::from(self.split.split_position()));
            self.dirty = true;
        }
        self.drain();
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.split.draw(scene, transform, text, theme, presence);
        self.divider.draw(scene, transform, text, theme, presence);
        self.menu.draw(scene, transform, text, theme, presence);
    }
    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.split
            .draw_with_presence(scene, transform, text, theme, presence, permissions);
        self.divider
            .draw_with_presence(scene, transform, text, theme, presence, permissions);
        self.menu
            .draw_with_presence(scene, transform, text, theme, presence, permissions);
    }
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.split
            .draw_overlay(scene, transform, text, theme, presence);
        self.menu
            .draw_overlay(scene, transform, text, theme, presence);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.split, &self.divider, &self.menu]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.split, &mut self.divider, &mut self.menu]
    }
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.split);
        visitor(&self.divider);
        visitor(&self.menu);
    }
    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open() || self.split.wants_pointer_capture()
    }
    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn name(&self) -> &'static str {
        "QuerySplit"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::headless::Driver;
    fn mounted(size: (u32, u32)) -> Driver {
        Driver::for_root(
            crate::app::sample_state(),
            QuerySplit::new(&Link::new()),
            size,
            Theme::dark(),
        )
    }
    fn divider(driver: &Driver) -> Rect {
        driver.targets()["db.query-divider"]
    }
    #[test]
    fn hosted_frame_paints_the_focused_query_divider() {
        // Use an otherwise unused focus color to identify the actual encoded
        // grip paint, without requiring a GPU or asserting incidental glyphs.
        let focus_color = ui::vello::peniko::Color::from_rgb8(17, 243, 119);
        let mut marker = Scene::new();
        marker.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            focus_color,
            None,
            &Rect::new(0.0, 0.0, 1.0, 1.0),
        );
        let focus_paint = marker.encoding().draw_data[0];
        let mut theme = Theme::dark();
        theme.border_focus = focus_color;
        let mut frame = ui::headless::FrameHarness::for_root(
            crate::app::sample_state(),
            QuerySplit::new(&Link::new()),
            (1000, 700),
            theme,
        );
        assert!(
            !frame
                .frame_output()
                .scene
                .encoding()
                .draw_data
                .contains(&focus_paint)
        );
        frame.click_named("db.query-divider").unwrap();
        assert!(frame.root::<QuerySplit>().divider.focused);
        assert!(
            frame
                .frame_output()
                .scene
                .encoding()
                .draw_data
                .contains(&focus_paint),
            "the native presence-aware frame must paint the focused divider grip"
        );
    }

    #[test]
    fn automatic_height_is_measured_on_the_first_frame_and_restored_after_reset() {
        let mut driver = mounted((1200, 800));
        let automatic = divider(&driver);
        assert!(
            automatic.center().y < 180.0,
            "the four-line fixture leaves room for results"
        );
        let from = automatic.center();
        driver.drag_at((from.x, from.y), (from.x, from.y + 240.0));
        let manual = divider(&driver);
        assert!((manual.center().y - automatic.center().y - 240.0).abs() < 1.0);
        let requested = driver.root_mut::<QuerySplit>().requested.unwrap();
        driver.resize((800, 300)).unwrap();
        assert!(divider(&driver).center().y < manual.center().y);
        driver.resize((1200, 800)).unwrap();
        assert_eq!(driver.root_mut::<QuerySplit>().requested, Some(requested));
        assert!((divider(&driver).center().y - manual.center().y).abs() < 1.0);
        driver.click_named("db.query-divider").unwrap();
        driver.key("Home");
        assert_eq!(driver.root_mut::<QuerySplit>().requested, None);
        assert!((divider(&driver).center().y - automatic.center().y).abs() < 1.0);
    }
    #[test]
    fn keyboard_and_semantic_menu_share_the_same_retained_divider() {
        let mut driver = mounted((1000, 700));
        let automatic = divider(&driver).center().y;
        driver.click_named("db.query-divider").unwrap();
        driver.key("ArrowDown");
        assert!((divider(&driver).center().y - automatic - 24.0).abs() < 1.0);
        driver.key("Enter");
        assert!(driver.root_mut::<QuerySplit>().menu.is_open());
        driver.resize((700, 500)).unwrap();
        let viewport = driver.root_mut::<QuerySplit>().get_rect();
        for name in [
            "db.increase-query-height",
            "db.decrease-query-height",
            "db.reset-query-height",
        ] {
            let r = driver.targets()[name];
            assert!(r.x0 >= viewport.x0 && r.x1 <= viewport.x1);
            assert!(r.y0 >= viewport.y0 && r.y1 <= viewport.y1);
        }
        driver
            .perform_semantic_named(
                "db.query-resize-menu",
                Some("db.reset-query-height"),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        assert!(!driver.root_mut::<QuerySplit>().menu.is_open());
        assert_eq!(driver.root_mut::<QuerySplit>().requested, None);
    }
}
