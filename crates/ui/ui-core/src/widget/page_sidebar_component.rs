//! [`PageSidebar`] as a described component: a rail of page thumbnails. The rail is a list of
//! pages: each thumbnail is the page's picture, where the view gives one, and its number, the
//! page open lifted.

use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::Value,
};

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
    /// Each page's picture, by address, in page order: what its thumbnail shows. A page with no
    /// address, or one whose picture is still loading, keeps its placeholder tile.
    pub pictures: Vec<String>,
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
    pub rail: PageSidebar,
    select: Event,
    reorder: Event,
    duplicate: Event,
    delete: Event,
    add: Event,
    count: usize,
    current: usize,
    /// Each page's picture, by address, as the view last gave them.
    pictures: Vec<String>,
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
            pictures: props.pictures.clone(),
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

impl tilo_ui_base::widget::Widget for DeclaredRail {
    fn needs_redraw(&self) -> bool {
        if self.rail.needs_redraw() || self.rail.needs_animation_frame() {
            return true;
        }
        // A page's picture on its way here keeps the rail painting frames, so the placeholder
        // becomes the page the moment its bytes are read.
        let pictures = crate::pictures::Pictures::window();
        self.pictures
            .iter()
            .filter(|address| !address.is_empty())
            .any(|address| {
                let source = crate::widget::PictureSource {
                    src: address.clone(),
                };
                pictures.loaded(&source).is_none() && !pictures.failed(&source)
            })
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

    fn sync(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) {
        self.report();
    }

    fn update(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        self.report()
    }

    /// A rail fills the height beside the viewer it stands with, at a rail's width; where
    /// nothing gives it a height (a column that grows with what it holds) it stands as tall as
    /// its pages, so the column grows to hold it rather than the rail painting over what follows.
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<
            taffy::NodeId,
            *mut dyn tilo_ui_base::widget::Widget,
        >,
    ) -> taffy::NodeId {
        self.rail.adopt_theme(theme);
        let width = theme.tokens.metrics.page_rail_width;
        let mut style = tilo_ui_base::widget::layout::natural_filling();
        style.size.width = taffy::Dimension::auto();
        style.max_size.width = taffy::Dimension::length(width);
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut tilo_ui_base::text_render::TextRenderer,
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
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        self.rail.begin_frame(self.rect, self.count, self.current);
        self.laid_out = Some(self.count);
    }

    fn is_layout_dirty(&self) -> bool {
        self.laid_out != Some(self.count)
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
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
                         _text: &mut tilo_ui_base::text_render::TextRenderer,
                         base: Affine,
                         i: usize,
                         rect: Rect| {
            let radius = f64::from(radii.detail);
            scene.fill(
                Fill::NonZero,
                base,
                tones.surfaces.raised,
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, radius),
            );
            // The page's own picture stands on the tile once it is here; until then the tile is
            // the placeholder it always was.
            if let Some(address) = self.pictures.get(i) {
                paint_page_picture(scene, base, rect, radius, address);
            }
        };
        self.rail.render(scene, transform, text, theme, &mut thumb);
    }

    /// A page pressed is chosen, a page dragged moves, a page's menu duplicates or deletes it,
    /// the wheel scrolls a rail taller than its box, and Escape lets go of a drag or a menu.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
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
                        -f64::from(*y) * tilo_ui_base::widget::wheel::line_of_wheel()
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

    fn tooltip_side(&self) -> tilo_ui_base::widget::TooltipSide {
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
        if props.pictures != self.pictures {
            self.pictures = props.pictures;
        }
        true
    }
}

/// Paint the page's picture at `address` on the tile `rect`, whole and centred, cut at the
/// tile's rounded corners. Nothing is painted while its bytes are on their way or it names
/// nothing: the tile stays the placeholder.
fn paint_page_picture(
    scene: &mut vello::Scene,
    base: Affine,
    rect: Rect,
    radius: f64,
    address: &str,
) {
    if address.is_empty() {
        return;
    }
    let source = crate::widget::PictureSource {
        src: address.to_owned(),
    };
    let Some(image) = crate::pictures::Pictures::window().get(&source) else {
        return;
    };
    let (wide, tall) = (f64::from(image.width), f64::from(image.height));
    if wide <= 0.0 || tall <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let scale = (rect.width() / wide).min(rect.height() / tall);
    let (drawn_w, drawn_h) = (wide * scale, tall * scale);
    let dest = Rect::new(
        rect.x0 + (rect.width() - drawn_w) / 2.0,
        rect.y0 + (rect.height() - drawn_h) / 2.0,
        rect.x0 + (rect.width() + drawn_w) / 2.0,
        rect.y0 + (rect.height() + drawn_h) / 2.0,
    );
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        base,
        &vello::kurbo::RoundedRect::from_rect(dest, radius),
    );
    let brush = vello::peniko::ImageBrush::new(image);
    let placement = Affine::translate((dest.x0, dest.y0)) * Affine::scale_non_uniform(scale, scale);
    scene.fill(Fill::NonZero, base, &brush, Some(placement), &dest);
    scene.pop_layer();
}
