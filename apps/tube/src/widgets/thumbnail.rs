//! `Thumbnail` — a leaf widget that stands in for a video still.
//!
//! There is no network on the `--shots` path and no decoded JPEG to show,
//! so we paint a deterministic two-stop gradient derived from the video's
//! seed (so every tile is distinct but the wall stays harmonious), plus
//! the chrome a real thumbnail carries: a duration pill, an optional
//! watched-progress bar, an optional LIVE badge, and — when DeArrow is on
//! and has data — a small "DeArrow" marker noting the frame was swapped
//! for a community-chosen, non-clickbait one.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill, Gradient, ImageData as PenikoImage};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::util::{duration_hms, seed_gradient};

pub struct Thumbnail {
    seed: String,
    duration: u32,
    height: f32,
    dearrow: bool,
    live: bool,
    progress: Option<f32>,
    hero: bool,
    /// Real decoded thumbnail image (from the [`ui::ImageStore`]). When present
    /// it's painted cover-fit in place of the seed gradient; the chrome (pills,
    /// progress, badges) still overlays it.
    image: Option<PenikoImage>,
    rect: Rect,
}

impl Thumbnail {
    pub fn new(seed: &str, duration: u32) -> Self {
        Self {
            seed: seed.to_string(),
            duration,
            height: 169.0,
            dearrow: false,
            live: false,
            progress: None,
            hero: false,
            image: None,
            rect: Rect::ZERO,
        }
    }

    /// Supply a real decoded thumbnail image (or `None` to keep the gradient).
    pub fn image(mut self, image: Option<PenikoImage>) -> Self {
        self.image = image;
        self
    }

    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }
    pub fn dearrow(mut self, on: bool) -> Self {
        self.dearrow = on;
        self
    }
    pub fn live(mut self, on: bool) -> Self {
        self.live = on;
        self
    }
    pub fn progress(mut self, p: f32) -> Self {
        self.progress = Some(p.clamp(0.0, 1.0));
        self
    }
    pub fn hero(mut self) -> Self {
        self.hero = true;
        self
    }

    fn pill(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        text: &str,
        size: f32,
        bg: Color,
        fg: Color,
        anchor: Point,
        right_align: bool,
    ) {
        let (tw, _) = text_renderer.measure(text, size, None);
        let padx = 6.0;
        let pad_y = 3.0;
        let w = tw as f64 + padx * 2.0;
        let h = size as f64 + pad_y * 2.0;
        let x0 = if right_align { anchor.x - w } else { anchor.x };
        let pill = RoundedRect::new(x0, anchor.y, x0 + w, anchor.y + h, 4.0);
        scene.fill(Fill::NonZero, transform, bg, None, &pill);
        text_renderer.draw_text(
            scene,
            text,
            size,
            fg,
            transform * Affine::translate((x0 + padx, anchor.y + pad_y)),
            None,
        );
    }

    fn paint(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let radius = 12.0;
        let shape = RoundedRect::from_rect(self.rect, radius);

        if let Some(img) = &self.image {
            // Real thumbnail: paint the decoded image cover-fit, clipped to the
            // rounded tile (no gradient, no play glyph — like a YouTube card).
            ui::image_store::paint_image_cover(scene, transform, self.rect, img, radius);
        } else {
            // No image yet (loading / offline / sample): the deterministic
            // seed gradient + a faint geometric motif + a centre play glyph.
            let (top, bottom) = seed_gradient(&self.seed);
            let grad = Gradient::new_linear(
                (self.rect.x0, self.rect.y0),
                (self.rect.x1, self.rect.y1),
            )
            .with_stops([(0.0_f32, top), (1.0_f32, bottom)]);
            scene.fill(Fill::NonZero, transform, &grad, None, &shape);

            let glass = Color::from_rgba8(255, 255, 255, 18);
            scene.fill(
                Fill::NonZero,
                transform,
                glass,
                None,
                &Circle::new(Point::new(self.rect.x1 - 30.0, self.rect.y0 + 26.0), 40.0),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(0, 0, 0, 24),
                None,
                &Circle::new(Point::new(self.rect.x0 + 24.0, self.rect.y1 - 18.0), 52.0),
            );

            // Center play glyph.
            let cx = self.rect.center().x;
            let cy = self.rect.center().y;
            let r = if self.hero { 36.0 } else { 22.0 };
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(0, 0, 0, 110),
                None,
                &Circle::new(Point::new(cx, cy), r),
            );
            let mut tri = BezPath::new();
            let t = r * 0.5;
            tri.move_to((cx - t * 0.5, cy - t));
            tri.line_to((cx - t * 0.5, cy + t));
            tri.line_to((cx + t, cy));
            tri.close_path();
            scene.fill(Fill::NonZero, transform, Color::WHITE, None, &tri);
        }

        // Duration / LIVE pill, bottom-right.
        if self.live {
            self.pill(
                scene,
                transform,
                text_renderer,
                "LIVE",
                11.0,
                Color::from_rgb8(0xcc, 0x00, 0x00),
                Color::WHITE,
                Point::new(self.rect.x1 - 8.0, self.rect.y1 - 26.0),
                true,
            );
        } else {
            self.pill(
                scene,
                transform,
                text_renderer,
                &duration_hms(self.duration),
                11.0,
                Color::from_rgba8(0, 0, 0, 200),
                Color::WHITE,
                Point::new(self.rect.x1 - 8.0, self.rect.y1 - 26.0),
                true,
            );
        }

        // DeArrow marker, top-left.
        if self.dearrow {
            self.pill(
                scene,
                transform,
                text_renderer,
                "DeArrow",
                10.0,
                Color::from_rgb8(0x9b, 0x59, 0xb6),
                Color::WHITE,
                Point::new(self.rect.x0 + 8.0, self.rect.y0 + 8.0),
                false,
            );
        }

        // Watched-progress bar at the bottom edge (YouTube-style red).
        if let Some(p) = self.progress {
            let track = Rect::new(self.rect.x0, self.rect.y1 - 4.0, self.rect.x1, self.rect.y1);
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(255, 255, 255, 60),
                None,
                &track,
            );
            let fill = Rect::new(
                self.rect.x0,
                self.rect.y1 - 4.0,
                self.rect.x0 + self.rect.width() * p as f64,
                self.rect.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgb8(0xff, 0x00, 0x33),
                None,
                &fill,
            );
        }

        // Hairline border to seat it on the surface.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            Color::from_rgba8(255, 255, 255, 20),
            None,
            &shape,
        );
    }
}

impl Widget for Thumbnail {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: percent(1.0),
                    height: length(self.height),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer);
    }

    fn handle_event(
        &mut self,
        _e: &winit::event::WindowEvent,
        _sf: f64,
        _m: &winit::event::Modifiers,
        _s: &mut AutoCommit,
        _p: &ObjId,
        _h: &mut bool,
        _ft: &mut Option<NodeId>,
        _n: NodeId,
        _t: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str {
        "Thumbnail"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format};
    use vello_encoding::DrawTag;

    fn tiny_image() -> PenikoImage {
        // 2×2 opaque RGBA.
        let blob = Blob::new(Arc::new(vec![255u8; 2 * 2 * 4]));
        PenikoImage {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 2,
        }
    }

    fn paint_count_images(thumb: &mut Thumbnail) -> usize {
        let mut tr = ui::text_render::TextRenderer::new();
        let mut scene = Scene::new();
        thumb.rect = Rect::new(0.0, 0.0, 300.0, 169.0);
        thumb.paint(&mut scene, Affine::IDENTITY, &mut tr);
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::IMAGE)
            .count()
    }

    /// A thumbnail with a real decoded image paints an IMAGE draw command —
    /// proof the fetched thumbnail actually reaches the scene (not the gradient).
    #[test]
    fn thumbnail_with_image_emits_an_image_draw() {
        let mut t = Thumbnail::new("seed", 120).image(Some(tiny_image()));
        assert!(paint_count_images(&mut t) >= 1, "expected an IMAGE draw for a real thumbnail");
    }

    /// Without an image the thumbnail paints the gradient poster (no IMAGE draw),
    /// so the hermetic `--shots` path is unchanged.
    #[test]
    fn thumbnail_without_image_paints_gradient_not_image() {
        let mut t = Thumbnail::new("seed", 120);
        assert_eq!(paint_count_images(&mut t), 0, "no image → gradient poster, no IMAGE draw");
    }
}
