//! `Thumbnail` — a leaf widget that stands in for a video still.
//!
//! There is no network on the `--shots` path and no decoded JPEG to show,
//! so we paint a deterministic two-stop gradient derived from the video's
//! seed (so every tile is distinct but the wall stays harmonious), plus
//! the chrome a real thumbnail carries: a duration pill, an optional
//! watched-progress bar, and an optional LIVE badge. `DeArrow` corrections are
//! intentionally reflected in the chosen title/image themselves: putting a
//! provider badge on every ordinary card makes normal browsing feel like a
//! diagnostics screen.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill, Gradient, ImageData as PenikoImage};

use ui::ImageStore;
use ui::paint::with_alpha;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::colors::Colors;
use ui::widget::Widget;

use crate::util::{duration_hms, seed_gradient};

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent display toggle (live, hero) or render \
              cache (dirty), not a state machine"
)]
pub struct Thumbnail {
    seed: String,
    duration: u32,
    height: f32,
    live: bool,
    progress: Option<f32>,
    hero: bool,
    /// Real decoded thumbnail image (from the [`ui::ImageStore`]). When present
    /// it's painted cover-fit in place of the seed gradient; the chrome (pills,
    /// progress, badges) still overlays it.
    image: Option<PenikoImage>,
    /// Lazy source: the store + URL the image is resolved from in [`sync`], so a
    /// freshly-decoded thumbnail folds in on a redraw rather than forcing a
    /// whole-tree structural rebuild on every image completion.
    store: Option<ImageStore>,
    url: String,
    rect: Rect,
    dirty: bool,
}

/// Everything about a corner pill's look and placement, apart from its text.
#[derive(Clone, Copy)]
struct PillSpec {
    size: f32,
    bg: Color,
    fg: Color,
    anchor: Point,
    right_align: bool,
}

impl Thumbnail {
    pub fn new(seed: &str, duration: u32) -> Self {
        Self {
            seed: seed.to_string(),
            duration,
            height: 169.0,
            live: false,
            progress: None,
            hero: false,
            image: None,
            store: None,
            url: String::new(),
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// Supply a real decoded thumbnail image directly (or `None` to keep the
    /// gradient). Used by tests/`--shots` that inject a pre-decoded image.
    #[must_use]
    pub fn image(mut self, image: Option<PenikoImage>) -> Self {
        self.image = image;
        self
    }

    /// Resolve the thumbnail image **lazily** from `store` at `url` (looked up in
    /// [`sync`](crate::sync)). Empty `url` / `None` store keeps the seed gradient. This is the
    /// live path — it avoids the build-time `imgs.get()` that made every decode
    /// completion rebuild the whole widget tree.
    #[must_use]
    pub fn image_source(mut self, store: Option<ImageStore>, url: &str) -> Self {
        self.store = store;
        self.url = url.to_string();
        self
    }

    #[must_use]
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }
    #[must_use]
    pub fn live(mut self, on: bool) -> Self {
        self.live = on;
        self
    }
    #[must_use]
    pub fn progress(mut self, p: f32) -> Self {
        self.progress = Some(p.clamp(0.0, 1.0));
        self
    }
    #[must_use]
    pub fn hero(mut self) -> Self {
        self.hero = true;
        self
    }

    fn pill(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        text: &str,
        spec: &PillSpec,
    ) {
        let &PillSpec {
            size,
            bg,
            fg,
            anchor,
            right_align,
        } = spec;
        let (tw, _) = text_renderer.measure(text, size, None);
        let padx = 6.0;
        let pad_y = 3.0;
        let w = f64::from(tw) + padx * 2.0;
        let h = f64::from(size) + pad_y * 2.0;
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

    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over this tile's fixed chrome (gradient/image, \
                  duration/live pill, dearrow marker, progress bar); splitting it would only \
                  relocate the line count, not the amount a reader must hold in mind"
    )]
    fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let radius = 12.0;
        let shape = RoundedRect::from_rect(self.rect, radius);
        let chrome = ThumbChrome::for_theme(theme);

        if let Some(img) = &self.image {
            // Real thumbnail: paint the decoded image cover-fit, clipped to the
            // rounded tile (no gradient, no play glyph — like a YouTube card).
            ui::image_store::paint_image_cover(scene, transform, self.rect, img, radius);
        } else {
            // No image yet (loading / offline / sample): the deterministic
            // seed gradient + a faint geometric motif + a centre play glyph.
            let (top, bottom) = seed_gradient(&self.seed);
            let grad =
                Gradient::new_linear((self.rect.x0, self.rect.y0), (self.rect.x1, self.rect.y1))
                    .with_stops([(0.0_f32, top), (1.0_f32, bottom)]);
            scene.fill(Fill::NonZero, transform, &grad, None, &shape);

            // Clip the decorative motif to the rounded tile so the offset
            // circles never bleed past the thumbnail. Unclipped they are
            // invisible on a dark card but paint stray blobs on a light one.
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);
            let glass = chrome.motif_light;
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
                chrome.motif_dark,
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
                chrome.play_plate,
                None,
                &Circle::new(Point::new(cx, cy), r),
            );
            let mut tri = BezPath::new();
            let t = r * 0.5;
            tri.move_to((cx - t * 0.5, cy - t));
            tri.line_to((cx - t * 0.5, cy + t));
            tri.line_to((cx + t, cy));
            tri.close_path();
            scene.fill(Fill::NonZero, transform, chrome.play_glyph, None, &tri);
            scene.pop_layer();
        }

        // Duration / LIVE pill, bottom-right.
        if self.live {
            Self::pill(
                scene,
                transform,
                text_renderer,
                "LIVE",
                &PillSpec {
                    size: 11.0,
                    bg: chrome.live_bg,
                    fg: chrome.live_fg,
                    anchor: Point::new(self.rect.x1 - 8.0, self.rect.y1 - 26.0),
                    right_align: true,
                },
            );
        } else {
            Self::pill(
                scene,
                transform,
                text_renderer,
                &duration_hms(self.duration),
                &PillSpec {
                    size: 11.0,
                    bg: chrome.duration_bg,
                    fg: chrome.duration_fg,
                    anchor: Point::new(self.rect.x1 - 8.0, self.rect.y1 - 26.0),
                    right_align: true,
                },
            );
        }

        // Watched-progress bar at the bottom edge (YouTube-style red).
        if let Some(p) = self.progress {
            let track = Rect::new(self.rect.x0, self.rect.y1 - 4.0, self.rect.x1, self.rect.y1);
            scene.fill(
                Fill::NonZero,
                transform,
                chrome.progress_track,
                None,
                &track,
            );
            let fill = Rect::new(
                self.rect.x0,
                self.rect.y1 - 4.0,
                self.rect.x0 + self.rect.width() * f64::from(p),
                self.rect.y1,
            );
            scene.fill(Fill::NonZero, transform, chrome.progress_fill, None, &fill);
        }

        // Hairline border to seat it on the surface.
        scene.stroke(&Stroke::new(1.0), transform, chrome.edge, None, &shape);
    }
}

/// Chrome drawn on top of a thumbnail's media (decoded still or seed
/// gradient). LIVE / progress take the theme's danger; duration and the play
/// plate sit on the scrim so high contrast strengthens the wash and the ink
/// is measured against it.
struct ThumbChrome {
    motif_light: Color,
    motif_dark: Color,
    play_plate: Color,
    play_glyph: Color,
    live_bg: Color,
    live_fg: Color,
    duration_bg: Color,
    duration_fg: Color,
    progress_track: Color,
    progress_fill: Color,
    edge: Color,
}

impl ThumbChrome {
    fn for_theme(theme: &Theme) -> Self {
        let c = &theme.tokens.colors;
        let scrim = c.surfaces.scrim;
        let on_scrim = theme.on_surface(scrim);
        let (live_bg, live_fg) = c.legible_fill(c.semantic.danger, Color::WHITE);
        Self {
            motif_light: c.overlay_subtle,
            motif_dark: with_alpha(scrim, 24),
            play_plate: with_alpha(scrim, 110),
            play_glyph: on_scrim.ink,
            live_bg,
            live_fg,
            duration_bg: with_alpha(scrim, 200),
            duration_fg: on_scrim.ink,
            progress_track: c.overlay_medium,
            progress_fill: c.semantic.danger,
            edge: Colors::hairline_on(scrim, 51),
        }
    }
}

impl Widget for Thumbnail {
    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        // Lazily resolve the real thumbnail. Once we have it, stop touching the
        // store; a freshly-decoded image flips `dirty` so the next frame repaints
        // it — no structural rebuild.
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
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer, theme);
    }

    fn name(&self) -> &'static str {
        "Thumbnail"
    }
    /// Fixed intrinsic size (percent width, fixed height) — never changes
    /// after the first layout, so it doesn't force a taffy re-solve when an
    /// unrelated sibling (e.g. the enclosing scroll list) repositions.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
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
        thumb.paint(&mut scene, Affine::IDENTITY, &mut tr, &Theme::dark());
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::IMAGE)
            .count()
    }

    #[test]
    fn thumbnail_paints_image_or_gradient_fallback() {
        // Case `thumbnail_with_image_emits_an_image_draw`:
        // A thumbnail with a real decoded image paints an IMAGE draw command —
        // proof the fetched thumbnail actually reaches the scene (not the gradient).
        {
            let mut t = Thumbnail::new("seed", 120).image(Some(tiny_image()));
            assert!(
                paint_count_images(&mut t) >= 1,
                "expected an IMAGE draw for a real thumbnail"
            );
        }
        // Case `thumbnail_without_image_paints_gradient_not_image`:
        // Without an image the thumbnail paints the gradient poster (no IMAGE draw),
        // so the hermetic `--shots` path is unchanged.
        {
            let mut t = Thumbnail::new("seed", 120);
            assert_eq!(
                paint_count_images(&mut t),
                0,
                "no image → gradient poster, no IMAGE draw"
            );
        }
    }

    /// LIVE / progress / duration pills are chrome on media: they come from
    /// the theme, and the LIVE label clears body-text contrast on its fill
    /// under every shipped preset (including high contrast).
    #[test]
    fn thumbnail_chrome_follows_the_theme() {
        use ui::a11y::contrast_ratio;
        use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            let chrome = ThumbChrome::for_theme(&theme);
            let c = &theme.tokens.colors;
            assert_eq!(
                chrome.progress_fill, c.semantic.danger,
                "{name} progress is the danger token"
            );
            assert_eq!(
                chrome.live_bg,
                c.legible_fill(c.semantic.danger, Color::WHITE).0,
                "{name} LIVE fill is the danger chip"
            );
            assert!(
                contrast_ratio(chrome.live_fg, chrome.live_bg) >= ON_FILL_MIN_CONTRAST,
                "{name} LIVE label {:.2}:1",
                contrast_ratio(chrome.live_fg, chrome.live_bg)
            );
            assert!(
                contrast_ratio(chrome.duration_fg, c.surfaces.scrim) >= ON_FILL_MIN_CONTRAST,
                "{name} duration ink {:.2}:1 on scrim",
                contrast_ratio(chrome.duration_fg, c.surfaces.scrim)
            );
        }
    }
}
