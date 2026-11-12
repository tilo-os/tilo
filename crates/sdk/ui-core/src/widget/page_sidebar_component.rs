//! [`PageSidebar`] as a described component: a rail of page thumbnails.
//!
//! The rail is a list of pages: each thumbnail is the page's shape and its number, the page
//! open lifted. A rail a view gives pages renders them, and reports what the person did on it
//! (choose, reorder, copy, remove, add) as the view's events.

use taffy::Style;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use crate::component::{Component, Described, Event, Value};
use crate::widget::TaffyTreeExt;

use super::{PageSidebar, PageSidebarEvent};

/// A rail down a document's side: one numbered thumbnail per page, the page open lifted, and,
/// unless read-only, pages that can be dragged to reorder, copied, moved and added.
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
    /// The rail's title above the thumbnails; "Pages" in the window's language when
    /// unset.
    pub header: Option<String>,
    /// The label of the button that adds a page at the foot ("Import…"). Without it there is
    /// no add button.
    pub add: Option<String>,
    /// The rail paints its own panel (a document's whole side column). Without it it paints
    /// only the thumbnails, on the ground its host gives it.
    pub panel: bool,
    /// A page was chosen: carries its index, from 0.
    #[component(event(value = i64))]
    pub select: Event,
    /// A page was moved: carries its old index, then its place among the others.
    #[component(event(value = i64, at = i64))]
    pub reorder: Event,
    /// A page was to be copied: carries its index.
    #[component(event(value = i64))]
    pub duplicate: Event,
    /// A page was to be removed: carries its index.
    #[component(event(value = i64))]
    pub delete: Event,
    /// A page was to be added.
    #[component(event)]
    pub add_page: Event,
}

/// A rail driven from a view: its pages select, move, copy, go and are added through the
/// view's events.
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
}

impl DeclaredRail {
    /// The rail the props describe.
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

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        // Fills the side column it stands in.
        taffy.widget_leaf(
            Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
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

    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {}

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

/// The cursor the rail tracks, for a host that places it by hand.
#[allow(dead_code, reason = "a host that hit-tests the rail keeps its pointer")]
impl DeclaredRail {
    fn pointer(&mut self, at: Point) {
        self.cursor = at;
    }
}
