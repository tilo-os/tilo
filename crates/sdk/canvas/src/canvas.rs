#![allow(
    clippy::arithmetic_side_effects,
    reason = "canvas geometry is kurbo float math (points, rects, affines), which cannot overflow or panic"
)]

use std::collections::HashMap;

use tilo_ui::a11y::AccessNode;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::taffy::{self, NodeId, Style, TaffyTree};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{EventCtx, TaffyTreeExt, Widget, layout_rect};
use tilo_ui::winit::event::WindowEvent;
use tilo_ui_paint::kurbo::{Affine, Point, Rect, Size};
use tilo_ui_paint::vello::Scene;

use crate::painter::CanvasPainter;

/// What changed on a canvas since it was last drawn, in its own coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasDamage {
    /// Nothing: the last frame still shows it.
    None,
    /// Only these regions.
    Regions(Vec<Rect>),
    /// The whole canvas.
    All,
}

/// Document or picture content a [`DocumentCanvas`] hosts.
pub trait CanvasContent {
    /// What assistive technology and tests call this canvas.
    fn name(&self) -> &str;

    /// Paint the content into `painter`, whose origin is the canvas's
    /// top-left corner.
    fn paint(&mut self, painter: &mut CanvasPainter<'_>);

    /// The size the content would like when its container lets it choose.
    /// `None` fills whatever the container gives.
    fn preferred_size(&self, _available: Size) -> Option<Size> {
        None
    }

    /// Input that reached the canvas. Pointer positions in `event` are in
    /// window coordinates; [`DocumentCanvas::to_canvas`] maps them.
    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {}

    /// What assistive technology reads for the content: its pages, cells or
    /// marks. The canvas places it at its own bounds when it has none.
    fn accessibility(&self) -> Option<AccessNode> {
        None
    }

    /// Whether the content takes keyboard focus.
    fn is_focusable(&self) -> bool {
        false
    }

    fn set_focus(&mut self, _focused: bool) {}

    /// What changed since [`Self::mark_drawn`] was last called.
    fn damage(&self) -> CanvasDamage {
        CanvasDamage::All
    }

    /// The frame that drew the content is on screen.
    fn mark_drawn(&mut self) {}
}

/// The component that draws document or picture content: it takes the box
/// layout gives it, clips its content to that box and hands it a
/// [`CanvasPainter`].
pub struct DocumentCanvas<C> {
    content: C,
    rect: Rect,
}

impl<C: CanvasContent> DocumentCanvas<C> {
    pub fn new(content: C) -> Self {
        Self {
            content,
            rect: Rect::ZERO,
        }
    }

    pub fn content(&self) -> &C {
        &self.content
    }

    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    /// `point` in window coordinates, in the canvas's own.
    #[must_use]
    pub fn to_canvas(&self, point: Point) -> Point {
        point - self.rect.origin().to_vec2()
    }

    fn to_window(&self, rect: Rect) -> Rect {
        rect + self.rect.origin().to_vec2()
    }
}

impl<C: CanvasContent + 'static> Widget for DocumentCanvas<C> {
    fn name(&self) -> &str {
        self.content.name()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let fills = self.content.preferred_size(Size::ZERO).is_none();
        let style = Style {
            size: taffy::Size {
                width: if fills {
                    taffy::Dimension::percent(1.0)
                } else {
                    taffy::Dimension::auto()
                },
                height: if fills {
                    taffy::Dimension::percent(1.0)
                } else {
                    taffy::Dimension::auto()
                },
            },
            min_size: taffy::Size::from_lengths(0.0, 0.0),
            flex_grow: if fills { 1.0 } else { 0.0 },
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let definite = |space: taffy::AvailableSpace| match space {
            taffy::AvailableSpace::Definite(value) => f64::from(value),
            _ => 0.0,
        };
        let offered = Size::new(definite(available.width), definite(available.height));
        let wanted = self.content.preferred_size(offered).unwrap_or(offered);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a canvas's logical size is far inside f32's range"
        )]
        taffy::Size {
            width: known.width.unwrap_or(wanted.width as f32),
            height: known.height.unwrap_or(wanted.height as f32),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.is_zero_area() {
            return;
        }
        let local = transform * Affine::translate(self.rect.origin().to_vec2());
        let mut painter = CanvasPainter::new(scene, local, text_renderer, theme, self.rect.size());
        let bounds = painter.bounds();
        painter.clipped(&bounds, |painter| self.content.paint(painter));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.content.handle_event(event, ctx);
    }

    fn is_focusable(&self) -> bool {
        self.content.is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.content.set_focus(focused);
    }

    fn accessibility(&self) -> Option<AccessNode> {
        self.content.accessibility()
    }

    fn needs_redraw(&self) -> bool {
        self.content.damage() != CanvasDamage::None
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        match self.content.damage() {
            CanvasDamage::None => {}
            CanvasDamage::All => regions.push(self.rect),
            CanvasDamage::Regions(changed) => regions.extend(
                changed
                    .into_iter()
                    .map(|rect| self.to_window(rect).intersect(self.rect))
                    .filter(|rect| !rect.is_zero_area()),
            ),
        }
    }

    fn mark_drawn(&mut self) {
        self.content.mark_drawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::{CanvasRole, DocColor, Ink};
    use tilo_ui_paint::kurbo::RoundedRect;

    /// A page on paper with a theme-coloured rule and a document-coloured mark.
    struct Page {
        damage: CanvasDamage,
        wants: Option<Size>,
    }

    impl Page {
        fn new() -> Self {
            Self {
                damage: CanvasDamage::All,
                wants: None,
            }
        }
    }

    impl CanvasContent for Page {
        fn name(&self) -> &'static str {
            "page"
        }

        fn paint(&mut self, painter: &mut CanvasPainter<'_>) {
            let bounds = painter.bounds();
            painter.fill(&bounds, Ink::from(CanvasRole::Paper));
            painter.fill(
                &RoundedRect::from_rect(Rect::new(10.0, 10.0, 40.0, 40.0), 4.0),
                Ink::from(DocColor::from_rgba8(200, 30, 30, 255)),
            );
            painter.clipped(&Rect::new(0.0, 0.0, 20.0, 20.0), |painter| {
                painter.fill(&bounds, Ink::from(CanvasRole::Rule));
            });
        }

        fn preferred_size(&self, _available: Size) -> Option<Size> {
            self.wants
        }

        fn damage(&self) -> CanvasDamage {
            self.damage.clone()
        }

        fn mark_drawn(&mut self) {
            self.damage = CanvasDamage::None;
        }
    }

    fn placed(content: Page, rect: Rect) -> DocumentCanvas<Page> {
        let mut canvas = DocumentCanvas::new(content);
        canvas.rect = rect;
        canvas
    }

    fn draw(canvas: &mut DocumentCanvas<Page>, theme: &Theme) -> Scene {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        canvas.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::default(),
        );
        scene
    }

    #[test]
    fn the_canvas_paints_theme_roles_in_the_theme_it_is_drawn_in() {
        let rect = Rect::new(100.0, 50.0, 300.0, 250.0);
        let dark = draw(&mut placed(Page::new(), rect), &Theme::dark());
        let light = draw(&mut placed(Page::new(), rect), &Theme::light());
        assert!(!dark.encoding_eq(&light), "paper and rule follow the theme");
        let dark_again = draw(&mut placed(Page::new(), rect), &Theme::dark());
        assert!(
            dark.encoding_eq(&dark_again),
            "the same theme paints the same"
        );
    }

    #[test]
    fn a_canvas_closes_every_layer_it_opens() {
        let scene = draw(
            &mut placed(Page::new(), Rect::new(0.0, 0.0, 120.0, 80.0)),
            &Theme::dark(),
        );
        assert_eq!(scene.encoding().n_open_clips, 0);
        assert!(
            scene.encoding().n_clips >= 2,
            "the canvas clip and the content's"
        );
    }

    #[test]
    fn an_unplaced_canvas_paints_nothing() {
        let scene = draw(&mut placed(Page::new(), Rect::ZERO), &Theme::dark());
        assert!(scene.encoding().draw_tags.is_empty());
    }

    #[test]
    fn damage_is_reported_in_window_coordinates_inside_the_canvas() {
        let mut canvas = placed(Page::new(), Rect::new(100.0, 50.0, 300.0, 250.0));
        let mut all = Vec::new();
        canvas.collect_damage(&mut all);
        assert_eq!(all, vec![canvas.get_rect()]);

        canvas.content_mut().damage = CanvasDamage::Regions(vec![
            Rect::new(10.0, 10.0, 20.0, 20.0),
            Rect::new(190.0, 190.0, 260.0, 260.0),
            Rect::new(500.0, 500.0, 510.0, 510.0),
        ]);
        let mut regions = Vec::new();
        canvas.collect_damage(&mut regions);
        assert_eq!(
            regions,
            vec![
                Rect::new(110.0, 60.0, 120.0, 70.0),
                Rect::new(290.0, 240.0, 300.0, 250.0),
            ],
            "regions move to the window and are clipped to the canvas; one outside it is dropped"
        );

        canvas.mark_drawn();
        assert!(!canvas.needs_redraw());
        let mut none = Vec::new();
        canvas.collect_damage(&mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn window_points_map_to_the_canvas_origin() {
        let canvas = placed(Page::new(), Rect::new(100.0, 50.0, 300.0, 250.0));
        assert_eq!(
            canvas.to_canvas(Point::new(110.0, 75.0)),
            Point::new(10.0, 25.0)
        );
    }

    #[test]
    fn a_canvas_fills_its_container_unless_its_content_has_a_size() {
        let mut text = TextRenderer::new();
        let offered = taffy::Size {
            width: taffy::AvailableSpace::Definite(640.0),
            height: taffy::AvailableSpace::Definite(480.0),
        };
        let unknown = taffy::Size {
            width: None,
            height: None,
        };
        let mut filling = DocumentCanvas::new(Page::new());
        assert_eq!(
            filling.measure(unknown, offered, &mut text),
            taffy::Size {
                width: 640.0,
                height: 480.0
            }
        );
        let mut sized = DocumentCanvas::new(Page {
            damage: CanvasDamage::All,
            wants: Some(Size::new(200.0, 100.0)),
        });
        assert_eq!(
            sized.measure(unknown, offered, &mut text),
            taffy::Size {
                width: 200.0,
                height: 100.0
            }
        );
    }
}
