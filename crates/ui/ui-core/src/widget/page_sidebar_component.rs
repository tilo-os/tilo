//! [`PageSidebar`] as a described component: a rail of page thumbnails. The rail is a list of
//! pages: each thumbnail is the page's shape and its number, the page open lifted.

use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use crate::component::{Component, Described, Event, Value};
use crate::widget::TaffyTreeExt;

use super::{PageSidebar, PageSidebarEvent};

/// A rail of numbered page thumbnails down a document's side; unless read-only, pages are
/// dragged, copied, moved and added.
#[derive(Component, Default)]
#[component(name = "PageSidebar", role = "List")]
pub struct PageSidebarProps {
    /// A page's height over its width (`1.414` for A4, `792/612` for US Letter).
    #[component(required)]
    pub aspect: f64,
    /// How many pages the document has.
    #[component(required)]
    pub count: i64,
    /// Which page is open, counted from 0.
    #[component(required)]
    pub current: i64,
    /// The rail's title above the thumbnails; "Pages" in the window's language when unset.
    pub header: Option<String>,
    /// The label of the button that adds a page at the foot ("Import…"); without it there is no
    /// add button.
    pub add: Option<String>,
    /// The rail paints the document's whole side panel; without it it paints only the
    /// thumbnails, on the host's background.
    pub panel: bool,
    /// A page was chosen: its index, counted from 0, is the value.
    #[component(event(value = i64))]
    pub select: Event,
    /// A page was moved: its old index, then its place among the others.
    #[component(event(value = i64, at = i64))]
    pub reorder: Event,
    /// A page was to be copied: its index is the value.
    #[component(event(value = i64))]
    pub duplicate: Event,
    /// A page was to be removed: its index is the value.
    #[component(event(value = i64))]
    pub delete: Event,
    /// A page was to be added.
    #[component(event)]
    pub add_page: Event,
}

pub struct DeclaredRail {
    rail: PageSidebar,
    select: Event,
    reorder: Event,
    duplicate: Event,
    delete: Event,
    add: Event,
    count: usize,
    current: usize,
    rect: Rect,
    cursor: Point,
    /// How many pages it was last laid out for: its height where nothing gives it one.
    laid_out: Option<usize>,
}

impl DeclaredRail {
    #[must_use]
    pub fn new(props: &PageSidebarProps) -> Self {
        let mut rail = PageSidebar::new(props.aspect).with_panel(props.panel);
        if let Some(header) = &props.header {
            rail = rail.with_header(header.clone());
        }
        if let Some(add) = &props.add {
            rail = rail.with_add(add.clone());
        }
        Self {
            rail,
            select: props.select.clone(),
            reorder: props.reorder.clone(),
            duplicate: props.duplicate.clone(),
            delete: props.delete.clone(),
            add: props.add_page.clone(),
            count: usize::try_from(props.count.max(0)).unwrap_or(0),
            current: usize::try_from(props.current.max(0)).unwrap_or(0),
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            laid_out: None,
        }
    }

    /// Report what the person asked since the last frame, as the view's events; `true` when
    /// one fired.
    fn report(&mut self) -> bool {
        let mut fired = false;
        let index = |i: usize| Some(Value::Int(i64::try_from(i).unwrap_or(0)));
        for event in self.rail.take_events() {
            match event {
                PageSidebarEvent::Select(i) => {
                    self.select.fire(index(i));
                    fired = true;
                }
                PageSidebarEvent::Reorder { from, to } => {
                    self.reorder.fire(Some(Value::List(vec![
                        Value::Int(i64::try_from(from).unwrap_or(0)),
                        Value::Int(i64::try_from(to).unwrap_or(0)),
                    ])));
                    fired = true;
                }
                PageSidebarEvent::Duplicate(i) => {
                    self.duplicate.fire(index(i));
                    fired = true;
                }
                PageSidebarEvent::Delete(i) => {
                    self.delete.fire(index(i));
                    fired = true;
                }
                PageSidebarEvent::Add => {
                    self.add.fire(None);
                    fired = true;
                }
            }
        }
        fired
    }
}

impl crate::widget::Widget for DeclaredRail {
    fn needs_redraw(&self) -> bool {
        self.rail.needs_redraw() || self.rail.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.rail.mark_drawn();
    }

    fn needs_animation_frame(&self) -> bool {
        self.rail.needs_animation_frame()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, _state: &crate::widget::AppModel, _path: &crate::widget::ModelPath) {
        self.report();
    }

    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        self.report()
    }

    /// A rail fills the height beside the viewer it stands with, at a rail's width; where
    /// nothing gives it a height (a column that grows with what it holds) it stands as tall as
    /// its pages, so the column grows to hold it rather than the rail painting over what follows.
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        self.rail.adopt_theme(theme);
        let width = theme.tokens.metrics.page_rail_width;
        let mut style = crate::widget::layout::natural_filling();
        style.size.width = taffy::Dimension::auto();
        style.max_size.width = taffy::Dimension::length(width);
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut crate::text_render::TextRenderer,
    ) -> taffy::Size<f32> {
        let offered = match available.width {
            taffy::AvailableSpace::Definite(width) => f64::from(width),
            taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => {
                self.rail.natural_width()
            }
        };
        let width = known
            .width
            .map_or_else(|| offered.min(self.rail.natural_width()), f64::from);
        #[allow(clippy::cast_possible_truncation, reason = "a rail's measures fit f32")]
        taffy::Size {
            width: width as f32,
            height: known
                .height
                .unwrap_or(self.rail.natural_height(width, self.count) as f32),
        }
    }

    /// The rail stands where the component is laid out, painted or not: a page composes it
    /// from there without ever painting it.
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.rail.begin_frame(self.rect, self.count, self.current);
        self.laid_out = Some(self.count);
    }

    fn is_layout_dirty(&self) -> bool {
        self.laid_out != Some(self.count)
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.rail
            .begin_frame_with_theme(theme, self.rect, self.count, self.current);
        let tones = &theme.tokens.colors;
        let radii = &theme.tokens.radii;
        let mut thumb = |scene: &mut vello::Scene,
                         _text: &mut crate::text_render::TextRenderer,
                         base: Affine,
                         _i: usize,
                         rect: Rect| {
            let radius = f64::from(radii.detail);
            scene.fill(
                Fill::NonZero,
                base,
                tones.surfaces.raised,
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, radius),
            );
        };
        self.rail.render(scene, transform, text, theme, &mut thumb);
    }

    /// A page pressed is chosen, a page dragged moves, a page's menu duplicates or deletes it,
    /// the wheel scrolls a rail taller than its box, and Escape lets go of a drag or a menu.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};
        let changed = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = ctx.logical(position);
                self.rail.on_motion(self.cursor)
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let inside = self.rect.contains(self.cursor);
                match (state, button) {
                    (ElementState::Pressed, MouseButton::Left) if inside => {
                        self.rail.on_press(self.cursor)
                    }
                    (ElementState::Pressed, MouseButton::Right) if inside => {
                        self.rail.on_right_press(self.cursor)
                    }
                    (ElementState::Released, MouseButton::Left) => self.rail.on_release(),
                    _ => false,
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.rect.contains(self.cursor) => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        -f64::from(*y) * crate::widget::wheel::line_of_wheel()
                    }
                    MouseScrollDelta::PixelDelta(at) => -at.y / ctx.scale_factor,
                };
                self.rail.on_wheel(dy)
            }
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && key.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) =>
            {
                self.rail.on_escape()
            }
            _ => false,
        };
        let fired = self.report();
        if changed || fired {
            *ctx.handled = true;
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.rail.is_dragging() || self.rail.is_pending_drag()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.rail.tooltip_target_at(point)
    }

    fn tooltip_side(&self) -> crate::widget::tooltip::TooltipSide {
        self.rail.tooltip_side()
    }

    fn name(&self) -> &'static str {
        "PageSidebar"
    }
}

impl Described for DeclaredRail {
    type Props = PageSidebarProps;

    fn from_props(_key: &str, props: PageSidebarProps) -> Self {
        Self::new(&props)
    }

    fn set_props(&mut self, props: PageSidebarProps) -> bool {
        let (select, reorder, duplicate, delete) = (
            props.select.clone(),
            props.reorder.clone(),
            props.duplicate.clone(),
            props.delete.clone(),
        );
        self.rail.set_aspect(props.aspect);
        self.count = usize::try_from(props.count.max(0)).unwrap_or(0);
        self.current = usize::try_from(props.current.max(0)).unwrap_or(0);
        self.select = select;
        self.reorder = reorder;
        self.duplicate = duplicate;
        self.delete = delete;
        self.add = props.add_page;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Fired, Intent};

    /// A rail a view declares answers the pointer: a page pressed is the page the view is told
    /// was chosen.
    #[test]
    fn a_page_pressed_on_a_declared_rail_is_chosen() {
        let queue = crate::Intents::<Fired>::default();
        let rail = DeclaredRail::from_props(
            "r",
            PageSidebarProps {
                aspect: 1.414,
                count: 3,
                current: 0,
                select: Event::View(Intent::new(&queue, "r", "select")),
                ..PageSidebarProps::default()
            },
        );
        let mut harness = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(rail),
            (240, 900),
            crate::theme::Theme::light(),
        );
        fn find(widget: &dyn crate::widget::Widget) -> Option<&DeclaredRail> {
            widget
                .as_any()
                .and_then(|any| any.downcast_ref::<DeclaredRail>())
                .or_else(|| widget.children().into_iter().find_map(find))
        }
        let second = find(harness.root_widget())
            .and_then(|rail| rail.rail.thumb_rects().get(1).map(|(_, at)| *at))
            .expect("the second page's thumbnail");
        harness.click_at(second.center().x, second.center().y);
        let fired: Vec<_> = queue
            .take()
            .into_iter()
            .map(|fired| (fired.event, fired.value))
            .collect();
        assert!(
            fired.contains(&("select".to_owned(), Some(Value::Int(1)))),
            "the second page is chosen: {fired:?}"
        );
    }
}
