//! `VideoSurface` — paints a real decoded video frame as the watch-page player.
//!
//! The backend decodes H.264 off the UI thread and hands finished frames to the
//! app as toolkit-free `RGBA8` bytes ([`crate::api::DecodedFrame`]). This leaf
//! wraps a set of those bytes in a [`vello::peniko::ImageData`] and paints it
//! **cover-fit** (crop to fill, preserving aspect) into the same width-driven
//! 16:9 player rect the poster [`crate::widgets::Thumbnail`] would occupy, so
//! the watch page shows the real picture the moment a frame lands and falls back
//! to the poster until then.
//!
//! Tube-local on purpose: the frame plumbing is Tube's, and the toolkit's
//! `ui::widget::Image`/`VideoPlayer` size themselves differently (auto / grow),
//! so neither drops cleanly into the poster's `width:100% height:player_h` slot.
//! The `peniko::ImageData` is built once (lazily, on first draw) and reused.

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{
    BlendMode, Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage,
    ImageFormat as Format,
};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// Stable id so headless tests can assert the watch view took the real-frame
/// paint path (this surface present) rather than the poster.
pub const VIDEO_SURFACE_ID: &str = "watch_video_surface";

/// Corner radius, matched to [`crate::widgets::Thumbnail`] so swapping the
/// poster for the live frame doesn't change the player's silhouette.
const RADIUS: f64 = 12.0;

pub struct VideoSurface {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    player_h: f32,
    cached: Option<PenikoImage>,
    rect: Rect,
}

impl VideoSurface {
    /// Build a player surface for one decoded frame. `rgba` must be
    /// `width * height * 4` bytes (RGBA8); a mismatch paints only the backdrop.
    pub fn new(rgba: Vec<u8>, width: u32, height: u32, player_h: f32) -> Self {
        Self {
            rgba,
            width,
            height,
            player_h,
            cached: None,
            rect: Rect::ZERO,
        }
    }

    /// Wrap the raw RGBA in a reusable `peniko::ImageData` the first time it is
    /// needed. A malformed buffer (wrong length / zero dims) leaves the cache
    /// empty so the draw path shows the letterbox backdrop rather than garbage.
    fn ensure_cached(&mut self) {
        if self.cached.is_some() {
            return;
        }
        let expected = self.width as usize * self.height as usize * 4;
        if self.width == 0 || self.height == 0 || self.rgba.len() != expected {
            return;
        }
        let blob = Blob::new(Arc::new(self.rgba.clone()));
        self.cached = Some(PenikoImage {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: self.width,
            height: self.height,
        });
    }

    fn paint(&mut self, scene: &mut Scene, transform: Affine) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let shape = RoundedRect::from_rect(self.rect, RADIUS);

        // Letterbox backdrop: fills any pillar/letterbox area the cover-fit
        // doesn't reach (it shouldn't, but a rounded corner still needs a base)
        // and reads as a real player surface before the first frame caches.
        scene.fill(Fill::NonZero, transform, Color::BLACK, None, &shape);

        self.ensure_cached();
        let Some(img) = self.cached.clone() else {
            return;
        };

        let iw = self.width as f64;
        let ih = self.height as f64;
        let rw = self.rect.width();
        let rh = self.rect.height();
        if iw <= 0.0 || ih <= 0.0 {
            return;
        }

        // Cover-fit: scale by the LARGER ratio so the image fills the rect on
        // both axes, cropping the overflow on the long axis (clipped below).
        let scale = (rw / iw).max(rh / ih);
        let dw = iw * scale;
        let dh = ih * scale;
        let dx = self.rect.x0 + (rw - dw) * 0.5;
        let dy = self.rect.y0 + (rh - dh) * 0.5;
        let brush_transform = Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);

        // Clip to the rounded player rect so the oversized (cover) image crops
        // to the silhouette instead of bleeding past the corners.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);
        let brush = ImageBrush::new(img);
        scene.fill(
            Fill::NonZero,
            transform,
            &brush,
            Some(brush_transform),
            &self.rect,
        );
        scene.pop_layer();

        // Hairline border, matching the poster, to seat it on the surface.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            Color::from_rgba8(255, 255, 255, 20),
            None,
            &shape,
        );
    }
}

impl Widget for VideoSurface {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Same slot as the poster: full column width, fixed 16:9-derived height.
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: length(self.player_h),
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
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform);
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
        "VideoSurface"
    }

    fn test_id(&self) -> Option<&str> {
        Some(VIDEO_SURFACE_ID)
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_valid_rgba_into_a_reusable_image() {
        // A correctly-sized RGBA buffer must cache as a peniko image with the
        // frame's dimensions — the RGBA→GPU-image build the watch page paints.
        let (w, h) = (16u32, 8u32);
        let mut s = VideoSurface::new(vec![255u8; (w * h * 4) as usize], w, h, 90.0);
        s.ensure_cached();
        let img = s.cached.as_ref().expect("valid rgba caches an image");
        assert_eq!((img.width, img.height), (w, h));
    }

    #[test]
    fn malformed_rgba_does_not_cache() {
        // Wrong-length buffer must not build an image (the draw path then shows
        // the backdrop instead of sampling out of bounds).
        let mut s = VideoSurface::new(vec![0u8; 10], 16, 8, 90.0);
        s.ensure_cached();
        assert!(s.cached.is_none());
    }

    #[test]
    fn paint_with_frame_emits_an_image_draw() {
        // Drawing a surface with a real frame must emit an IMAGE draw command —
        // proof the decoded picture actually reaches the scene (not the poster).
        use vello_encoding::DrawTag;
        let (w, h) = (16u32, 16u32);
        let mut s = VideoSurface::new(vec![200u8; (w * h * 4) as usize], w, h, 90.0);
        s.rect = Rect::new(0.0, 0.0, 160.0, 90.0);
        let mut scene = Scene::new();
        s.paint(&mut scene, Affine::IDENTITY);
        let images = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::IMAGE)
            .count();
        assert!(images >= 1, "expected an IMAGE draw, got {images}");
    }
}
