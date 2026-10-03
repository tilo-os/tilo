//! `ImageBox` — a fixed-height leaf that paints a real decoded image cover-fit,
//! falling back to a deterministic seed gradient before the image loads.
//!
//! Used for the channel banner: full column width at a fixed height. When the
//! [`ui::ImageStore`] has decoded the banner URL it's painted cover-fit and
//! clipped to a rounded rect; until then (and on the offline/`--shots` path)
//! the seed gradient shows, so the header never reads as an empty box.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill, Gradient, ImageData as PenikoImage};

use ui::ImageStore;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::util::seed_gradient;

pub struct ImageBox {
    seed: String,
    height: f32,
    radius: f64,
    image: Option<PenikoImage>,
    /// Lazy source (store + URL), resolved in [`sync`] so a decoded banner folds
    /// in on a redraw without a structural rebuild.
    store: Option<ImageStore>,
    url: String,
    rect: Rect,
    dirty: bool,
}

impl ImageBox {
    /// A box `height` px tall, filling its column width. `image` paints
    /// cover-fit when present; otherwise the `seed` gradient shows.
    pub fn new(seed: &str, height: f32, radius: f64, image: Option<PenikoImage>) -> Self {
        Self {
            seed: seed.to_string(),
            height,
            radius,
            image,
            store: None,
            url: String::new(),
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// Resolve the image lazily from `store` at `url` (looked up in [`sync`](crate::sync)),
    /// instead of passing a pre-decoded image at build time.
    #[must_use]
    pub fn source(mut self, store: Option<ImageStore>, url: &str) -> Self {
        self.store = store;
        self.url = url.to_string();
        self
    }

    fn paint(&self, scene: &mut Scene, transform: Affine) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        if let Some(img) = &self.image {
            ui::image_store::paint_image_cover(scene, transform, self.rect, img, self.radius);
        } else {
            let (top, bottom) = seed_gradient(&self.seed);
            let shape = RoundedRect::from_rect(self.rect, self.radius);
            let grad =
                Gradient::new_linear((self.rect.x0, self.rect.y0), (self.rect.x1, self.rect.y1))
                    .with_stops([(0.0_f32, top), (1.0_f32, bottom)]);
            scene.fill(Fill::NonZero, transform, &grad, None, &shape);
            // Faint bottom vignette so overlaid header text stays legible.
            let veil = Color::from_rgba8(0, 0, 0, 30);
            scene.fill(Fill::NonZero, transform, veil, None, &shape);
        }
    }
}

impl Widget for ImageBox {
    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        if self.image.is_none()
            && !self.url.is_empty()
            && let Some(store) = &self.store
            && let Some(img) = store.get(&self.url)
        {
            self.image = Some(img);
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = super::leaf_node(
            taffy,
            Style {
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: length(self.height),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let new = super::layout_rect(taffy, node, origin);
        if new != self.rect {
            self.dirty = true;
        }
        self.rect = new;
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

    fn name(&self) -> &'static str {
        "ImageBox"
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
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
    use std::sync::Arc;
    use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format};
    use vello_encoding::DrawTag;

    fn paint(image: Option<PenikoImage>) -> Scene {
        let mut b = ImageBox::new("seed", 160.0, 12.0, image);
        b.rect = Rect::new(0.0, 0.0, 800.0, 160.0);
        let mut scene = Scene::new();
        b.paint(&mut scene, Affine::IDENTITY);
        scene
    }

    fn count(scene: &Scene, tag: DrawTag) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == tag)
            .count()
    }

    /// A real banner image paints an IMAGE draw; without one, only a COLOR
    /// gradient fill (no IMAGE), so the offline/`--shots` path is unchanged.
    #[test]
    fn banner_image_vs_gradient() {
        let blob = Blob::new(Arc::new(vec![128u8; 4 * 4 * 4]));
        let img = PenikoImage {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 4,
            height: 4,
        };
        assert!(
            count(&paint(Some(img)), DrawTag::IMAGE) >= 1,
            "real banner → IMAGE draw"
        );

        let gradient = paint(None);
        assert_eq!(
            count(&gradient, DrawTag::IMAGE),
            0,
            "no image → no IMAGE draw"
        );
        // The gradient fallback still paints (gradient + a solid COLOR veil).
        assert!(
            !gradient.encoding().draw_tags.is_empty(),
            "gradient fallback painted something"
        );
    }
}
