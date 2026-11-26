//! [`PaintSurface`]: the one surface a person paints on, in every app that paints.
//!
//! It shows the picture of what is already there — the artwork's layers, composited by the engine
//! that owns them — and lays the stroke under the hand over it while it moves, through the same
//! brush code that bakes it when the hand lifts ([`tilo_paint_strokes`]). Pressure comes from a
//! pen where one reports it, and from the hand's speed otherwise; a pen's tilt turns the nib.
//! The view zooms and pans through the app that holds it, which the surface reports the wheel,
//! a pinch and a middle-button drag to.
//!
//! The surface takes no document: it reports where the pen went, in the artwork's own units, and
//! what a stroke means is the app's action.

use std::sync::Arc;

use taffy::TaffyTree;
use tilo_paint_strokes::{Brush, DabInput, Preset, dab, silhouette};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Shape as _};
use vello::peniko::{Blob, Color, Fill, ImageBrush};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::pictures::Pictures;
use crate::widget::PictureSource;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

/// The widest a stroke's brush may be, in artwork units.
const MAX_SIZE: f64 = 512.0;

/// The artwork as it stands and the stroke under the hand over it.
pub struct PaintSurface {
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) brush: Brush,
    pub(crate) size: f64,
    pub(crate) colour: Color,
    pub(crate) zoom: f64,
    pub(crate) pan: Point,
    pub(crate) label: Option<String>,
    pub(crate) picture: Option<PictureSource>,
    /// A picture laid over the artwork, such as a comparison's changed region.
    pub(crate) overlay: Option<PictureSource>,
    /// The frame the overlay fills, in artwork units; the whole artwork when absent.
    pub(crate) overlay_rect: Option<[f64; 4]>,
    pub(crate) overlay_opacity: f64,
    pub(crate) pictures: Option<Pictures>,
    pub(crate) rect: Rect,
    /// The stroke under the hand, in the artwork's units, its pressure with each point.
    stroke: Vec<Vec<f64>>,
    /// The last point reported, for the speed a mouse's pressure follows.
    last: Option<Point>,
    last_cursor: Point,
    dragging: Drag,
    /// The brush the stroke being drawn uses: the pen's tilt may turn its nib.
    live: Brush,
    dirty: bool,
    pub(crate) layout_dirty: bool,
    pub(crate) on_stroke_start: Option<Box<dyn FnMut(Vec<f64>)>>,
    pub(crate) on_stroke: Option<Box<dyn FnMut(Vec<f64>)>>,
    pub(crate) on_stroke_end: Option<Box<dyn FnMut(Vec<f64>)>>,
    pub(crate) on_hover: Option<Box<dyn FnMut(Vec<f64>)>>,
    pub(crate) on_wheel: Option<Box<dyn FnMut(Vec<f64>)>>,
    pub(crate) on_panned: Option<Box<dyn FnMut(Vec<f64>)>>,
    pub(crate) on_pinch: Option<Box<dyn FnMut(f64)>>,
}

/// What the pointer is doing on the surface.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Drag {
    #[default]
    None,
    Paint,
    Pan,
}

impl Default for PaintSurface {
    fn default() -> Self {
        Self {
            width: 1024.0,
            height: 768.0,
            brush: Brush::default(),
            size: 6.0,
            colour: Color::from_rgba8(29, 27, 25, 255),
            zoom: 1.0,
            pan: Point::ZERO,
            label: None,
            picture: None,
            overlay: None,
            overlay_rect: None,
            overlay_opacity: 1.0,
            pictures: None,
            rect: Rect::ZERO,
            stroke: Vec::new(),
            last: None,
            last_cursor: Point::ZERO,
            dragging: Drag::None,
            live: Brush::default(),
            dirty: false,
            layout_dirty: false,
            on_stroke_start: None,
            on_stroke: None,
            on_stroke_end: None,
            on_hover: None,
            on_wheel: None,
            on_panned: None,
            on_pinch: None,
        }
    }
}

impl PaintSurface {
    /// A brush of `preset`, `size` units wide: what a view drawing with one sets.
    #[must_use]
    pub fn sized(mut self, preset: Preset, size: f64) -> Self {
        self.brush = Brush::of(preset, &std::collections::BTreeMap::new(), None);
        self.size = size.clamp(0.25, MAX_SIZE);
        self
    }

    /// The pressure a hand lays with, at `speed` units a sample: a fast sweep thins, a slow one
    /// swells, and a pen that reports pressure says its own.
    fn pressure(&self, speed: f64) -> f64 {
        if let Some(pen) = tilo_ui_base::pen::current()
            && pen.pressure > 0.0
        {
            return pen.pressure.clamp(0.0, 1.0);
        }
        (1.0 - speed / (self.size.max(1.0) * 2.5)).clamp(0.3, 1.0)
    }

    /// The brush a stroke drawn now uses: the pen's tilt turns a calligraphy nib.
    fn live_brush(&self) -> Brush {
        let mut brush = self.brush;
        if brush.preset == Preset::Calligraphy
            && let Some(pen) = tilo_ui_base::pen::current()
            && (pen.tilt.0.abs() + pen.tilt.1.abs()) > 0.5
        {
            brush.angle = Some(90.0 - pen.tilt.0);
        }
        brush
    }

    /// The scale from artwork units to logical pixels, and where the view's top left stands in
    /// the surface.
    fn fit(&self) -> (f64, Point) {
        let base = (self.rect.width() / self.width.max(1e-6))
            .min(self.rect.height() / self.height.max(1e-6))
            .max(1e-6);
        (base * self.zoom.max(1e-6), self.rect.origin())
    }

    /// The transform from artwork units to the surface's logical pixels.
    fn artwork_to_screen(&self, transform: Affine) -> Affine {
        let (scale, origin) = self.fit();
        transform
            * Affine::translate((origin.x, origin.y))
            * Affine::translate((-self.pan.x, -self.pan.y))
            * Affine::scale(scale)
    }

    /// The artwork unit under `screen`, in the surface's logical coordinates.
    fn artwork_at(&self, screen: Point) -> Point {
        let (scale, origin) = self.fit();
        Point::new(
            self.pan.x + (screen.x - origin.x) / scale,
            self.pan.y + (screen.y - origin.y) / scale,
        )
    }

    /// Paint what is there and the stroke under the hand.
    fn paint(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        // The ground under a picture still on its way, so the surface reads as paper, not a hole.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.rect,
        );
        let to_screen = self.artwork_to_screen(transform);
        if let (Some(pictures), Some(picture)) = (&self.pictures, &self.picture)
            && let Some(image) = pictures.get(picture)
        {
            let (image_width, image_height) = (f64::from(image.width), f64::from(image.height));
            if image_width > 0.0 && image_height > 0.0 {
                let pixels_to_units =
                    Affine::scale_non_uniform(self.width / image_width, self.height / image_height);
                let brush = ImageBrush::new(image);
                scene.fill(
                    Fill::NonZero,
                    to_screen,
                    &brush,
                    Some(pixels_to_units),
                    &Rect::new(0.0, 0.0, self.width, self.height).to_path(0.1),
                );
            }
        }
        self.paint_overlay(scene, to_screen);
        self.paint_stroke(scene, to_screen);
        scene.pop_layer();
    }

    /// The picture laid over the artwork, such as a comparison's changed region, within its
    /// frame in the artwork's units.
    fn paint_overlay(&self, scene: &mut Scene, to_screen: Affine) {
        let (Some(pictures), Some(overlay)) = (&self.pictures, &self.overlay) else {
            return;
        };
        let Some(image) = pictures.get(overlay) else {
            return;
        };
        let opacity = self.overlay_opacity.clamp(0.0, 1.0);
        let [image_width, image_height] = [f64::from(image.width), f64::from(image.height)];
        if opacity <= 0.0 || image_width <= 0.0 || image_height <= 0.0 {
            return;
        }
        let [x, y, width, height] =
            self.overlay_rect
                .unwrap_or([0.0, 0.0, self.width, self.height]);
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let frame = Rect::new(x, y, x + width, y + height);
        let fit = Affine::scale_non_uniform(width / image_width, height / image_height);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an overlay's alpha is a fraction the scene's layer takes"
        )]
        let alpha = opacity as f32;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            to_screen,
            &frame,
        );
        scene.fill(
            Fill::NonZero,
            to_screen,
            &ImageBrush::new(image),
            Some(Affine::translate((x, y)) * fit),
            &frame.to_path(0.1),
        );
        scene.pop_layer();
    }

    /// The stroke under the hand, laid the way it will be baked.
    fn paint_stroke(&self, scene: &mut Scene, to_screen: Affine) {
        if self.stroke.is_empty() {
            return;
        }
        let pieces = [(self.stroke.clone(), self.size)];
        if self.live.preset.dabbed(&self.live.dynamics) {
            let (scale, _) = self.fit();
            let rgb = self.colour.to_rgba8();
            let Some(dabs) = dab(&DabInput {
                pieces: &pieces,
                brush: &self.live,
                scale: scale.clamp(0.5, 4.0),
                rgb: [rgb.r, rgb.g, rgb.b],
                tooth: &flat,
                to_artwork: Affine::IDENTITY,
            }) else {
                return;
            };
            if dabs.width == 0 || dabs.height == 0 {
                return;
            }
            let image =
                crate::raster::rgba8(Blob::new(Arc::new(dabs.pixels)), dabs.width, dabs.height);
            let pixels_to_units =
                Affine::translate((dabs.origin.x, dabs.origin.y)) * Affine::scale(1.0 / dabs.scale);
            let brush = ImageBrush::new(image);
            scene.fill(
                Fill::NonZero,
                to_screen,
                &brush,
                Some(pixels_to_units),
                &Rect::new(
                    dabs.origin.x,
                    dabs.origin.y,
                    dabs.origin.x + f64::from(dabs.width) / dabs.scale,
                    dabs.origin.y + f64::from(dabs.height) / dabs.scale,
                )
                .to_path(0.1),
            );
        } else {
            let path = silhouette(&pieces, &self.live);
            scene.fill(Fill::NonZero, to_screen, self.colour, None, &path);
        }
    }

    /// Where a point stands on the surface as `[x, y, pressure]`.
    fn point_at(&self, screen: Point, pressure: f64) -> Vec<f64> {
        let at = self.artwork_at(screen);
        vec![at.x, at.y, pressure]
    }
}

/// A ground with no tooth: a preview is laid as if on smooth paper.
fn flat(_: Point) -> f64 {
    0.5
}

impl Widget for PaintSurface {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let style = taffy::Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        self.paint(ctx.scene, ctx.transform, ctx.theme);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        let factor = ctx.scale_factor;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let here = Point::new(position.x / factor, position.y / factor);
                let previous = self.last_cursor;
                self.last_cursor = here;
                match self.dragging {
                    Drag::Paint => {
                        let at = self.artwork_at(here);
                        let speed = self.last.map_or(0.0, |last| (at - last).hypot());
                        self.last = Some(at);
                        let point = self.point_at(here, self.pressure(speed));
                        self.stroke.push(point.clone());
                        if let Some(stroke) = self.on_stroke.as_mut() {
                            stroke(point);
                        }
                        self.dirty = true;
                        *ctx.handled = true;
                    }
                    Drag::Pan => {
                        let (scale, _) = self.fit();
                        let (dx, dy) = (here.x - previous.x, here.y - previous.y);
                        if dx.abs() > 1e-9 || dy.abs() > 1e-9 {
                            self.last_cursor = here;
                            if let Some(panned) = self.on_panned.as_mut() {
                                panned(vec![-dx / scale, -dy / scale]);
                            }
                            self.dirty = true;
                        }
                        *ctx.handled = true;
                    }
                    Drag::None => {
                        if self.rect.contains(here) {
                            let at = self.artwork_at(here);
                            if let Some(hover) = self.on_hover.as_mut() {
                                hover(vec![at.x, at.y]);
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.last_cursor) => {
                let here = self.last_cursor;
                let at = self.artwork_at(here);
                self.dragging = Drag::Paint;
                self.last = Some(at);
                self.live = self.live_brush();
                let point = self.point_at(here, self.pressure(0.0));
                self.stroke = vec![point.clone()];
                if let Some(start) = self.on_stroke_start.as_mut() {
                    start(point);
                }
                self.dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } if self.rect.contains(self.last_cursor) => {
                self.dragging = Drag::Pan;
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.dragging == Drag::Paint => {
                self.dragging = Drag::None;
                let at = self.artwork_at(self.last_cursor);
                let pressure = self.stroke.last().map_or(1.0, |point| point[2]);
                let point = vec![at.x, at.y, pressure];
                if let Some(end) = self.on_stroke_end.as_mut() {
                    end(point);
                }
                self.stroke.clear();
                self.dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Middle,
                ..
            } if self.dragging == Drag::Pan => {
                self.dragging = Drag::None;
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } if self.rect.contains(self.last_cursor) => {
                let (across, down) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => {
                        (f64::from(*x) * 40.0, f64::from(*y) * 40.0)
                    }
                    winit::event::MouseScrollDelta::PixelDelta(delta) => (delta.x, delta.y),
                };
                if let Some(wheel) = self.on_wheel.as_mut() {
                    wheel(vec![across, down]);
                    *ctx.handled = true;
                }
            }
            WindowEvent::PinchGesture { delta, .. } if self.rect.contains(self.last_cursor) => {
                if let Some(pinch) = self.on_pinch.as_mut() {
                    pinch(1.0 + *delta);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "PaintSurface"
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
        Some(AccessNode::new(Role::Image).with_optional_name(self.label.clone()))
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.layout_dirty
    }

    /// The solve read the size the surface stands at: what it paints changed, not its box.
    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }
}

mod paint_surface_component;
pub use paint_surface_component::PaintSurfaceProps;

#[cfg(test)]
mod tests {
    use super::{PaintSurface, PaintSurfaceProps};
    use tilo_paint_strokes::Preset;
    use tilo_ui_base::component::Described;
    use vello::kurbo::{Point, Rect};

    /// A surface 100 by 50 units, showing it at twice its size from (10, 20).
    fn surface() -> PaintSurface {
        let mut surface = PaintSurface::from_props(
            "paint",
            PaintSurfaceProps {
                width: 100.0,
                height: 50.0,
                zoom: Some(2.0),
                pan: Some(vec![10.0, 20.0]),
                ..PaintSurfaceProps::default()
            },
        );
        surface.rect = Rect::new(0.0, 0.0, 200.0, 100.0);
        surface
    }

    #[test]
    fn the_surface_maps_the_hand_to_the_artwork() {
        let surface = surface();
        // The artwork fits twice into the box, zoomed twice again: four pixels to a unit.
        let (scale, origin) = surface.fit();
        assert!((scale - 4.0).abs() < 1e-9, "{scale}");
        assert_eq!(origin, Point::new(0.0, 0.0));
        let at = surface.artwork_at(Point::new(40.0, 20.0));
        assert_eq!(at, Point::new(20.0, 25.0));
        let back = surface.artwork_to_screen(vello::kurbo::Affine::IDENTITY) * at;
        assert_eq!(back, Point::new(40.0, 20.0));
    }

    #[test]
    fn props_read_the_brush_and_its_paint() {
        let surface = PaintSurface::from_props(
            "paint",
            PaintSurfaceProps {
                width: 100.0,
                height: 100.0,
                brush: Some("pencil".into()),
                size: Some(9.0),
                colour: Some("#ff0000".into()),
                ..PaintSurfaceProps::default()
            },
        );
        assert_eq!(surface.brush.preset, Preset::Pencil);
        assert!((surface.size - 9.0).abs() < 1e-9);
        assert_eq!(surface.colour.to_rgba8().r, 255);
        assert!((surface.zoom - 1.0).abs() < 1e-9);
    }
}
